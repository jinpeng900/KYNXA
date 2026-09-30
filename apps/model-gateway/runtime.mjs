import { createHash } from 'node:crypto';
import { join } from 'node:path';
import { atomicJson, readJson } from './store.mjs';
import { authorization, chatRequest, responseText } from './protocols.mjs';
import { readModelStream, StreamFailure } from './streaming.mjs';

/** KYNXA's text-chat transport. No external agent runtime or executable tools. */
export class ModelRuntime {
  constructor({ modelStore, dataHome, timeoutMs = 180000, idleTimeoutMs = timeoutMs, streamTimeoutMs = 900000 }) {
    this.store = modelStore;
    this.dataHome = dataHome;
    this.timeoutMs = timeoutMs;
    this.idleTimeoutMs = idleTimeoutMs;
    this.streamTimeoutMs = streamTimeoutMs;
    this.queues = new Map();
    this.shutdown = new AbortController();
  }

  async reply(input) {
    return this.enqueue(input, key => this.send(input, key));
  }

  async enqueue(input, send) {
    const key = createHash('sha256').update(JSON.stringify([
      input.conversationId, input.provider, input.model])).digest('hex');
    const previous = this.queues.get(key) ?? Promise.resolve();
    const operation = previous.catch(() => {}).then(() => send(key));
    this.queues.set(key, operation);
    try { return await operation; }
    finally { if (this.queues.get(key) === operation) this.queues.delete(key); }
  }

  async replyStream(input, emit, signal) {
    let content = '', reasoning = '';
    const receive = event => {
      if (event.type === 'text_delta') content += event.delta;
      if (event.type === 'reasoning_delta') reasoning += event.delta;
      emit(event);
    };
    const cancellation = signal ? AbortSignal.any([signal, this.shutdown.signal]) : this.shutdown.signal;
    let cancelQueued;
    let started = false;
    try {
      // A cancelled request need not wait for the preceding conversation turn.
      // Its queued operation remains in place and observes cancellation before
      // fetching, preserving order for any later requests.
      const operation = this.enqueue(input, key => {
        started = true;
        return this.sendStream(input, key, receive, signal);
      });
      const cancelled = new Promise((_, reject) => {
        cancelQueued = () => { if (!started) reject(new StreamFailure('已停止生成。', 'interrupted')); };
        if (cancellation.aborted) cancelQueued();
        else cancellation.addEventListener('abort', cancelQueued, { once: true });
      });
      return await Promise.race([operation, cancelled]);
    }
    catch (error) {
      // Raw network/provider/filesystem errors never cross the gateway boundary.
      const failure = error instanceof StreamFailure ? error
        : new StreamFailure('模型调用失败，已保留生成的内容。');
      failure.content = content; failure.reasoning = reasoning;
      throw failure;
    }
    finally { if (cancelQueued) cancellation.removeEventListener('abort', cancelQueued); }
  }

  async sendStream({ message, provider, model, requestId }, key, emit, clientSignal) {
    const idle = new AbortController();
    const lifetime = new AbortController();
    const timeout = setTimeout(() => lifetime.abort(), this.streamTimeoutMs);
    let idleTimer;
    const activity = () => { clearTimeout(idleTimer); idleTimer = setTimeout(() => idle.abort(), this.idleTimeoutMs); };
    const signals = [this.shutdown.signal, idle.signal, lifetime.signal];
    if (clientSignal) signals.push(clientSignal);
    const signal = AbortSignal.any(signals);
    const throwIfCancelled = () => {
      if (clientSignal?.aborted) throw new StreamFailure('已停止生成。', 'interrupted');
      if (this.shutdown.signal.aborted) throw new StreamFailure('模型服务已停止，已保留生成的内容。', 'interrupted');
      if (idle.signal.aborted) throw new StreamFailure('模型长时间没有返回内容，已保留生成的内容。', 'interrupted');
      if (lifetime.signal.aborted) throw new StreamFailure('生成时间超过上限，已保留生成的内容。', 'interrupted');
    };
    try {
      throwIfCancelled();
      const connection = await this.store.connectionFor(provider);
      if (!connection || !connection.models.includes(model)) throw new StreamFailure('请先选择已配置的模型。');
      const filename = join(this.dataHome, 'sessions', `${key}.json`);
      const history = await readJson(filename, []);
      if (!Array.isArray(history)) throw new StreamFailure('模型会话记录无效。');
      const requestHash = createHash('sha256').update(JSON.stringify([message, provider, model])).digest('hex');
      const receipt = requestId && history.find(item => item.role === 'assistant' && item.requestId === requestId);
      if (receipt) {
        if (receipt.requestHash !== requestHash) throw new StreamFailure('请求 ID 已用于其他消息，请创建新请求。');
        return { content: receipt.content, reasoning: receipt.reasoning ?? '' };
      }
      const messages = [...history.slice(-100).map(({ role, content }) => ({ role, content })),
        { role: 'user', content: message }];
      const request = chatRequest(connection, model, messages, { stream: true });
      throwIfCancelled();
      activity();
      let response;
      try {
        response = await fetch(`${connection.baseUrl}${request.path}`, {
          method: 'POST', redirect: 'error',
          headers: { 'Content-Type': 'application/json', Accept: 'text/event-stream', ...authorization(connection) },
          body: JSON.stringify(request.body), signal
        });
      } catch {
        throwIfCancelled();
        throw new StreamFailure('无法连接模型服务，请检查网络和服务地址。');
      }
      activity();
      if (!response.ok) {
        await response.body?.cancel();
        const hint = ({ 401: '请检查 API Key', 403: '当前密钥没有访问权限',
          402: '请检查账号余额', 404: '请检查服务地址与模型 ID', 429: '请求频繁或额度不足，请稍后重试' })[response.status];
        throw new StreamFailure(`模型服务返回 HTTP ${response.status}${hint ? `，${hint}` : ''}。`);
      }
      const result = await readModelStream(response, connection.protocol, emit, activity);
      throwIfCancelled();
      if (!result.content.trim()) throw new StreamFailure('模型没有返回文本内容。');
      // Only successfully terminated answers enter future context, once, under
      // the same per-conversation lock used by the non-streaming API.
      clearTimeout(idleTimer); clearTimeout(timeout);
      await atomicJson(filename, [...history, { role: 'user', content: message },
        { role: 'assistant', content: result.content,
          ...(requestId ? { requestId, requestHash, reasoning: result.reasoning } : {}) }].slice(-100));
      return result;
    } catch (error) {
      throwIfCancelled();
      throw error;
    } finally { clearTimeout(idleTimer); clearTimeout(timeout); }
  }

  async send({ message, provider, model }, key) {
    const connection = await this.store.connectionFor(provider);
    if (!connection || !connection.models.includes(model)) throw new Error('请先选择已配置的模型。');
    const filename = join(this.dataHome, 'sessions', `${key}.json`);
    const history = await readJson(filename, []);
    if (!Array.isArray(history)) throw new Error('模型会话记录无效。');
    const messages = [...history.slice(-100).map(({ role, content }) => ({ role, content })), { role: 'user', content: message }];
    const request = chatRequest(connection, model, messages);
    let response;
    try {
      response = await fetch(`${connection.baseUrl}${request.path}`, {
        method: 'POST', redirect: 'error',
        headers: { 'Content-Type': 'application/json', ...authorization(connection) },
        body: JSON.stringify(request.body),
        signal: AbortSignal.any([this.shutdown.signal, AbortSignal.timeout(this.timeoutMs)])
      });
    } catch (error) {
      if (error.name === 'TimeoutError') throw new Error('模型响应超时，请稍后重试。');
      if (this.shutdown.signal.aborted) throw new Error('模型服务已停止。');
      throw new Error('无法连接模型服务，请检查网络和服务地址。');
    }
    if (!response.ok) {
      await response.body?.cancel();
      const hint = ({ 401: '请检查 API Key', 403: '当前密钥没有访问权限',
        402: '请检查账号余额', 404: '请检查服务地址与模型 ID', 429: '请求频繁或额度不足，请稍后重试' })[response.status];
      throw new Error(`模型服务返回 HTTP ${response.status}${hint ? `，${hint}` : ''}。`);
    }
    let result;
    try { result = await response.json(); }
    catch { throw new Error('模型接口返回了无效的 JSON 响应。'); }
    const content = responseText(connection.protocol, result);
    if (typeof content !== 'string' || !content.trim()) throw new Error('模型没有返回文本内容。');
    await atomicJson(filename, [...history, { role: 'user', content: message }, { role: 'assistant', content }].slice(-100));
    return content;
  }

  async close() {
    this.shutdown.abort();
    await Promise.allSettled([...this.queues.values()]);
  }
}

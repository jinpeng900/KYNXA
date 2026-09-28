import { createHash } from 'node:crypto';
import { join } from 'node:path';
import { atomicJson, readJson } from './store.mjs';
import { authorization, chatRequest, responseText } from './protocols.mjs';

/** KYNXA's text-chat transport. No external agent runtime or executable tools. */
export class ModelRuntime {
  constructor({ modelStore, dataHome, timeoutMs = 180000 }) {
    this.store = modelStore;
    this.dataHome = dataHome;
    this.timeoutMs = timeoutMs;
    this.queues = new Map();
    this.shutdown = new AbortController();
  }

  async reply(input) {
    const key = createHash('sha256').update(JSON.stringify([
      input.conversationId, input.provider, input.model])).digest('hex');
    const previous = this.queues.get(key) ?? Promise.resolve();
    const operation = previous.catch(() => {}).then(() => this.send(input, key));
    this.queues.set(key, operation);
    try { return await operation; }
    finally { if (this.queues.get(key) === operation) this.queues.delete(key); }
  }

  async send({ message, provider, model }, key) {
    const connection = await this.store.connectionFor(provider);
    if (!connection || !connection.models.includes(model)) throw new Error('请先选择已配置的模型。');
    const filename = join(this.dataHome, 'sessions', `${key}.json`);
    const history = await readJson(filename, []);
    if (!Array.isArray(history)) throw new Error('模型会话记录无效。');
    const messages = [...history.slice(-100), { role: 'user', content: message }];
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
    await atomicJson(filename, [...messages, { role: 'assistant', content }].slice(-100));
    return content;
  }

  async close() {
    this.shutdown.abort();
    await Promise.allSettled([...this.queues.values()]);
  }
}

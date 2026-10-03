import { createHash, randomUUID } from 'node:crypto';
import { ConversationStore, validateId } from './conversations.mjs';
import { authorization, chatRequest, responseText } from './protocols.mjs';
import { readModelStream, StreamFailure } from './streaming.mjs';
import { MemoryService } from './memory-service.mjs';
import { buildContext, ContextError, estimateTokens } from './context.mjs';

import { ToolService } from './tool-service.mjs';
import { SandboxRunner } from './sandbox-runner.mjs';
import { wireCatalog, toolDeclarations, decodeToolTurn } from './tool-protocols.mjs';
import { readToolStream } from './tool-streaming.mjs';
import { runToolLoop, toolPolicyHash } from './tool-loop.mjs';

// Compatibility helper for older callers. ModelRuntime uses buildContext below.
// Failed attempts are visible in the transcript but excluded from model context.
// Only the request is bounded; durable history is never truncated.
export function completedContext(messages, beforeUserId) {
  const result = [];
  let user;
  for (const item of messages) {
    if (item.Id === beforeUserId) break;
    if (item.Role === 'user') user = item;
    else if (item.Role === 'assistant' && item.Status === 'completed' && item.Content?.trim() && user) {
      result.push({ role: 'user', content: user.Content }, { role: 'assistant', content: item.Content });
      user = undefined;
    }
  }
  return result.slice(-100);
}

/** Transport adapters consume the same conversation log the desktop displays. */
export class ModelRuntime {
  constructor({ modelStore, dataHome, conversationStore, memoryService, toolService, timeoutMs = 180000, idleTimeoutMs = timeoutMs, streamTimeoutMs = 900000 }) {
    this.store = modelStore;
    this.conversations = conversationStore ?? new ConversationStore({ dataHome });
    this.memory = memoryService ?? new MemoryService({ conversationStore: this.conversations });
    this.tools = toolService ?? new ToolService({ conversationStore: this.conversations, dataHome,
      sandboxRunner: new SandboxRunner({ excludedRoots: [dataHome, this.conversations.root].filter(Boolean) }) });
    this.timeoutMs = timeoutMs;
    this.idleTimeoutMs = idleTimeoutMs;
    this.streamTimeoutMs = streamTimeoutMs;
    this.queues = new Map();
    this.shutdown = new AbortController();
  }

  async enqueue(input, send) {
    const key = validateId(input.conversationId).toLowerCase();
    const previous = this.queues.get(key) ?? Promise.resolve();
    const operation = previous.catch(() => {}).then(() => send(key));
    this.queues.set(key, operation);
    try { return await operation; }
    finally { if (this.queues.get(key) === operation) this.queues.delete(key); }
  }

  async prepare(input, id) {
    await this.conversations.ensureConversation(id, { title: input.message.slice(0, 24) });
    const history = await this.conversations.readMessages(id);
    const requestId = validateId(input.requestId ?? randomUUID());
    const hash = createHash('sha256').update(JSON.stringify([input.message, input.provider, input.model])).digest('hex');
    const toolContext = input.permissionMode == null ? null : await this.tools.createContext(id,
      { requestId, permissionMode: input.permissionMode, message: input.message });
    const policyHash = toolContext ? toolPolicyHash(toolContext) : null;
    const previous = history.find(item => item.Id === requestId && item.Role === 'assistant');
    if (previous && (previous.RequestHash !== hash || (previous.ToolPolicyHash && previous.ToolPolicyHash !== policyHash)))
      throw new StreamFailure('请求 ID 已用于其他消息或权限，请创建新请求。');
    if (previous?.Status === 'completed') {
      if (previous.RequestHash !== hash) throw new StreamFailure('请求 ID 已用于其他消息，请创建新请求。');
      return { receipt: { content: previous.Content, reasoning: previous.Reasoning ?? '' } };
    }
    if (previous?.ToolActivities?.length)
      throw Object.assign(new StreamFailure('此请求已有工具执行记录，请检查结果后发送新消息；不能自动重做。', 'interrupted'), { code: 'TOOL_RETRY_REQUIRES_NEW_REQUEST' });
    const userId = validateId(input.userMessageId ?? previous?.ReplyTo ?? randomUUID());
    const user = history.find(item => item.Id === userId);
    if (user && (user.Role !== 'user' || user.Content !== input.message))
      throw new StreamFailure('用户消息 ID 已用于其他内容。');
    if (history.some(item => item.Id === requestId && item.Role !== 'assistant'))
      throw new StreamFailure('请求 ID 无效。');
    const createdAt = new Date().toISOString();
    await this.conversations.upsertMessage(id, user ?? {
      Id: userId, Role: 'user', Content: input.message, Status: 'completed', CreatedAt: createdAt
    });
    const assistant = { Id: requestId, Role: 'assistant', Content: '', Reasoning: '',
      Status: 'streaming', Error: '', Provider: input.provider, Model: input.model,
      CreatedAt: previous?.CreatedAt ?? createdAt, RequestHash: hash, ReplyTo: userId, ReasoningDurationMs: 0, ...(toolContext ? { ToolPolicyHash: policyHash, ToolActivities: [] } : {}) };
    await this.conversations.upsertMessage(id, assistant);
    try {
      await this.memory.captureExplicit(id, userId, input.message);
      const memory = await this.memory.contextFor(id);
      const summary = await this.memory.repository.readSummary(id);
      // Freeze one validated connection for this turn. A settings edit during
      // MCP discovery must not mix one provider's schemas with another protocol.
      const connection = Object.freeze(structuredClone(await this.connection(input)));
      const catalog = toolContext ? wireCatalog(await this.tools.catalog(toolContext, { connectMcp: true })) : [];
      const declarations = toolDeclarations(connection?.protocol, catalog);
      const toolSystem = catalog.length ? await this.tools.systemPrompt(toolContext) : '';
      const context = buildContext({ conversationId: id,
        projectId: memory.isFolderlessWorkspace ? null : memory.projectId,
        history, currentMessage: input.message, beforeUserId: userId,
        memoryEntries: memory.entries, summary, contextWindowTokens: connection?.contextWindowTokens,
        additionalSystem: toolSystem, reservedInputTokens: declarations.length ? estimateTokens(JSON.stringify(declarations)) : 0 });
      if (context.summaryUpdate) await this.memory.repository.writeSummary(id, context.summaryUpdate);
      return { assistant, messages: context.messages, connection, toolContext, catalog, declarations,
        inputBudgetTokens: context.metrics.inputBudgetTokens,
        requestOptions: { system: context.system, maxOutputTokens: context.maxOutputTokens } };
    } catch (error) {
      const publicError = error instanceof ContextError || error.code?.includes('MEMORY') || error.code?.includes('SUMMARY');
      const failure = publicError
        ? Object.assign(new StreamFailure(error.message), { code: error.code, statusCode: error.statusCode })
        : safeFailure(error);
      await this.conversations.upsertMessage(id, { ...assistant, Status: 'error', Error: failure.message });
      throw failure;
    }
  }

  async connection(input) {
    const connection = await this.store.connectionFor(input.provider);
    if (!connection || !connection.models.includes(input.model)) throw new StreamFailure('请先选择已配置的模型。');
    return connection;
  }

  async reply(input) {
    return this.enqueue(input, async id => {
      const turn = await this.prepare(input, id);
      if (turn.receipt) return turn.receipt.content;
      try {
        const connection = turn.connection;
        if (turn.catalog.length) {
          const signal = AbortSignal.any([this.shutdown.signal, AbortSignal.timeout(this.streamTimeoutMs)]);
          let content = '', reasoning = '';
          const receive = event => {
            if (event.type === 'text_delta') content += event.delta;
            if (event.type === 'reasoning_delta') reasoning += event.delta;
            if (event.type === 'content_snapshot') { content = event.content; reasoning = event.reasoning; }
            turn.assistant.Content = content; turn.assistant.Reasoning = reasoning;
          };
          const result = await runToolLoop({ protocol: connection.protocol, messages: turn.messages,
            system: turn.requestOptions.system, declarations: turn.declarations, inputBudgetTokens: turn.inputBudgetTokens,
            context: turn.toolContext, service: this.tools, signal, interactive: false, emit: receive,
            onRoundComplete: result => receive({ type: 'content_snapshot', ...result }),
            saveActivity: activity => this.saveToolActivity(id, turn, activity),
            requestTurn: async messages => {
              const request = chatRequest(connection, input.model, messages, { ...turn.requestOptions, tools: turn.declarations });
              const response = await fetch(connection.baseUrl + request.path, { method: 'POST', redirect: 'error',
                headers: { 'Content-Type': 'application/json', ...authorization(connection) },
                body: JSON.stringify(request.body), signal: AbortSignal.any([signal, AbortSignal.timeout(this.timeoutMs)]) });
              await checkResponse(response);
              const result = decodeToolTurn(connection.protocol, await response.json(), turn.catalog);
              receive({ type: 'text_delta', delta: result.content });
              receive({ type: 'reasoning_delta', delta: result.reasoning });
              return result;
            } });
          if (!result.content.trim()) throw new StreamFailure('模型没有返回文本内容。');
          await this.conversations.upsertMessage(id, { ...turn.assistant, Content: result.content, Reasoning: result.reasoning, Status: 'completed' });
          return result.content;
        }
        const request = chatRequest(connection, input.model, turn.messages, turn.requestOptions);
        let response;
        try {
          response = await fetch(connection.baseUrl + request.path, {
            method: 'POST', redirect: 'error',
            headers: { 'Content-Type': 'application/json', ...authorization(connection) },
            body: JSON.stringify(request.body),
            signal: AbortSignal.any([this.shutdown.signal, AbortSignal.timeout(this.timeoutMs)])
          });
        } catch (error) {
          if (error.name === 'TimeoutError') throw new StreamFailure('模型响应超时，请稍后重试。');
          if (this.shutdown.signal.aborted) throw new StreamFailure('模型服务已停止。', 'interrupted');
          throw new StreamFailure('无法连接模型服务，请检查网络和服务地址。');
        }
        await checkResponse(response);
        let result;
        try { result = await response.json(); }
        catch { throw new StreamFailure('模型接口返回了无效的 JSON 响应。'); }
        const content = responseText(connection.protocol, result);
        if (typeof content !== 'string' || !content.trim()) throw new StreamFailure('模型没有返回文本内容。');
        await this.conversations.upsertMessage(id, { ...turn.assistant, Content: content, Status: 'completed' });
        return content;
      } catch (error) {
        const failure = safeFailure(error);
        try { await this.conversations.upsertMessage(id, { ...turn.assistant, Status: failure.type, Error: failure.message }); }
        catch { throw new StreamFailure('回复未能保存：记录已删除或存储位置不可用。'); }
        throw failure;
      } finally { if (turn.toolContext) await this.tools.releaseContext(turn.toolContext); }
    });
  }

  async replyStream(input, emit, signal) {
    let content = '', reasoning = '';
    const receive = event => {
      if (event.type === 'text_delta') content += event.delta;
      if (event.type === 'reasoning_delta') reasoning += event.delta;
      if (event.type === 'content_snapshot') { content = event.content; reasoning = event.reasoning; }
      emit(event);
    };
    const cancellation = signal ? AbortSignal.any([signal, this.shutdown.signal]) : this.shutdown.signal;
    let cancelQueued, started = false;
    try {
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
    } catch (error) {
      const failure = safeFailure(error);
      failure.content = content; failure.reasoning = reasoning;
      throw failure;
    } finally { if (cancelQueued) cancellation.removeEventListener('abort', cancelQueued); }
  }

  async sendStream(input, id, emit, clientSignal) {
    const idle = new AbortController(), lifetime = new AbortController();
    const timeout = setTimeout(() => lifetime.abort(), this.streamTimeoutMs);
    let idleTimer, turn, checkpoint, checkpointError;
    let content = '', reasoning = '', thinkingStarted, thinkingDuration = 0, lastSave = Date.now();
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
    const snapshot = () => ({ ...turn.assistant, Content: content, Reasoning: reasoning,
      ReasoningDurationMs: thinkingDuration + (thinkingStarted ? Date.now() - thinkingStarted : 0) });
    const receive = event => {
      if (event.type === 'content_snapshot') {
        if (event.content === content && event.reasoning === reasoning) return;
        content = event.content; reasoning = event.reasoning;
      }
      if (event.type === 'reasoning_delta') { thinkingStarted ??= Date.now(); reasoning += event.delta; }
      if (event.type === 'text_delta') {
        if (thinkingStarted) { thinkingDuration += Date.now() - thinkingStarted; thinkingStarted = undefined; }
        content += event.delta;
      }
      emit(event);
      if (!checkpoint && Date.now() - lastSave >= 1500) {
        lastSave = Date.now();
        checkpoint = this.conversations.upsertMessage(id, snapshot())
          .catch(error => { checkpointError = error; lifetime.abort(); })
          .finally(() => { checkpoint = undefined; });
      }
    };
    try {
      throwIfCancelled();
      turn = await this.prepare(input, id);
      if (turn.receipt) return turn.receipt;
      const connection = turn.connection;
      if (turn.catalog.length) {
        const result = await runToolLoop({ protocol: connection.protocol, messages: turn.messages,
          system: turn.requestOptions.system, declarations: turn.declarations, inputBudgetTokens: turn.inputBudgetTokens,
          context: turn.toolContext, service: this.tools, signal, interactive: true, emit: receive,
          onRoundComplete: result => receive({ type: 'content_snapshot', ...result }),
          saveActivity: async tool => {
            await checkpoint;
            if (checkpointError) throw new StreamFailure('当前回复未能保存，请检查存储位置。');
            turn.assistant = snapshot();
            await this.saveToolActivity(id, turn, tool);
          },
          requestTurn: async messages => {
            const request = chatRequest(connection, input.model, messages, { ...turn.requestOptions, stream: true, tools: turn.declarations });
            throwIfCancelled(); activity();
            try {
              const response = await fetch(connection.baseUrl + request.path, { method: 'POST', redirect: 'error',
                headers: { 'Content-Type': 'application/json', Accept: request.body.stream ? 'text/event-stream' : 'application/json', ...authorization(connection) },
                body: JSON.stringify(request.body), signal });
              activity(); await checkResponse(response);
              return await readToolStream(response, connection.protocol, turn.catalog, receive, activity);
            } finally { clearTimeout(idleTimer); }
          } });
        throwIfCancelled(); clearTimeout(timeout); await checkpoint;
        if (checkpointError) throw new StreamFailure('当前回复未能保存，请检查存储位置。');
        if (!result.content.trim()) throw new StreamFailure('模型没有返回文本内容。');
        await this.conversations.upsertMessage(id, { ...snapshot(), Content: result.content, Reasoning: result.reasoning, Status: 'completed' });
        return result;
      }
      const request = chatRequest(connection, input.model, turn.messages, { ...turn.requestOptions, stream: true });
      throwIfCancelled();
      activity();
      let response;
      try {
        response = await fetch(connection.baseUrl + request.path, {
          method: 'POST', redirect: 'error',
          headers: { 'Content-Type': 'application/json',
            Accept: request.body.stream ? 'text/event-stream' : 'application/json', ...authorization(connection) },
          body: JSON.stringify(request.body), signal
        });
      } catch {
        throwIfCancelled();
        throw new StreamFailure('无法连接模型服务，请检查网络和服务地址。');
      }
      activity();
      await checkResponse(response);
      const result = await readModelStream(response, connection.protocol, receive, activity);
      throwIfCancelled();
      if (!result.content.trim()) throw new StreamFailure('模型没有返回文本内容。');
      clearTimeout(idleTimer); clearTimeout(timeout);
      await checkpoint;
      if (checkpointError) throw new StreamFailure('当前回复未能保存，请检查存储位置。');
      await this.conversations.upsertMessage(id, { ...snapshot(), Content: result.content,
        Reasoning: result.reasoning, Status: 'completed' });
      return result;
    } catch (error) {
      try { throwIfCancelled(); } catch (cancelled) { error = cancelled; }
      await checkpoint;
      let failure = checkpointError ? new StreamFailure('当前回复未能保存，请检查存储位置。') : safeFailure(error);
      if (turn?.assistant) {
        try { await this.conversations.upsertMessage(id, { ...snapshot(), Status: failure.type, Error: failure.message }); }
        catch { failure = new StreamFailure('回复已中断；记录已删除或存储位置不可用。', 'interrupted'); }
      }
      throw failure;
    } finally {
      clearTimeout(idleTimer); clearTimeout(timeout);
      if (turn?.toolContext) await this.tools.releaseContext(turn.toolContext);
    }
  }

  async saveToolActivity(id, turn, activity) {
    const items = (turn.assistant.ToolActivities ?? []).filter(item => item.toolCallId !== activity.toolCallId);
    turn.assistant = { ...turn.assistant, ToolActivities: [...items, activity] };
    await this.conversations.upsertMessage(id, turn.assistant);
  }

  async close() {
    this.shutdown.abort();
    await this.tools.close();
    await Promise.allSettled([...this.queues.values()]);
  }
}

function safeFailure(error) {
  return error instanceof StreamFailure ? error : new StreamFailure('模型调用失败，请检查服务与数据存储位置。');
}

async function checkResponse(response) {
  if (response.ok) return;
  await response.body?.cancel();
  const hint = ({ 401: '请检查 API Key', 403: '当前密钥没有访问权限',
    402: '请检查账号余额', 404: '请检查服务地址与模型 ID', 429: '请求频繁或额度不足，请稍后重试' })[response.status];
  throw new StreamFailure('模型服务返回 HTTP ' + response.status + (hint ? '，' + hint : '') + '。');
}

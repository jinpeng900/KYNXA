import { createHash, randomUUID } from 'node:crypto';
import { dirname, resolve } from 'node:path';
import { ensureExtensionLayout, extensionControlPaths, extensionPointerPath } from './extension-storage.mjs';
import { ConversationStore, validateId } from './conversations.mjs';
import { authorization, chatRequest } from './protocols.mjs';
import { readModelStream, StreamFailure, finalParts, checkFinish } from './streaming.mjs';
import { OutputBudgetError } from './output-budget.mjs';
import { MemoryService } from './memory-service.mjs';
import { buildContext, ContextError, estimateTokens, estimateMessageTokens } from './context.mjs';

import { ToolService } from './tool-service.mjs';
import { SandboxRunner } from './sandbox-runner.mjs';
import { DesktopRunner } from './desktop-runner.mjs';
import { HostTerminalRunner } from './host-terminal-runner.mjs';
import { toolDeclarations, decodeToolTurn, estimateToolMessageTokens } from './tool-protocols.mjs';
import { readToolStream } from './tool-streaming.mjs';
import { runToolLoop, toolPolicyHash } from './tool-loop.mjs';
import { DEFAULT_TOOL_RUN_LIMITS, toolRunLimits } from './tool-run.mjs';
import { applyAssistantSegmentEvent, assistantSegmentText } from './assistant-segments.mjs';
import { ModelHistoryProjection, MODEL_HISTORY_NOTICE } from './model-history.mjs';
import { appendModelRound, modelOrigin, modelPrefixFingerprint, nativeContinuation } from './model-transcript.mjs';
import { resolveModelCapabilities } from './model-capabilities.mjs';
import { replyDurationMs } from './reply-timing.mjs';

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
  return result;
}

/** Transport adapters consume the same conversation log the desktop displays. */
export class ModelRuntime {
  constructor({ modelStore, dataHome, extensionRoot, conversationStore, memoryService, toolService, timeoutMs = 180000, idleTimeoutMs = timeoutMs, streamTimeoutMs = DEFAULT_TOOL_RUN_LIMITS.maxDurationMs }) {
    this.store = modelStore;
    this.conversations = conversationStore ?? new ConversationStore({ dataHome });
    this.extensionRoot = resolve(extensionRoot ?? toolService?.extensionRoot ?? this.conversations.root);
    this.memory = memoryService ?? new MemoryService({ conversationStore: this.conversations });
    this.tools = toolService ?? new ToolService({ conversationStore: this.conversations, dataHome, extensionRoot: this.extensionRoot,
      desktopRunner: new DesktopRunner(), hostTerminalRunner: new HostTerminalRunner(), sandboxRunner: new SandboxRunner({ conversationWorkspaceHome: dataHome, excludedRoots: [dataHome, this.conversations.root, this.extensionRoot,
        ...extensionControlPaths(extensionPointerPath()).map(path => dirname(path))].filter(Boolean) }) });
    this.timeoutMs = timeoutMs;
    this.idleTimeoutMs = idleTimeoutMs;
    this.streamTimeoutMs = streamTimeoutMs;
    this.queues = new Map();
    this.shutdown = new AbortController();
  }

  initializeExtensionStorage(options) {
    if (!this.extensionInitialization) this.extensionInitialization = ensureExtensionLayout(this.extensionRoot, options)
      .catch(error => { this.extensionInitialization = null; throw error; });
    return this.extensionInitialization;
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
    const limits = toolRunLimits(input.runLimits);
    const startedAtMonotonicMs = performance.now();
    await this.conversations.ensureConversation(id, { title: input.message.slice(0, 24) });
    const history = await this.conversations.readModelMessages(id);
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
      return { receipt: { content: previous.Content, reasoning: previous.Reasoning ?? '',
        durationMs: previous.DurationMs ?? 0,
        ...(previous.AssistantSegments?.length ? { assistantSegments: previous.AssistantSegments, toolStreamProtocol: 3 } : {}) } };
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
      CreatedAt: previous?.CreatedAt ?? createdAt, RequestHash: hash, ReplyTo: userId, ReasoningDurationMs: 0, DurationMs: 0,
      ...(toolContext ? { ToolPolicyHash: policyHash, ToolActivities: [], AssistantSegments: [] } : {}) };
    await this.conversations.upsertMessage(id, assistant);
    try {
      await this.memory.captureExplicit(id, userId, input.message);
      const memory = await this.memory.contextFor(id);
      const summary = await this.memory.repository.readSummary(id);
      // Freeze one validated connection for this turn. A settings edit during
      // MCP discovery must not mix one provider's schemas with another protocol.
      const connection = Object.freeze(structuredClone(await this.connection(input)));
      const capabilities = resolveModelCapabilities(connection, input.model);
      const contextInput = { conversationId: id,
        projectId: memory.isFolderlessWorkspace ? null : memory.projectId,
        history, currentMessage: input.message, beforeUserId: userId,
        memoryEntries: memory.entries, summary,
        contextWindowTokens: Math.min(connection.contextWindowTokens ?? capabilities.contextWindowTokens ?? 8192,
          capabilities.contextWindowTokens ?? Infinity), maxOutputTokens: connection.maxOutputTokens,
        providerMaxOutputTokens: capabilities.maxOutputTokens, providerMaxInputTokens: capabilities.maxInputTokens };
      let context = buildContext(contextInput), catalog = [], declarations = [];
      let toolSystem = '';
      if (toolContext) {
        await this.tools.catalog(toolContext, { connectMcp: true });
        const schemaReserve = Math.min(24000, Math.floor(context.metrics.inputBudgetTokens * .40));
        const maximumPromptTokens = Math.max(0, context.metrics.inputBudgetTokens - schemaReserve
          - estimateMessageTokens([{ role: 'user', content: input.message }]) - estimateMessageTokens([], MODEL_HISTORY_NOTICE) - 512);
        toolSystem = await this.tools.systemPrompt(toolContext, { maximumTokens: maximumPromptTokens });
        const tokenBudget = Math.min(24000, Math.floor(context.metrics.inputBudgetTokens * .40),
          context.metrics.inputBudgetTokens - estimateMessageTokens([{ role: 'user', content: input.message }])
            - estimateMessageTokens([], toolSystem + MODEL_HISTORY_NOTICE) - 512);
        const recentHistory = history.slice(-12);
        catalog = this.tools.configureModelCatalog(toolContext, { protocol: connection.protocol, tokenBudget, message: input.message,
          historySignals: recentHistory.filter(item => item.Role === 'user' && item.Id !== userId).slice(-3)
            .map(item => String(item.Content ?? '').slice(0, 1000)),
          previousToolNames: recentHistory.filter(item => item.Role === 'assistant').flatMap(item =>
            (item.ToolActivities ?? []).filter(activity => ['completed', 'error', 'unknown'].includes(activity.status)).map(activity => activity.name)).slice(-32) });
        declarations = toolDeclarations(connection.protocol, catalog);
      }
      const projection = new ModelHistoryProjection({ history, beforeUserId: userId, protocol: connection.protocol,
        resultStore: this.tools.results, resultContext: toolContext ?? { conversationId: id, requestId },
        inputBudgetTokens: context.metrics.inputBudgetTokens, availableTools: catalog });
      await projection.loadResults();
      const hasToolHistory = [...projection.records.values()].some(record => record.rounds.some(round => round.calls.length));
      const additionalSystem = [catalog.length ? toolSystem : '', hasToolHistory ? MODEL_HISTORY_NOTICE : ''].filter(Boolean).join('\n');
      const schemaTokens = estimateTokens(JSON.stringify(declarations));
      const historyBudget = context.metrics.inputBudgetTokens - estimateToolMessageTokens([{ role: 'user', content: input.message }], additionalSystem)
        - schemaTokens - estimateMessageTokens([], context.system);
      const historyCompaction = projection.compact({ inputBudgetTokens: historyBudget });
      const projectedInput = { ...contextInput, historyTurns: projection.historyTurns,
        projectTurn: item => projection.projectTurn(item), estimateContextMessages: estimateToolMessageTokens, additionalSystem };
      try { context = buildContext({ ...projectedInput, reservedInputTokens: schemaTokens }); }
      catch (error) {
        if (!catalog.length || (!(error instanceof ContextError) && !(error instanceof OutputBudgetError))) throw error;
        // Disabled/unavailable tools retain a portable low-trust chronology, never old executable schemas.
        catalog = []; declarations = []; projection.availableTools.clear();
        context = buildContext({ ...projectedInput, additionalSystem: hasToolHistory ? MODEL_HISTORY_NOTICE : '' });
      }
      if (context.summaryUpdate) await this.memory.repository.writeSummary(id, context.summaryUpdate);
      return { assistant, startedAtMonotonicMs, messages: context.messages, connection, toolContext, catalog, declarations, runLimits: limits,
        modelOrigin: modelOrigin(connection, { providerId: input.provider, model: input.model }),
        historySources: projection.historySources(context.messages, context.historySources),
        contextMetrics: { ...context.metrics, historyCompaction, toolCompactions: [] },
        inputBudgetTokens: context.metrics.inputBudgetTokens,
        requestOptions: { system: context.system, maxOutputTokens: context.maxOutputTokens } };
    } catch (error) {
      const publicError = error instanceof ContextError || error instanceof OutputBudgetError || error.code?.includes('MEMORY') || error.code?.includes('SUMMARY');
      const failure = publicError
        ? Object.assign(new StreamFailure(error.message), { code: error.code, statusCode: error.statusCode })
        : safeFailure(error);
      failure.durationMs = replyDurationMs(startedAtMonotonicMs);
      await this.conversations.upsertMessage(id, { ...assistant, DurationMs: failure.durationMs, Status: 'error', Error: failure.message });
      throw failure;
    }
  }

  async connection(input) {
    const connection = await this.store.connectionFor(input.provider);
    if (!connection || !connection.models.includes(input.model)) throw new StreamFailure('请先选择已配置的模型。');
    return connection;
  }

  async replyResult(input) { return this.reply(input, { includeTiming: true }); }

  async reply(input, { includeTiming = false } = {}) {
    return this.enqueue(input, async id => {
      const turn = await this.prepare(input, id);
      if (turn.receipt) return includeTiming ? { content: turn.receipt.content, durationMs: turn.receipt.durationMs } : turn.receipt.content;
      try {
        const connection = turn.connection;
        if (turn.catalog.length) {
          const signal = AbortSignal.any([this.shutdown.signal, AbortSignal.timeout(this.streamTimeoutMs)]);
          let content = '', reasoning = '';
          const receive = event => {
            if (applyAssistantSegmentEvent(turn.assistant, event)) {
              content = assistantSegmentText(turn.assistant); reasoning = assistantSegmentText(turn.assistant, 'reasoning');
            } else {
              if (event.type === 'text_delta') content += event.delta;
              if (event.type === 'reasoning_delta') reasoning += event.delta;
              if (event.type === 'content_snapshot') { content = event.content; reasoning = event.reasoning; }
            }
            turn.assistant.Content = content; turn.assistant.Reasoning = reasoning;
          };
          const result = await runToolLoop({ protocol: connection.protocol, messages: turn.messages,
            limits: turn.runLimits, saveRunState: state => this.saveRunState(id, turn, state),
            historySources: turn.historySources,
            onContextCompacted: metrics => turn.contextMetrics.toolCompactions.push(metrics),
            system: turn.requestOptions.system, declarations: turn.declarations, inputBudgetTokens: turn.inputBudgetTokens,
            context: turn.toolContext, service: this.tools, signal, interactive: false, emit: receive,
            onRoundComplete: result => receive({ type: 'content_snapshot', ...result }),
            declarationsForRound: () => toolDeclarations(connection.protocol, this.tools.modelCatalog(turn.toolContext)),
            saveActivity: activity => this.saveToolActivity(id, turn, activity),
            saveModelRound: step => this.saveModelRound(id, turn, step),
            requestTurn: async (messages, roundDeclarations, roundSignal, receiveTurn) => {
              const catalog = this.tools.modelCatalog(turn.toolContext);
              const request = chatRequest(connection, input.model, messages, { ...turn.requestOptions, tools: roundDeclarations });
              const response = await fetch(connection.baseUrl + request.path, { method: 'POST', redirect: 'error',
                headers: { 'Content-Type': 'application/json', ...authorization(connection) },
                body: JSON.stringify(request.body), signal: AbortSignal.any([roundSignal, AbortSignal.timeout(this.timeoutMs)]) });
              await checkResponse(response);
              const raw = await response.json();
              const parts = finalParts(connection.protocol, raw);
              receiveTurn({ type: 'reasoning_delta', delta: parts.reasoning });
              receiveTurn({ type: 'text_delta', delta: parts.content });
              return decodeToolTurn(connection.protocol, raw, catalog);
            } });
          if (!result.content.trim()) throw new StreamFailure('模型没有返回文本内容。');
          turn.assistant.DurationMs = replyDurationMs(turn.startedAtMonotonicMs);
          await this.conversations.upsertMessage(id, { ...turn.assistant, ...runMetadata(turn, 'completed'), Content: result.content, Reasoning: result.reasoning, Status: 'completed' });
          return includeTiming ? { content: result.content, durationMs: turn.assistant.DurationMs } : result.content;
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
        const parts = finalParts(connection.protocol, result);
        const content = parts.content;
        // Persist returned text even when the provider reports truncation.
        turn.assistant.Content = content;
        turn.assistant.Reasoning = parts.reasoning;
        checkFinish(parts.finish);
        if (typeof content !== 'string' || !content.trim()) throw new StreamFailure('模型没有返回文本内容。');
        turn.assistant.DurationMs = replyDurationMs(turn.startedAtMonotonicMs);
        await this.conversations.upsertMessage(id, { ...turn.assistant, Content: content, Status: 'completed' });
        return includeTiming ? { content, durationMs: turn.assistant.DurationMs } : content;
      } catch (error) {
        const failure = safeFailure(error);
        failure.durationMs = replyDurationMs(turn.startedAtMonotonicMs);
        turn.assistant.DurationMs = failure.durationMs;
        try { await this.conversations.upsertMessage(id, { ...turn.assistant, ...runMetadata(turn, failure.type, failure.code), Status: failure.type, Error: failure.message }); }
        catch { throw new StreamFailure('回复未能保存：记录已删除或存储位置不可用。'); }
        throw failure;
      } finally { if (turn.toolContext) await this.tools.releaseContext(turn.toolContext); }
    });
  }

  async replyStream(input, emit, signal) {
    let content = '', reasoning = '';
    const publicMessage = {};
    const receive = event => {
      if (applyAssistantSegmentEvent(publicMessage, event)) {
        content = assistantSegmentText(publicMessage); reasoning = assistantSegmentText(publicMessage, 'reasoning');
      } else {
        if (event.type === 'text_delta') content += event.delta;
        if (event.type === 'reasoning_delta') reasoning += event.delta;
        if (event.type === 'content_snapshot') { content = event.content; reasoning = event.reasoning; }
      }
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
      if (publicMessage.AssistantSegments?.length) failure.assistantSegments = publicMessage.AssistantSegments;
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
      DurationMs: replyDurationMs(turn.startedAtMonotonicMs),
      ReasoningDurationMs: thinkingDuration + (thinkingStarted ? Date.now() - thinkingStarted : 0) });
    const receive = event => {
      if (applyAssistantSegmentEvent(turn?.assistant ?? {}, event)) {
        content = assistantSegmentText(turn.assistant); reasoning = assistantSegmentText(turn.assistant, 'reasoning');
      } else if (event.type === 'content_snapshot') {
        if (event.content === content && event.reasoning === reasoning && !turn.assistant.AssistantSegments?.length) return;
        content = event.content; reasoning = event.reasoning;
      }
      if (event.type === 'reasoning_delta') { thinkingStarted ??= Date.now(); if (!event.segmentId) reasoning += event.delta; }
      if (event.type === 'text_delta') {
        if (thinkingStarted) { thinkingDuration += Date.now() - thinkingStarted; thinkingStarted = undefined; }
        if (!event.segmentId) content += event.delta;
      }
      if (event.type === 'assistant_segment' && event.segment.status !== 'streaming' && thinkingStarted) {
        thinkingDuration += Date.now() - thinkingStarted; thinkingStarted = undefined;
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
          limits: turn.runLimits, saveRunState: async state => {
            await checkpoint;
            if (checkpointError) throw new StreamFailure('当前回复未能保存，请检查存储位置。');
            turn.assistant = snapshot();
            await this.saveRunState(id, turn, state);
          },
          historySources: turn.historySources,
          onContextCompacted: metrics => turn.contextMetrics.toolCompactions.push(metrics),
          system: turn.requestOptions.system, declarations: turn.declarations, inputBudgetTokens: turn.inputBudgetTokens,
          context: turn.toolContext, service: this.tools, signal, interactive: true, emit: receive,
          onRoundComplete: result => receive({ type: 'content_snapshot', ...result }),
          declarationsForRound: () => toolDeclarations(connection.protocol, this.tools.modelCatalog(turn.toolContext)),
          saveActivity: async tool => {
            await checkpoint;
            if (checkpointError) throw new StreamFailure('当前回复未能保存，请检查存储位置。');
            turn.assistant = snapshot();
            await this.saveToolActivity(id, turn, tool);
          },
          saveModelRound: async step => {
            await checkpoint;
            if (checkpointError) throw new StreamFailure('当前回复未能保存，请检查存储位置。');
            turn.assistant = snapshot();
            await this.saveModelRound(id, turn, step);
          },
          requestTurn: async (messages, roundDeclarations, roundSignal, receiveTurn) => {
            const catalog = this.tools.modelCatalog(turn.toolContext);
            const request = chatRequest(connection, input.model, messages, { ...turn.requestOptions, stream: true, tools: roundDeclarations });
            throwIfCancelled(); activity();
            try {
              const response = await fetch(connection.baseUrl + request.path, { method: 'POST', redirect: 'error',
                headers: { 'Content-Type': 'application/json', Accept: request.body.stream ? 'text/event-stream' : 'application/json', ...authorization(connection) },
                body: JSON.stringify(request.body), signal: roundSignal });
              activity(); await checkResponse(response);
              return await readToolStream(response, connection.protocol, catalog, receiveTurn, activity);
            } finally { clearTimeout(idleTimer); }
          } });
        throwIfCancelled(); clearTimeout(timeout); await checkpoint;
        if (checkpointError) throw new StreamFailure('当前回复未能保存，请检查存储位置。');
        if (!result.content.trim()) throw new StreamFailure('模型没有返回文本内容。');
        const completed = { ...snapshot(), ...runMetadata(turn, 'completed'), Content: result.content, Reasoning: result.reasoning, Status: 'completed' };
        await this.conversations.upsertMessage(id, completed);
        return { ...result, durationMs: completed.DurationMs, contextUsage: turn.contextMetrics };
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
      const completed = { ...snapshot(), Content: result.content, Reasoning: result.reasoning, Status: 'completed' };
      await this.conversations.upsertMessage(id, completed);
      return { ...result, durationMs: completed.DurationMs, contextUsage: turn.contextMetrics };
    } catch (error) {
      try { throwIfCancelled(); } catch (cancelled) { error = cancelled; }
      await checkpoint;
      let failure = checkpointError ? new StreamFailure('当前回复未能保存，请检查存储位置。') : safeFailure(error);
      failure.durationMs = turn?.assistant ? replyDurationMs(turn.startedAtMonotonicMs) : error.durationMs ?? 0;
      if (turn?.assistant) {
        try { await this.conversations.upsertMessage(id, { ...snapshot(), DurationMs: failure.durationMs, ...runMetadata(turn, failure.type, failure.code), Status: failure.type, Error: failure.message }); }
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

  async saveModelRound(id, turn, { round, turn: modelTurn, messages, system, declarations }) {
    const continuation = nativeContinuation(turn.connection.protocol, modelTurn);
    const prefixFingerprint = modelPrefixFingerprint(messages, system, declarations);
    const nativeContinuationRef = continuation ? await this.tools.results.saveModelContinuation(
      turn.toolContext ?? { conversationId: id, requestId: turn.assistant.Id },
      { origin: turn.modelOrigin, prefixFingerprint, round, continuation }) : null;
    turn.assistant = { ...turn.assistant, ModelTranscript: appendModelRound(turn.assistant, turn.modelOrigin,
      { round, text: modelTurn.content ?? '', calls: modelTurn.calls ?? [],
        ...(nativeContinuationRef ? { nativeContinuationRef, prefixFingerprint } : {}) }) };
    // Commit the decoded round before dispatching effects. Results retain their one existing receipt owner.
    await this.conversations.upsertMessage(id, turn.assistant);
  }

  async saveRunState(id, turn, state) {
    turn.assistant = { ...turn.assistant, ToolRun: state };
    await this.conversations.upsertMessage(id, turn.assistant);
  }

  async close() {
    this.shutdown.abort();
    try { await this.tools.close(); }
    finally {
      // Even failed process teardown must wait for final receipts and context releases.
      await Promise.allSettled([...this.queues.values()]);
    }
  }
}

function runMetadata(turn, phase, code = null) {
  return turn.assistant.ToolRun ? { ToolRun: { ...turn.assistant.ToolRun, phase,
    updatedAt: new Date().toISOString(), code } } : {};
}

function safeFailure(error) {
  if (error instanceof StreamFailure) return error;
  if (['AbortError', 'TimeoutError'].includes(error?.name))
    return new StreamFailure('连续执行已停止，已保留生成内容和完成的工具记录。', 'interrupted');
  return new StreamFailure('模型调用失败，请检查服务与数据存储位置。');
}

async function checkResponse(response) {
  if (response.ok) return;
  await response.body?.cancel();
  const hint = ({ 401: '请检查 API Key', 403: '当前密钥没有访问权限',
    402: '请检查账号余额', 404: '请检查服务地址与模型 ID', 429: '请求频繁或额度不足，请稍后重试' })[response.status];
  throw new StreamFailure('模型服务返回 HTTP ' + response.status + (hint ? '，' + hint : '') + '。');
}

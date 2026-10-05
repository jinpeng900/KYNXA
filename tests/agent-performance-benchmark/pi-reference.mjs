import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { estimateToolMessageTokens, wireCatalog } from '../../apps/model-gateway/models/tool-protocols.mjs';
import { estimateTokens } from '../../apps/model-gateway/models/context-tokens.mjs';
import { canRunInParallel } from '../../apps/model-gateway/tools/tool-scheduling.mjs';
import { readUsage } from './evaluation.mjs';

export const PI_REFERENCE_VERSION = '1.0.3';
export const PI_REFERENCE_COMMIT = 'd78dc83d633229d12f8b79631384c4c2717c399f';
export const PI_REFERENCE_SDK_ROOT = fileURLToPath(new URL('../../artifacts/verification/agent-performance-benchmark/pi-sdk-v1.0.3/', import.meta.url));
const failure = code => Object.assign(new Error(code), { code });
const statusFor = result => result.status ?? (result.isError ? 'error' : 'completed');
const finalText = message => (message?.content ?? []).filter(block => block.type === 'text').map(block => block.text).join('');
const digest = text => createHash('sha256').update(text).digest('hex');
const emptyUsage = () => ({ input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0,
  cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } });

function nativeHistory(messages, model) {
  const toolNames = new Map();
  return messages.filter(message => message.role !== 'system').map(message => {
    const timestamp = Date.now();
    if (message.role === 'user' && typeof message.content === 'string') return { ...message, timestamp };
    if (message.role === 'assistant' && (typeof message.content === 'string' || message.content == null)) {
      const calls = (message.tool_calls ?? []).map(call => {
        const args = typeof call.function?.arguments === 'string' ? JSON.parse(call.function.arguments) : call.function?.arguments;
        if (!call.id || !call.function?.name || !args || typeof args !== 'object' || Array.isArray(args)) throw failure('INVALID_PI_REFERENCE_HISTORY');
        toolNames.set(call.id, call.function.name);
        return { type: 'toolCall', id: call.id, name: call.function.name, arguments: args };
      });
      return { role: 'assistant', content: [...(message.content ? [{ type: 'text', text: message.content }] : []),
        ...(typeof message.reasoning_content === 'string' && message.reasoning_content
          ? [{ type: 'thinking', thinking: message.reasoning_content, thinkingSignature: 'reasoning_content' }] : []), ...calls],
        api: model.api, provider: model.provider, model: model.id, usage: emptyUsage(), stopReason: calls.length ? 'toolUse' : 'stop', timestamp };
    }
    if (message.role === 'tool' && typeof message.content === 'string' && toolNames.has(message.tool_call_id))
      return { role: 'toolResult', toolCallId: message.tool_call_id, toolName: toolNames.get(message.tool_call_id),
        content: [{ type: 'text', text: message.content }], isError: message.isError === true, timestamp };
    throw failure('INVALID_PI_REFERENCE_HISTORY');
  });
}

/** Load the actual pinned SDK only; no CLI, personal auth/settings, discovered tools or extensions.
 * 只加载固定版本的真实 SDK，不初始化 CLI、个人凭据/设置、自动发现工具或扩展。 */
export async function createPiReference({ sdkRoot = PI_REFERENCE_SDK_ROOT, connection, model, service,
  catalog, systemPrompt = '', initialMessages = [], limits, signal, onToolActivity, inputBudgetTokens = 24576, maxOutputTokens = 8192 } = {}) {
  if (connection?.protocol !== 'openai-completions') throw failure('PI_REFERENCE_PROTOCOL_UNSUPPORTED');
  if (!connection.apiKey || !connection.baseUrl || !model || !service?.execute || !Array.isArray(catalog) ||
      typeof onToolActivity !== 'function' || !Array.isArray(initialMessages) || typeof systemPrompt !== 'string' ||
      !Number.isSafeInteger(inputBudgetTokens) || inputBudgetTokens < 1 ||
      !Number.isSafeInteger(maxOutputTokens) || maxOutputTokens < 1 || !limits ||
      !['maxRounds', 'maxToolCalls', 'maxGeneratedTokens', 'maxDurationMs'].every(key => Number.isSafeInteger(limits[key]) && limits[key] > 0))
    throw failure('INVALID_PI_REFERENCE_OPTION');
  const packageRoot = name => join(sdkRoot, 'node_modules', '@earendil-works', name);
  const versions = await Promise.all(['pi-agent-core', 'pi-ai', 'pi-telemetry'].map(async name =>
    JSON.parse(await readFile(join(packageRoot(name), 'package.json'), 'utf8')).version));
  if (versions.some(version => version !== PI_REFERENCE_VERSION)) throw failure('PI_REFERENCE_VERSION_MISMATCH');
  const lock = await readFile(join(sdkRoot, 'package-lock.json'));
  const [{ Agent }, { streamSimple }] = await Promise.all([
    import(pathToFileURL(join(packageRoot('pi-agent-core'), 'dist', 'index.js')).href),
    import(pathToFileURL(join(packageRoot('pi-ai'), 'dist', 'api', 'openai-completions.js')).href)
  ]);
  const reference = { engine: 'pi-agent-core-native', package: '@earendil-works/pi-agent-core', version: PI_REFERENCE_VERSION,
    commit: PI_REFERENCE_COMMIT, license: 'MIT', dependencyLockSha256: digest(lock),
    loopSource: `https://github.com/earendil-works/pi/blob/${PI_REFERENCE_COMMIT}/packages/agent/src/agent.ts`,
    transportSource: `https://github.com/earendil-works/pi/blob/${PI_REFERENCE_COMMIT}/packages/ai/src/api/openai-completions.ts`,
    toolProvider: 'shared-KYNXA-ToolService', automaticRetries: 0, contextWindowTokens: inputBudgetTokens + maxOutputTokens, maxOutputTokens };
  const piModel = { id: model, name: model, api: 'openai-completions', provider: 'benchmark-fixed', baseUrl: connection.baseUrl,
    reasoning: false, input: ['text'], cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
    contextWindow: inputBudgetTokens + maxOutputTokens, maxTokens: maxOutputTokens,
    compat: { supportsStore: false, supportsDeveloperRole: false, supportsReasoningEffort: false, maxTokensField: 'max_tokens' } };
  const descriptors = wireCatalog(catalog), names = new Map(descriptors.map(tool => [tool.wireName, tool.name]));
  const assistants = [], modelCalls = [], toolTrace = [], seenIds = new Set(), pendingCalls = new Map(), callStarts = new WeakMap(), reservedCalls = new Set();
  let active, currentCall, rounds = 0, toolCalls = 0, executedToolCalls = 0, generatedTokens = 0, errorCode, closed = false;
  const controller = new AbortController(), combinedSignal = signal ? AbortSignal.any([controller.signal, signal]) : controller.signal;
  const deadline = performance.now() + limits.maxDurationMs;
  const failRun = code => { errorCode ??= code; throw failure(code); };
  const allowedEndpoint = connection.baseUrl.replace(/\/$/u, '') + '/chat/completions';
  const fetchProvider = async (input, options) => {
    const url = input instanceof Request ? input.url : String(input);
    if (url !== allowedEndpoint) throw failure('PI_REFERENCE_NETWORK_DENIED');
    if (currentCall) currentCall.attempted = true;
    const response = await globalThis.fetch(input, options);
    if (currentCall) currentCall.httpStatus = response.status;
    return response;
  };
  const tools = descriptors.map(tool => ({ name: tool.wireName, label: tool.name,
    description: `${tool.name}: ${tool.description}`, parameters: tool.inputSchema,
    executionMode: canRunInParallel({ name: tool.name }) ? 'parallel' : 'sequential',
    execute: async (toolCallId, args, toolSignal) => {
      combinedSignal.throwIfAborted(); toolSignal?.throwIfAborted();
      // A parallel preflight reserves admission once; the execution boundary independently enforces the total.
      // 并行预检只保留一次准入资格，实际执行边界独立核对总量，后续拒绝不能扩大已保留配额。
      if (!reservedCalls.delete(toolCallId) || executedToolCalls >= limits.maxToolCalls) failRun('BENCHMARK_TOOL_CALL_LIMIT');
      executedToolCalls++;
      const activity = { toolCallId, name: tool.name, arguments: structuredClone(args), status: 'running', summary: tool.name,
        round: active.round, order: active.order++, workspaceRoot: active.context.workspaceRoot ?? null };
      active.ToolActivities.push(activity);
      await onToolActivity(structuredClone(activity), active.context);
      const result = await service.execute(active.context, { id: toolCallId, name: tool.name, arguments: args },
        { signal: toolSignal ? AbortSignal.any([toolSignal, combinedSignal]) : combinedSignal, interactive: false });
      Object.assign(activity, { status: statusFor(result), result: result.content,
        ...(result.resultRef ? { resultRef: structuredClone(result.resultRef) } : {}), ...(result.code ? { code: result.code } : {}),
        ...(result.reused ? { reused: true, observationCapturedAt: result.observationCapturedAt } : {}) });
      // Persist the actual completed receipt before cancellation; a short evidence handle needs its formal owner.
      // 取消前先保存真实执行回执，短证据句柄必须能定位正式归属，不能只留内存模拟记录。
      await onToolActivity(structuredClone(activity), active.context);
      toolTrace.push(structuredClone(activity));
      combinedSignal.throwIfAborted(); toolSignal?.throwIfAborted();
      return { content: [{ type: 'text', text: result.content }], details: { status: activity.status, resultRef: activity.resultRef },
        isError: result.isError === true || activity.status !== 'completed' };
    }
  }));
  const agent = new Agent({ initialState: { systemPrompt, model: piModel, tools, messages: nativeHistory(initialMessages, piModel) }, toolExecution: 'parallel',
    prepareRequest: async () => {
      combinedSignal.throwIfAborted();
      if (modelCalls.length >= limits.maxRounds) failRun('BENCHMARK_MODEL_CALL_LIMIT');
      if (performance.now() >= deadline) failRun('BENCHMARK_TASK_TIMEOUT');
      if (generatedTokens >= limits.maxGeneratedTokens) failRun('BENCHMARK_GENERATED_TOKEN_LIMIT');
    },
    beforeToolCall: async ({ assistantMessage, toolCall }) => {
      combinedSignal.throwIfAborted();
      const batch = assistantMessage.content.filter(block => block.type === 'toolCall');
      const code = errorCode ?? (batch.length > 8 ? 'PI_REFERENCE_TOOL_BATCH_LIMIT' : null);
      if (code) { errorCode ??= code; return { block: true, reason: code, terminate: true }; }
      reservedCalls.add(toolCall.id);
    },
    finishTurn: async () => errorCode ? { action: 'end' } : undefined,
    streamFn: (selectedModel, transcript, options) => {
      const call = { number: modelCalls.length + 1, attempted: false, durationMs: 0, status: 'error', usage: readUsage('openai-completions', null) };
      modelCalls.push(call); currentCall = call;
      const started = performance.now();
      callStarts.set(call, started);
      const rawCallIndexes = new Map();
      // Let Pi build and execute the provider request; the host only bounds and meters it.
      // 供应商请求由 Pi 原生适配器生成与执行，测试宿主只负责边界与计量。
      return streamSimple(selectedModel, transcript, { ...options, apiKey: connection.apiKey, fetch: fetchProvider,
        maxTokens: Math.min(maxOutputTokens, Math.max(1, limits.maxGeneratedTokens - generatedTokens)),
        maxRetries: 0, cacheRetention: 'none', timeoutMs: Math.max(1, Math.floor(deadline - performance.now())),
        signal: options?.signal ? AbortSignal.any([combinedSignal, options.signal]) : combinedSignal,
        onPayload: payload => {
          if (estimateToolMessageTokens(payload.messages) + estimateTokens(JSON.stringify(payload.tools ?? [])) > inputBudgetTokens)
            failRun('PI_REFERENCE_CONTEXT_BUDGET_EXCEEDED');
        },
        onResponse: response => { call.httpStatus = response.status; },
        onProviderStreamEvent: chunk => {
          // The provider adapter can merge blocks by ID; reject distinct raw indexes sharing an identity first.
          // 供应商适配器可能按 ID 合并块，合并前先拒绝两个不同原始索引复用同一调用身份。
          for (const block of chunk?.choices?.[0]?.delta?.tool_calls ?? []) {
            if (typeof block.id !== 'string' || !block.id || !Number.isSafeInteger(block.index)) continue;
            if (rawCallIndexes.has(block.id) && rawCallIndexes.get(block.id) !== block.index) failRun('PI_REFERENCE_DUPLICATE_CALL_ID');
            rawCallIndexes.set(block.id, block.index);
          }
          const usage = chunk?.usage ?? chunk?.choices?.[0]?.usage;
          if (usage) call.usage = readUsage('openai-completions', { usage });
          call.durationMs = Math.ceil(performance.now() - started);
        } });
    } });
  const unsubscribe = agent.subscribe(async event => {
    if (event.type === 'turn_start') { rounds++; active.round++; currentCall = undefined; }
    if (event.type === 'tool_execution_start') {
      if (seenIds.has(event.toolCallId)) errorCode ??= 'PI_REFERENCE_DUPLICATE_CALL_ID';
      seenIds.add(event.toolCallId); toolCalls++;
      if (toolCalls > limits.maxToolCalls) errorCode ??= 'BENCHMARK_TOOL_CALL_LIMIT';
      pendingCalls.set(event.toolCallId, { arguments: structuredClone(event.args) });
    }
    if (event.type === 'message_end' && event.message.role === 'assistant') {
      const call = currentCall, message = event.message;
      if (call) { call.status = ['error', 'aborted'].includes(message.stopReason) ? message.stopReason : 'completed';
        call.stopReason = message.stopReason; call.durationMs = Math.ceil(performance.now() - callStarts.get(call));
        generatedTokens += call.usage.outputTokens ?? 0; }
      active.Content = finalText(message);
      const calls = message.content.filter(block => block.type === 'toolCall');
      if (calls.length > 8) errorCode ??= 'PI_REFERENCE_TOOL_BATCH_LIMIT';
      if (new Set(calls.map(block => block.id)).size !== calls.length) errorCode ??= 'PI_REFERENCE_DUPLICATE_CALL_ID';
      if (['error', 'aborted', 'length'].includes(message.stopReason)) {
        errorCode ??= message.stopReason === 'aborted' ? 'PI_REFERENCE_ABORTED' : message.stopReason === 'length'
          ? 'PI_REFERENCE_OUTPUT_INCOMPLETE' : 'PI_REFERENCE_MODEL_ERROR';
        active.Status = 'interrupted';
      }
    }
    // Failed validation/unavailable calls are outcomes too, even though the broker did not execute them.
    // 参数校验失败或未声明工具也计入观察结果，即使代理并未执行该操作。
    if (event.type === 'tool_execution_end' && !active.ToolActivities.some(item => item.toolCallId === event.toolCallId)) {
      const activity = { toolCallId: event.toolCallId, name: names.get(event.toolName) ?? event.toolName,
        arguments: pendingCalls.get(event.toolCallId)?.arguments ?? {}, status: 'error', round: active.round,
        order: active.order++, result: (event.result?.content ?? []).filter(block => block.type === 'text').map(block => block.text).join(''), executed: false };
      active.ToolActivities.push(activity); toolTrace.push(structuredClone(activity));
      await onToolActivity(structuredClone(activity), active.context);
    }
  });
  const onAbort = () => agent.abort(); combinedSignal.addEventListener('abort', onAbort);
  const timer = setTimeout(() => { errorCode ??= 'BENCHMARK_TASK_TIMEOUT'; controller.abort(); }, limits.maxDurationMs);
  timer.unref();
  const setSystemPrompt = text => {
    if (active || closed || typeof text !== 'string') throw failure('INVALID_PI_REFERENCE_SYSTEM_UPDATE');
    const messages = agent.state.messages;
    // Change only the request's leading prompt; retain Pi's tool declarations and native conversation.
    // 仅更新请求首条提示，保留 Pi 的工具声明与原生对话过程，不追加旧证据提示。
    agent.state.messages = messages[0]?.role === 'system' ? [{ ...messages[0], content: text }, ...messages.slice(1)]
      : [{ role: 'system', content: text, timestamp: 0 }, ...messages];
  };
  return {
    reference,
    setSystemPrompt,
    async prompt(message, { context, systemPrompt: nextSystemPrompt } = {}) {
      if (closed || active || !context?.requestId || !context?.conversationId || typeof message !== 'string') throw failure('INVALID_PI_REFERENCE_PROMPT');
      combinedSignal.throwIfAborted();
      if (nextSystemPrompt !== undefined) setSystemPrompt(nextSystemPrompt);
      if (errorCode) throw failure(errorCode);
      active = { Id: context.requestId, context, Content: '', Status: 'completed', ToolActivities: [], round: 0, order: 0 };
      seenIds.clear(); pendingCalls.clear(); reservedCalls.clear();
      try { await agent.prompt(message); }
      catch (error) { errorCode ??= /^[A-Z0-9_]+$/u.test(error?.code ?? '') ? error.code : 'PI_REFERENCE_FAILED'; active.Status = 'interrupted'; }
      finally { await agent.waitForIdle(); if (errorCode) active.Status = 'interrupted';
        const { context: omitted, round, order, ...assistant } = active;
        assistants.push(assistant); active = undefined; }
      return { assistant: structuredClone(assistants.at(-1)), ...(errorCode ? { errorCode } : {}) };
    },
    abort() { controller.abort(); },
    async close() { closed = true; clearTimeout(timer); controller.abort(); await agent.waitForIdle(); unsubscribe();
      combinedSignal.removeEventListener('abort', onAbort); },
    snapshot() { return structuredClone({ reference, assistants, modelCalls, toolTrace, rounds, toolCalls, executedToolCalls, generatedTokens,
      ...(errorCode ? { errorCode } : {}) }); }
  };
}

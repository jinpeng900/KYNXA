import { createHash } from 'node:crypto';
import { StreamFailure, finalParts, checkFinish } from './streaming.mjs';
import { estimateMessageTokens, estimateTokens } from './context.mjs';

function nativeValueTokens(value, field = '') {
  if (typeof value === 'string') return estimateTokens(value);
  if (value == null) return 1;
  if (typeof value !== 'object') return estimateTokens(String(value));
  // Native tool input objects represent business JSON. Function argument strings
  // already contain that JSON source and were counted once in the string branch.
  // 原生工具输入对象是业务 JSON，函数参数字符串已包含同一 JSON 源，因此在字符串分支只计数一次。
  if (field === 'input' || field === 'arguments') return estimateTokens(JSON.stringify(value));
  if (Array.isArray(value)) return 2 + value.reduce((sum, item) => sum + 1 + nativeValueTokens(item), 0);
  return 2 + Object.entries(value).reduce((sum, [key, item]) =>
    sum + estimateTokens(key) + 4 + nativeValueTokens(item, key), 0);
}

/**
 * Match ordinary history accounting while retaining every native continuation field.
 * 与普通历史采用相同计量方式，同时保留所有原生续传字段。
 */
export function estimateToolMessageTokens(messages, system = '') {
  return estimateMessageTokens([], system) + messages.reduce((sum, message) => {
    if (!message || typeof message !== 'object' || Array.isArray(message)) return sum + 8 + nativeValueTokens(message);
    let tokens = 8 + (Object.hasOwn(message, 'content') ? nativeValueTokens(message.content, 'content') : 0);
    for (const [key, value] of Object.entries(message)) {
      if (key !== 'role' && key !== 'content') tokens += estimateTokens(key) + 4 + nativeValueTokens(value, key);
    }
    return sum + tokens;
  }, 0);
}

// Internal names identify policy-owned operations. Provider aliases only identify
// declarations in this immutable request catalog; they never confer permission.
// 内部名称标识策略拥有的操作，供应商别名只用于当前不可变请求目录的声明，不授予权限。
export const MAX_MODEL_TOOLS = 96;
export function wireCatalog(descriptors) {
  if (!Array.isArray(descriptors) || descriptors.length > MAX_MODEL_TOOLS)
    throw new StreamFailure('工具目录过大，请停用部分 MCP 服务后重试。');
  return descriptors.map(tool => ({ ...tool, wireName: 'k_' + tool.name.replace(/[^a-zA-Z0-9_]/g, '_').slice(0, 40)
    + '_' + createHash('sha256').update(tool.name).digest('hex').slice(0, 8) }));
}

export function toolDeclarations(protocol, catalog) {
  return catalog.map(tool => {
    const spec = { name: tool.wireName, description: `${tool.name}: ${tool.description}`, parameters: tool.inputSchema };
    if (protocol === 'anthropic-messages') return { name: spec.name, description: spec.description, input_schema: spec.parameters };
    if (protocol === 'openai-responses') return { type: 'function', ...spec, strict: false };
    return { type: 'function', function: spec };
  });
}

function decodeCall(rawCall, catalog) {
  if (typeof rawCall.id !== 'string' || !rawCall.id || rawCall.id.length > 200 || typeof rawCall.name !== 'string' ||
      !rawCall.name || rawCall.name.length > 256 || /[\0\r\n]/.test(rawCall.name))
    throw new StreamFailure('模型返回了无效的工具调用身份。');
  const descriptor = catalog.find(tool => tool.wireName === rawCall.name);
  let args = rawCall.arguments;
  if (typeof args === 'string') {
    if (args.length > 65536) throw new StreamFailure('工具参数超过大小限制。');
    try { args = JSON.parse(args); } catch { throw new StreamFailure('模型返回了不完整的工具参数。'); }
  }
  if (!args || typeof args !== 'object' || Array.isArray(args) || JSON.stringify(args).length > 65536)
    throw new StreamFailure('工具参数必须为大小受限的 JSON 对象。');
  // Unknown names become non-executable observations, never permission or an implicit tool load.
  // 未知名称形成不可执行的失败观察，不能因此取得权限或隐式加载工具。
  return { id: rawCall.id, name: descriptor?.name ?? rawCall.name, arguments: args,
    ...(!descriptor ? { unavailable: true } : {}) };
}

export function decodeToolTurn(protocol, result, catalog) {
  const parts = finalParts(protocol, result);
  if (protocol === 'anthropic-messages') {
    parts.content = (result.content ?? []).filter(providerItem => providerItem.type === 'text').map(providerItem => providerItem.text ?? '').join('');
    parts.reasoning = (result.content ?? []).filter(providerItem => providerItem.type === 'thinking').map(providerItem => providerItem.thinking ?? '').join('');
  }
  let rawCalls, continuation;
  if (protocol === 'anthropic-messages') {
    continuation = [{ role: 'assistant', content: result.content ?? [] }];
    rawCalls = (result.content ?? []).filter(providerItem => providerItem.type === 'tool_use').map(providerItem => ({ id: providerItem.id, name: providerItem.name, arguments: providerItem.input }));
  } else if (protocol === 'openai-responses') {
    // Keep reasoning/encrypted items only in this upstream exchange. They are not
    // visible thought text and are never persisted in the public transcript.
    // 推理或加密项只留在本轮上游交换中，不作为可见思考文本，也不写入公开聊天。
    continuation = result.output ?? [];
    rawCalls = continuation.filter(providerItem => providerItem.type === 'function_call').map(providerItem => ({ id: providerItem.call_id, name: providerItem.name, arguments: providerItem.arguments }));
  } else {
    const message = result.choices?.[0]?.message ?? {};
    continuation = [{ ...message, role: 'assistant' }];
    rawCalls = (message.tool_calls ?? []).map(providerItem => ({ id: providerItem.id, name: providerItem.function?.name, arguments: providerItem.function?.arguments }));
  }
  // Check the stop status before parsing or dispatching any business arguments.
  // A syntactically valid prefix is still unsafe when its generation was cut off.
  // 解析或派发业务参数前先检查停止状态；生成被截断时，即使前缀语法有效也不能执行。
  if (!['tool_calls', 'tool_use'].includes(parts.finish)) checkFinish(parts.finish);
  if (rawCalls.length > 8) throw new StreamFailure('模型单次请求的工具数量超过上限。');
  const calls = rawCalls.map(call => decodeCall(call, catalog));
  if (new Set(calls.map(call => call.id)).size !== calls.length) throw new StreamFailure('模型重复了工具调用 ID。');
  if (parts.finish && ['tool_calls', 'tool_use'].includes(parts.finish)) {
    if (!calls.length) throw new StreamFailure('模型结束了工具调用，但未返回完整参数。');
  } else checkFinish(parts.finish);
  if (protocol === 'openai-responses' && result.status && result.status !== 'completed')
    throw new StreamFailure('模型未完整结束本次工具回复。', 'interrupted');
  return { ...parts, calls, continuation };
}

export function appendToolResults(protocol, messages, turn, results, { onResult } = {}) {
  const recordProviderResult = (entry, field, pair) => { onResult?.(entry, { ...pair, field }); return entry; };
  if (protocol === 'anthropic-messages') return [...messages, ...turn.continuation, { role: 'user', content:
    results.map(pair => recordProviderResult({ type: 'tool_result', tool_use_id: pair.call.id, content: pair.result.content, is_error: !!pair.result.isError }, 'content', pair)) }];
  if (protocol === 'openai-responses') return [...messages, ...turn.continuation,
    ...results.map(pair => recordProviderResult({ type: 'function_call_output', call_id: pair.call.id, output: pair.result.content }, 'output', pair))];
  return [...messages, ...turn.continuation,
    ...results.map(pair => recordProviderResult({ role: 'tool', tool_call_id: pair.call.id, content: pair.result.content }, 'content', pair))];
}

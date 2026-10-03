import { createHash } from 'node:crypto';
import { StreamFailure, finalParts, checkFinish } from './streaming.mjs';
import { estimateMessageTokens, estimateTokens } from './context.mjs';

function nativeValueTokens(value, field = '') {
  if (typeof value === 'string') return estimateTokens(value);
  if (value == null) return 1;
  if (typeof value !== 'object') return estimateTokens(String(value));
  // Native tool input objects represent business JSON. Function argument strings
  // already contain that JSON source and were counted once in the string branch.
  if (field === 'input' || field === 'arguments') return estimateTokens(JSON.stringify(value));
  if (Array.isArray(value)) return 2 + value.reduce((sum, item) => sum + 1 + nativeValueTokens(item), 0);
  return 2 + Object.entries(value).reduce((sum, [key, item]) =>
    sum + estimateTokens(key) + 4 + nativeValueTokens(item, key), 0);
}

/** Match ordinary history accounting while retaining every native continuation field. */
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

function decodeCall(raw, catalog) {
  if (typeof raw.id !== 'string' || !raw.id || raw.id.length > 200 || typeof raw.name !== 'string')
    throw new StreamFailure('模型返回了无效的工具调用身份。');
  const descriptor = catalog.find(tool => tool.wireName === raw.name);
  if (!descriptor) throw new StreamFailure('模型请求了本次未提供的工具。');
  let args = raw.arguments;
  if (typeof args === 'string') {
    if (args.length > 65536) throw new StreamFailure('工具参数超过大小限制。');
    try { args = JSON.parse(args); } catch { throw new StreamFailure('模型返回了不完整的工具参数。'); }
  }
  if (!args || typeof args !== 'object' || Array.isArray(args) || JSON.stringify(args).length > 65536)
    throw new StreamFailure('工具参数必须为大小受限的 JSON 对象。');
  return { id: raw.id, name: descriptor.name, arguments: args };
}

export function decodeToolTurn(protocol, result, catalog) {
  const parts = finalParts(protocol, result);
  if (protocol === 'anthropic-messages') {
    parts.content = (result.content ?? []).filter(x => x.type === 'text').map(x => x.text ?? '').join('');
    parts.reasoning = (result.content ?? []).filter(x => x.type === 'thinking').map(x => x.thinking ?? '').join('');
  }
  let rawCalls, continuation;
  if (protocol === 'anthropic-messages') {
    continuation = [{ role: 'assistant', content: result.content ?? [] }];
    rawCalls = (result.content ?? []).filter(x => x.type === 'tool_use').map(x => ({ id: x.id, name: x.name, arguments: x.input }));
  } else if (protocol === 'openai-responses') {
    // Keep reasoning/encrypted items only in this upstream exchange. They are not
    // visible thought text and are never persisted in the public transcript.
    continuation = result.output ?? [];
    rawCalls = continuation.filter(x => x.type === 'function_call').map(x => ({ id: x.call_id, name: x.name, arguments: x.arguments }));
  } else {
    const message = result.choices?.[0]?.message ?? {};
    continuation = [{ ...message, role: 'assistant' }];
    rawCalls = (message.tool_calls ?? []).map(x => ({ id: x.id, name: x.function?.name, arguments: x.function?.arguments }));
  }
  // Check the stop status before parsing or dispatching any business arguments.
  // A syntactically valid prefix is still unsafe when its generation was cut off.
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
  const output = (entry, field, pair) => { onResult?.(entry, { ...pair, field }); return entry; };
  if (protocol === 'anthropic-messages') return [...messages, ...turn.continuation, { role: 'user', content:
    results.map(pair => output({ type: 'tool_result', tool_use_id: pair.call.id, content: pair.result.content, is_error: !!pair.result.isError }, 'content', pair)) }];
  if (protocol === 'openai-responses') return [...messages, ...turn.continuation,
    ...results.map(pair => output({ type: 'function_call_output', call_id: pair.call.id, output: pair.result.content }, 'output', pair))];
  return [...messages, ...turn.continuation,
    ...results.map(pair => output({ role: 'tool', tool_call_id: pair.call.id, content: pair.result.content }, 'content', pair))];
}

import { createHash } from 'node:crypto';
import { StreamFailure, finalParts, checkFinish } from './streaming.mjs';

// Internal names identify policy-owned operations. Provider aliases only identify
// declarations in this immutable request catalog; they never confer permission.
export function wireCatalog(descriptors) {
  if (!Array.isArray(descriptors) || descriptors.length > 96)
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

export function appendToolResults(protocol, messages, turn, results) {
  if (protocol === 'anthropic-messages') return [...messages, ...turn.continuation, { role: 'user', content:
    results.map(({ call, result }) => ({ type: 'tool_result', tool_use_id: call.id, content: result.content, is_error: !!result.isError })) }];
  if (protocol === 'openai-responses') return [...messages, ...turn.continuation,
    ...results.map(({ call, result }) => ({ type: 'function_call_output', call_id: call.id, output: result.content }))];
  return [...messages, ...turn.continuation,
    ...results.map(({ call, result }) => ({ role: 'tool', tool_call_id: call.id, content: result.content }))];
}

import { createHash } from 'node:crypto';

const protocols = new Set(['openai-completions', 'openai-responses', 'anthropic-messages']);
const hash = value => createHash('sha256').update(JSON.stringify(value)).digest('hex');
const hex = /^[a-f0-9]{64}$/;
const uuid = /^[a-f0-9]{8}-[a-f0-9]{4}-4[a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}$/i;
const record = value => value && typeof value === 'object' && !Array.isArray(value);
const text = (value, maximum = 2 * 1024 * 1024) => typeof value === 'string' && value.length <= maximum;
const identity = value => text(value, 256) && value.length > 0 && !/[\0\r\n]/.test(value);
const jsonObject = value => record(value) && JSON.stringify(value).length <= 65536;

function invalid(message = '模型转录格式无效，原记录已保留。', code = 'INVALID_MODEL_TRANSCRIPT') {
  return Object.assign(new Error(message), { code, statusCode: 400 });
}

export function modelOrigin(connection, { providerId, model }) {
  return { providerId, model, protocol: connection.protocol,
    // A changed account, endpoint or model cannot receive another origin's private continuation.
    // Only the digest is durable; the authentication value never becomes transcript text.
    // 账号、端点或模型变化后，不能获取其他来源的私有续传状态。
    connectionFingerprint: hash([providerId, model, connection.protocol, connection.baseUrl, connection.apiKey ?? '']) };
}

export function modelPrefixFingerprint(messages, system = '', declarations = []) {
  return hash([system, declarations, messages]);
}

export function validateModelTranscript(value) {
  if (!record(value) || value.version !== 1) throw invalid('模型转录版本不受支持，原记录已保留。', 'UNSUPPORTED_MODEL_TRANSCRIPT_VERSION');
  const origin = value.origin;
  if (!record(origin) || !identity(origin.providerId) || !identity(origin.model) || !protocols.has(origin.protocol) ||
      !hex.test(origin.connectionFingerprint ?? '') || !Array.isArray(value.rounds) || value.rounds.length > 128) throw invalid();
  let previous = 0, totalCalls = 0;
  const rounds = value.rounds.map(round => {
    const calls = new Set();
    if (!record(round) || !Number.isSafeInteger(round.round) || round.round <= previous || round.round > 128 ||
        !text(round.text) || !Array.isArray(round.calls) || round.calls.length > 8) throw invalid();
    previous = round.round;
    const normalized = round.calls.map(call => {
      if (!record(call) || !identity(call.id) || call.id.length > 200 || !identity(call.name) || !jsonObject(call.arguments) ||
          calls.has(call.id) || ++totalCalls > 512) throw invalid();
      calls.add(call.id);
      return { id: call.id, name: call.name, arguments: JSON.parse(JSON.stringify(call.arguments)) };
    });
    let nativeContinuationRef;
    if (round.nativeContinuationRef !== undefined) {
      const ref = round.nativeContinuationRef;
      if (!record(ref) || !uuid.test(ref.id ?? '') || !hex.test(ref.sha256 ?? '') ||
          !Number.isSafeInteger(ref.bytes) || ref.bytes < 0 || !hex.test(round.prefixFingerprint ?? '')) throw invalid();
      nativeContinuationRef = { id: ref.id.toLowerCase(), bytes: ref.bytes, sha256: ref.sha256 };
    }
    return { round: round.round, text: round.text, calls: normalized,
      ...(nativeContinuationRef ? { nativeContinuationRef, prefixFingerprint: round.prefixFingerprint } : {}) };
  });
  return { version: 1, origin: { providerId: origin.providerId, model: origin.model, protocol: origin.protocol,
    connectionFingerprint: origin.connectionFingerprint }, rounds };
}

export function appendModelRound(assistant, origin, round) {
  const previous = assistant.ModelTranscript ?? { version: 1, origin, rounds: [] };
  if (JSON.stringify(previous.origin) !== JSON.stringify(origin)) throw invalid('模型转录来源已变化。', 'MODEL_TRANSCRIPT_ORIGIN_CHANGED');
  const rounds = [...previous.rounds.filter(item => item.round !== round.round), round].sort((a, b) => a.round - b.round);
  return validateModelTranscript({ version: 1, origin, rounds });
}

/**
 * Only known provider continuation fields can enter the protected archive.
 * 只有已知供应商续传字段可以进入受保护附件。
 */
export function nativeContinuation(protocol, turn) {
  if (protocol === 'anthropic-messages') {
    const blocks = turn.continuation?.[0]?.content;
    if (!Array.isArray(blocks) || !blocks.some(block => block.type === 'thinking' && text(block.signature) || block.type === 'redacted_thinking')) return null;
    const content = blocks.flatMap(block => {
      if (block.type === 'text' && text(block.text)) return [{ type: 'text', text: block.text }];
      if (block.type === 'thinking' && text(block.thinking) && text(block.signature))
        return [{ type: 'thinking', thinking: block.thinking, signature: block.signature }];
      if (block.type === 'redacted_thinking' && text(block.data)) return [{ type: 'redacted_thinking', data: block.data }];
      if (block.type === 'tool_use' && identity(block.id) && identity(block.name) && jsonObject(block.input))
        return [{ type: 'tool_use', id: block.id, name: block.name, input: JSON.parse(JSON.stringify(block.input)) }];
      return [];
    });
    return [{ role: 'assistant', content }];
  }
  if (protocol === 'openai-responses') {
    if (!turn.continuation?.some(item => item.type === 'reasoning' && text(item.encrypted_content))) return null;
    return turn.continuation.flatMap(item => {
      if (item.type === 'reasoning' && text(item.encrypted_content)) return [{ type: 'reasoning',
        ...(identity(item.id) ? { id: item.id } : {}), encrypted_content: item.encrypted_content, summary: [] }];
      if (item.type === 'function_call' && identity(item.call_id) && identity(item.name) && text(item.arguments, 65536))
        return [{ type: 'function_call', call_id: item.call_id, name: item.name, arguments: item.arguments }];
      if (item.type === 'message') {
        const content = (Array.isArray(item.content) ? item.content : []).flatMap(block =>
          block.type === 'output_text' && text(block.text) ? [{ type: 'output_text', text: block.text }] : []);
        return content.length ? [{ type: 'message', role: 'assistant', content }] : [];
      }
      return [];
    });
  }
  const message = turn.continuation?.[0];
  if (!turn.calls?.length || !text(message?.reasoning_content) || !message.reasoning_content) return null;
  return [{ role: 'assistant', content: text(message.content) ? message.content : '', reasoning_content: message.reasoning_content,
    tool_calls: turn.calls.map(call => {
      const native = message.tool_calls?.find(item => item.id === call.id);
      if (!native || !identity(native.function?.name)) throw invalid();
      if (!text(native.function.arguments, 65536)) throw invalid();
      return { id: call.id, type: 'function', function: { name: native.function.name, arguments: native.function.arguments } };
    }) }];
}

/**
 * Display/API callers see public messages, never private continuation pointers or origins.
 * 界面和公开 API 只见公开消息，不暴露私有续传引用或来源。
 */
export function publicConversationMessage(message) {
  const { ModelTranscript: _private, ...publicMessage } = message;
  return publicMessage;
}

export function historicalCallId(assistantId, round, callId) {
  return `h_${hash([assistantId, round, callId]).slice(0, 40)}`;
}

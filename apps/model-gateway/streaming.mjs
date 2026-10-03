// Upstream stream decoding is separate from HTTP delivery and session persistence.
// Never return provider error payloads: they can contain credentials or request bodies.
export class StreamFailure extends Error {
  constructor(message, type = 'error') { super(message); this.type = type; }
}

export function checkFinish(reason) {
  if (!reason || ['stop', 'end_turn', 'stop_sequence'].includes(reason)) return;
  throw new StreamFailure(reason === 'length' || reason === 'max_tokens'
    ? '回复达到模型输出上限，已保留生成的内容。'
    : reason === 'tool_calls' || reason === 'tool_use'
      ? '模型请求使用工具，当前聊天尚未执行该工具，已保留生成的内容。'
      : '模型未完整生成回复，已保留生成的内容。', 'interrupted');
}

/** SSE parser with streaming UTF-8 decoding, CR/LF handling and bounded frames. */
export async function* readSse(body, onActivity = () => {}) {
  if (!body) throw new StreamFailure('模型接口没有返回响应内容。');
  const reader = body.getReader(), decoder = new TextDecoder();
  let buffer = '', data = [], event = '', frameSize = 0;
  try {
    while (true) {
      const { value, done } = await reader.read();
      if (!done) onActivity();
      buffer += done ? decoder.decode() : decoder.decode(value, { stream: true });
      if (buffer.length + frameSize > 2 * 1024 * 1024)
        throw new StreamFailure('模型返回的单条流式事件过大。');
      while (true) {
        const match = /\r\n|\n|\r/.exec(buffer);
        if (!match || (!done && match[0] === '\r' && match.index === buffer.length - 1)) break;
        const line = buffer.slice(0, match.index);
        buffer = buffer.slice(match.index + match[0].length);
        if (!line) {
          if (data.length) yield { event, data: data.join('\n') };
          data = []; event = ''; frameSize = 0;
        } else if (!line.startsWith(':')) {
          const colon = line.indexOf(':');
          const field = colon < 0 ? line : line.slice(0, colon);
          let text = colon < 0 ? '' : line.slice(colon + 1);
          if (text.startsWith(' ')) text = text.slice(1);
          if (field === 'data') { data.push(text); frameSize += text.length; }
          else if (field === 'event') event = text;
        }
      }
      if (done) break;
    }
    // A final unterminated SSE frame is not dispatched. A missing terminal event
    // is detected by the protocol adapter rather than saved as a successful turn.
  } finally {
    await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}

export function textParts(value) {
  if (typeof value === 'string') return value;
  if (Array.isArray(value)) return value.filter(x => x?.type === 'text').map(x => x.text ?? '').join('');
  return '';
}

export function finalParts(protocol, result) {
  if (result?.error) throw new StreamFailure('模型服务返回错误，请检查连接配置或稍后重试。');
  if (protocol === 'anthropic-messages') return {
    content: result.content?.filter(x => x.type === 'text').map(x => x.text ?? '').join('\n') ?? '',
    reasoning: result.content?.filter(x => x.type === 'thinking').map(x => x.thinking ?? '').join('\n') ?? '',
    finish: result.stop_reason
  };
  if (protocol === 'openai-responses') return {
    content: result.output?.filter(x => x.type === 'message').flatMap(x => x.content ?? [])
      .filter(x => x.type === 'output_text').map(x => x.text ?? '').join('\n') ?? result.output_text ?? '',
    reasoning: result.output?.filter(x => x.type === 'reasoning').flatMap(x => x.summary ?? [])
      .filter(x => x.type === 'summary_text').map(x => x.text ?? '').join('\n') ?? '',
    finish: ['incomplete', 'failed', 'cancelled'].includes(result.status) ? result.status : null
  };
  const choice = result.choices?.[0];
  return { content: textParts(choice?.message?.content),
    reasoning: textParts(choice?.message?.reasoning_content ?? choice?.message?.reasoning),
    finish: choice?.finish_reason };
}

/** emit receives only { type, delta } events; returned snapshots reconcile final content. */
export async function readModelStream(response, protocol, emit, onActivity) {
  let content = '', reasoning = '', finished = false;
  const append = (type, delta) => {
    if (typeof delta !== 'string' || !delta) return;
    if (type === 'text_delta') content += delta; else reasoning += delta;
    if (content.length + reasoning.length > 8 * 1024 * 1024)
      throw new StreamFailure('模型回复过长，已停止接收。', 'interrupted');
    emit({ type, delta });
  };
  if (!(response.headers.get('content-type') ?? '').toLowerCase().includes('text/event-stream')) {
    // Some compatible/local servers ignore stream:true and return a JSON response.
    let result;
    try { result = await response.json(); } catch { throw new StreamFailure('模型接口返回了无效的 JSON 响应。'); }
    onActivity();
    const parts = finalParts(protocol, result);
    append('reasoning_delta', parts.reasoning); append('text_delta', parts.content);
    checkFinish(parts.finish);
    return { content, reasoning };
  }
  for await (const frame of readSse(response.body, onActivity)) {
    if (frame.data === '[DONE]') {
      if (protocol === 'openai-completions' || !protocol) { finished = true; break; }
      continue;
    }
    let event;
    try { event = JSON.parse(frame.data); } catch { throw new StreamFailure('模型接口返回了无效的流式事件。'); }
    if (event.error || event.type === 'error' || frame.event === 'error')
      throw new StreamFailure('模型服务返回错误，已保留生成的内容。');
    if (protocol === 'anthropic-messages') {
      const type = event.type ?? frame.event;
      if (type === 'content_block_start') {
        append('text_delta', event.content_block?.text);
        append('reasoning_delta', event.content_block?.thinking);
      } else if (type === 'content_block_delta') {
        if (event.delta?.type === 'text_delta') append('text_delta', event.delta.text);
        if (event.delta?.type === 'thinking_delta') append('reasoning_delta', event.delta.thinking);
      } else if (type === 'message_delta') checkFinish(event.delta?.stop_reason);
      else if (type === 'message_stop') { finished = true; break; }
    } else if (protocol === 'openai-responses') {
      const type = event.type ?? frame.event;
      if (type === 'response.output_text.delta') append('text_delta', event.delta);
      else if (type === 'response.reasoning_summary_text.delta') append('reasoning_delta', event.delta);
      else if (type === 'response.completed') {
        if (event.response?.output) {
          const parts = finalParts(protocol, event.response);
          content = parts.content; reasoning = parts.reasoning || reasoning;
          checkFinish(parts.finish);
        }
        finished = true; break;
      } else if (['response.incomplete', 'response.failed', 'response.cancelled'].includes(type)) {
        throw new StreamFailure('模型未完整生成回复，已保留生成的内容。', 'interrupted');
      }
    } else {
      const choice = event.choices?.find(x => x.index === 0) ?? event.choices?.find(x => x.index == null);
      append('reasoning_delta', textParts(choice?.delta?.reasoning_content ?? choice?.delta?.reasoning));
      append('text_delta', textParts(choice?.delta?.content));
      if (choice?.finish_reason) { checkFinish(choice.finish_reason); finished = true; break; }
    }
  }
  if (!finished) throw new StreamFailure('连接提前结束，已保留生成的内容。', 'interrupted');
  return { content, reasoning };
}

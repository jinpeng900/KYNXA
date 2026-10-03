import { StreamFailure, readSse, textParts, finalParts, checkFinish } from './streaming.mjs';
import { decodeToolTurn } from './tool-protocols.mjs';

/** Decode complete tool arguments before dispatch. A dropped stream never executes a call. */
export async function readToolStream(response, protocol, catalog, emit = () => {}, activity = () => {}) {
  let content = '', reasoning = '';
  const send = (type, delta) => {
    delta = textParts(delta);
    if (!delta) return;
    if (type === 'text_delta') content += delta; else reasoning += delta;
    if (content.length + reasoning.length > 2 * 1024 * 1024) throw new StreamFailure('本次回复超过大小限制。');
    emit({ type, delta });
  };
  const finish = result => {
    let turn;
    try { turn = decodeToolTurn(protocol, result, catalog); }
    catch (error) {
      // Local servers may ignore stream:true. Keep their returned draft without
      // treating incomplete tool arguments as an executable call.
      const parts = finalParts(protocol, result);
      emit({ type: 'content_snapshot', content: parts.content || content, reasoning: parts.reasoning || reasoning });
      throw error;
    }
    if (turn.content.startsWith(content)) send('text_delta', turn.content.slice(content.length));
    // Final snapshots may revise a streamed draft. The completed event reconciles the UI.
    if (turn.reasoning.startsWith(reasoning)) send('reasoning_delta', turn.reasoning.slice(reasoning.length));
    // Public thought summaries may also be revised in a completed response.
    return turn;
  };
  if (!(response.headers.get('content-type') ?? '').toLowerCase().includes('text/event-stream')) {
    let result;
    try { result = await response.json(); } catch { throw new StreamFailure('模型接口返回了无效的 JSON 响应。'); }
    return finish(result);
  }
  const calls = new Map(), blocks = new Map();
  let stopReason, rawReasoning = '';
  for await (const frame of readSse(response.body, activity)) {
    if (frame.data === '[DONE]') {
      if (protocol !== 'openai-completions') throw new StreamFailure('模型工具流缺少完成事件。');
      return finish({ choices: [{ finish_reason: stopReason ?? 'stop', message: {
        role: 'assistant', content, ...(rawReasoning ? { reasoning_content: rawReasoning } : {}),
        ...(calls.size ? { tool_calls: [...calls.values()] } : {}) } }] });
    }
    let item;
    try { item = JSON.parse(frame.data); } catch { throw new StreamFailure('模型流返回了无法识别的数据。'); }
    const type = item.type ?? frame.event;
    if (item.error || type === 'error') throw new StreamFailure('模型服务返回错误，请检查连接配置或稍后重试。');
    if (protocol === 'openai-responses') {
      if (type === 'response.output_text.delta') send('text_delta', item.delta);
      if (['response.reasoning_summary_text.delta', 'response.reasoning_text.delta'].includes(type)) send('reasoning_delta', item.delta);
      if (type === 'response.completed') return finish(item.response ?? item);
      if (['response.incomplete', 'response.failed', 'response.cancelled'].includes(type)) {
        if (item.response) return finish({ ...item.response, status: type.slice('response.'.length) });
        throw new StreamFailure('模型未完整结束本次工具回复。', 'interrupted');
      }
    } else if (protocol === 'anthropic-messages') {
      if (type === 'content_block_start') {
        if (!Number.isSafeInteger(item.index) || item.index < 0 || item.index > 32 || blocks.has(item.index))
          throw new StreamFailure('模型返回了无效的内容块。');
        const block = { ...item.content_block };
        if (block.type === 'tool_use') block.partialInput = '';
        blocks.set(item.index, block);
        if (block.type === 'text') send('text_delta', block.text);
        if (block.type === 'thinking') send('reasoning_delta', block.thinking);
      }
      if (type === 'content_block_delta') {
        let block = blocks.get(item.index);
        const delta = item.delta;
        // Compatibility with text-only streams omitting block_start. Tool arguments
        // still require an explicit tool_use identity before any input fragments.
        if (!block && ['text_delta', 'thinking_delta'].includes(delta?.type)) {
          block = delta.type === 'text_delta' ? { type: 'text', text: '' } : { type: 'thinking', thinking: '' };
          blocks.set(item.index, block);
        }
        if (!block && delta?.type === 'signature_delta') continue;
        if (!block) throw new StreamFailure('模型流内容块顺序无效。');
        if (delta?.type === 'text_delta') { block.text = (block.text ?? '') + delta.text; send('text_delta', delta.text); }
        if (delta?.type === 'thinking_delta') { block.thinking = (block.thinking ?? '') + delta.thinking; send('reasoning_delta', delta.thinking); }
        if (delta?.type === 'signature_delta') block.signature = (block.signature ?? '') + delta.signature;
        if (delta?.type === 'input_json_delta') {
          block.partialInput += delta.partial_json ?? '';
          if (block.partialInput.length > 65536) throw new StreamFailure('工具参数超过大小限制。');
        }
      }
      if (type === 'message_delta') stopReason = item.delta?.stop_reason;
      if (type === 'message_stop') {
        if (!stopReason) throw new StreamFailure('模型工具流缺少结束状态。');
        if (stopReason !== 'tool_use') checkFinish(stopReason);
        const raw = [...blocks.entries()].sort((a,b) => a[0]-b[0]).map(([, block]) => {
          if (block.type !== 'tool_use') return block;
          const { partialInput, ...rest } = block;
          if (partialInput) { try { rest.input = JSON.parse(partialInput); } catch { throw new StreamFailure('模型返回了不完整的工具参数。'); } }
          return rest;
        });
        return finish({ content: raw, stop_reason: stopReason });
      }
    } else {
      const choice = item.choices?.find(x => x.index === 0) ?? item.choices?.[0];
      const delta = choice?.delta;
      send('text_delta', delta?.content);
      const thinking = delta?.reasoning_content ?? delta?.reasoning;
      if (thinking) { rawReasoning += textParts(thinking); send('reasoning_delta', thinking); }
      for (const part of delta?.tool_calls ?? []) {
        if (!Number.isInteger(part.index) || part.index < 0 || part.index > 7) throw new StreamFailure('工具调用序号无效。');
        const call = calls.get(part.index) ?? { id: '', type: 'function', function: { name: '', arguments: '' } };
        if (part.id) { if (call.id && call.id !== part.id) throw new StreamFailure('工具调用身份发生改变。'); call.id = part.id; }
        call.function.name += part.function?.name ?? '';
        call.function.arguments += part.function?.arguments ?? '';
        if (call.function.arguments.length > 65536) throw new StreamFailure('工具参数超过大小限制。');
        calls.set(part.index, call);
      }
      if (choice?.finish_reason) {
        stopReason = choice.finish_reason;
        return finish({ choices: [{ finish_reason: stopReason, message: { role: 'assistant', content,
          ...(rawReasoning ? { reasoning_content: rawReasoning } : {}), ...(calls.size ? { tool_calls: [...calls.values()] } : {}) } }] });
      }
    }
  }
  throw new StreamFailure('模型连接已断开，未执行未完成的工具调用。', 'interrupted');
}

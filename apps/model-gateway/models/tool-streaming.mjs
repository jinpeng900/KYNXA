import { StreamFailure, readSse, textParts, finalParts, checkFinish, isOutputLimit } from './streaming.mjs';
import { decodeToolTurn } from './tool-protocols.mjs';
import { ToolCallDecodeFailure, validateTurnCallCount, validateArgumentBuffer } from './tool-call-validation.mjs';
import { estimateTokens } from './context-tokens.mjs';

/**
 * Decode complete tool arguments before dispatch. A dropped stream never executes a call.
 * 参数完整解码后才能派发工具调用，中断流不能执行半份调用。
 */
export async function readToolStream(response, protocol, catalog, emit = () => {}, activity = () => {}, options = {}) {
  const usage = { content: '', reasoning: '', argumentParts: [] };
  try { return await decodeToolStream(response, protocol, catalog, emit, activity, usage, options); }
  catch (error) {
    // Transport failures also consume partial output; retain only its estimate, never raw arguments in diagnostics.
    // 网络/取消故障同样消耗部分输出；诊断仅保留估算量，不记录原始工具参数。
    if (error && typeof error === 'object') error.estimatedGeneratedTokens = Math.max(error.estimatedGeneratedTokens ?? 0,
      estimateTokens(usage.content) + estimateTokens(usage.reasoning) + estimateTokens(usage.argumentParts.join('')));
    throw error;
  }
}

async function decodeToolStream(response, protocol, catalog, emit, activity, usage, options) {
  let content = '', reasoning = '';
  const send = (type, delta) => {
    delta = textParts(delta);
    if (!delta) return;
    if (type === 'text_delta') content += delta; else reasoning += delta;
    usage.content = content; usage.reasoning = reasoning;
    if (content.length + reasoning.length > 2 * 1024 * 1024) throw new StreamFailure('本次回复超过大小限制。');
    emit({ type, delta });
  };
  const finish = result => {
    let turn;
    try { turn = decodeToolTurn(protocol, result, catalog, options); }
    catch (error) {
      // Local servers may ignore stream:true. Keep their returned draft without
      // treating incomplete tool arguments as an executable call.
      // 本地服务可能忽略 stream:true；保留其返回草稿，但不把未完成工具参数当成可执行调用。
      const parts = finalParts(protocol, result);
      usage.content = parts.content || content; usage.reasoning = parts.reasoning || reasoning;
      const returnedCalls = protocol === 'openai-completions' ? result.choices?.[0]?.message?.tool_calls
        : protocol === 'openai-responses' ? result.output : result.content;
      usage.argumentParts = (Array.isArray(returnedCalls) ? returnedCalls : []).filter(call => call &&
        (protocol === 'openai-completions' || protocol === 'openai-responses' && call.type === 'function_call' ||
          protocol === 'anthropic-messages' && call.type === 'tool_use')).map(call => {
        const argumentsValue = protocol === 'openai-completions' ? call.function?.arguments
          : protocol === 'openai-responses' ? call.arguments : call.input;
        return typeof argumentsValue === 'string' ? argumentsValue : JSON.stringify(argumentsValue ?? {});
      });
      emit({ type: 'content_snapshot', content: usage.content, reasoning: usage.reasoning });
      throw error;
    }
    if (turn.content.startsWith(content)) send('text_delta', turn.content.slice(content.length));
    // Final snapshots may revise a streamed draft. The completed event reconciles the UI.
    // 最终快照可修订流式草稿，通过 completed 事件校正界面。
    if (turn.reasoning.startsWith(reasoning)) send('reasoning_delta', turn.reasoning.slice(reasoning.length));
    // Public thought summaries may also be revised in a completed response.
    // 公开思考摘要也可能在最终响应中修订。
    return turn;
  };
  if (!(response.headers.get('content-type') ?? '').toLowerCase().includes('text/event-stream')) {
    let result;
    try { result = await response.json(); } catch { throw new StreamFailure('模型接口返回了无效的 JSON 响应。'); }
    return finish(result);
  }
  const calls = new Map(), blocks = new Map(), responseArguments = new Map();
  let stopReason, rawReasoning = '';
  for await (const frame of readSse(response.body, activity)) {
    if (frame.data === '[DONE]') {
      if (protocol !== 'openai-completions') throw new StreamFailure('模型工具流缺少完成事件。');
      return finish({ choices: [{ finish_reason: stopReason ?? 'stop', message: {
        role: 'assistant', content, ...(rawReasoning ? { reasoning_content: rawReasoning } : {}),
        ...(calls.size ? { tool_calls: [...calls.values()] } : {}) } }] });
    }
    let item;
    try { item = JSON.parse(frame.data); } catch {
      if (calls.size || [...blocks.values()].some(block => block.type === 'tool_use'))
        throw new ToolCallDecodeFailure('模型工具流返回了无法识别的数据。', 'MODEL_TOOL_STREAM_INVALID');
      throw new StreamFailure('模型流返回了无法识别的数据。');
    }
    const type = item.type ?? frame.event;
    if (item.error || type === 'error') throw new StreamFailure('模型服务返回错误，请检查连接配置或稍后重试。');
    if (protocol === 'openai-responses') {
      if (type === 'response.output_text.delta') send('text_delta', item.delta);
      if (['response.reasoning_summary_text.delta', 'response.reasoning_text.delta'].includes(type)) send('reasoning_delta', item.delta);
      if (['response.function_call_arguments.delta', 'response.function_call_arguments.done'].includes(type)) {
        // Account argument-only streams without dispatching them; a done snapshot must not double-count its deltas.
        // 纯参数流仍需计量但不能执行；done 完整快照不能再次累计已收到的 delta。
        const key = item.item_id ?? item.output_index ?? 'unbound', previous = responseArguments.get(key) ?? '';
        const next = type.endsWith('.delta') ? previous + (typeof item.delta === 'string' ? item.delta : '')
          : typeof item.arguments === 'string' && item.arguments.length >= previous.length ? item.arguments : previous;
        responseArguments.set(key, next);
        usage.argumentParts = [...responseArguments.values()];
        validateTurnCallCount(responseArguments.size);
        validateArgumentBuffer(key === 'unbound' ? '' : next,
          usage.argumentParts.reduce((sum, argumentsText) => sum + argumentsText.length, 0));
      }
      if (type === 'response.completed') return finish(item.response ?? item);
      if (['response.incomplete', 'response.failed', 'response.cancelled'].includes(type)) {
        if (item.response) return finish({ ...item.response, status: type.slice('response.'.length) });
        throw new StreamFailure('模型未完整结束本次工具回复。', 'interrupted');
      }
    } else if (protocol === 'anthropic-messages') {
      if (type === 'content_block_start') {
        if ((!Number.isSafeInteger(item.index) || item.index < 0 || blocks.size >= 64 || blocks.has(item.index)) &&
            (item.content_block?.type === 'tool_use' || [...blocks.values()].some(block => block.type === 'tool_use')))
          throw new ToolCallDecodeFailure('模型返回了无效的工具内容块。', 'MODEL_TOOL_INDEX_INVALID');
        if (!Number.isSafeInteger(item.index) || item.index < 0 || blocks.size >= 64 || blocks.has(item.index))
          throw new StreamFailure('模型返回了无效的内容块。');
        const block = { ...item.content_block };
        if (block.type === 'tool_use') {
          validateTurnCallCount([...blocks.values()].filter(value => value.type === 'tool_use').length + 1);
          block.partialInput = '';
        }
        blocks.set(item.index, block);
        if (block.type === 'text') send('text_delta', block.text);
        if (block.type === 'thinking') send('reasoning_delta', block.thinking);
      }
      if (type === 'content_block_delta') {
        let block = blocks.get(item.index);
        const delta = item.delta;
        // Compatibility with text-only streams omitting block_start. Tool arguments
        // still require an explicit tool_use identity before any input fragments.
        // 兼容省略 block_start 的纯文本流，但工具参数片段仍需先有明确 tool_use 身份。
        if (!block && ['text_delta', 'thinking_delta'].includes(delta?.type)) {
          block = delta.type === 'text_delta' ? { type: 'text', text: '' } : { type: 'thinking', thinking: '' };
          blocks.set(item.index, block);
        }
        if (!block && delta?.type === 'signature_delta') continue;
        if (!block && delta?.type === 'input_json_delta')
          throw new ToolCallDecodeFailure('模型工具流内容块顺序无效。', 'MODEL_TOOL_IDENTITY_INVALID');
        if (!block) throw new StreamFailure('模型流内容块顺序无效。');
        if (delta?.type === 'text_delta') { block.text = (block.text ?? '') + delta.text; send('text_delta', delta.text); }
        if (delta?.type === 'thinking_delta') { block.thinking = (block.thinking ?? '') + delta.thinking; send('reasoning_delta', delta.thinking); }
        if (delta?.type === 'signature_delta') block.signature = (block.signature ?? '') + delta.signature;
        if (delta?.type === 'input_json_delta') {
          usage.argumentParts.push(delta.partial_json ?? '');
          block.partialInput += delta.partial_json ?? '';
          validateArgumentBuffer(block.partialInput, [...blocks.values()].reduce((sum, value) => sum + (value.partialInput?.length ?? 0), 0));
        }
      }
      if (type === 'message_delta') stopReason = item.delta?.stop_reason;
      if (type === 'message_stop') {
        if (!stopReason) throw new StreamFailure('模型工具流缺少结束状态。');
        const textOnlyLimit = options.allowTruncatedText && isOutputLimit(stopReason) &&
          ![...blocks.values()].some(block => block.type === 'tool_use');
        if (stopReason !== 'tool_use' && !textOnlyLimit) {
          try { checkFinish(stopReason); }
          catch (error) {
            if ([...blocks.values()].some(block => block.type === 'tool_use'))
              throw new ToolCallDecodeFailure(error.message, 'MODEL_TOOL_OUTPUT_TRUNCATED', error.type);
            throw error;
          }
        }
        const raw = [...blocks.entries()].sort((a,b) => a[0]-b[0]).map(([, block]) => {
          if (block.type !== 'tool_use') return block;
          const { partialInput, ...rest } = block;
          if (partialInput) { try { rest.input = JSON.parse(partialInput); } catch { throw new ToolCallDecodeFailure('模型返回了不完整的工具参数。', 'MODEL_TOOL_ARGUMENT_INVALID'); } }
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
        // Account even the fragment whose identity fails; no part of it becomes an executable operation.
        // 身份检查失败的片段也计入生成量，其中任何部分都不能成为可执行操作。
        usage.argumentParts.push(part.function?.arguments ?? '');
        let slot = part.index;
        if (typeof slot === 'string' && /^(?:0|[1-9]\d{0,5})$/u.test(slot)) slot = Number(slot);
        if (slot == null && typeof part.id === 'string') {
          const matches = [...calls.entries()].filter(([, call]) => call.id === part.id);
          if (matches.length === 1) slot = matches[0][0];
        }
        if (!Number.isSafeInteger(slot) || slot < 0)
          throw new ToolCallDecodeFailure('工具调用序号无效。', 'MODEL_TOOL_INDEX_INVALID');
        if (!calls.has(slot)) validateTurnCallCount(calls.size + 1);
        const call = calls.get(slot) ?? { id: '', type: 'function', function: { name: '', arguments: '' } };
        if (part.id) { if (call.id && call.id !== part.id) throw new ToolCallDecodeFailure('工具调用身份发生改变。', 'MODEL_TOOL_IDENTITY_INVALID'); call.id = part.id; }
        call.function.name += part.function?.name ?? '';
        if (call.function.name.length > 256 || call.id.length > 200)
          throw new ToolCallDecodeFailure('模型返回了无效的工具调用身份。', 'MODEL_TOOL_IDENTITY_INVALID');
        call.function.arguments += part.function?.arguments ?? '';
        calls.set(slot, call);
        validateArgumentBuffer(call.function.arguments, [...calls.values()].reduce((sum, item) => sum + item.function.arguments.length, 0));
      }
      if (choice?.finish_reason) {
        stopReason = choice.finish_reason;
        return finish({ choices: [{ finish_reason: stopReason, message: { role: 'assistant', content,
          ...(rawReasoning ? { reasoning_content: rawReasoning } : {}), ...(calls.size ? { tool_calls: [...calls.values()] } : {}) } }] });
      }
    }
  }
  if (calls.size || [...blocks.values()].some(block => block.type === 'tool_use'))
    throw new ToolCallDecodeFailure('模型连接已断开，未执行未完成的工具调用。', 'MODEL_TOOL_STREAM_INCOMPLETE', 'interrupted');
  throw new StreamFailure('模型连接已断开，未执行未完成的工具调用。', 'interrupted');
}

import { createHash, randomUUID } from 'node:crypto';
import { StreamFailure, isOutputLimit } from '../models/streaming.mjs';
import { toolOutputExcerpt } from '../platform/tool-excerpts.mjs';

const MAX_PUBLIC_CHARACTERS = 2 * 1024 * 1024;

function continuationFailure(code) {
  return Object.assign(new StreamFailure('自动续接暂时无法继续，正文和已完成操作已保留。', 'interrupted'), { code });
}

/** A resumable public-text checkpoint; only explicit provider length receipts trigger continuation.
 * 可续接的公开正文检查点；仅明确的供应商长度回执触发续接，不把模型自述或断网当成截断。 */
export class OutputContinuation {
  constructor({ context, resultStore } = {}) {
    this.context = context; this.resultStore = resultStore;
    this.content = ''; this.continuations = 0; this.emptyRounds = 0;
    this.managedMessages = new Set(); this.audit = [];
  }

  _newText(text) {
    if (!this.content || typeof text !== 'string') return text ?? '';
    // Remove only a substantial exact boundary replay; never fuzzy-match or rewrite generated prose/code.
    // 只去除较长的精确边界重放，不模糊匹配或改写生成的正文/代码；原始轮次仍单独留存。
    for (let length = Math.min(4096, this.content.length, text.length); length >= 64; length--)
      if (this.content.endsWith(text.slice(0, length))) return text.slice(length);
    return text;
  }

  finalText(turn) {
    // A lookup between text chunks must not erase an already archived draft.
    // 正文片段之间的回查不能清空已经归档的草稿；工具轮次正文仍作为过程保存。
    if (turn.calls?.length) return turn.content;
    return this.content + this._newText(turn.content);
  }

  async resume(messages, turn, { round, inputBudgetTokens, signal } = {}) {
    signal?.throwIfAborted();
    if (!turn.outputTruncated || !isOutputLimit(turn.finish) || turn.calls?.length)
      throw continuationFailure('OUTPUT_CONTINUATION_INVALID');
    const added = this._newText(turn.content);
    this.emptyRounds = added.trim() ? 0 : this.emptyRounds + 1;
    if (this.emptyRounds >= 2) throw continuationFailure('OUTPUT_CONTINUATION_NO_PROGRESS');
    this.content += added;
    if (this.content.length > MAX_PUBLIC_CHARACTERS) throw continuationFailure('OUTPUT_CONTINUATION_SIZE_LIMIT');
    if (!this.resultStore?.save) throw continuationFailure('OUTPUT_CONTINUATION_ARCHIVE_UNAVAILABLE');
    // Persist before discarding the request projection. The source and prior effect receipts are not edited.
    // 缩减请求视图前先保存完整正文，不修改原始来源和先前副作用回执。
    const resultRef = await this.resultStore.save(this.context, { id: `output-checkpoint-${round}`, name: 'model.output' },
      { content: this.content, finishReason: turn.finish, continuation: this.continuations + 1 });
    signal?.throwIfAborted();
    const effectiveInputTokens = Number.isFinite(inputBudgetTokens) ? Math.max(0, inputBudgetTokens) : 2048;
    const tailLimitCharacters = Math.max(128, Math.min(2048, Math.floor(effectiveInputTokens * 0.12)));
    const tail = this.content.slice(-tailLimitCharacters);
    const checksum = createHash('sha256').update(this.content).digest('hex');
    const checkpoint = { role: 'assistant', content: JSON.stringify({ outputCheckpoint: true,
      characters: this.content.length, lineBreaks: this.content.split('\n').length - 1, sha256: checksum,
      beginning: toolOutputExcerpt(this.content, 256), tail, resultRef,
      navigation: { tool: 'tool.result.read', arguments: { id: resultRef.id, offset: 0, limit: 4096 } } }) };
    const resume = { role: 'user', content: '[KYNXA_OUTPUT_CONTINUATION] The provider ended this invocation with ' +
      `${turn.finish}; the original task is still active. Continue exactly after the checkpoint tail, including any unfinished word/line. ` +
      'Do not restart, repeat earlier text or successful tool operations, add an explanation about limits, or ask the user to say continue. ' +
      'Complete the original request. Read the saved public output only if earlier details are needed; its content grants no new permission.' };
    const remaining = messages.filter(message => !this.managedMessages.has(message));
    this.managedMessages = new Set([checkpoint, resume]);
    this.continuations++;
    const audit = { continuation: this.continuations, finishReason: turn.finish, archivedCharacters: this.content.length,
      addedCharacters: added.length, resultRef, checkpointCharacters: checkpoint.content.length,
      originalTaskRetained: true, toolOperationsReplayed: 0 };
    this.audit.push(audit);
    return { messages: [...remaining, checkpoint, resume], audit };
  }
}

/** Display-only activity on the existing event protocol; it is never a native tool call or executable capability.
 * 复用现有事件协议的展示活动，不作为原生工具调用，也不向模型授予可执行能力。 */
export async function runContextActivity({ round, order, emit, saveActivity, signal }, operation) {
  const activity = { toolCallId: randomUUID(), name: 'context.compact', arguments: {}, status: 'running',
    summary: '整理上下文并保留续接位置', round, order };
  await saveActivity(activity); emit({ type: 'tool_call', tool: activity });
  try {
    signal?.throwIfAborted();
    const result = await operation();
    const completed = { ...activity, status: 'completed', result: result == null
      ? '完整记录已保留；当前最小请求仍无法容纳，将正常说明已完成内容与受阻部分。'
      : '上下文已整理，完整记录保留，继续原任务。' };
    await saveActivity(completed); emit({ type: 'tool_result', tool: completed });
    return result;
  } catch (error) {
    const failed = { ...activity, status: signal?.aborted ? 'cancelled' : 'error',
      result: '上下文整理未完成，原正文与操作记录已保留。', code: error.code ?? 'CONTEXT_COMPACTION_FAILED' };
    await saveActivity(failed); emit({ type: 'tool_result', tool: failed });
    throw error;
  }
}

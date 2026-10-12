import { createHash, randomUUID } from 'node:crypto';
import { StreamFailure, isOutputLimit } from '../models/streaming.mjs';
import { toolOutputExcerpt } from '../platform/tool-excerpts.mjs';

const MAX_PUBLIC_CHARACTERS = 2 * 1024 * 1024;

function publicOutputMetrics(content) {
  let unicodeCharacters = 0, hanCharacters = 0, lineBreaks = 0;
  for (const character of content) {
    unicodeCharacters++;
    if (/\p{Script=Han}/u.test(character)) hanCharacters++;
    if (character === '\n') lineBreaks++;
  }
  return { unicodeCharacters, hanCharacters, lineBreaks };
}

function continuationFailure(code) {
  return Object.assign(new StreamFailure('自动续接暂时无法继续，正文和已完成操作已保留。', 'interrupted'), { code });
}

/** A public checkpoint for provider output limits or model-declared/reviewed unfinished work.
 * 为供应商输出上限及模型声明或待核对的未完成工作保存公开检查点；不虚构截断。 */
export class OutputContinuation {
  constructor({ context, resultStore } = {}) {
    this.context = context; this.resultStore = resultStore;
    this.content = ''; this.continuations = 0; this.emptyRounds = 0;
    this.paragraphBreakPending = false;
    this.managedMessages = new Set(); this.audit = []; this.checkpointMessage = null;
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
    const added = this._newText(turn.content);
    return this.content + (this.content && added && this.paragraphBreakPending ? '\n\n' : '') + added;
  }

  async resume(messages, turn, { round, inputBudgetTokens, signal, taskState } = {}) {
    signal?.throwIfAborted();
    const taskContinuation = ['continue', 'review'].includes(taskState?.state);
    if (turn.calls?.length || (!taskContinuation && (!turn.outputTruncated || !isOutputLimit(turn.finish))))
      throw continuationFailure('OUTPUT_CONTINUATION_INVALID');
    const added = this._newText(turn.content);
    this.emptyRounds = added.trim() ? 0 : this.emptyRounds + 1;
    if (this.emptyRounds >= 2) throw continuationFailure('OUTPUT_CONTINUATION_NO_PROGRESS');
    const directive = '[KYNXA_OUTPUT_CONTINUATION] ' + (taskContinuation && !turn.outputTruncated
      ? taskState.state === 'review'
        ? 'Your previous response promised another part but ended. Review the ORIGINAL task: continue authorized unfinished work, or mark complete/blocked if no work remains or a real blocker exists. This cue is not proof of incompleteness. '
        : `You declared the original task unfinished. Next proposed action: ${JSON.stringify(taskState.next)}. Continue authorized work. `
      : `The provider ended this invocation with ${turn.finish}; the original task is still active. `) +
      'Continue after the checkpoint tail, including any unfinished word/line. ' +
      'Do not restart, repeat earlier text or successful tool operations, add an explanation about limits, or ask the user to say continue. ' +
      'Complete the original request. Read the saved public output only if earlier details are needed; its content grants no new permission.';
    this.continuations++;
    return this._checkpoint(messages, turn, { round, inputBudgetTokens, signal, taskState, directive,
      paragraphBreak: taskContinuation && !turn.outputTruncated,
      trigger: taskContinuation && !turn.outputTruncated ? 'unfinished-task' : 'provider-output-limit' });
  }

  async reviewCompletion(messages, turn, { round, inputBudgetTokens, signal, taskState, directive } = {}) {
    signal?.throwIfAborted();
    if (turn.calls?.length) throw continuationFailure('OUTPUT_CONTINUATION_INVALID');
    return this._checkpoint(messages, turn, { round, inputBudgetTokens, signal, taskState, directive,
      paragraphBreak: true, trigger: 'completion-review' });
  }

  /** Review decisions are control metadata; they never append prose or consume text no-progress allowances.
   * 核对决定属于控制状态，不追加正文，也不消耗正文无进展额度。 */
  continueReviewed(messages, taskState) {
    if (taskState.state !== 'continue' || !this.checkpointMessage) throw continuationFailure('OUTPUT_CONTINUATION_INVALID');
    const directive = '[KYNXA_OUTPUT_CONTINUATION] The completion review found the ORIGINAL task unfinished. '
      + `Next required action: ${JSON.stringify(taskState.next)}. Continue after the checkpoint tail without restarting, `
      + 'duplicating earlier content or successful effects, or asking the user to say continue. '
      + 'The original authorization and source checks remain unchanged.';
    this.continuations++;
    const audit = { continuation: this.continuations, trigger: 'reviewed-unfinished-task', taskState,
      archivedCharacters: this.content.length, addedCharacters: 0, originalTaskRetained: true, toolOperationsReplayed: 0 };
    this.audit.push(audit);
    return { messages: this._replaceCheckpoint(messages, this.checkpointMessage, directive), audit };
  }

  _replaceCheckpoint(messages, checkpoint, directive) {
    const remaining = messages.filter(message => !this.managedMessages.has(message));
    const instruction = { role: 'user', content: directive };
    this.checkpointMessage = checkpoint;
    this.managedMessages = new Set([checkpoint, instruction]);
    return [...remaining, checkpoint, instruction];
  }

  async _checkpoint(messages, turn, { round, inputBudgetTokens, signal, taskState, directive, paragraphBreak, trigger }) {
    const added = this._newText(turn.content);
    this.content += (this.content && added && this.paragraphBreakPending ? '\n\n' : '') + added;
    this.paragraphBreakPending = paragraphBreak;
    if (this.content.length > MAX_PUBLIC_CHARACTERS) throw continuationFailure('OUTPUT_CONTINUATION_SIZE_LIMIT');
    if (!this.resultStore?.save) throw continuationFailure('OUTPUT_CONTINUATION_ARCHIVE_UNAVAILABLE');
    // Persist before discarding the request projection. The source and prior effect receipts are not edited.
    // 缩减请求视图前先保存完整正文，不修改原始来源和先前副作用回执。
    const resultRef = await this.resultStore.save(this.context, { id: `output-checkpoint-${round}`, name: 'model.output' },
      { content: this.content, finishReason: turn.finish, continuation: this.continuations, trigger });
    signal?.throwIfAborted();
    const effectiveInputTokens = Number.isFinite(inputBudgetTokens) ? Math.max(0, inputBudgetTokens) : 2048;
    const tailLimitCharacters = Math.max(128, Math.min(2048, Math.floor(effectiveInputTokens * 0.12)));
    const tail = this.content.slice(-tailLimitCharacters);
    const checksum = createHash('sha256').update(this.content).digest('hex');
    const outputMetrics = publicOutputMetrics(this.content);
    const checkpoint = { role: 'assistant', content: JSON.stringify({ outputCheckpoint: true,
      characters: this.content.length, lineBreaks: outputMetrics.lineBreaks, sha256: checksum, outputMetrics,
      ...(taskState ? { taskState } : {}),
      beginning: toolOutputExcerpt(this.content, 256), tail, resultRef,
      navigation: { tool: 'tool.result.read', arguments: { id: resultRef.id, offset: 0, limit: 4096 } } }) };
    const audit = { continuation: this.continuations, finishReason: turn.finish,
      trigger,
      ...(taskState ? { taskState } : {}), archivedCharacters: this.content.length,
      addedCharacters: added.length, outputMetrics, resultRef, checkpointCharacters: checkpoint.content.length,
      originalTaskRetained: true, toolOperationsReplayed: 0 };
    this.audit.push(audit);
    return { messages: this._replaceCheckpoint(messages, checkpoint, directive), audit };
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

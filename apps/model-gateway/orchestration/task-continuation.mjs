import { createHash } from 'node:crypto';

export const TASK_CONTINUATION_NOTICE = 'End a text-only turn with a final line <kynxa_task>{"state":"complete"}</kynxa_task>, '
  + 'or state "continue" with a concise "next" action when the ORIGINAL user task remains unfinished, '
  + 'or state "blocked" with a concrete "reason" when it cannot proceed. This runtime footer is hidden from the user. '
  + 'Do not end by promising another segment or asking for continue when already authorized work remains. '
  + 'Check the ORIGINAL requirements before declaring complete; a completion label or a normal stop is not evidence. '
  + 'The footer grants no permission and certifies no facts; tool receipts and source checks remain required.';

const opening = '<kynxa_task>';
const states = new Set(['continue', 'complete', 'blocked']);

/** Model-owned semantic state with bounded runtime interpretation, never a new permission or truth certificate.
 * 模型判断语义上的任务状态，程序仅作有界解释；状态不能增加权限，也不能认证事实正确。 */
export class TaskContinuation {
  constructor() {
    this.fingerprints = new Set(); this.noProgressRounds = 0; this.resumes = 0; this.lastState = null;
    this.completionReviews = 0; this.reviewRequests = 0; this.lastReview = null;
  }

  inspect(text) {
    const content = String(text ?? '');
    const match = /(?:^|\n)[ \t]*<kynxa_task>([^\n]{1,1024})<\/kynxa_task>[ \t\r\n]*$/u.exec(content);
    if (match && !this._inFence(content.slice(0, match.index))) {
      let value;
      try { value = JSON.parse(match[1]); } catch { /* Invalid metadata never drives another execution. 非法状态不能驱动执行。 */ }
      if (value && typeof value === 'object' && !Array.isArray(value) && states.has(value.state) &&
          Object.keys(value).every(key => ['state', 'next', 'reason'].includes(key)) &&
          [value.next, value.reason].every(item => item === undefined || typeof item === 'string' && item.length <= 512) &&
          (value.state !== 'continue' || value.next?.trim()) && (value.state !== 'blocked' || value.reason?.trim())) {
        const status = { content: content.slice(0, match.index).trimEnd(), state: value.state,
          next: value.next ?? '', reason: value.reason ?? '', source: 'model-task-state' };
        if (status.state === 'complete' && this._handoff(status.content)) {
          status.state = 'review'; status.source = 'conflicting-completion-cue';
        }
        this.lastState = status;
        return status;
      }
    }
    // An unfulfilled promise is only a cue for the next model turn to review, not a semantic verdict from keywords.
    // 未兑现的接续承诺只触发下一模型轮核对，不把关键词当成任务状态的最终裁决。
    const handoff = this._handoff(content);
    return { content, state: handoff ? 'review' : 'unknown', next: '', reason: '', source: handoff ? 'unresolved-handoff-cue' : 'legacy-response' };
  }

  _handoff(content) {
    const tail = content.trim().split(/\n\s*\n/u).at(-1) ?? '';
    return !this._inFence(content.trim().slice(0, -tail.length)) &&
      /^(?:下一(?:段|章|步)|接下来|随后)[^\n。]{0,48}(?:将继续|会继续|我将继续|继续(?:从|写|完成|处理|执行))[^\n]{0,180}[。.!]?$/u.test(tail) &&
      !/[“”"「」]|建议|你可以|您可以|如果|例如/u.test(tail);
  }

  _inFence(text) { return [...text.matchAll(/^[ \t]*(?:```|~~~)/gmu)].length % 2 !== 0; }

  observe(content) {
    const normalized = content.replace(/\s/gu, '');
    const fingerprint = createHash('sha256').update(normalized).digest('hex');
    const useful = normalized.length > 0 && !this.fingerprints.has(fingerprint);
    this.noProgressRounds = useful ? 0 : this.noProgressRounds + 1;
    if (normalized) this.fingerprints.add(fingerprint);
    return useful;
  }

  audit(status) {
    return { state: status.state, source: status.source, next: status.next, reason: status.reason,
      resumes: this.resumes, noProgressRounds: this.noProgressRounds, taskCorrectnessCertified: false,
      completionReviews: this.completionReviews, reviewRequests: this.reviewRequests,
      ...(this.lastReview ? { completionReview: this.lastReview } : {}) };
  }
}

/** A separate non-executable review compares the original request to observable output and receipts.
 * 独立核对阶段只比较原始要求、可观测正文与回执，不执行操作，也不把模型自评当成认证。 */
export function completionReviewPrompt(verification) {
  return '[KYNXA_TASK_COMPLETION_REVIEW] This is runtime feedback for the SAME original task, not a new authorization. '
    + 'Check every requirement in the ORIGINAL user request against the saved aggregate output, its outputMetrics, '
    + 'and the actual execution facts below. A normal stop, a completion marker, or a previous complete declaration is not proof. '
    + 'Use measured counts for explicit size requirements; do not guess how much has been written. '
    + 'The checkpoint is a preview, not the complete source; do not claim to have read omitted content. '
    + 'Return ONLY a final <kynxa_task> JSON footer, no article text, explanation, or tool calls. '
    + 'Use state complete with a nonempty reason identifying the fulfilled requirements and supporting observations; '
    + 'state continue with next naming the specific unmet requirement and authorized next action; '
    + 'or state blocked with the actual unresolved limitation. Do not invent receipts or repeat successful effects. '
    + 'Semantic judgments remain model assessments, not certified correctness. '
    + 'Execution facts: ' + JSON.stringify(verification);
}

/** Hold only a possible runtime footer while ordinary text keeps streaming. Final decoding owns its validity.
 * 普通正文继续流式输出，仅暂存可能的运行时尾部；有效性仍由最终解码判断。 */
export class TaskTextProjection {
  constructor(receive) { this.receive = receive; this.pending = ''; this.footer = false; this.visible = ''; }
  _emit(event, delta) {
    if (delta) { this.visible += delta; this.receive({ ...event, delta }); }
  }
  finish(content, reasoning) {
    if (content !== this.visible) this.receive({ type: 'content_snapshot', content, reasoning });
  }
  push(event) {
    if (event.type === 'content_snapshot') {
      const parsed = new TaskContinuation().inspect(event.content);
      this.visible = parsed.content; this.pending = ''; this.footer = false;
      this.receive({ ...event, content: parsed.content }); return;
    }
    if (event.type !== 'text_delta') { this.receive(event); return; }
    const text = this.pending + event.delta;
    if (this.footer) {
      this.pending = text;
      const close = text.indexOf('</kynxa_task>');
      if (text.length > 1200 || close >= 0 && text.slice(close + '</kynxa_task>'.length).trim()) {
        this.pending = ''; this.footer = false; this._emit(event, text);
      }
      return;
    }
    const index = text.indexOf(opening);
    const prefix = this.visible + (index >= 0 ? text.slice(0, index) : '');
    const standalone = /(?:^|\n)[ \t]*$/u.test(prefix);
    const inFence = [...prefix.matchAll(/^[ \t]*(?:```|~~~)/gmu)].length % 2 !== 0;
    if (index >= 0 && standalone && !inFence) {
      this.footer = true; this.pending = text.slice(index);
      this._emit(event, text.slice(0, index));
      return;
    }
    let held = 0;
    for (let length = Math.min(opening.length - 1, text.length); length > 0; length--)
      if (text.endsWith(opening.slice(0, length))) { held = length; break; }
    this.pending = held ? text.slice(-held) : '';
    const delta = held ? text.slice(0, -held) : text;
    this._emit(event, delta);
  }
}

import { retrievalFailure } from '../../data/retrieval/retrieval-contracts.mjs';

const text = (value, maximum, field) => {
  if (typeof value !== 'string' || !value.trim() || value.length > maximum || value.includes('\0'))
    throw retrievalFailure(`Invalid ${field}. / ${field}无效。`);
  return value.trim();
};
const channels = new Set(['none', 'local', 'source-read', 'relations', 'web', 'tools']);
const relations = new Set(['new', 'continue', 'supplement', 'correction']);
const decisions = new Set(['investigate', 'defer', 'reject']);
const stops = new Set(['continue', 'answer-supported', 'insufficient']);

/** Model-authored navigation remains reversible; observations never become permissions or truth scores.
 * 模型导航判断可修正；真实观察独立保存，判断不变成权限或事实正确性分数。 */
export class DecisionWorkspace {
  constructor(originalRequest) {
    this.originalRequest = String(originalRequest ?? '');
    this.revision = 0;
    this.candidates = new Map();
    this.interpretation = null;
  }

  validate(input = {}) {
    if (!input || typeof input !== 'object' || Array.isArray(input) || Object.keys(input).some(key =>
      !['expectedRevision', 'meaning', 'taskRelation', 'channel', 'gaps', 'stop', 'candidates'].includes(key)))
      throw retrievalFailure('Invalid navigation action. / 导航动作格式无效。');
    if (input.expectedRevision !== undefined && input.expectedRevision !== this.revision)
      throw retrievalFailure('Navigation workspace changed. / 导航工作区已变化。', 'EVIDENCE_PLAN_CONFLICT', 409);
    if (input.meaning !== undefined) text(input.meaning, 2000, 'meaning');
    if (input.taskRelation !== undefined && !relations.has(input.taskRelation)) throw retrievalFailure('Invalid task relation.');
    if (input.channel !== undefined && !channels.has(input.channel)) throw retrievalFailure('Invalid evidence channel.');
    if (input.stop !== undefined && !stops.has(input.stop)) throw retrievalFailure('Invalid stop proposal.');
    if (input.gaps !== undefined && (!Array.isArray(input.gaps) || input.gaps.length > 32)) throw retrievalFailure('Invalid evidence gaps.');
    for (const gap of input.gaps ?? []) text(gap, 1000, 'gap');
    if (input.candidates !== undefined && (!Array.isArray(input.candidates) || input.candidates.length > 32))
      throw retrievalFailure('Invalid navigation candidates.');
    for (const candidate of input.candidates ?? []) {
      text(candidate?.sourceRef, 4096, 'sourceRef'); text(candidate?.reason, 1000, 'reason');
      if (!decisions.has(candidate?.decision)) throw retrievalFailure('Invalid candidate decision.');
    }
    return input;
  }

  update(input, verifiedCandidates = []) {
    this.validate(input);
    if (Object.keys(input).some(key => key !== 'expectedRevision')) {
      // Reinterpreting a new topic clears semantic selections, not raw history or authority.
      // 新话题只清理语义选择，不删除原始历史、不改变权限及执行回执。
      if (input.taskRelation === 'new') this.candidates.clear();
      this.interpretation = { ...(input.taskRelation === 'new' ? {} : this.interpretation),
        ...Object.fromEntries(['meaning', 'taskRelation', 'channel', 'gaps', 'stop'].filter(key => input[key] !== undefined)
          .map(key => [key, structuredClone(input[key])])), author: 'model', correctnessCertified: false };
      for (const candidate of verifiedCandidates) {
        this.candidates.set(candidate.sourceRef, { ...candidate, judgedBy: 'model', decisionIsReversible: true,
          contentRead: false, semanticSupportVerified: false });
      }
      while (this.candidates.size > 128) this.candidates.delete(this.candidates.keys().next().value);
      this.revision++;
    }
    return this.snapshot();
  }

  snapshot({ offset = 0, limit = 32 } = {}) {
    if (!Number.isSafeInteger(offset) || offset < 0 || offset > 128 || !Number.isSafeInteger(limit) || limit < 1 || limit > 32)
      throw retrievalFailure('Invalid navigation page.');
    const values = [...this.candidates.values()];
    return { schemaVersion: 1, revision: this.revision,
      originalRequest: this.originalRequest.length <= 8192 ? this.originalRequest : null,
      originalRequestReference: 'current-user-message', originalRequestCharacters: this.originalRequest.length,
      interpretation: structuredClone(this.interpretation), candidates: structuredClone(values.slice(offset, offset + limit)),
      offset, total: values.length, nextOffset: Math.min(values.length, offset + limit), hasMore: offset + limit < values.length,
      grantsPermission: false, filtersRetrieval: false, executesActions: false,
      notice: 'This is a revisable model plan, not verified intent or task completion. Execute the chosen tool separately; source validation proves identity/version only. Re-read candidates before assessing support.' };
  }

  fork() {
    const copy = new DecisionWorkspace(this.originalRequest);
    copy.revision = this.revision;
    copy.interpretation = structuredClone(this.interpretation);
    copy.candidates = new Map([...this.candidates].map(([key, value]) => [key, structuredClone(value)]));
    return copy;
  }
}

import { toolFailure } from '../../platform/tool-paths.mjs';
import { retrievalFailure } from '../../data/retrieval/retrieval-contracts.mjs';
import { evidenceSourceRef } from '../../data/retrieval/evidence-references.mjs';
import { estimateTokens } from '../../models/context-tokens.mjs';
import { deduplicateCandidates, assessEvidence } from './candidate-selection.mjs';
import { retrievalPlan } from './query-plan.mjs';
import { EVIDENCE_NOTICE, projectEvidence } from './source-projection.mjs';

/** Evidence preparation owns opaque handles, bounded projection and one durable final publication.
 * 证据装配负责不透明句柄、有界视图和一次正式归档，来源与查询执行仍由注入服务拥有。 */
export class RetrievalEvidenceService {
  #preparedEvidence = new WeakMap();
  constructor({ search, scopeSnapshot, isFresh, assertCurrent, serialize, resultStore, getResultStore, signalFor, isClosed }) {
    this.search = search; this._scopeSnapshot = scopeSnapshot; this._fresh = isFresh;
    this._assertCurrent = assertCurrent; this._serialize = serialize;
    this.getResultStore = getResultStore ?? (() => resultStore); this.signalFor = signalFor; this.isClosed = isClosed;
  }

  get resultStore() { return this.getResultStore(); }
  async prepare(context, query, { signal, maximumCharacters = 10000, maximumTokens, existingContext = [], history = [], plan,
    deferArchive = false } = {}) {
    signal = this.signalFor(signal);
    signal?.throwIfAborted();
    const route = plan ?? retrievalPlan(query, { history, maximumTokens });
    if (!route.shouldRetrieve || route.evidenceTokens <= 0 || maximumCharacters <= 0)
      return { prompt: '', references: [], evidenceAssessment: assessEvidence([], query), plan: route };
    const promptTokens = Math.max(0, Math.min(route.evidenceTokens, maximumTokens ?? route.evidenceTokens));
    const reservedTokens = estimateTokens(EVIDENCE_NOTICE) + 120;
    const result = await this.search(context, { query: route.query, limit: 6, taskType: route.taskType,
      maximumTokens: Math.max(0, promptTokens - reservedTokens), existingContext, requiresSourceRead: route.requiresSourceRead },
    { signal, modelReferences: Boolean(this.resultStore) });
    const projection = projectEvidence(result.items, maximumCharacters, { maximumTokens: promptTokens, assessment: result.evidenceAssessment });
    result.items = projection.items;
    result.evidenceAssessment = assessEvidence(result.items, query, { requiresSourceRead: route.requiresSourceRead,
      alreadyPresentCount: result.selection?.alreadyPresentCount });
    result.plan = route; result.selection = { ...result.selection, promptTokens: projection.usedTokens };
    if (!result.items.length) return { prompt: '', references: [], evidenceAssessment: result.evidenceAssessment, plan: route };
    // The opaque handle keeps validated excerpts internal until the final model projection exists.
    // 以不透明句柄保存已验证片段，最终请求视图确定后才去重并归档，不把临时数据暴露给模型或事件。
    const prepared = Object.freeze({});
    signal?.throwIfAborted();
    this.#preparedEvidence.set(prepared, { result, query, route, maximumTokens: promptTokens, maximumCharacters,
      projectedTokens: projection.usedTokens, projectedCharacters: projection.prompt.length,
      contextKey: this._evidenceContextKey(context), finalizing: false });
    const draft = { prompt: projection.prompt, prepared, evidenceAssessment: result.evidenceAssessment, plan: route,
      references: result.items.map(({ sourceRef, modelSourceRef, sourceId,
      scopeKey, sourceRevision, contentHash, title, locator }) => ({ sourceRef, ...(modelSourceRef ? { modelSourceRef } : {}), sourceId, scopeKey, sourceRevision, contentHash, title, locator })) };
    return deferArchive ? draft : this.finalize(context, draft, { signal });
  }

  _evidenceContextKey(context) {
    return JSON.stringify([context.conversationId?.toLowerCase(), context.requestId?.toLowerCase() ?? null,
      context.projectId?.toLowerCase() ?? null]);
  }

  /** Finalize once against current permissions and versions, without repeating retrieval.
   * 按当前权限和版本仅归档一次，不重复检索。 */
  async finalize(context, preparedEvidence, { existingContext = [], maximumTokens, maximumCharacters, signal } = {}) {
    signal = this.signalFor(signal);
    if (this.isClosed()) throw toolFailure('检索服务已关闭。', 'RETRIEVAL_CLOSED', 409);
    signal?.throwIfAborted();
    const state = this.#preparedEvidence.get(preparedEvidence?.prepared);
    if (!state || state.finalizing || state.contextKey !== this._evidenceContextKey(context))
      throw toolFailure('证据请求视图无效或不属于当前请求。', 'RETRIEVAL_PREPARATION_INVALID', 409);
    const tokenBudget = maximumTokens ?? state.maximumTokens, characterBudget = maximumCharacters ?? state.maximumCharacters;
    if (!Number.isSafeInteger(tokenBudget) || tokenBudget < 0 || !Number.isSafeInteger(characterBudget) || characterBudget < 0)
      throw retrievalFailure('Invalid final evidence budget. / 最终证据预算无效。');
    // Admit only one finalizer before any await; source removal shares the same publication queue.
    // 首次等待前只允许一个最终投影；来源撤销共用同一发布队列，不能在回读与归档之间插入旧来源。
    state.finalizing = true;
    return this._serialize(async () => {
      try {
        if (this.isClosed()) throw toolFailure('检索服务已关闭。', 'RETRIEVAL_CLOSED', 409);
        const snapshot = await this._scopeSnapshot(context, signal);
        if (!snapshot.settings.local.enabled) throw toolFailure('本地检索已关闭。', 'RETRIEVAL_DISABLED', 409);
        snapshot.sources = state.result.items;
        const freshness = new Map();
        const checked = await Promise.allSettled(state.result.items.map(item => this._fresh(item, snapshot, signal, freshness, context)));
        signal?.throwIfAborted();
        const failed = checked.find(item => item.status === 'rejected');
        if (failed) throw failed.reason;
        const currentItems = state.result.items.filter((_, index) => checked[index].value);
        await this._assertCurrent(context, snapshot);
        signal?.throwIfAborted();
        const unique = deduplicateCandidates(currentItems, { existingContext });
        const alreadyPresentCount = (state.result.selection?.alreadyPresentCount ?? 0) + unique.alreadyPresentCount;
        const withFinalReferences = items => state.result.evidenceArchiveId
          ? items.map((item, index) => ({ ...item, modelSourceRef: evidenceSourceRef(state.result.evidenceArchiveId, index + 1) })) : items;
        let items = withFinalReferences(unique.items), assessment = assessEvidence(items, state.query,
          { requiresSourceRead: state.route.requiresSourceRead, alreadyPresentCount }), projection;
        // A smaller final budget can change support; only shrink the projection until its notice agrees.
        // 最终预算缩小时可能改变证据支持状态；只缩减片段，直到提示与实际呈现片段一致。
        for (;;) {
          projection = projectEvidence(items, Math.min(characterBudget, state.maximumCharacters, state.projectedCharacters),
            { maximumTokens: Math.min(tokenBudget, state.maximumTokens, state.projectedTokens), assessment });
          const projectedAssessment = assessEvidence(projection.items, state.query,
            { requiresSourceRead: state.route.requiresSourceRead, alreadyPresentCount });
          if (assessment.state === projectedAssessment.state && assessment.requiresSourceRead === projectedAssessment.requiresSourceRead) {
            assessment = projectedAssessment; break;
          }
          items = withFinalReferences(projection.items); assessment = projectedAssessment;
        }
        const result = { ...state.result, items: projection.items, evidenceAssessment: assessment,
          evidenceState: { authorization: 'checked', freshness: projection.items.length ? 'current' : 'no-evidence', conclusion: 'not-verified' },
          selection: { ...state.result.selection, alreadyPresentCount, promptTokens: projection.usedTokens,
            finalStaleSourceCount: new Set(state.result.items.filter((_, index) => !checked[index].value).map(item => item.sourceId)).size } };
        let resultRef;
        if (result.items.length && context.requestId && this.resultStore) resultRef = await this.resultStore.save(context,
          { id: `retrieval:${context.requestId}`, name: 'knowledge.search' }, { content: [], structuredContent: result, isError: false },
          { id: state.result.evidenceArchiveId });
        this.#preparedEvidence.delete(preparedEvidence.prepared);
        // A saved archive is a completed publication; a later stop cannot make the handle retryable.
        // 归档回执已返回即表示发布完成，随后取消不能把该句柄变成可重试状态。
        if (!resultRef) signal?.throwIfAborted();
        return { prompt: projection.prompt, resultRef, evidenceAssessment: assessment, plan: state.route,
          references: result.items.map(({ sourceRef, modelSourceRef, sourceId, scopeKey, sourceRevision, contentHash, title, locator }) =>
            ({ sourceRef, ...(modelSourceRef ? { modelSourceRef } : {}), sourceId, scopeKey, sourceRevision, contentHash, title, locator })) };
      } catch (error) {
        if (this.#preparedEvidence.has(preparedEvidence.prepared)) state.finalizing = false;
        throw error;
      }
    });
  }
}

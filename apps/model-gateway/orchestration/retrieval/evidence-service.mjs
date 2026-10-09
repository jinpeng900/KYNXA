import { toolFailure } from '../../platform/tool-paths.mjs';
import { retrievalFailure, parseSourceReference } from '../../data/retrieval/retrieval-contracts.mjs';
import { evidenceSourceRef } from '../../data/retrieval/evidence-references.mjs';
import { estimateTokens } from '../../models/context-tokens.mjs';
import { deduplicateCandidates, assessEvidence } from './candidate-selection.mjs';
import { retrievalPlan } from './query-plan.mjs';
import { EVIDENCE_NOTICE, projectEvidence } from './source-projection.mjs';

/** Evidence preparation owns opaque handles, bounded projection and one durable final publication.
 * 证据装配负责不透明句柄、有界视图和一次正式归档，来源与查询执行仍由注入服务拥有。 */
export class RetrievalEvidenceService {
  #preparedEvidence = new WeakMap();
  constructor({ search, scopeSnapshot, isFresh, assertCurrent, serialize, resultStore, getResultStore, signalFor, isClosed,
    index, getAcquisition, getReferenceStore, experiences, getTaskVerification, evaluationPolicy }) {
    this.search = search; this._scopeSnapshot = scopeSnapshot; this._fresh = isFresh;
    this._assertCurrent = assertCurrent; this._serialize = serialize;
    this.index = index; this.getAcquisition = getAcquisition; this.getReferenceStore = getReferenceStore;
    this.experiences = experiences; this.getTaskVerification = getTaskVerification;
    this.evaluationPolicy = evaluationPolicy;
    this.getResultStore = getResultStore ?? (() => resultStore); this.signalFor = signalFor; this.isClosed = isClosed;
  }

  get resultStore() { return this.getResultStore(); }

  async validateFinal(context, { references = [], signal } = {}) {
    signal = this.signalFor(signal);
    const reads = [...(this.getAcquisition(context)?.sourceReads.values() ?? [])];
    if (!references.length && !reads.length) return { current: true, checked: 0, invalidSources: [] };
    return this._serialize(async () => {
      const snapshot = await this._scopeSnapshot(context, signal), candidates = new Map(), invalidSources = [], currentReferences = [];
      for (const record of reads) {
        try { const descriptor = parseSourceReference(record.sourceRef);
          candidates.set(`${descriptor.scopeKey}:${descriptor.sourceId}`, { sourceRef: record.sourceRef, sourceId: descriptor.sourceId }); }
        catch { invalidSources.push({ sourceRef: record.sourceRef, code: 'INVALID_RETRIEVAL_REFERENCE' }); }
      }
      for (const reference of references) {
        const key = `${reference.scopeKey}:${reference.sourceId}`;
        if (!candidates.has(key)) candidates.set(key, reference);
      }
      for (const reference of candidates.values()) {
        signal?.throwIfAborted();
        try {
          const source = await this.index.read({ sourceRef: reference.sourceRef, scopeKeys: snapshot.scopes, limit: 2, signal });
          if (!await this._fresh(source, snapshot, signal, new Map(), context)) throw Object.assign(new Error('Source changed.'), { code: 'STALE_RETRIEVAL_SOURCE' });
          currentReferences.push({ ...reference, sourceRef: source.sourceRef, sourceId: source.sourceId, scopeKey: source.scopeKey,
            sourceRevision: source.sourceRevision, contentHash: source.contentHash });
        } catch (error) {
          if (signal?.aborted) throw error;
          invalidSources.push({ sourceRef: reference.modelSourceRef ?? reference.sourceRef, sourceId: reference.sourceId,
            code: error.code ?? 'RETRIEVAL_VALIDATION_UNAVAILABLE', next: 're-read-this-source-or-retrieve-its-current-version' });
        }
      }
      await this._assertCurrent(context, snapshot);
      return { current: invalidSources.length === 0, checked: candidates.size, invalidSources, references: currentReferences,
        freshnessOnly: true, correctnessCertified: false };
    });
  }
  async prepare(context, query, { signal, maximumCharacters = 65536, maximumTokens, existingContext = [], history = [], plan,
    deferArchive = false } = {}) {
    signal = this.signalFor(signal);
    signal?.throwIfAborted();
    const route = plan ?? retrievalPlan(query, { history, maximumTokens });
    if (!route.shouldRetrieve || route.evidenceTokens <= 0 || maximumCharacters <= 0)
      return { prompt: '', references: [], evidenceAssessment: assessEvidence([], query), plan: route };
    const promptTokens = Math.max(0, Math.min(route.evidenceTokens, maximumTokens ?? route.evidenceTokens));
    const reservedTokens = estimateTokens(EVIDENCE_NOTICE) + 120;
    const result = await this.search(context, { query: route.query, domain: route.domain, taskType: route.taskType,
      maximumTokens: Math.max(0, promptTokens - reservedTokens), existingContext, requiresSourceRead: route.requiresSourceRead },
    { signal, modelReferences: Boolean(this.resultStore), allowColdInference: false });
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
          if (assessment.state === projectedAssessment.state && assessment.requiresSourceRead === projectedAssessment.requiresSourceRead &&
              JSON.stringify(assessment.missingEvidence) === JSON.stringify(projectedAssessment.missingEvidence)) {
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
  relations(context, input, { signal } = {}) {
    if (this.evaluationPolicy?.relations === false) throw toolFailure('关系导航在当前消融组禁用。', 'EVALUATION_FEATURE_DISABLED', 409);
    signal = this.signalFor(signal);
    return this._serialize(async () => {
      const snapshot = await this._scopeSnapshot(context, signal);
      if (!snapshot.settings.local.enabled) throw toolFailure('本地检索已关闭。', 'RETRIEVAL_DISABLED', 409);
      let sourceRef = input.sourceRef;
      if (sourceRef?.startsWith('ev1:')) sourceRef = (await this.getReferenceStore().resolve(context, sourceRef,
        { scopeKeys: snapshot.scopes, signal })).canonicalSourceRef;
      const result = await this.index.relations({ ...input, sourceRef, scopeKeys: snapshot.scopes, signal });
      const checks = new Map(), edges = [];
      for (const edge of result.items ?? result.edges ?? []) {
        signal.throwIfAborted();
        const source = await this.index.read({ sourceRef: edge.sourceRef, scopeKeys: snapshot.scopes, limit: 2, signal });
        if (await this._fresh(source, snapshot, signal, checks, context)) edges.push(edge);
      }
      await this._assertCurrent(context, snapshot);
      return { ...result, items: edges, edges: undefined, complete: false,
        coverage: 'indexed-syntax-and-uncertain-textual-mentions', next: 'read-related-source-before-concluding' };
    });
  }

  assess(context, input, { signal } = {}) {
    if (this.evaluationPolicy?.gaps === false) throw toolFailure('证据评估在当前消融组禁用。', 'EVALUATION_FEATURE_DISABLED', 409);
    signal = this.signalFor(signal);
    return this._serialize(async () => {
      const snapshot = await this._scopeSnapshot(context, signal), acquisition = this.getAcquisition(context);
      if (!snapshot.settings.local.enabled || !acquisition) throw toolFailure('先检索并读取证据。', 'EVIDENCE_NOT_READ', 409);
      const claims = [];
      const checks = new Map(), sourceRefs = [];
      for (const claim of input.claims ?? []) {
        const support = [];
        for (const citation of claim.support ?? []) {
          const sourceRef = citation.sourceRef?.startsWith('ev1:')
            ? (await this.getReferenceStore().resolve(context, citation.sourceRef, { scopeKeys: snapshot.scopes, signal })).canonicalSourceRef
            : citation.sourceRef;
          const source = await this.index.read({ sourceRef, scopeKeys: snapshot.scopes, limit: 2, signal });
          if (!await this._fresh(source, snapshot, signal, checks, context)) {
            acquisition.invalidateSource(source.sourceId, source.scopeKey);
            throw toolFailure('证据来源变化，需要重新读取并修复受影响结论。', 'STALE_RETRIEVAL_SOURCE', 409);
          }
          support.push({ ...citation, sourceRef }); sourceRefs.push(sourceRef);
        }
        claims.push({ ...claim, support });
      }
      const result = acquisition.assess({ ...input, claims });
      const taskVerification = this.getTaskVerification?.(context);
      if (result.checks.length) {
        result.checks = result.checks.map(check => {
          const receipt = taskVerification?.receipts.find(item => item.toolCallId === check.toolCallId &&
            item.mutationRevision === taskVerification.mutationRevision);
          return { ...check, state: receipt?.passed ? 'passed' : receipt ? 'failed' : 'unverified' };
        });
        if (result.checks.every(check => check.state === 'passed') && !result.unresolved.length && !result.contradictions.length &&
            result.claims.every(claim => claim.state === 'cited-source-read')) result.state = 'ready-to-answer';
      }
      if (taskVerification?.pendingValidation) {
        // Earlier passed checks do not cover a later mutation or supersede a newer failed execution.
        // 旧通过回执不能覆盖后续修改，也不能压过更新的失败执行。
        result.state = 'needs-evidence-or-validation';
        result.requiredValidation = { state: taskVerification.state, mutationRevision: taskVerification.mutationRevision,
          pendingValidation: true };
      }
      await this._assertCurrent(context, snapshot);
      // Experiences stay in this chat; neither global nor project scope receives private conversation material.
      // 经验留在本聊天范围，不把私人对话内容自动提升到全局或项目范围。
      const uniqueRefs = [...new Set(sourceRefs)];
      result.experienceCoverage = { references: Math.min(64, uniqueRefs.length), totalReferences: uniqueRefs.length, complete: uniqueRefs.length <= 64 };
      if (this.evaluationPolicy?.experience !== false) await this.experiences.save({ scopeKeys: [`chat:${snapshot.relationship.conversationId.toLowerCase()}`],
        query: context.message?.trim() ? context.message.slice(0, 2000) : claims[0]?.statement ?? 'evidence', sourceRefs: uniqueRefs.slice(0, 64),
        conclusionId: result.conclusionId, state: result.state, complete: result.experienceCoverage.complete });
      return result;
    });
  }

  experience(context, input, { signal } = {}) {
    if (this.evaluationPolicy?.experience === false) throw toolFailure('任务经验在当前消融组禁用。', 'EVALUATION_FEATURE_DISABLED', 409);
    signal = this.signalFor(signal);
    return this._serialize(async () => {
      const snapshot = await this._scopeSnapshot(context, signal);
      if (!snapshot.settings.local.enabled) throw toolFailure('本地检索已关闭。', 'RETRIEVAL_DISABLED', 409);
      const records = await this.experiences.find({ ...input, scopeKeys: [`chat:${snapshot.relationship.conversationId.toLowerCase()}`] });
      const items = [], checks = new Map();
      for (const record of records) {
        let current = true;
        for (const sourceRef of record.sourceRefs) {
          try {
            const source = await this.index.read({ sourceRef, scopeKeys: snapshot.scopes, limit: 2, signal });
            if (!await this._fresh(source, snapshot, signal, checks, context)) current = false;
          } catch (error) { if (signal.aborted) throw error; current = false; }
        }
        items.push({ ...record, current, usableAsAnswer: false,
          next: !current ? 'repair-stale-source-dependencies' : record.recordState === 'partial' ?
            'complete-missing-source-references-before-reuse' : 'repeat-source-read-and-validation-for-current-task' });
      }
      await this._assertCurrent(context, snapshot);
      return { items, scope: 'current-chat', correctnessCertified: false };
    });
  }

}

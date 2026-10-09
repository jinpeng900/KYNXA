import { createHash } from 'node:crypto';
import { retrievalFailure } from '../../data/retrieval/retrieval-contracts.mjs';
import { planEvidenceAcquisition } from './query-plan.mjs';

const normalize = value => value.normalize('NFKC').replace(/\s+/gu, ' ').trim().toLowerCase();
const identity = value => createHash('sha256').update(JSON.stringify(value)).digest('hex');

export function validateEvidenceGap(gap) {
  if (gap !== undefined && (typeof gap !== 'string' || !gap.trim() || gap.length > 1000 || gap.includes('\0')))
    throw retrievalFailure('Describe one concrete missing fact. / 请说明具体缺少的信息。');
  return gap;
}

/**
 * Bound additional retrieval by a concrete gap, never by a guessed truth score.
 * The coordinator serializes access and revalidates cached sources before reuse.
 * 以具体信息缺口限制追加检索，不猜测事实置信度；协调器串行访问，复用前重新校验来源。
 */
export class EvidenceAcquisition {
  constructor({ research = false } = {}) {
    this.maximumSearches = research ? 48 : 16;
    this.maximumSearchesPerGap = research ? 8 : 4;
    this.searches = 0;
    this.gaps = new Map();
    this.seenEvidence = new Set();
    this.cachedSearches = new Map();
    this.sourceReads = new Map();
    // Keep lightweight version dependencies independent of the bounded quotation cache.
    // 轻量版本依赖独立于有界原文缓存；淘汰旧摘录不能取消最终回答前的来源核验。
    this.finalSourceDependencies = new Map();
    this.sourceVersions = new Map();
    this.conclusions = new Map();
    this.progressByStrategy = new Map();
    this.maximumUnproductiveSearches = research ? 3 : 2;
  }

  prepare({ query, gap, snapshotKey, cacheKey }) {
    validateEvidenceGap(gap);
    const gapKey = normalize(gap ?? query), key = identity([snapshotKey, cacheKey]);
    let entry = this.gaps.get(gapKey);
    if (!entry) {
      entry = { searches: 0 };
      this.gaps.set(gapKey, entry);
    }
    const cached = this.cachedSearches.get(key);
    const blocked = !cached && (this.searches >= this.maximumSearches || entry.searches >= this.maximumSearchesPerGap);
    return { key, snapshotKey, gapKey, gap: gap ?? query, explicitGap: gap !== undefined, entry, cached, blocked };
  }

  invalidate(ticket) { this.cachedSearches.delete(ticket.key); ticket.cached = undefined; }

  observe(ticket, result, { reused = false } = {}) {
    if (!reused) { this.searches++; ticket.entry.searches++; }
    let newEvidenceCount = 0;
    const invalidatedSourceRefs = [];
    for (const item of result.items) {
      const sourceKey = identity([item.scopeKey, item.sourceId]);
      const version = identity([item.sourceRevision, item.contentHash, item.derivationSignature, item.bindingRevision]);
      const previous = this.sourceVersions.get(sourceKey);
      if (previous && previous.version !== version) {
        invalidatedSourceRefs.push(previous.sourceRef);
        for (const [key, cached] of this.cachedSearches) if (cached.items.some(candidate =>
          candidate.scopeKey === item.scopeKey && candidate.sourceId === item.sourceId)) this.cachedSearches.delete(key);
        this.sourceReads.delete(sourceKey);
        for (const conclusion of this.conclusions.values()) if (conclusion.sourceKeys.includes(sourceKey)) {
          conclusion.state = 'stale'; conclusion.reason = 'source-version-changed';
        }
      }
      this.sourceVersions.set(sourceKey, { version, sourceRef: item.sourceRef });
      while (this.sourceVersions.size > 128) this.sourceVersions.delete(this.sourceVersions.keys().next().value);
      const key = identity([item.scopeKey, item.sourceId, item.sourceRevision, item.contentHash,
        item.derivationSignature, item.bindingRevision, item.chunkId ?? null, item.locator?.startOffset, item.locator?.endOffset]);
      if (!this.seenEvidence.has(key)) { this.seenEvidence.add(key); newEvidenceCount++; }
    }
    // Empty/failed searches are not cached as resolved gaps; all originals remain in the tool archive.
    // 空结果和失败不能缓存为缺口已解决；完整原结果仍由工具归档保存。
    if (!reused && result.items.length) this.cachedSearches.set(ticket.key, structuredClone(result));
    while (this.cachedSearches.size > 32) this.cachedSearches.delete(this.cachedSearches.keys().next().value);
    const strategyKey = identity([ticket.snapshotKey, result.strategy ?? 'default']);
    let noProgressSearches = this.progressByStrategy.get(strategyKey) ?? 0;
    if (!reused) {
      noProgressSearches = newEvidenceCount ? 0 : noProgressSearches + 1;
      this.progressByStrategy.delete(strategyKey);
      this.progressByStrategy.set(strategyKey, noProgressSearches);
      while (this.progressByStrategy.size > 32) this.progressByStrategy.delete(this.progressByStrategy.keys().next().value);
    }
    const stopSameStrategy = reused || noProgressSearches >= this.maximumUnproductiveSearches;
    const status = { ...this.status(ticket, { hasEvidence: result.items.length > 0, newEvidenceCount, reused }),
      noProgressSearches, stopSameStrategy };
    const decision = planEvidenceAcquisition({ query: ticket.gap, gap: ticket.explicitGap ? ticket.gap : undefined,
      items: result.items, assessment: result.evidenceAssessment, indexingPending: result.indexingPending,
      remainingSearches: Math.min(status.remainingSearches, status.remainingGapSearches), newEvidenceCount, reused });
    const boundedDecision = this.applyProgressDecision(decision, status);
    return { ...status, decision: boundedDecision, next: boundedDecision.next, invalidatedSourceRefs,
      repairRequired: invalidatedSourceRefs.length > 0 };
  }

  /** Rephrasing a stalled search is not progress; a different channel or exact source read may still recover it.
   * 改写无收益搜索不算进展；切换检索通道或精确回读仍可恢复任务，不能宣称答案已验证。 */
  applyProgressDecision(decision, progress) {
    if (!progress?.stopSameStrategy || ['read-source', 'state-unresolved-gap'].includes(decision.next)) return decision;
    return { ...decision, shouldContinueSearch: false, next: 'switch-channel-or-read-known-source',
      stopReason: 'no-new-versioned-evidence', suggestedOperations: ['source-read', 'symbol-search', 'path-search', 'relation-lookup'],
      sufficiency: 'not-evaluated' };
  }

  observeProjection(items) {
    // Search excerpts carry freshness dependencies, but do not count as original-source reads or verified support.
    // 搜索摘录携带新鲜度依赖，不能冒充已回读原文或已验证支持结论。
    const dependencies = new Map(items.map(item => [identity([item.scopeKey, item.sourceId]), {
      sourceRef: item.sourceRef, sourceId: item.sourceId, scopeKey: item.scopeKey,
      version: identity([item.sourceRevision, item.contentHash, item.derivationSignature, item.bindingRevision]) }]));
    if (this.finalSourceDependencies.size + [...dependencies.keys()].filter(key => !this.finalSourceDependencies.has(key)).length > 1024)
      throw retrievalFailure('Source verification capacity reached. / 本任务来源核验容量已满，不能静默遗漏后续来源。',
        'RETRIEVAL_VERSION_LEDGER_LIMIT');
    for (const [key, dependency] of dependencies) this.finalSourceDependencies.set(key, dependency);
  }

  observeRead(item, { gap, mode = 'page', sourceRef } = {}) {
    const key = identity([item.scopeKey, item.sourceId]);
    if (!this.finalSourceDependencies.has(key) && this.finalSourceDependencies.size >= 1024)
      throw retrievalFailure('Source verification capacity reached; use the observed sources or summarize current findings before expanding. / 本任务来源核验容量已满，不能静默遗漏后续来源。',
        'RETRIEVAL_VERSION_LEDGER_LIMIT');
    const version = identity([item.sourceRevision, item.contentHash, item.derivationSignature, item.bindingRevision]);
    const observed = this.sourceVersions.get(key);
    if (observed && observed.version !== version) this.invalidateSource(item.sourceId, item.scopeKey);
    this.sourceVersions.set(key, { version, sourceRef: item.sourceRef });
    while (this.sourceVersions.size > 128) this.sourceVersions.delete(this.sourceVersions.keys().next().value);
    const previous = this.sourceReads.get(key);
    const fragments = previous?.version === version ? [...(previous.fragments ?? [])] : [];
    const previousFragmentCount = fragments.length;
    const text = String(item.text ?? item.excerpt ?? '').slice(0, 65536);
    for (const reference of new Set([item.sourceRef, sourceRef].filter(Boolean)))
      if (!fragments.some(fragment => fragment.sourceRef === reference && fragment.text === text)) fragments.push({ sourceRef: reference, text });
    if (fragments.length !== previousFragmentCount) this.progressByStrategy.clear();
    while (fragments.length > 16 || fragments.reduce((sum, fragment) => sum + fragment.text.length, 0) > 131072) fragments.shift();
    this.sourceReads.set(key, { sourceRef: item.sourceRef, mode, text, version, fragments });
    this.finalSourceDependencies.set(key, { sourceRef: item.sourceRef, sourceId: item.sourceId,
      scopeKey: item.scopeKey, version });
    while (this.sourceReads.size > 64) this.sourceReads.delete(this.sourceReads.keys().next().value);
    return { sufficiency: 'not-evaluated', missingInformation: gap ?? null, sourceVersionChecked: true,
      contentRead: true, next: 'evaluate-support-then-answer-or-name-the-remaining-gap' };
  }

  /** Track cited support and version dependencies without treating an LLM verdict as a truth proof.
   * 记录引用支持及版本依赖，但不把模型自评等同于事实正确性或官方测试通过。 */
  assess({ conclusionId = 'current', claims, unresolved = [], contradictions = [], verification = [] } = {}) {
    if (typeof conclusionId !== 'string' || !conclusionId || conclusionId.length > 128 ||
        !Array.isArray(claims) || !claims.length || claims.length > 32 ||
        !Array.isArray(unresolved) || unresolved.length > 32 || !Array.isArray(contradictions) || contradictions.length > 32 ||
        !Array.isArray(verification) || verification.length > 32)
      throw retrievalFailure('Invalid evidence assessment. / 证据充分性记录格式无效。');
    const evidence = [], sourceKeys = new Set();
    for (const claim of claims) {
      if (typeof claim?.statement !== 'string' || !claim.statement.trim() || claim.statement.length > 2000 ||
          !Array.isArray(claim.support) || claim.support.length > 16)
        throw retrievalFailure('Each claim needs bounded citations. / 每项结论需要有界引用。');
      const supports = claim.support.map(citation => {
        if (typeof citation?.sourceRef !== 'string' || typeof citation.quote !== 'string' ||
            !citation.quote.trim() || citation.quote.length > 8000)
          throw retrievalFailure('Citations must contain a source and exact quotation. / 引用必须包含来源和原文摘录。');
        const read = [...this.sourceReads.entries()].find(([, item]) => item.sourceRef === citation.sourceRef ||
          item.fragments?.some(fragment => fragment.sourceRef === citation.sourceRef));
        const current = read && this.sourceVersions.get(read[0]);
        const versionCurrent = Boolean(read && (!current || current.version === read[1].version));
        const quotePresent = Boolean(read && (read[1].fragments ?? [{ sourceRef: read[1].sourceRef, text: read[1].text }])
          .some(fragment => fragment.sourceRef === citation.sourceRef && fragment.text.includes(citation.quote)));
        if (read) sourceKeys.add(read[0]);
        return { sourceRef: citation.sourceRef, contentRead: Boolean(read), versionCurrent, quotePresent };
      });
      evidence.push({ statement: claim.statement, support: supports,
        state: supports.length && supports.every(item => item.versionCurrent && item.quotePresent) ? 'cited-source-read' : 'missing-support' });
    }
    const checks = verification.map(check => ({ name: String(check?.name ?? '').slice(0, 200), state: 'unverified',
      reason: 'requires-execution-receipt', toolCallId: check?.toolCallId ?? null }));
    const ready = evidence.every(claim => claim.state === 'cited-source-read') && !unresolved.length &&
      !contradictions.length && !checks.length;
    const conclusion = { conclusionId, sourceKeys: [...sourceKeys], claims: evidence, unresolved, contradictions, checks,
      state: ready ? 'ready-to-answer' : contradictions.length ? 'contradictory' : 'needs-evidence-or-validation',
      sufficiency: 'cited-support-checked', correctnessCertified: false,
      next: contradictions.length ? 'resolve-conflicting-sources' : ready ? 'answer-with-citations' : 'acquire-the-named-gap-or-run-validation' };
    this.conclusions.set(conclusionId, conclusion);
    while (this.conclusions.size > 32) this.conclusions.delete(this.conclusions.keys().next().value);
    return structuredClone(conclusion);
  }

  invalidateSource(sourceId, scopeKey) {
    const key = identity([scopeKey, sourceId]);
    this.sourceReads.delete(key);
    for (const conclusion of this.conclusions.values()) if (conclusion.sourceKeys.includes(key)) conclusion.state = 'stale';
    return [...this.conclusions.values()].filter(conclusion => conclusion.state === 'stale').map(item => item.conclusionId);
  }

  status(ticket, { hasEvidence = false, newEvidenceCount = 0, reused = false, blocked = false } = {}) {
    return { state: blocked ? 'search-budget-exhausted' : hasEvidence ? 'evidence-available' : 'no-evidence',
      sufficiency: 'not-evaluated', missingInformation: ticket.gap, explicitGap: ticket.explicitGap,
      newEvidenceCount, reused, executed: !reused && !blocked,
      remainingSearches: Math.max(0, this.maximumSearches - this.searches),
      remainingGapSearches: Math.max(0, this.maximumSearchesPerGap - ticket.entry.searches),
      next: blocked ? 'answer-supported-parts-and-state-the-gap' :
        hasEvidence ? 'answer-if-supported-otherwise-read-the-missing-section' : 'search-a-specific-gap-or-state-it-is-unknown',
      notice: 'Relevance is not sufficiency. Check the requested entity, time, scope and conditions against the evidence. Answer now when it supports the requested facts; otherwise describe the missing fact and read its section. Exhausted retrieval does not mean the answer is verified; file edits and tests may continue.' };
  }
}

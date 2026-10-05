import { createHash } from 'node:crypto';
import { retrievalFailure } from '../../data/retrieval/retrieval-contracts.mjs';

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
    return { key, gapKey, gap: gap ?? query, explicitGap: gap !== undefined, entry, cached, blocked };
  }

  invalidate(ticket) { this.cachedSearches.delete(ticket.key); ticket.cached = undefined; }

  observe(ticket, result, { reused = false } = {}) {
    if (!reused) { this.searches++; ticket.entry.searches++; }
    let newEvidenceCount = 0;
    for (const item of result.items) {
      const key = identity([item.scopeKey, item.sourceId, item.sourceRevision, item.contentHash,
        item.chunkId ?? null, item.locator?.startOffset, item.locator?.endOffset]);
      if (!this.seenEvidence.has(key)) { this.seenEvidence.add(key); newEvidenceCount++; }
    }
    // Empty/failed searches are not cached as resolved gaps; all originals remain in the tool archive.
    // 空结果和失败不能缓存为缺口已解决；完整原结果仍由工具归档保存。
    if (!reused && result.items.length) this.cachedSearches.set(ticket.key, structuredClone(result));
    while (this.cachedSearches.size > 32) this.cachedSearches.delete(this.cachedSearches.keys().next().value);
    return this.status(ticket, { hasEvidence: result.items.length > 0, newEvidenceCount, reused });
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

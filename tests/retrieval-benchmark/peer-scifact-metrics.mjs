import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { hashText, sourceReference } from '../../apps/model-gateway/data/retrieval/retrieval-contracts.mjs';
import { evidenceSourceRef, validatedEvidenceReference } from '../../apps/model-gateway/data/retrieval/evidence-references.mjs';
import { selectCandidates, assessEvidence } from '../../apps/model-gateway/orchestration/retrieval/candidate-selection.mjs';
import { EVIDENCE_NOTICE, projectEvidence } from '../../apps/model-gateway/orchestration/retrieval/source-projection.mjs';
import { estimateTokens } from '../../apps/model-gateway/models/context-tokens.mjs';
import { evidenceMetrics } from './metrics.mjs';

export const PEER_EVIDENCE_BUDGETS = Object.freeze([2048, 4096, 8192]);
export const PEER_METRIC_FIELDS = Object.freeze(['ndcg', 'recall', 'mrr', 'map', 'precision', 'hit']);

/** Follow BEIR's document cutoff and default self-ID exclusion; do not score duplicate chunks twice.
 * 遵循 BEIR 文档截点与默认相同 ID 排除规则，重复分块不能获得重复相关性得分。 */
export function peerDocumentMetrics(documentIds, relevance, { queryId, cutoff = 10, ignoreIdenticalIds = true } = {}) {
  assert.ok(Number.isSafeInteger(cutoff) && cutoff > 0);
  const positives = [...relevance.values()].filter(grade => grade > 0);
  assert.ok(positives.length, 'Every scored query must have public positive judgments.');
  const ranking = [...new Set(documentIds.map(String))]
    .filter(id => !ignoreIdenticalIds || id !== String(queryId)).slice(0, cutoff);
  let retrievedRelevant = 0, reciprocalRank = 0, precisionSum = 0, discountedGain = 0;
  for (const [index, id] of ranking.entries()) {
    const grade = relevance.get(id) ?? 0;
    if (grade > 0) {
      retrievedRelevant++;
      reciprocalRank ||= 1 / (index + 1);
      precisionSum += retrievedRelevant / (index + 1);
      discountedGain += grade / Math.log2(index + 2);
    }
  }
  const idealGain = positives.sort((left, right) => right - left).slice(0, cutoff)
    .reduce((sum, grade, index) => sum + grade / Math.log2(index + 2), 0);
  return { ndcg: discountedGain / idealGain, recall: retrievedRelevant / positives.length,
    mrr: reciprocalRank, map: precisionSum / positives.length, precision: retrievedRelevant / cutoff,
    hit: retrievedRelevant ? 1 : 0 };
}

export function meanPeerMetrics(records) {
  assert.ok(records.length > 0);
  return Object.fromEntries(PEER_METRIC_FIELDS.map(key =>
    [key, records.reduce((sum, item) => sum + item[key], 0) / records.length]));
}

/** Verify full public source bytes against the original corpus before issuing any compact alias.
 * 分配短引用前，以原公开语料核验完整来源字节，不能仅相信索引里的哈希字段。 */
export function verifyPeerSource(row, document) {
  assert.ok(document, 'Every indexed source must exist in the original public corpus.');
  assert.equal(row.scope_key, 'user');
  assert.equal(row.source_type, 'public-benchmark');
  assert.equal(row.source_id, `beir:scifact:${String(document._id)}`);
  assert.equal(row.title, document.title);
  const originalText = `${document.title}\n\n${document.text}`;
  assert.equal(row.text, originalText, 'Source text must retain the exact public title and abstract.');
  assert.equal(row.content_hash, hashText(originalText), 'Full source SHA-256 must match the original bytes.');
  assert.equal(JSON.parse(row.source_revision), row.content_hash);
  assert.equal(JSON.parse(row.locator).beirDocumentId, String(document._id));
  return { documentId: String(document._id), sourceId: row.source_id, contentHash: row.content_hash,
    characters: originalText.length };
}

export function peerChunkFromRow(row, source) {
  assert.equal(row.source_id, source.source_id);
  assert.equal(hashText(row.text), row.chunk_hash);
  assert.equal(source.text.slice(row.start_offset, row.end_offset), row.text);
  const identity = { sourceId: source.source_id, scopeKey: source.scope_key,
    sourceRevision: JSON.parse(source.source_revision), contentHash: source.content_hash };
  const item = { ...identity, sourceType: source.source_type, title: source.title,
    locator: { ...JSON.parse(source.locator), startLine: row.start_line, endLine: row.end_line,
      startOffset: row.start_offset, endOffset: row.end_offset },
    chunkId: row.chunk_id, chunkIndex: row.chunk_index, chunkHash: row.chunk_hash, excerpt: row.text,
    sourceRef: sourceReference(identity, { chunkId: row.chunk_id, chunkHash: row.chunk_hash }) };
  validatedEvidenceReference(item);
  return item;
}

/** Use actual short-reference and projection helpers. This simulation is not a formal chat receipt.
 * 使用真实短引用和投影实现；本公开组件模拟不冒充正式聊天回执或真实模型输入观测。 */
export function peerEvidenceProjection(candidates, query, relevance, maximumTokens,
  { queryId, maximumCharacters = 10000, requiresSourceRead = true, archiveId = randomUUID() } = {}) {
  const originalCandidates = candidates.filter(item => item.locator.beirDocumentId !== String(queryId)).slice(0, 48);
  for (const item of originalCandidates) validatedEvidenceReference(item);
  const referenced = originalCandidates.map((item, index) =>
    ({ ...item, modelSourceRef: evidenceSourceRef(archiveId, index + 1) }));
  const reserve = estimateTokens(EVIDENCE_NOTICE) + 120;
  const selected = selectCandidates(referenced, { query, limit: 6,
    maximumTokens: Math.max(0, maximumTokens - reserve), requiresSourceRead });
  const renumber = items => items.map((item, index) =>
    ({ ...item, modelSourceRef: evidenceSourceRef(archiveId, index + 1) }));
  const initial = projectEvidence(renumber(selected.items), maximumCharacters,
    { maximumTokens, assessment: selected.evidenceAssessment });
  let items = initial.items, assessment = assessEvidence(items, query, { requiresSourceRead }), projected;
  // Match finalization's shrink-only notice agreement; a changed warning cannot expand the budget.
  // 与正式提交保持只缩减的提示核对，警告变化不能扩大最终预算。
  for (let iteration = 0; iteration <= 6; iteration++) {
    projected = projectEvidence(renumber(items), Math.min(maximumCharacters, initial.prompt.length),
      { maximumTokens: Math.min(maximumTokens, initial.usedTokens), assessment });
    const updated = assessEvidence(projected.items, query, { requiresSourceRead });
    if (assessment.state === updated.state && assessment.requiresSourceRead === updated.requiresSourceRead) break;
    assert.ok(iteration < 6, 'Bounded projection must converge without expanding its evidence.');
    items = projected.items;
    assessment = updated;
  }
  assert.ok(projected.usedTokens <= maximumTokens);
  assert.ok(projected.prompt.length <= maximumCharacters);
  assert.ok(!projected.prompt.includes('rag1:'));
  const metrics = evidenceMetrics(projected.items.map(item => item.locator.beirDocumentId), relevance);
  return { metrics, usedTokens: projected.usedTokens, usedCharacters: projected.prompt.length,
    selectedBeforeProjection: selected.items.length, reservedTokens: reserve,
    projectionSha256: hashText(projected.prompt), requiresSourceRead,
    references: projected.items.map(item => ({ documentId: item.locator.beirDocumentId, chunkId: item.chunkId,
      contentHash: item.contentHash, chunkHash: item.chunkHash, sourceRef: item.sourceRef,
      modelSourceRef: item.modelSourceRef, excerptSha256: hashText(item.excerpt) })) };
}

import assert from 'node:assert/strict';

export const RETRIEVAL_CUTOFFS = Object.freeze([1, 3, 5, 6, 10]);

/** Standard document-level metrics; the caller collapses repeated chunks before scoring.
 * 标准文档级指标；调用方先将重复分块归并为文档，再计算分数。
 */
export function retrievalMetrics(rankedIds, relevance, cutoff) {
  const positives = [...relevance.values()].filter(grade => grade > 0);
  if (!positives.length) throw new Error('Scored benchmark queries must have positive relevance judgments.');
  const ranking = [...new Set(rankedIds)].slice(0, cutoff);
  const relevantRanks = ranking.flatMap((id, index) => (relevance.get(id) ?? 0) > 0 ? [index + 1] : []);
  const discountedGain = grades => grades.reduce((sum, grade, index) => sum + (2 ** grade - 1) / Math.log2(index + 2), 0);
  const ideal = positives.sort((left, right) => right - left).slice(0, cutoff);
  return { recall: relevantRanks.length / positives.length, hit: relevantRanks.length ? 1 : 0,
    mrr: relevantRanks.length ? 1 / relevantRanks[0] : 0,
    ndcg: discountedGain(ranking.map(id => relevance.get(id) ?? 0)) / discountedGain(ideal) };
}

export function meanMetrics(records) {
  if (!records.length) throw new Error('Metric aggregation requires scored queries.');
  return Object.fromEntries(['recall', 'hit', 'mrr', 'ndcg'].map(key =>
    [key, records.reduce((sum, record) => sum + record[key], 0) / records.length]));
}

export function latencySummary(samples) {
  const sorted = [...samples].sort((left, right) => left - right);
  if (!sorted.length) throw new Error('Latency aggregation requires observations.');
  const percentile = fraction => sorted[Math.max(0, Math.ceil(fraction * sorted.length) - 1)];
  return { samples: sorted.length, p50Ms: percentile(.5), p95Ms: percentile(.95),
    meanMs: sorted.reduce((sum, value) => sum + value, 0) / sorted.length, maximumMs: sorted.at(-1) };
}

/** Coverage is scored after selection, where duplicated chunks still consume the six-slot budget.
 * 按选择后的真实证据计算覆盖；重复分块仍消耗六个位置，不能偷偷扩展为六篇不同文档。
 */
export function evidenceMetrics(documentIds, relevance) {
  const documents = [...new Set(documentIds)];
  const coverage = retrievalMetrics(documents, relevance, Math.max(1, documents.length));
  const relevantChunks = documentIds.filter(id => (relevance.get(id) ?? 0) > 0).length;
  return { recall: coverage.recall, hit: coverage.hit, selectedChunks: documentIds.length,
    uniqueDocuments: documents.length, repeatedDocumentSlots: documentIds.length - documents.length,
    relevantChunks, chunkPrecision: documentIds.length ? relevantChunks / documentIds.length : 0 };
}

export function meanEvidenceMetrics(records) {
  if (!records.length) throw new Error('Evidence aggregation requires scored queries.');
  return Object.fromEntries(Object.keys(records[0]).map(key =>
    [key, records.reduce((sum, record) => sum + record[key], 0) / records.length]));
}

/** Independent known answers guard denominators, duplicate collapse and logarithmic discounts.
 * 独立的已知答案校验分母、重复文档归并与对数折损，避免把实现结果本身当预期值。
 */
export function verifyMetricExamples() {
  const relevance = new Map([['a', 1], ['b', 1]]);
  assert.deepEqual(retrievalMetrics(['a'], relevance, 1), { recall: .5, hit: 1, mrr: 1, ndcg: 1 });
  const example = retrievalMetrics(['unjudged', 'a', 'a', 'b'], relevance, 3);
  assert.equal(example.recall, 1);
  assert.equal(example.mrr, .5);
  assert.ok(Math.abs(example.ndcg - ((1 / Math.log2(3) + .5) / (1 + 1 / Math.log2(3)))) < 1e-12);
  assert.deepEqual(retrievalMetrics(['unjudged'], relevance, 1), { recall: 0, hit: 0, mrr: 0, ndcg: 0 });
}

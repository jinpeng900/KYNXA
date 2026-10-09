import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';

export const RETRIEVAL_CUTOFFS = Object.freeze([1, 3, 5, 6, 10]);
const evaluationHash = value => createHash('sha256').update(JSON.stringify(value)).digest('hex');
const isIdentifier = value => typeof value === 'string' && value.length > 0 && value.trim() === value;
const hasText = value => typeof value === 'string' && value.trim().length > 0;
const compareEvaluationIds = (left, right) => left < right ? -1 : left > right ? 1 : 0;

/** Freeze paired query IDs, corpus bytes and independent evidence gold before running either method.
 * 比较前固定配对查询 ID、语料字节与独立证据金标；development 标签不能作为 heldout 成绩。 */
export function createRetrievalEvaluationDataset({ datasetId, datasetVersion, partition, sources, queries }) {
  assert.ok(isIdentifier(datasetId) && isIdentifier(datasetVersion));
  assert.ok(['development', 'heldout'].includes(partition));
  assert.ok(Array.isArray(sources) && Array.isArray(queries) && queries.length > 0);
  const corpus = new Map();
  for (const source of sources) {
    assert.ok(isIdentifier(source.sourceId) && typeof source.text === 'string');
    assert.ok(!corpus.has(source.sourceId), 'Evaluation source IDs must be unique.');
    corpus.set(source.sourceId, source.text);
  }
  const queryIds = new Set(), frozenQueries = queries.map(query => {
    assert.ok(isIdentifier(query.queryId) && hasText(query.query));
    assert.ok(!queryIds.has(query.queryId), 'Paired query IDs must be unique.');
    queryIds.add(query.queryId);
    assert.ok(Array.isArray(query.gold));
    assert.equal(query.noAnswer === true, query.gold.length === 0, 'No-answer queries must explicitly declare empty gold.');
    const goldSources = new Set(), gold = query.gold.map(item => {
      assert.ok(corpus.has(item.sourceId) && !goldSources.has(item.sourceId));
      goldSources.add(item.sourceId);
      assert.ok(Array.isArray(item.evidence) && item.evidence.length > 0);
      assert.equal(new Set(item.evidence).size, item.evidence.length);
      for (const evidence of item.evidence)
        assert.ok(hasText(evidence) && corpus.get(item.sourceId).includes(evidence), 'Gold evidence must occur in its original source.');
      return Object.freeze({ sourceId: item.sourceId, evidence: Object.freeze([...item.evidence].sort()) });
    }).sort((left, right) => compareEvaluationIds(left.sourceId, right.sourceId));
    return Object.freeze({ queryId: query.queryId, query: query.query, noAnswer: query.noAnswer === true,
      diagnosticOnly: query.diagnosticOnly === true, gold: Object.freeze(gold) });
  });
  const identity = { schemaVersion: 1, datasetId, datasetVersion, partition, sourceCount: corpus.size,
    queryCount: frozenQueries.length, queryIds: Object.freeze(frozenQueries.map(query => query.queryId)),
    corpusSha256: evaluationHash([...corpus].sort(([left], [right]) => compareEvaluationIds(left, right))),
    queriesSha256: evaluationHash(frozenQueries.map(({ queryId, query }) => ({ queryId, query }))),
    goldSha256: evaluationHash(frozenQueries.map(({ queryId, noAnswer, diagnosticOnly, gold }) =>
      ({ queryId, noAnswer, diagnosticOnly, gold }))) };
  return Object.freeze({ identity: Object.freeze({ ...identity, pairingId: evaluationHash(identity) }),
    queries: Object.freeze(frozenQueries) });
}

/** Failed attempts count in the planned denominator; skipped queries leave the complete rate unknown.
 * 失败尝试计入计划分母；跳过查询保留为未观测，不能默认为通过或伪造完整得分。 */
function evaluationRate(numerator, records, planned) {
  const completed = records.filter(record => record.status === 'completed').length;
  const failed = records.filter(record => record.status === 'failed').length;
  return { value: planned && completed + failed === planned ? numerator / planned : null,
    completedOnlyValue: completed ? numerator / completed : null, numerator, denominator: planned,
    completed, failed, unresolved: planned - completed - failed };
}

function scoreEvaluationObservation(query, observation, pairingId, cutoffs) {
  const status = observation?.status ?? 'skipped';
  assert.ok(['completed', 'failed', 'skipped'].includes(status));
  const diagnosticCodes = observation?.diagnosticCodes ?? (observation ? [] : ['EVALUATION_NOT_RUN']);
  assert.ok(Array.isArray(diagnosticCodes) && diagnosticCodes.every(isIdentifier));
  assert.ok(status === 'completed' || diagnosticCodes.length > 0, 'Failed and skipped observations need a recorded reason.');
  const durationMs = observation?.durationMs ?? null;
  assert.ok(status === 'skipped' ? durationMs === null : Number.isFinite(durationMs) && durationMs >= 0);
  const row = { queryId: query.queryId, pairId: evaluationHash([pairingId, query.queryId]), status, durationMs,
    noAnswer: query.noAnswer, diagnosticOnly: query.diagnosticOnly, diagnosticCodes: [...diagnosticCodes] };
  if (status !== 'completed') return row;
  assert.ok(Array.isArray(observation.items) && observation.items.every(item =>
    isIdentifier(item.sourceId) && typeof item.excerpt === 'string'));
  row.returnedItems = observation.items.length;
  row.emptyResult = observation.items.length === 0;
  if (query.noAnswer || query.diagnosticOnly) return row;
  const evidenceCount = query.gold.reduce((sum, item) => sum + item.evidence.length, 0);
  row.expectedSourceIds = query.gold.map(item => item.sourceId);
  row.relevantEvidenceRank = observation.items.findIndex(item => query.gold.some(gold =>
    gold.sourceId === item.sourceId && gold.evidence.some(evidence => item.excerpt.includes(evidence)))) + 1 || null;
  row.metrics = Object.fromEntries(cutoffs.flatMap(cutoff => {
    const selected = observation.items.slice(0, cutoff);
    const sourceHits = query.gold.filter(gold => selected.some(item => item.sourceId === gold.sourceId)).length;
    const evidenceHits = query.gold.reduce((sum, gold) => sum + gold.evidence.filter(evidence =>
      selected.some(item => item.sourceId === gold.sourceId && item.excerpt.includes(evidence))).length, 0);
    return [[`sourceRecallAt${cutoff}`, sourceHits / query.gold.length], [`evidenceRecallAt${cutoff}`, evidenceHits / evidenceCount],
      [`evidenceHitAt${cutoff}`, Number(evidenceHits > 0)], [`allEvidenceAt${cutoff}`, Number(evidenceHits === evidenceCount)]];
  }));
  return row;
}

/** Report paired methods without dropping missing, failed, no-answer or diagnostic-only observations.
 * 配对方法报告保留缺失、失败、无答案与纯诊断记录；空结果率仅衡量检索，不代表生成拒答或幻觉率。 */
export function retrievalEvaluationReport(dataset, methods, { cutoffs = [1, 5] } = {}) {
  assert.ok(cutoffs.length > 0 && cutoffs.every(value => Number.isSafeInteger(value) && value > 0));
  assert.equal(new Set(cutoffs).size, cutoffs.length);
  const queryIds = new Set(dataset.queries.map(query => query.queryId));
  const reports = Object.fromEntries(Object.entries(methods).map(([method, observations]) => {
    assert.ok(isIdentifier(method) && Array.isArray(observations));
    const observed = new Map();
    for (const observation of observations) {
      assert.ok(queryIds.has(observation.queryId) && !observed.has(observation.queryId), 'Observations must match unique frozen query IDs.');
      observed.set(observation.queryId, observation);
    }
    const rows = dataset.queries.map(query => scoreEvaluationObservation(query, observed.get(query.queryId), dataset.identity.pairingId, cutoffs));
    const answerable = rows.filter(row => !row.noAnswer && !row.diagnosticOnly);
    const noAnswer = rows.filter(row => row.noAnswer && !row.diagnosticOnly);
    const completed = rows.filter(row => row.status === 'completed'), failed = rows.filter(row => row.status === 'failed');
    const metricFields = cutoffs.flatMap(cutoff => ['sourceRecall', 'evidenceRecall', 'evidenceHit', 'allEvidence'].map(name => `${name}At${cutoff}`));
    const attemptedLatencies = rows.filter(row => row.durationMs !== null).map(row => row.durationMs);
    return [method, { counts: { planned: rows.length, attempted: completed.length + failed.length, completed: completed.length,
      failed: failed.length, skipped: rows.length - completed.length - failed.length,
      answerable: answerable.length, noAnswer: noAnswer.length, diagnosticOnly: rows.filter(row => row.diagnosticOnly).length },
    metrics: Object.fromEntries(metricFields.map(field => [field,
      evaluationRate(answerable.reduce((sum, row) => sum + (row.metrics?.[field] ?? 0), 0), answerable, answerable.length)])),
    noAnswerEmptyResult: evaluationRate(noAnswer.filter(row => row.emptyResult).length, noAnswer, noAnswer.length),
    latency: { allAttempts: attemptedLatencies.length ? latencySummary(attemptedLatencies) : null,
      completed: completed.length ? latencySummary(completed.map(row => row.durationMs)) : null }, rows }];
  }));
  return { schemaVersion: 1, metricVersion: 'source-and-literal-evidence-v1', dataset: dataset.identity, methods: reports,
    scope: 'Retrieval component checks against literal gold spans; no answer-generation scoring or API-usage measurement',
    limitations: ['The partition is an explicit dataset designation, not proof that a heldout set has remained unseen',
      'No-answer empty-result rates do not measure generated-answer abstention or hallucinations',
      'Each labeled span must occur in one returned excerpt; literal coverage does not infer semantic support',
      'Completed-only rates exclude failures and skipped cases; complete rates retain failures and remain unknown with skipped cases'] };
}

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

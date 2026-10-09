import assert from 'node:assert/strict';
import { mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { basename, dirname, join, resolve } from 'node:path';
import test from 'node:test';
import { createRetrievalEvaluationDataset, evidenceMetrics, latencySummary, meanMetrics, retrievalEvaluationReport,
  retrievalMetrics, verifyMetricExamples } from './metrics.mjs';
import { BenchmarkVectorCache } from './vector-cache.mjs';

test('Independent manual metric examples include graded qrels and duplicate documents', () => {
  verifyMetricExamples();
  const grades = new Map([['strong', 2], ['weak', 1], ['negative', 0]]);
  const ranking = retrievalMetrics(['unknown', 'weak', 'weak', 'strong'], grades, 3);
  assert.equal(ranking.recall, 1);
  assert.equal(ranking.mrr, .5);
  assert.ok(Math.abs(ranking.ndcg - ((1 / Math.log2(3) + 3 / 2) / (3 + 1 / Math.log2(3)))) < 1e-12);
  assert.deepEqual(meanMetrics([retrievalMetrics(['negative'], grades, 1), retrievalMetrics(['strong'], grades, 1)]),
    { recall: .25, hit: .5, mrr: .5, ndcg: .5 });
  assert.deepEqual(latencySummary([5, 1, 4, 2, 3]), { samples: 5, p50Ms: 3, p95Ms: 5, meanMs: 3, maximumMs: 5 });
});

test('Evidence metrics keep the actual chunk budget instead of replacing duplicates', () => {
  const grades = new Map([['first', 1], ['second', 1]]);
  assert.deepEqual(evidenceMetrics(['first', 'first', 'unknown'], grades), {
    recall: .5, hit: 1, selectedChunks: 3, uniqueDocuments: 2, repeatedDocumentSlots: 1,
    relevantChunks: 2, chunkPrecision: 2 / 3 });
  assert.deepEqual(evidenceMetrics([], grades), { recall: 0, hit: 0, selectedChunks: 0,
    uniqueDocuments: 0, repeatedDocumentSlots: 0, relevantChunks: 0, chunkPrecision: 0 });
});

const evaluationSources = [{ sourceId: 'a', text: 'Alpha first evidence. Alpha second evidence.' },
  { sourceId: 'b', text: 'Beta independent evidence.' }];
const evaluationQuery = { queryId: 'joined', query: 'Combine Alpha and Beta.', gold: [
  { sourceId: 'a', evidence: ['Alpha first evidence.', 'Alpha second evidence.'] },
  { sourceId: 'b', evidence: ['Beta independent evidence.'] }] };
const evaluationDataset = (extra = {}) => createRetrievalEvaluationDataset({ datasetId: 'paired-synthetic',
  datasetVersion: 'v1', partition: 'development', sources: evaluationSources, queries: [evaluationQuery], ...extra });

test('frozen evaluation identities track original source, query, gold, version and development partition', () => {
  const identity = evaluationDataset().identity;
  assert.equal(identity.partition, 'development');
  assert.equal(identity.corpusSha256, evaluationDataset({ sources: [...evaluationSources].reverse() }).identity.corpusSha256);
  for (const change of [{ datasetVersion: 'v2' }, { partition: 'heldout' },
    { sources: [{ ...evaluationSources[0], text: evaluationSources[0].text + ' Changed.' }, evaluationSources[1]] },
    { queries: [{ ...evaluationQuery, query: 'A different question.' }] },
    { queries: [{ ...evaluationQuery, gold: [evaluationQuery.gold[1]] }] }])
    assert.notEqual(identity.pairingId, evaluationDataset(change).identity.pairingId);
  assert.throws(() => { evaluationDataset().queries[0].gold[0].evidence.push('invented'); }, TypeError);
  assert.throws(() => evaluationDataset({ queries: [evaluationQuery, evaluationQuery] }));
  assert.throws(() => evaluationDataset({ queries: [{ ...evaluationQuery, gold: [{ sourceId: 'a', evidence: ['invented'] }] }] }));
  assert.throws(() => evaluationDataset({ queries: [{ queryId: 'missing', query: 'No answer', gold: [] }] }));
  const indentedEvidence = '  public int Read() => 1;';
  const indented = evaluationDataset({ sources: [{ sourceId: 'snippet', text: indentedEvidence + '\r\n' }],
    queries: [{ queryId: 'snippet', query: ' Find Read. ', gold: [{ sourceId: 'snippet', evidence: [indentedEvidence] }] }] });
  assert.equal(indented.queries[0].gold[0].evidence[0], indentedEvidence);
});

test('paired reports distinguish multiple source and evidence coverage from negative results and diagnostics', () => {
  const dataset = evaluationDataset({ queries: [evaluationQuery,
    { queryId: 'absent', query: 'Unavailable synthetic project?', gold: [], noAnswer: true },
    { ...evaluationQuery, queryId: 'diagnostic', diagnosticOnly: true }] });
  const report = retrievalEvaluationReport(dataset, { baseline: [
    { queryId: 'joined', status: 'completed', durationMs: 2, items: [
      { sourceId: 'a', excerpt: evaluationSources[0].text }, { sourceId: 'a', excerpt: evaluationSources[0].text }] },
    { queryId: 'absent', status: 'completed', durationMs: 3, items: [{ sourceId: 'a', excerpt: 'Unrelated Alpha content.' }] },
    { queryId: 'diagnostic', status: 'failed', durationMs: 13, diagnosticCodes: ['SYNTHETIC_FAILURE'] }], structured: [
    { queryId: 'joined', status: 'completed', durationMs: 1, items: evaluationSources.map(source => ({ sourceId: source.sourceId, excerpt: source.text })) },
    { queryId: 'absent', status: 'completed', durationMs: 4, items: [] }] });
  assert.equal(report.dataset.partition, 'development');
  assert.equal(report.methods.baseline.metrics.sourceRecallAt5.value, .5);
  assert.equal(report.methods.baseline.metrics.evidenceRecallAt5.value, 2 / 3);
  assert.equal(report.methods.baseline.metrics.evidenceHitAt5.value, 1);
  assert.equal(report.methods.baseline.metrics.allEvidenceAt5.value, 0);
  assert.equal(report.methods.structured.metrics.allEvidenceAt5.value, 1);
  assert.equal(report.methods.baseline.noAnswerEmptyResult.value, 0);
  assert.equal(report.methods.structured.noAnswerEmptyResult.value, 1);
  assert.equal(report.methods.baseline.counts.failed, 1);
  assert.equal(report.methods.baseline.latency.allAttempts.maximumMs, 13);
  assert.equal(report.methods.baseline.latency.completed.maximumMs, 3);
  assert.equal(report.methods.structured.counts.skipped, 1);
  assert.deepEqual(report.methods.structured.rows[2].diagnosticCodes, ['EVALUATION_NOT_RUN']);
  assert.equal(report.methods.baseline.rows[0].pairId, report.methods.structured.rows[0].pairId);
});

test('failed attempts retain metric denominators while skipped queries leave complete results unknown', () => {
  const dataset = evaluationDataset({ queries: ['complete', 'failed', 'skipped'].map(queryId => ({ ...evaluationQuery, queryId })) });
  const completed = { queryId: 'complete', status: 'completed', durationMs: 2, items: [{ sourceId: 'a', excerpt: evaluationSources[0].text }] };
  const failed = { queryId: 'failed', status: 'failed', durationMs: 5, diagnosticCodes: ['SEARCH_FAILED'] };
  const partial = retrievalEvaluationReport(dataset, { measured: [completed, failed] }).methods.measured;
  assert.deepEqual(partial.counts, { planned: 3, attempted: 2, completed: 1, failed: 1, skipped: 1,
    answerable: 3, noAnswer: 0, diagnosticOnly: 0 });
  assert.equal(partial.metrics.evidenceHitAt5.value, null);
  assert.equal(partial.metrics.evidenceHitAt5.completedOnlyValue, 1);
  assert.equal(partial.metrics.evidenceHitAt5.denominator, 3);
  assert.equal(partial.metrics.evidenceHitAt5.unresolved, 1);
  const failedOnly = retrievalEvaluationReport(dataset, { measured: [completed, failed,
    { ...failed, queryId: 'skipped' }] }).methods.measured;
  assert.equal(failedOnly.metrics.evidenceHitAt5.value, 1 / 3);
  assert.throws(() => retrievalEvaluationReport(dataset, { measured: [completed, completed] }));
  assert.throws(() => retrievalEvaluationReport(dataset, { measured: [{ ...failed, diagnosticCodes: [] }] }));
  assert.throws(() => retrievalEvaluationReport(dataset, { measured: [{ ...completed, durationMs: NaN }] }));
});

test('a source hit without the independent literal evidence cannot pass evidence checks', () => {
  const report = retrievalEvaluationReport(evaluationDataset(), { measured: [{ queryId: 'joined', status: 'completed',
    durationMs: 1, items: evaluationSources.map(source => ({ sourceId: source.sourceId, excerpt: 'Other content.' })) }] });
  assert.equal(report.methods.measured.metrics.sourceRecallAt5.value, 1);
  assert.equal(report.methods.measured.metrics.evidenceHitAt5.value, 0);
  assert.equal(report.methods.measured.rows[0].relevantEvidenceRank, null);
});

test('Vector cache rejects changed input/version/model and corrupted binary receipts', async () => {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-vector-cache-'));
  try {
    const records = [{ input: 'Public synthetic fixture', originalSha256: 'original-a', chunkHash: 'chunk-a',
      chunkerVersion: 'fixture-v1', tokenizerVersion: 'fixture-tokenizer-v1' }];
    let computations = 0;
    // These numeric fixtures test byte persistence only; they are never scored as model embeddings.
    // 数字夹具只验证字节持久化，绝不计入真实模型嵌入评分。
    const compute = async () => { computations++; return [[.25, .75]]; };
    const settings = { root, modelIdentity: { revision: 'fixture-model-v1', sha256: 'weights-a' },
      inputVersion: 'fixture-input-v1', dimensions: 2 };
    const cache = new BenchmarkVectorCache(settings);
    assert.deepEqual(await cache.getOrCompute(records, compute), [[.25, .75]]);
    assert.deepEqual(await cache.getOrCompute(records, compute), [[.25, .75]]);
    assert.equal(computations, 1);
    const prefix = (await readdir(root))[0], directory = join(root, prefix);
    const manifestFile = (await readdir(directory)).find(file => file.endsWith('.json'));
    const manifest = JSON.parse(await readFile(join(directory, manifestFile), 'utf8'));
    assert.equal(manifest.identity.encoding, 'float32-little-endian');
    await writeFile(join(directory, manifestFile.replace(/\.json$/u, '.bin')), Buffer.alloc(8));
    await cache.getOrCompute(records, compute);
    assert.equal(computations, 2);
    assert.equal(cache.stats.invalidBatches, 1);
    await cache.getOrCompute([{ ...records[0], originalSha256: 'original-b' }], compute);
    await new BenchmarkVectorCache({ ...settings, inputVersion: 'fixture-input-v2' }).getOrCompute(records, compute);
    await new BenchmarkVectorCache({ ...settings, modelIdentity: { revision: 'fixture-model-v2', sha256: 'weights-b' } }).getOrCompute(records, compute);
    assert.equal(computations, 5);
  } finally {
    assert.equal(dirname(resolve(root)), resolve(tmpdir()));
    assert.ok(basename(root).startsWith('kynxa-vector-cache-'));
    await rm(root, { recursive: true, force: true });
  }
});

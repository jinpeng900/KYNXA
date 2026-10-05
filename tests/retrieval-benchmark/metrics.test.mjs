import assert from 'node:assert/strict';
import { mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { basename, dirname, join, resolve } from 'node:path';
import test from 'node:test';
import { evidenceMetrics, latencySummary, meanMetrics, retrievalMetrics, verifyMetricExamples } from './metrics.mjs';
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

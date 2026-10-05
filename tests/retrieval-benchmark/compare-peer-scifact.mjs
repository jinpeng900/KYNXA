import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { readFile, writeFile } from 'node:fs/promises';
import { createInterface } from 'node:readline';
import { join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { peerDocumentMetrics } from './peer-scifact-metrics.mjs';

const REPOSITORY_ROOT = fileURLToPath(new URL('../../', import.meta.url));
const mean = values => values.reduce((sum, value) => sum + value, 0) / values.length;

/** Resample paired queries, never treat repeats of one task as independent samples.
 * 以配对查询重采样，同一任务的多次执行先归并，不能当成独立样本扩大置信度。 */
export function pairedDifferenceBootstrap(differences, { seed = 20261006, resamples = 4000 } = {}) {
  assert.ok(differences.length > 0 && differences.every(Number.isFinite));
  assert.ok(Number.isSafeInteger(resamples) && resamples > 0);
  let state = seed >>> 0;
  const random = () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let value = Math.imul(state ^ state >>> 15, state | 1);
    value ^= value + Math.imul(value ^ value >>> 7, value | 61);
    return ((value ^ value >>> 14) >>> 0) / 4294967296;
  };
  const samples = Array.from({ length: resamples }, () => {
    let sum = 0;
    for (let index = 0; index < differences.length; index++) sum += differences[Math.floor(random() * differences.length)];
    return sum / differences.length;
  }).sort((left, right) => left - right);
  return { queries: differences.length, meanDifference: mean(differences),
    percentile95Interval: [samples[Math.ceil(samples.length * .025) - 1], samples[Math.ceil(samples.length * .975) - 1]],
    wins: differences.filter(value => value > 1e-12).length,
    losses: differences.filter(value => value < -1e-12).length,
    ties: differences.filter(value => Math.abs(value) <= 1e-12).length, seed, resamples };
}

async function recordsWithHash(filename, project) {
  const hash = createHash('sha256'), records = [];
  const stream = createReadStream(filename);
  stream.on('data', bytes => hash.update(bytes));
  const lines = createInterface({ input: stream, crlfDelay: Infinity });
  for await (const line of lines) if (line.trim()) records.push(project(JSON.parse(line)));
  return { records, sha256: hash.digest('hex') };
}

async function main() {
  const settings = {};
  for (let offset = 2; offset < process.argv.length; offset++) {
    const name = process.argv[offset].replace(/^--/u, ''), value = process.argv[++offset];
    assert.ok(value && ['input', 'historical-optimized', 'historical-legacy'].includes(name));
    settings[name] = resolve(value);
  }
  assert.ok(settings.input);
  const manifest = JSON.parse(await readFile(join(settings.input, 'manifest.json'), 'utf8'));
  const originalQrels = await readFile(join(REPOSITORY_ROOT, 'artifacts/verification/rag-benchmark/datasets/scifact/qrels/test.tsv'));
  assert.equal(createHash('sha256').update(originalQrels).digest('hex'), manifest.dataset.fileSha256['qrels/test.tsv']);
  const qrels = new Map();
  for (const line of originalQrels.toString('utf8').trim().split(/\r?\n/u).slice(1)) {
    const [queryId, documentId, score] = line.split('\t');
    if (!qrels.has(queryId)) qrels.set(queryId, new Map());
    qrels.get(queryId).set(documentId, Number(score));
  }
  const run = await recordsWithHash(join(settings.input, 'rankings.jsonl'), record => ({ queryId: record.queryId,
    methods: Object.fromEntries(Object.entries(record.methods).map(([name, method]) => [name, {
      documentAt10: method.documentAt10,
      ...Object.fromEntries(Object.entries(method.evidenceBudget).map(([budget, projection]) =>
        [`evidence${budget}`, projection.metrics])) }])) }));
  const queryIds = [...new Set(run.records.map(record => record.queryId))];
  assert.equal(queryIds.length, 300);
  assert.deepEqual(new Set(queryIds), new Set(qrels.keys()));
  const modes = {};
  for (const name of Object.keys(run.records[0].methods)) modes[name] = new Map(queryIds.map(queryId => {
    const repeats = run.records.filter(record => record.queryId === queryId).map(record => record.methods[name]);
    const values = Object.fromEntries(Object.entries(repeats[0]).map(([kind, fields]) => [kind,
      Object.fromEntries(Object.keys(fields).map(field => [field, mean(repeats.map(record => record[kind][field]))]))]));
    return [queryId, values];
  }));
  const provenance = { current: { path: settings.input, rankingsSha256: run.sha256 } };
  for (const [argument, name] of [['historical-optimized', 'historicalOptimized'], ['historical-legacy', 'historicalLegacy']]) {
    if (!settings[argument]) continue;
    const directory = settings[argument], oldManifest = JSON.parse(await readFile(join(directory, 'manifest.json'), 'utf8'));
    assert.equal(oldManifest.sample.queryCount, 300);
    assert.equal(oldManifest.sample.documentCount, 5183);
    assert.equal(oldManifest.dataset.sha256, manifest.dataset.sha256);
    assert.equal(oldManifest.implementation.modelVersion, manifest.embedding.modelVersion);
    const historical = await recordsWithHash(join(directory, 'rankings.jsonl'), record => ({ queryId: record.queryId,
      documentAt10: peerDocumentMetrics(record.hybrid.documents, qrels.get(record.queryId), { queryId: record.queryId }) }));
    assert.deepEqual(new Set(historical.records.map(record => record.queryId)), new Set(queryIds));
    modes[name] = new Map(historical.records.map(record => [record.queryId, { documentAt10: record.documentAt10 }]));
    provenance[name] = { path: directory, rankingsSha256: historical.sha256,
      embeddingInputVersion: oldManifest.implementation.embeddingInputVersion,
      preservedLegacyWorker: Boolean(oldManifest.implementation.baselineRoot) };
  }
  const contrasts = [['hybrid', 'documentBm25'], ['hybrid', 'chunkLexical'], ['hybrid', 'vectorOnly'],
    ...(modes.neuralRerank ? [['neuralRerank', 'hybrid']] : []),
    ...(modes.historicalOptimized ? [['hybrid', 'historicalOptimized']] : []),
    ...(modes.historicalLegacy ? [['hybrid', 'historicalLegacy']] : [])];
  const comparisons = Object.fromEntries(contrasts.map(([left, right]) => {
    const commonKinds = Object.keys(modes[left].get(queryIds[0])).filter(kind => modes[right].get(queryIds[0])[kind]);
    return [`${left}-minus-${right}`, Object.fromEntries(commonKinds.map(kind => [kind,
      Object.fromEntries(Object.keys(modes[left].get(queryIds[0])[kind]).map(field => [field,
        pairedDifferenceBootstrap(queryIds.map(queryId => modes[left].get(queryId)[kind][field] - modes[right].get(queryId)[kind][field]))]))]))];
  }));
  const result = { schemaVersion: 1, createdAt: new Date().toISOString(), provenance, comparisons,
    samplingUnit: 'One official SciFact queryId; same-query repetitions averaged before pairing',
    limitations: ['Descriptive query bootstrap conditional on this English scientific test set, not a universal improvement guarantee',
      'Claims sharing evidence may be correlated; this is not a paper-cluster or out-of-domain confidence interval',
      'Historical comparison changes the tokenizer and source embedding context together, so it is not a single-variable causal estimate',
      'Cached NN logits are actual historical inference; this computation introduces no new inference or timing',
      'No Agent task repeats, generated-answer or billed-token scores are analyzed here'] };
  const output = join(settings.input, 'paired-comparison.json');
  await writeFile(output, JSON.stringify(result, null, 2) + '\n', { flag: 'wx' });
  process.stdout.write(JSON.stringify({ output, queries: queryIds.length,
    documentAt10: Object.fromEntries(Object.entries(comparisons).map(([name, values]) => [name, values.documentAt10])) }) + '\n');
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  try { await main(); }
  catch (error) { process.stderr.write(`Paired component comparison failed: ${error.message}\n`); process.exitCode = 1; }
}

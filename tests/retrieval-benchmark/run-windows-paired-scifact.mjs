import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { constants } from 'node:fs';
import { copyFile, mkdir, mkdtemp, readFile, readdir, stat, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { Worker, isMainThread, parentPort, workerData } from 'node:worker_threads';
import { BenchmarkVectorCache } from './vector-cache.mjs';
import { latencySummary, meanMetrics, retrievalMetrics } from './metrics.mjs';
import { peerChunkFromRow, verifyPeerSource } from './peer-scifact-metrics.mjs';

const REPOSITORY_ROOT = fileURLToPath(new URL('../../', import.meta.url));
const ORIGINAL_FILE_HASHES = Object.freeze({
  'corpus.jsonl': 'dec31c8182f3d744c7d2c09423756fd1d17cbef75808db13ba01cc0aab4d1ac6',
  'queries.jsonl': '8ff84a7c903f722981cd8d595c022660140c51867b27608a6d4910db86080313',
  'qrels/test.tsv': '0864bb985e0ca2367ba217977e72004d549054b2b06666ed9d4825ac7c21284c'
});
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');
const jsonFile = async path => JSON.parse(await readFile(path, 'utf8'));

/** Preserve the historical twenty IDs; expansion is frozen independently of scores.
 * 保留历史二十题身份；扩展题在评分之前固定，不能从高分题里挑选。 */
export function freezeQuerySelection(originalQueryIds, eligibleQueryIds, seed = 'kynxa-windows-light-20261010') {
  assert.ok(Array.isArray(originalQueryIds) && originalQueryIds.length === 20, 'Original twenty query IDs are required.');
  assert.equal(new Set(originalQueryIds).size, 20);
  const eligible = new Set(eligibleQueryIds);
  assert.ok(originalQueryIds.every(id => typeof id === 'string' && eligible.has(id)));
  const addedQueryIds = [...eligible].filter(id => !originalQueryIds.includes(id))
    .sort((left, right) => sha256(`${seed}:${left}`).localeCompare(sha256(`${seed}:${right}`))).slice(0, 80);
  assert.equal(addedQueryIds.length, 80);
  return { originalQueryIds: [...originalQueryIds], addedQueryIds, selectionVersion: 'sha256-seeded-order-v1', seed };
}

export function validateCorpusIdentity(manifest, counts, expectedChunks = 6559) {
  assert.equal(manifest.sample.documentCount, 5183, 'Reduced corpus cannot replace the full corpus.');
  assert.equal(counts.sources, 5183);
  assert.equal(counts.chunks, expectedChunks, 'A different chunk corpus needs a separately recorded comparison.');
  assert.equal(manifest.sample.chunkCount, expectedChunks);
  assert.equal(manifest.implementation.dimensions, 384);
  assert.equal(manifest.implementation.actualLocalVectors, true);
}

function options() {
  const settings = { stage: 20, expectedChunks: 6559, execute: false, latency: false, latencyRepeats: 3, candidateLimit: 60,
    dataset: join(REPOSITORY_ROOT, 'artifacts/verification/rag-benchmark/datasets/scifact'),
    vectorCache: join(REPOSITORY_ROOT, 'artifacts/verification/rag-benchmark/vectors'),
    output: join(REPOSITORY_ROOT, 'artifacts/verification/windows-light-20261010') };
  for (let offset = 2; offset < process.argv.length; offset++) {
    const name = process.argv[offset].replace(/^--/u, '');
    if (name === 'execute') { settings.execute = true; continue; }
    if (name === 'latency') { settings.latency = true; continue; }
    if (name === 'help') return null;
    const value = process.argv[++offset];
    assert.ok(value && ['input', 'original20', 'old-root', 'new-root', 'dataset', 'vector-cache',
      'output', 'stage', 'expected-chunks', 'candidate-limit', 'review', 'completed-run', 'latency-repeats'].includes(name), 'Unknown or incomplete argument.');
    const key = ({ 'old-root': 'oldRoot', 'new-root': 'newRoot', 'vector-cache': 'vectorCache',
      'expected-chunks': 'expectedChunks', 'candidate-limit': 'candidateLimit', 'completed-run': 'completedRun',
      'latency-repeats': 'latencyRepeats' })[name] ?? name;
    settings[key] = ['stage', 'expectedChunks', 'candidateLimit', 'latencyRepeats'].includes(key) ? Number(value) : resolve(value);
  }
  assert.ok(settings.input && settings.original20 && settings.oldRoot && settings.newRoot);
  assert.ok([20, 100].includes(settings.stage));
  assert.ok(Number.isSafeInteger(settings.expectedChunks) && settings.expectedChunks > 0);
  assert.ok(Number.isSafeInteger(settings.candidateLimit) && settings.candidateLimit >= 10 && settings.candidateLimit <= 192);
  assert.ok(Number.isSafeInteger(settings.latencyRepeats) && settings.latencyRepeats >= 1 && settings.latencyRepeats <= 5);
  return settings;
}

async function inspectInput(settings) {
  const manifestPath = join(settings.input, 'manifest.json');
  const manifestBytes = await readFile(manifestPath), manifest = JSON.parse(manifestBytes);
  const datasetHashes = {};
  for (const [name, expected] of Object.entries(ORIGINAL_FILE_HASHES)) {
    datasetHashes[name] = sha256(await readFile(join(settings.dataset, name)));
    assert.equal(datasetHashes[name], expected, `${name} must preserve the official bytes.`);
  }
  const queryDefinitions = new Map((await readFile(join(settings.dataset, 'queries.jsonl'), 'utf8'))
    .trim().split(/\r?\n/u).map(line => { const row = JSON.parse(line); return [String(row._id), row.text]; }));
  const corpus = new Map((await readFile(join(settings.dataset, 'corpus.jsonl'), 'utf8'))
    .trim().split(/\r?\n/u).map(line => { const row = JSON.parse(line); return [String(row._id), row]; }));
  const judgments = new Map();
  for (const line of (await readFile(join(settings.dataset, 'qrels/test.tsv'), 'utf8')).trim().split(/\r?\n/u).slice(1)) {
    const [queryId, documentId, rawGrade] = line.split('\t');
    if (!judgments.has(queryId)) judgments.set(queryId, new Map());
    judgments.get(queryId).set(documentId, Number(rawGrade));
  }
  const original20Bytes = await readFile(settings.original20);
  const original20 = JSON.parse(original20Bytes);
  const selection = freezeQuerySelection(original20.queryIds ?? original20.sample?.queryIds, [...judgments.keys()]);
  const folder = (await readdir(settings.input)).find(name => name.startsWith('temporary-data-'));
  assert.ok(folder, 'A completed index data directory is required.');
  const dataRoot = join(settings.input, folder), databasePath = join(dataRoot, 'Index/retrieval.sqlite');
  const wal = await stat(databasePath + '-wal').catch(error => { if (error.code !== 'ENOENT') throw error; return null; });
  assert.ok(!wal?.size, 'Active WAL databases cannot be copied as completed input.');
  const database = new DatabaseSync(databasePath, { readOnly: true });
  let counts;
  try {
    counts = { sources: database.prepare('SELECT count(*) AS n FROM sources').get().n,
      chunks: database.prepare('SELECT count(*) AS n FROM chunks').get().n };
    validateCorpusIdentity(manifest, counts, settings.expectedChunks);
    assert.equal(database.prepare("SELECT count(*) AS n FROM sources WHERE scope_key != 'user' OR source_type != 'public-benchmark' OR source_id NOT LIKE 'beir:scifact:%'").get().n, 0);
    assert.equal(database.prepare('SELECT count(*) AS n FROM chunks WHERE dimensions != 384 OR vector IS NULL OR length(vector) != 1536').get().n, 0);
    const sources = new Map();
    for (const row of database.prepare('SELECT * FROM sources').iterate()) {
      verifyPeerSource(row, corpus.get(String(JSON.parse(row.locator).beirDocumentId)));
      sources.set(row.source_id, row);
    }
    for (const row of database.prepare('SELECT * FROM chunks').iterate()) {
      peerChunkFromRow(row, sources.get(row.source_id));
      assert.equal(row.embedding_model_version,
        manifest.implementation.indexedEmbeddingModelVersion ?? manifest.implementation.modelVersion);
    }
  } finally { database.close(); }
  return { manifest, databasePath, dataRoot, queryDefinitions, judgments,
    identity: { datasetHashes, manifestSha256: sha256(manifestBytes), original20Sha256: sha256(original20Bytes),
      databaseSha256: sha256(await readFile(databasePath)), ...counts, ...selection,
      querySetOrigin: original20.origin ?? 'Provided manifest; historical twenty-query origin not independently verified',
      modelIdentity: manifest.implementation.modelIdentity,
      indexedEmbeddingModelVersion: manifest.implementation.indexedEmbeddingModelVersion ?? manifest.implementation.modelVersion } };
}

async function copyCompletedIndex(input, output, label) {
  const root = join(output, label);
  await mkdir(join(root, 'Index'), { recursive: true });
  await mkdir(join(root, 'Retrieval'));
  await copyFile(input.databasePath, join(root, 'Index/retrieval.sqlite'), constants.COPYFILE_EXCL);
  assert.equal(sha256(await readFile(join(root, 'Index/retrieval.sqlite'))), input.identity.databaseSha256);
  await copyFile(join(input.dataRoot, 'Retrieval/source-identities.json'),
    join(root, 'Retrieval/source-identities.json'), constants.COPYFILE_EXCL);
  return root;
}

async function workerMain() {
  const { RetrievalIndex } = await import(pathToFileURL(join(workerData.sourceRoot,
    'apps/model-gateway/data/retrieval/index.mjs')).href);
  const index = new RetrievalIndex({ root: workerData.dataRoot });
  parentPort.on('message', async ({ id, query, vector, close }) => {
    let observedResult;
    try {
      if (close) { await index.close(); parentPort.postMessage({ id, result: { closed: true } }); parentPort.close(); return; }
      const started = performance.now();
      const result = await index.search({ query, scopeKeys: ['user'], queryVector: vector,
        embeddingProfileId: 'builtin-multilingual', embeddingModelVersion: workerData.modelVersion,
        limit: workerData.candidateLimit });
      observedResult = { strategy: result.strategy, degradedReason: result.degradedReason ?? null,
        semanticBackend: result.semanticBackend ?? null, returnedChunks: result.items.length };
      assert.equal(result.strategy, 'hybrid', `Semantic degradation (${result.degradedReason ?? 'unreported'}) must not be scored as a working hybrid channel.`);
      assert.ok(!result.degradedReason, result.degradedReason);
      const durationMs = performance.now() - started;
      const status = await index.status();
      parentPort.postMessage({ id, result: { durationMs,
        documents: [...new Set(result.items.map(item => String(item.locator.beirDocumentId)))],
        returnedChunks: result.items.length, strategy: result.strategy, semanticBackend: result.semanticBackend ?? null,
        budgetAudit: { requestedReturnChunks: workerData.candidateLimit,
          requestedChannelCandidates: 40, returnedChunks: result.items.length,
          returnedLexicalCandidates: result.items.filter(item => item.lexicalRank !== undefined).length,
          returnedVectorCandidates: result.items.filter(item => item.vectorRank !== undefined).length,
          configuredAnnOptions: status.ann?.options ?? null, effectiveAnnOptions: status.ann?.effectiveOptions ?? null,
          annRouting: status.ann?.routingAudit ?? null, annPlanning: status.ann?.planningAudit ?? null } } });
    } catch (error) { parentPort.postMessage({ id, error: { code: error.code ?? 'BENCHMARK_FAILED', message: error.message,
      ...(observedResult ? { details: { observedResult, indexStatus: await index.status() } } : {}) } }); }
  });
}

function openWorker(sourceRoot, dataRoot, identity, candidateLimit) {
  const worker = new Worker(new URL(import.meta.url), { execArgv: [],
    workerData: { sourceRoot, dataRoot, modelVersion: identity.indexedEmbeddingModelVersion, candidateLimit } });
  let sequence = 0;
  let failed;
  const pending = new Map();
  const fail = error => {
    failed = error;
    for (const item of pending.values()) { clearTimeout(item.timer); item.reject(error); }
    pending.clear();
  };
  worker.on('error', fail);
  worker.on('exit', code => fail(new Error(`Owned evaluation worker exited with code ${code}.`)));
  worker.on('message', message => {
    const promise = pending.get(message.id); if (!promise) return;
    pending.delete(message.id);
    clearTimeout(promise.timer);
    if (message.error) promise.reject(Object.assign(new Error(message.error.message), { code: message.error.code, details: message.error.details }));
    else promise.resolve(message.result);
  });
  const call = input => new Promise((resolve, reject) => {
    if (failed) { reject(failed); return; }
    const id = ++sequence;
    const timer = setTimeout(() => {
      pending.delete(id);
      reject(new Error('Owned retrieval worker exceeded the 120-second call budget.'));
    }, 120_000);
    pending.set(id, { resolve, reject, timer });
    worker.postMessage({ id, ...input });
  });
  return { call, close: async () => {
    try { if (!failed) await call({ close: true }); }
    finally { await worker.terminate(); }
  } };
}

async function runLatency(settings, input, pairingId) {
  assert.ok(settings.completedRun, '--latency requires --completed-run results.json.');
  const priorBytes = await readFile(settings.completedRun), prior = JSON.parse(priorBytes);
  assert.equal(prior.pairingId, pairingId);
  assert.equal(prior.completedQueries, prior.plannedQueries);
  assert.equal(prior.failure, null);
  const roots = prior.indexCopies.roots;
  for (const [offset, root] of roots.entries()) {
    assert.equal(sha256(await readFile(join(root, 'Index/retrieval.sqlite'))), prior.indexCopies.closedDatabaseSha256[offset]);
  }
  const queryIds = input.identity.originalQueryIds.slice(0, 3);
  await mkdir(settings.output, { recursive: true });
  const output = await mkdtemp(join(settings.output, 'scifact-paired-warm-db-'));
  await writeFile(join(output, 'manifest.json'), JSON.stringify({ pairingId, queryIds,
    repeats: settings.latencyRepeats, priorResultsSha256: sha256(priorBytes),
    selection: 'First three IDs in the previously frozen initial twenty; no selection by latency or quality' }, null, 2) + '\n');
  const workers = { old: openWorker(settings.oldRoot, roots[0], input.identity, settings.candidateLimit),
    new: openWorker(settings.newRoot, roots[1], input.identity, settings.candidateLimit) };
  const cache = new BenchmarkVectorCache({ root: settings.vectorCache, modelIdentity: input.identity.modelIdentity,
    inputVersion: 'original-query-v1', dimensions: 384 });
  const rows = [];
  let failure;
  try {
    for (const queryId of queryIds) {
      const query = input.queryDefinitions.get(queryId);
      const [vector] = await cache.getOrCompute([{ input: query, originalSha256: sha256(query), kind: 'original-public-query' }],
        async () => { throw new Error('Warm latency requires a verified cached query vector.'); });
      for (const label of ['old', 'new']) await workers[label].call({ query, vector });
      for (let repeat = 0; repeat < settings.latencyRepeats; repeat++) {
        const row = { queryId, repeat: repeat + 1, methods: {} };
        for (const label of repeat % 2 ? ['new', 'old'] : ['old', 'new'])
          row.methods[label] = await workers[label].call({ query, vector });
        rows.push(row);
      }
    }
  } catch (error) { failure = { code: error.code ?? 'BENCHMARK_FAILED', message: error.message, details: error.details ?? null }; }
  finally { await Promise.allSettled(Object.values(workers).map(worker => worker.close())); }
  const result = { pairingId, createdAt: new Date().toISOString(), priorResultsSha256: sha256(priorBytes),
    queryIds, repeats: settings.latencyRepeats, observations: rows.length, failure: failure ?? null,
    databaseLatency: Object.fromEntries(['old', 'new'].map(label => [label, rows.length
      ? latencySummary(rows.map(row => row.methods[label].durationMs)) : null])),
    backendObservations: Object.fromEntries(['old', 'new'].map(label => [label, rows.reduce((counts, row) => {
      const backend = row.methods[label].semanticBackend ?? 'unknown';
      counts[backend] = (counts[backend] ?? 0) + 1;
      return counts;
    }, {})])),
    rows, freshQueryEmbedding: false, documentEmbedding: false, network: false, answerGeneration: false,
    limitations: ['Three frozen queries; repeated timing observations are not independent quality tasks.',
      'Database search only; fresh query embedding, model loading, reranking and Agent generation are excluded.',
      'One warm request precedes each query; default exact-to-ANN transitions may still occur during observations.',
      'Other user and OS workloads are untouched; this is not a system-exclusive throughput benchmark.'] };
  await writeFile(join(output, 'results.json'), JSON.stringify(result, null, 2) + '\n');
  process.stdout.write(JSON.stringify({ output, failure, databaseLatency: result.databaseLatency }) + '\n');
  if (failure) process.exitCode = 1;
}

async function main() {
  const settings = options();
  if (!settings) {
    process.stdout.write('Use --input COMPLETED_RUN --original20 QUERY_MANIFEST --old-root ROOT --new-root ROOT\n'
      + '[--execute] [--stage 20|100 --review REVIEW_JSON] [--expected-chunks 6559].\n'
      + 'Separate DB timing: --execute --latency --completed-run RESULTS_JSON [--latency-repeats 3].\n'
      + 'Default is identity preflight only: no inference, downloads, database copying or ranking.\n');
    return;
  }
  const input = await inspectInput(settings);
  const componentHashes = {};
  for (const [label, root] of [['old', settings.oldRoot], ['new', settings.newRoot]]) {
    componentHashes[label] = {};
    for (const name of ['index.mjs', 'index-worker.mjs', 'retrieval-text.mjs', 'vector-search.mjs'])
      componentHashes[label][name] = sha256(await readFile(join(root, 'apps/model-gateway/data/retrieval', name)));
  }
  const pairingId = sha256(JSON.stringify({ identity: input.identity, componentHashes, candidateLimit: settings.candidateLimit }));
  if (!settings.execute) {
    process.stdout.write(JSON.stringify({ status: 'prepared', pairingId, identity: input.identity, componentHashes,
      inference: false, network: false, originalIndexModified: false }, null, 2) + '\n');
    return;
  }
  assert.equal(process.platform, 'win32', 'This entry reports a Windows-only acceptance run.');
  if (settings.latency) { await runLatency(settings, input, pairingId); return; }
  let reviewedRun;
  let reviewedOutput;
  if (settings.stage === 100) {
    const review = await jsonFile(settings.review);
    assert.equal(review.pairingId, pairingId, 'Expansion must review this exact twenty-query comparison.');
    assert.equal(review.allowExpansion, true, 'The twenty-query differences must be reviewed before expansion.');
    const priorBytes = await readFile(resolve(review.resultsPath));
    assert.equal(sha256(priorBytes), review.resultsSha256);
    const prior = JSON.parse(priorBytes);
    assert.equal(prior.pairingId, pairingId);
    assert.equal(prior.plannedQueries, 20);
    assert.equal(prior.completedQueries, 20);
    assert.equal(prior.failure, null);
    reviewedRun = prior;
    reviewedOutput = dirname(resolve(review.resultsPath));
    assert.deepEqual(prior.indexCopies.roots, ['old', 'new'].map(label => join(reviewedOutput, label)));
    for (const [offset, root] of prior.indexCopies.roots.entries()) {
      const databasePath = join(root, 'Index/retrieval.sqlite');
      assert.equal(sha256(await readFile(databasePath)), prior.indexCopies.closedDatabaseSha256[offset]);
      const wal = await stat(databasePath + '-wal').catch(error => { if (error.code !== 'ENOENT') throw error; return null; });
      assert.ok(!wal?.size, 'Reviewed owned index must be closed before additional queries.');
    }
  }
  await mkdir(settings.output, { recursive: true });
  const output = await mkdtemp(join(settings.output, `scifact-paired-q${settings.stage}-`));
  const roots = reviewedRun?.indexCopies.roots
    ?? await Promise.all(['old', 'new'].map(label => copyCompletedIndex(input, output, label)));
  const workers = { old: openWorker(settings.oldRoot, roots[0], input.identity, settings.candidateLimit),
    new: openWorker(settings.newRoot, roots[1], input.identity, settings.candidateLimit) };
  const queryIds = settings.stage === 20 ? input.identity.originalQueryIds : input.identity.addedQueryIds;
  const cache = new BenchmarkVectorCache({ root: settings.vectorCache, modelIdentity: input.identity.modelIdentity,
    inputVersion: 'original-query-v1', dimensions: 384 });
  const rows = reviewedRun ? (await readFile(join(reviewedOutput, 'rankings.jsonl'), 'utf8'))
    .trim().split(/\r?\n/u).map(JSON.parse) : [];
  if (reviewedRun) assert.deepEqual(rows.map(row => row.queryId), input.identity.originalQueryIds);
  let failure;
  let activeRow;
  try {
    for (const [offset, queryId] of queryIds.entries()) {
      const started = performance.now();
      activeRow = { queryId, subset: input.identity.originalQueryIds.includes(queryId) ? 'original20' : 'added80',
        methods: {}, status: 'running', started };
      const query = input.queryDefinitions.get(queryId);
      const [vector] = await cache.getOrCompute([{ input: query, originalSha256: sha256(query), kind: 'original-public-query' }],
        async () => { throw new Error(`Verified query vector cache missing for ${queryId}; inference is not authorized by this entry.`); });
      // Alternate old/new first position; preparation and query-cache I/O stay outside retrieval timing.
      // 交替旧新执行顺序；准备与缓存读取不计入数据库检索时间。
      for (const label of offset % 2 ? ['new', 'old'] : ['old', 'new']) {
        const result = await workers[label].call({ query, vector });
        activeRow.methods[label] = { ...result, documentAt10: retrievalMetrics(result.documents
          .filter(id => id !== queryId), input.judgments.get(queryId), 10) };
      }
      activeRow.status = 'completed';
      activeRow.durationMs = performance.now() - started;
      delete activeRow.started;
      rows.push(activeRow);
      activeRow = null;
      await writeFile(join(output, 'rankings.jsonl'), rows.map(value => JSON.stringify(value)).join('\n') + '\n');
    }
  } catch (error) {
    failure = { code: error.code ?? 'BENCHMARK_FAILED', message: error.message, details: error.details ?? null };
    if (activeRow) {
      activeRow.durationMs = performance.now() - activeRow.started;
      delete activeRow.started;
      rows.push({ ...activeRow, status: 'failed', failure });
      await writeFile(join(output, 'rankings.jsonl'), rows.map(value => JSON.stringify(value)).join('\n') + '\n');
    }
  }
  finally { await Promise.allSettled(Object.values(workers).map(worker => worker.close())); }
  const completeRows = rows.filter(row => row.status === 'completed');
  const groups = Object.fromEntries(['original20', 'added80'].map(subset => [subset, Object.fromEntries(['old', 'new'].map(label => {
    const selected = completeRows.filter(row => row.subset === subset);
    return [label, selected.length ? { completed: selected.length,
      documentAt10: meanMetrics(selected.map(row => row.methods[label].documentAt10)),
      observedSearchDuration: latencySummary(selected.map(row => row.methods[label].durationMs)) } : null];
  }))]));
  const result = { schemaVersion: 1, pairingId, createdAt: new Date().toISOString(), identity: input.identity,
    componentHashes, plannedQueries: settings.stage, completedQueries: completeRows.length,
    failedQueries: rows.length - completeRows.length, skippedQueries: settings.stage - rows.length,
    failure: failure ?? null, groups, groupedMetricsUseCompletedPairsOnly: true,
    indexCopies: { roots, closedDatabaseSha256: await Promise.all(roots.map(async root =>
      sha256(await readFile(join(root, 'Index/retrieval.sqlite'))))),
      copiedThisRun: !reviewedRun, previouslyCompletedQueriesReused: reviewedRun ? 20 : 0 },
    queryVectorCache: cache.stats, queryEmbeddingTime: null, neuralRerank: 'not-configured-in-this-entry',
    querySetLabel: input.identity.querySetOrigin,
    originalIndexModified: false, documentVectorsComputed: 0, network: false, answerGeneration: false,
    limitations: ['Quality reuses verified real E5 query vectors; no fresh query embedding is timed.',
      'Per-query search observations include first query cold cost; dedicated warm latency uses a separate run.',
      'Current entry compares the same frozen index through two production index versions, not Agent task completion.',
      'Twenty-query review must precede expansion; missing historical IDs/chunk identity are blockers.'] };
  await writeFile(join(output, 'results.json'), JSON.stringify(result, null, 2) + '\n');
  process.stdout.write(JSON.stringify({ output, completedQueries: completeRows.length, failure, groups }) + '\n');
  if (failure) process.exitCode = 1;
}

if (!isMainThread) await workerMain();
else if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try { await main(); }
  catch (error) { process.stderr.write(`Windows paired preflight failed: ${error.message}\n`); process.exitCode = 1; }
}

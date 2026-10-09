import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { appendFileSync, existsSync } from 'node:fs';
import { mkdir, mkdtemp, readFile, writeFile, stat } from 'node:fs/promises';
import { cpus, availableParallelism, totalmem } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { spawnSync, execFileSync } from 'node:child_process';
import { RetrievalIndex, chunkSource } from '../../apps/model-gateway/data/retrieval/index.mjs';
import { hashText } from '../../apps/model-gateway/data/retrieval/retrieval-contracts.mjs';
import { EmbeddingService } from '../../apps/model-gateway/models/retrieval/embedding-service.mjs';
import { BUILTIN_EMBEDDING_PROFILE } from '../../apps/model-gateway/models/retrieval/embedding-profile.mjs';
import * as retrievalText from '../../apps/model-gateway/data/retrieval/retrieval-text.mjs';
import { selectCandidates, RETRIEVAL_CANDIDATE_LIMIT } from '../../apps/model-gateway/orchestration/retrieval/candidate-selection.mjs';
import { retrievalBudget } from '../../apps/model-gateway/orchestration/retrieval/retrieval-budget.mjs';
import { projectEvidence } from '../../apps/model-gateway/orchestration/retrieval/source-projection.mjs';
import { allocateEvidenceArchiveId, evidenceSourceRef } from '../../apps/model-gateway/data/retrieval/evidence-references.mjs';
import { RETRIEVAL_CUTOFFS, retrievalMetrics, meanMetrics, evidenceMetrics, meanEvidenceMetrics,
  latencySummary, verifyMetricExamples } from './metrics.mjs';
import { BenchmarkVectorCache } from './vector-cache.mjs';

const REPOSITORY_ROOT = fileURLToPath(new URL('../../', import.meta.url));
const DATASET_URL = 'https://public.ukp.informatik.tu-darmstadt.de/thakur/BEIR/datasets/scifact.zip';
const DATASET_MD5 = '5f7d1de60b170fc8027bb7898e2efca1';
const DATASET_SHA256 = '536e14446a0ba56ed1398ab1055f39fe852686ecad24a6306c80c490fa8e0165';
const DATASET_FILE_HASHES = Object.freeze({
  'corpus.jsonl': 'dec31c8182f3d744c7d2c09423756fd1d17cbef75808db13ba01cc0aab4d1ac6',
  'queries.jsonl': '8ff84a7c903f722981cd8d595c022660140c51867b27608a6d4910db86080313',
  'qrels/test.tsv': '0864bb985e0ca2367ba217977e72004d549054b2b06666ed9d4825ac7c21284c'
});
const COMPONENT_FILES = ['apps/model-gateway/data/retrieval/index.mjs', 'apps/model-gateway/data/retrieval/index-worker.mjs',
  'apps/model-gateway/data/retrieval/retrieval-text.mjs', 'apps/model-gateway/models/retrieval/embedding-service.mjs',
  'apps/model-gateway/models/retrieval/embedding-worker.mjs', 'apps/model-gateway/models/retrieval/embedding-profile.mjs',
  'apps/model-gateway/orchestration/retrieval/candidate-selection.mjs',
  'apps/model-gateway/data/retrieval/scoped-lexical-rank.mjs',
  'apps/model-gateway/orchestration/retrieval/source-projection.mjs',
  'apps/model-gateway/orchestration/retrieval/retrieval-budget.mjs'];
const EVIDENCE_TOKEN_BUDGETS = Object.freeze([2048, 4096, 6144, 8192]);
const compareIds = (left, right) => left < right ? -1 : left > right ? 1 : 0;
const digest = (bytes, algorithm = 'sha256') => createHash(algorithm).update(bytes).digest('hex');
const duration = started => performance.now() - started;
let progressPath;
const progress = (phase, details = {}) => {
  const line = JSON.stringify({ time: new Date().toISOString(), phase, ...details }) + '\n';
  if (progressPath) appendFileSync(progressPath, line);
  const interval = phase === 'embed-chunk-batch' ? 320 : phase.endsWith('-batch') ? 1000
    : phase === 'score-query' || phase === 'warm-latency-query' ? 20 : 0;
  if (!interval || details.completed % interval === 0 || details.completed === details.total) process.stderr.write(line);
};

function options() {
  const result = { queries: 40, documents: 500, seed: 20261005, repeats: 3, offline: false, full: false,
    embeddingInput: 'contextual', rerank: false, policyEvidence: false,
    cache: join(REPOSITORY_ROOT, 'artifacts', 'verification', 'rag-benchmark', 'datasets'),
    vectorCache: join(REPOSITORY_ROOT, 'artifacts', 'verification', 'rag-benchmark', 'vectors') };
  const numeric = new Set(['queries', 'documents', 'seed', 'repeats']);
  for (let index = 2; index < process.argv.length; index++) {
    const flag = process.argv[index];
    if (flag === '--offline') { result.offline = true; continue; }
    if (flag === '--full') { result.full = true; result.queries = 300; result.documents = 5183; continue; }
    if (flag === '--rerank') { result.rerank = true; continue; }
    if (flag === '--policy-evidence') { result.policyEvidence = true; continue; }
    const name = flag.replace(/^--/u, ''), value = process.argv[++index];
    if (!value || !['queries', 'documents', 'seed', 'repeats', 'cache', 'output', 'model-root',
      'embedding-input', 'vector-cache', 'reranker-root', 'baseline-root'].includes(name))
      throw new Error(`Unknown or incomplete benchmark argument: ${flag}`);
    const key = ({ 'model-root': 'modelRoot', 'embedding-input': 'embeddingInput',
      'vector-cache': 'vectorCache', 'reranker-root': 'rerankerRoot', 'baseline-root': 'baselineRoot' })[name] ?? name;
    result[key] = numeric.has(name) ? Number(value) : name === 'embedding-input' ? value : resolve(value);
  }
  for (const [name, minimum, maximum] of [['queries', 1, 300], ['documents', 1, 5183], ['seed', 0, 0xffffffff], ['repeats', 1, 10]])
    if (!Number.isSafeInteger(result[name]) || result[name] < minimum || result[name] > maximum)
      throw new Error(`Invalid ${name}; expected an integer between ${minimum} and ${maximum}.`);
  assert.ok(['legacy', 'contextual'].includes(result.embeddingInput), '--embedding-input must be legacy or contextual');
  if (result.full) assert.ok(result.queries === 300 && result.documents === 5183, '--full requires every test query and corpus document');
  return result;
}

function shuffled(values, seed) {
  const result = [...values];
  let state = seed >>> 0;
  const random = () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let value = state;
    value = Math.imul(value ^ value >>> 15, value | 1);
    value ^= value + Math.imul(value ^ value >>> 7, value | 61);
    return ((value ^ value >>> 14) >>> 0) / 4294967296;
  };
  for (let index = result.length - 1; index > 0; index--) {
    const other = Math.floor(random() * (index + 1));
    [result[index], result[other]] = [result[other], result[index]];
  }
  return result;
}

async function dataset(cache, offline) {
  await mkdir(cache, { recursive: true });
  const archive = join(cache, 'scifact.zip');
  if (!existsSync(archive)) {
    if (offline) throw new Error('The pinned public SciFact archive is missing from the offline cache.');
    progress('download-public-dataset', { url: DATASET_URL });
    const response = await fetch(DATASET_URL, { signal: AbortSignal.timeout(120000) });
    if (!response.ok) throw new Error(`Public dataset download failed with HTTP ${response.status}.`);
    const bytes = Buffer.from(await response.arrayBuffer());
    if (bytes.length > 16 * 1024 * 1024) throw new Error('Public dataset archive exceeds the download safety bound.');
    await writeFile(archive, bytes, { flag: 'wx' });
  }
  const archiveBytes = await readFile(archive);
  assert.equal(digest(archiveBytes, 'md5'), DATASET_MD5, 'archive matches the published BEIR checksum');
  assert.equal(digest(archiveBytes), DATASET_SHA256, 'archive matches this checked-in evaluation snapshot');
  const root = join(cache, 'scifact');
  if (!existsSync(join(root, 'corpus.jsonl'))) {
    const extracted = process.platform === 'win32'
      ? spawnSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command',
        "$ErrorActionPreference='Stop'; Expand-Archive -LiteralPath $env:KYNXA_RAG_ARCHIVE -DestinationPath $env:KYNXA_RAG_DATASET"],
      { env: { ...process.env, KYNXA_RAG_ARCHIVE: archive, KYNXA_RAG_DATASET: cache }, windowsHide: true, encoding: 'utf8' })
      : spawnSync('unzip', ['-q', archive, '-d', cache], { encoding: 'utf8' });
    if (extracted.status !== 0) throw new Error('Public dataset extraction failed: ' + (extracted.stderr ?? extracted.error?.message));
  }
  const files = {};
  for (const [name, expectedHash] of Object.entries(DATASET_FILE_HASHES)) {
    files[name] = await readFile(join(root, name));
    assert.equal(digest(files[name]), expectedHash, `${name} preserves the original public bytes`);
  }
  return files;
}

function records(bytes, kind) {
  const result = new Map();
  let duplicateIds = 0;
  for (const line of bytes.toString('utf8').split(/\r?\n/u).filter(Boolean)) {
    const value = JSON.parse(line), id = String(value._id);
    if (!id || id.trim() !== id || id === 'undefined') throw new Error(`Invalid public ${kind} ID.`);
    if (result.has(id)) { duplicateIds++; assert.deepEqual(result.get(id), value, `duplicate ${kind} ID cannot change the original`); }
    result.set(id, value);
  }
  return { values: result, duplicateIds };
}

function relevanceJudgments(bytes) {
  const lines = bytes.toString('utf8').trim().split(/\r?\n/u);
  assert.equal(lines.shift(), 'query-id\tcorpus-id\tscore');
  const relevance = new Map();
  let duplicatePairs = 0, explicitNegativePairs = 0;
  for (const line of lines) {
    const [queryId, documentId, score] = line.split('\t'), grade = Number(score);
    assert.ok(queryId && documentId && Number.isFinite(grade) && grade >= 0);
    if (!relevance.has(queryId)) relevance.set(queryId, new Map());
    const grades = relevance.get(queryId);
    if (grades.has(documentId)) { duplicatePairs++; assert.equal(grades.get(documentId), grade); }
    if (grade === 0) explicitNegativePairs++;
    grades.set(documentId, grade);
  }
  return { relevance, duplicatePairs, explicitNegativePairs, pairs: lines.length };
}

function selectSample(corpus, queries, relevance, settings) {
  // Freeze the query sample before retrieval and keep every gold; this makes the reduced corpus optimistic.
  // 检索前固定查询样本并保留全部金标；这种缩减语料比完整评测更乐观。
  const eligible = [...relevance.entries()].filter(([, grades]) => [...grades.values()].some(grade => grade > 0))
    .map(([id]) => id).sort(compareIds);
  const queryIds = shuffled(eligible, settings.seed).slice(0, settings.queries);
  const goldIds = new Set(queryIds.flatMap(id => [...relevance.get(id)].filter(([, grade]) => grade > 0).map(([id]) => id)));
  assert.ok(goldIds.size <= settings.documents, 'document sample must include every positive gold document');
  for (const id of goldIds) assert.ok(corpus.has(id), 'every positive judgment refers to an original corpus document');
  const distractors = shuffled([...corpus.keys()].filter(id => !goldIds.has(id)).sort(compareIds), settings.seed ^ 0xa5a5a5a5)
    .slice(0, settings.documents - goldIds.size);
  const documentIds = [...goldIds, ...distractors].sort(compareIds);
  const selectedQueries = queryIds.map(id => { assert.ok(queries.has(id)); return { id, ...queries.get(id) }; });
  return { queries: selectedQueries, documents: documentIds.map(id => ({ id, ...corpus.get(id) })),
    queryIds, documentIds, goldIds: [...goldIds].sort(compareIds), distractorIds: distractors.sort(compareIds) };
}

function cosine(left, right) {
  let dot = 0, leftSquared = 0, rightSquared = 0;
  for (let index = 0; index < left.length; index++) {
    dot += left[index] * right[index]; leftSquared += left[index] ** 2; rightSquared += right[index] ** 2;
  }
  return dot / Math.sqrt(leftSquared * rightSquared);
}

function denseDocumentRanking(queryVector, sources) {
  // Compare actual embeddings with exact cosine; max-chunk is diagnostic, not a production retrieval mode.
  // 用真实向量做精确余弦比较；最大分块得分仅用于诊断，不代表生产检索模式。
  return sources.map(source => ({ documentId: source.locator.beirDocumentId,
    score: Math.max(...source.vectors.map(vector => cosine(queryVector, vector))) }))
    .sort((left, right) => right.score - left.score || compareIds(left.documentId, right.documentId));
}

// Collapse chunk duplicates in returned order before applying document-level cutoffs.
// 保留返回顺序归并重复分块，再应用文档级前 k 名截点。
const documentRanking = items => [...new Set(items.map(item => item.locator.beirDocumentId))];
const chunkRanking = items => items.map(item => ({ documentId: item.locator.beirDocumentId, sourceId: item.sourceId,
  chunkId: item.chunkId, score: item.score }));
const scoreRanking = (ids, relevance) => Object.fromEntries(RETRIEVAL_CUTOFFS.map(cutoff =>
  [cutoff, retrievalMetrics(ids, relevance, cutoff)]));
const coverageOnly = metrics => ({ recall: metrics.recall, hit: metrics.hit });
const meanCoverage = values => Object.fromEntries(['recall', 'hit'].map(name =>
  [name, values.reduce((sum, value) => sum + value[name], 0) / values.length]));

// Fixed-width provisional references simulate model accounting without publishing benchmark-only archives.
// 固定宽度占位引用模拟模型计费，不发布只存在于评测中的正式证据归档。
function modelBudgetCandidates(items) {
  const modelSourceRef = evidenceSourceRef(allocateEvidenceArchiveId(), 1);
  return items.map(item => ({ ...item, modelSourceRef }));
}

function evidenceBudgetSweep(items, query, relevance) {
  items = modelBudgetCandidates(items);
  return Object.fromEntries(EVIDENCE_TOKEN_BUDGETS.map(maximumTokens => {
    const selected = selectCandidates(items, { query, limit: 6, maximumTokens });
    return [maximumTokens, { metrics: evidenceMetrics(selected.items.map(item => item.locator.beirDocumentId), relevance),
      selection: selected.selection, evidenceAssessment: selected.evidenceAssessment, chunks: chunkRanking(selected.items) }];
  }));
}

async function indexStorageFootprint(dataRoot) {
  // WAL and shared-memory files are part of an open index's footprint; sampling is not an atomic snapshot.
  // WAL 与共享内存文件也计入开放索引占用；逐文件采样不是原子快照。
  const databasePath = join(dataRoot, 'Index', 'retrieval.sqlite');
  const files = {};
  for (const suffix of ['', '-wal', '-shm']) {
    try { files[`retrieval.sqlite${suffix}`] = (await stat(databasePath + suffix)).size; }
    catch (error) { if (error.code !== 'ENOENT') throw error; files[`retrieval.sqlite${suffix}`] = 0; }
  }
  return { files, totalBytes: Object.values(files).reduce((total, bytes) => total + bytes, 0),
    measurement: 'Main database plus WAL and SHM, sequentially sampled before close/checkpoint' };
}

async function main() {
  const settings = options();
  verifyMetricExamples();
  const outputBase = settings.output ?? join(REPOSITORY_ROOT, 'artifacts', 'verification', 'rag-benchmark');
  await mkdir(outputBase, { recursive: true });
  const output = await mkdtemp(join(outputBase, `scifact-q${settings.queries}-d${settings.documents}-s${settings.seed}-${settings.embeddingInput}-`));
  progressPath = join(output, 'progress.jsonl');
  progress('starting', { output, settings });
  const files = await dataset(settings.cache, settings.offline);
  const corpus = records(files['corpus.jsonl'], 'document'), queries = records(files['queries.jsonl'], 'query');
  const judgments = relevanceJudgments(files['qrels/test.tsv']);
  const sample = selectSample(corpus.values, queries.values, judgments.relevance, settings);
  const dataRoot = await mkdtemp(join(output, 'temporary-data-'));
  const indexModule = settings.baselineRoot ? await import(pathToFileURL(join(settings.baselineRoot, 'index.mjs')).href)
    : { RetrievalIndex, chunkSource };
  const sources = sample.documents.map(document => {
    const source = { sourceId: `beir:scifact:${document.id}`, scopeKey: 'user', sourceType: 'public-benchmark',
      title: document.title, text: `${document.title}\n\n${document.text}`,
      locator: { dataset: 'BEIR/SciFact', split: 'test', beirDocumentId: document.id } };
    return { ...source, chunks: indexModule.chunkSource(source) };
  });
  const contentHashes = sample.documents.map(document => hashText(JSON.stringify({ title: document.title, text: document.text })));
  let gitHead = null;
  try { gitHead = execFileSync('git', ['rev-parse', 'HEAD'], { cwd: REPOSITORY_ROOT, encoding: 'utf8', windowsHide: true }).trim(); } catch {}
  const componentHashes = Object.fromEntries(await Promise.all(COMPONENT_FILES.map(async file => [file, digest(await readFile(join(REPOSITORY_ROOT, file)))])));
  const baselineHashes = settings.baselineRoot ? Object.fromEntries(await Promise.all(
    ['index.mjs', 'index-worker.mjs', 'retrieval-text.mjs', 'retrieval-contracts.mjs'].map(async name =>
      [name, digest(await readFile(join(settings.baselineRoot, name)))]))) : null;
  const isFull = sample.queries.length === judgments.relevance.size && sample.documents.length === corpus.values.size;
  const embeddingInputVersion = settings.embeddingInput === 'legacy' ? 'raw-chunk-v1' : retrievalText.EMBEDDING_TEXT_VERSION;
  assert.equal(typeof embeddingInputVersion, 'string', 'The contextual embedding helper must be available before evaluation.');
  if (settings.embeddingInput === 'contextual') assert.equal(typeof retrievalText.embeddingTextForChunk, 'function');
  const modelIdentity = { modelId: BUILTIN_EMBEDDING_PROFILE.modelId, modelVersion: BUILTIN_EMBEDDING_PROFILE.modelVersion,
    revision: BUILTIN_EMBEDDING_PROFILE.revision, dtype: BUILTIN_EMBEDDING_PROFILE.dtype,
    modelRevisionSha256: digest(JSON.stringify(BUILTIN_EMBEDDING_PROFILE.files.map(file => ({ path: file.path, sha256: file.sha256 })))),
    queryPrefix: BUILTIN_EMBEDDING_PROFILE.queryPrefix, documentPrefix: BUILTIN_EMBEDDING_PROFILE.documentPrefix,
    inferenceCodeSha256: [componentHashes[COMPONENT_FILES[3]], componentHashes[COMPONENT_FILES[4]]] };
  const manifest = { schemaVersion: 2, label: isFull ? 'Full SciFact test split and corpus; custom KYNXA chunk retrieval evaluation'
    : 'Sampled reduced-corpus SciFact pilot; not an official full BEIR score',
    createdAt: new Date().toISOString(), dataset: { url: DATASET_URL, publishedMd5: DATASET_MD5, sha256: DATASET_SHA256,
      fileSha256: DATASET_FILE_HASHES, corpusDocuments: corpus.values.size, queryDefinitions: queries.values.size,
      testQueries: judgments.relevance.size, testJudgmentPairs: judgments.pairs, explicitNegativePairs: judgments.explicitNegativePairs,
      reference: 'https://github.com/beir-cellar/beir/wiki/Datasets-available' },
    sample: { seed: settings.seed, shuffle: 'Mulberry32 Fisher-Yates over ASCII-sorted IDs', queryCount: sample.queries.length,
      documentCount: sample.documents.length, positiveDocumentCount: sample.goldIds.length, distractorCount: sample.distractorIds.length,
      queryIds: sample.queryIds, documentIds: sample.documentIds, goldDocumentIds: sample.goldIds, distractorDocumentIds: sample.distractorIds,
      sourceIds: sources.map(source => source.sourceId), chunkCount: sources.reduce((sum, source) => sum + source.chunks.length, 0) },
    normalization: { IDs: 'String(_id), no case folding or whitespace rewriting', text: 'Original title + two LF characters + original abstract',
      duplicateCorpusIds: corpus.duplicateIds, duplicateQueryIds: queries.duplicateIds, duplicateJudgmentPairs: judgments.duplicatePairs,
      duplicateSelectedTitleAbstractPairs: contentHashes.length - new Set(contentHashes).size,
      unjudgedDocuments: 'Random distractors are unjudged, treated as nonrelevant under the public qrels; no fabricated negative judgments' },
    implementation: { gitHead, componentSha256: componentHashes, modelId: BUILTIN_EMBEDDING_PROFILE.modelId,
      modelVersion: BUILTIN_EMBEDDING_PROFILE.modelVersion, dimensions: BUILTIN_EMBEDDING_PROFILE.dimensions,
      cpuThreads: 2, actualLocalVectors: true, benchmarkCandidateLimits: { lexicalChunks: 40, denseChunks: 40, returnedChunks: 60 },
      embeddingInputVersion, modelIdentity, baselineRoot: settings.baselineRoot ?? null,
      indexedEmbeddingModelVersion: settings.embeddingInput === 'legacy' ? BUILTIN_EMBEDDING_PROFILE.modelVersion
        : `${BUILTIN_EMBEDDING_PROFILE.modelVersion}|${embeddingInputVersion}`,
      baselineComponentSha256: baselineHashes,
      selectedEvidence: { candidateLimit: RETRIEVAL_CANDIDATE_LIMIT, chunkLimit: 6, maximumTokens: 2048,
        policy: 'legacy-six-fragment-diagnostic; not the current production budget',
        referenceAccounting: 'fixed-length ev1 provisional references; canonical references retained',
        additionalTokenBudgets: EVIDENCE_TOKEN_BUDGETS.slice(1),
        helper: 'Actual orchestration selectCandidates; no existing chat context in this public corpus benchmark' },
      lexicalAndHybrid: 'Actual RetrievalIndex.search; public authorized scope only',
      denseAblation: 'Diagnostic exact cosine over every selected document chunk; document score=max chunk cosine; raw files retain top 60 documents; no production-only dense mode exists',
      documentMetrics: 'Collapse repeated chunks by first returned occurrence, then score top k documents',
      legacyEvidence6: 'First six actual returned chunks, unique gold document hits; this is not six unique documents or current production policy',
      policyEvidence: settings.policyEvidence ? { ...retrievalBudget({ taskType: 'research' }),
        scope: 'production policy projection simulation; no model answer, coordinator freshness or finalization evaluation' } : null },
    system: { node: process.version, platform: process.platform, architecture: process.arch, cpu: cpus()[0]?.model,
      availableParallelism: availableParallelism(), totalMemoryBytes: totalmem() }, latency: { repeats: settings.repeats,
      networkIncluded: false, modelGenerationIncluded: false, coordinatorPreparationIncluded: false, concurrency: 1 } };
  await writeFile(join(output, 'manifest.json'), JSON.stringify(manifest, null, 2) + '\n');
  await writeFile(join(output, 'sampled-corpus.jsonl'), sample.documents.map(document => JSON.stringify(document)).join('\n') + '\n');
  await writeFile(join(output, 'sampled-queries.jsonl'), sample.queries.map(query => JSON.stringify({ ...query,
    judgments: [...judgments.relevance.get(query.id)].map(([documentId, grade]) => ({ documentId, grade })) })).join('\n') + '\n');
  progress('sample-ready', { output, queries: sample.queries.length, documents: sources.length, chunks: manifest.sample.chunkCount });
  const index = new indexModule.RetrievalIndex({ root: dataRoot }), embeddings = new EmbeddingService({ modelRoot: settings.modelRoot });
  const documentCache = new BenchmarkVectorCache({ root: settings.vectorCache, modelIdentity,
    inputVersion: embeddingInputVersion, dimensions: BUILTIN_EMBEDDING_PROFILE.dimensions });
  const queryCache = new BenchmarkVectorCache({ root: settings.vectorCache, modelIdentity,
    inputVersion: 'original-query-v1', dimensions: BUILTIN_EMBEDDING_PROFILE.dimensions });
  const queryVector = query => queryCache.getOrCompute([{ input: query.text, originalSha256: hashText(query.text),
    kind: 'original-public-query' }], async inputs => [(await embeddings.embedQuery(inputs[0])).vector]).then(vectors => vectors[0]);
  let reranker;
  if (settings.rerank) {
    const { RerankerService } = await import('../../apps/model-gateway/models/retrieval/reranker-service.mjs');
    reranker = new RerankerService({ modelRoot: settings.rerankerRoot, cpuThreads: 2 });
  }
  const rssStartBytes = process.memoryUsage().rss;
  let peakRssBytes = rssStartBytes;
  const rssTimer = setInterval(() => { peakRssBytes = Math.max(peakRssBytes, process.memoryUsage().rss); }, 20);
  rssTimer.unref();
  const timings = {}, scopeKeys = ['user'], rankings = [];
  let scoredResults;
  try {
    assert.equal(embeddings.status().state, 'ready', 'The fixed offline model must already be prepared; no download or fake vectors is allowed.');
    let started = performance.now();
    const initialStatus = await index.status();
    timings.sqliteColdOpenMs = duration(started);
    assert.equal(initialStatus.vectorAvailable, true, 'Actual sqlite-vec must be available for the hybrid baseline.');
    started = performance.now();
    for (let offset = 0; offset < sources.length; offset += 100) {
      await index.upsertSources(sources.slice(offset, offset + 100));
      progress('lexical-index-batch', { completed: Math.min(offset + 100, sources.length), total: sources.length });
    }
    timings.lexicalIndexBuildMs = duration(started);
    started = performance.now();
    const coldQuery = await embeddings.embedQuery(sample.queries[0].text);
    timings.embeddingColdLoadAndFirstQueryMs = duration(started);
    assert.equal(coldQuery.vector.length, 384);
    progress('offline-model-loaded', { elapsedMs: timings.embeddingColdLoadAndFirstQueryMs });
    started = performance.now();
    const chunks = sources.flatMap(source => source.chunks.map(chunk => ({ ...chunk,
      sourceTextHash: hashText(source.text), sourceId: source.sourceId,
      embeddingInput: settings.embeddingInput === 'legacy' ? chunk.text : retrievalText.embeddingTextForChunk(source, chunk) })));
    const vectors = [];
    for (let offset = 0; offset < chunks.length; offset += 32) {
      const batch = chunks.slice(offset, offset + 32).map(chunk => ({ input: chunk.embeddingInput,
        originalSha256: chunk.sourceTextHash, chunkHash: chunk.chunkHash, chunkId: chunk.chunkId,
        chunkerVersion: chunk.chunkerVersion, tokenizerVersion: chunk.tokenizerVersion, startOffset: chunk.startOffset, endOffset: chunk.endOffset }));
      const result = await documentCache.getOrCompute(batch, async inputs => (await embeddings.embedDocuments(inputs)).vectors);
      vectors.push(...result);
      progress('embed-chunk-batch', { completed: vectors.length, total: chunks.length, cache: documentCache.stats });
    }
    timings.documentEmbeddingMs = duration(started);
    let vectorOffset = 0;
    for (const source of sources) {
      source.vectors = vectors.slice(vectorOffset, vectorOffset + source.chunks.length);
      source.embeddingProfileId = coldQuery.profileId;
      source.embeddingModelVersion = manifest.implementation.indexedEmbeddingModelVersion;
      vectorOffset += source.chunks.length;
    }
    assert.equal(vectorOffset, vectors.length);
    started = performance.now();
    for (let offset = 0; offset < sources.length; offset += 100) {
      await index.upsertSources(sources.slice(offset, offset + 100));
      progress('publish-vector-batch', { completed: Math.min(offset + 100, sources.length), total: sources.length });
    }
    timings.vectorPublicationMs = duration(started);
    const vectorOptions = vector => ({ queryVector: vector, embeddingProfileId: coldQuery.profileId,
      embeddingModelVersion: manifest.implementation.indexedEmbeddingModelVersion });
    for (const query of sample.queries) {
      const vector = await queryVector(query);
      const lexical = await index.search({ query: query.text, scopeKeys, limit: 60 });
      const hybrid = await index.search({ query: query.text, scopeKeys, limit: 60, ...vectorOptions(vector) });
      assert.equal(hybrid.strategy, 'hybrid'); assert.equal(hybrid.degradedReason, undefined);
      const dense = denseDocumentRanking(vector, sources), relevance = judgments.relevance.get(query.id);
      const selectedLexical = selectCandidates(modelBudgetCandidates(lexical.items.slice(0, RETRIEVAL_CANDIDATE_LIMIT)),
        { query: query.text, limit: 6, maximumTokens: 2048 });
      const selectedHybrid = selectCandidates(modelBudgetCandidates(hybrid.items.slice(0, RETRIEVAL_CANDIDATE_LIMIT)),
        { query: query.text, limit: 6, maximumTokens: 2048 });
      let rerankRecord;
      let policyEvidence;
      if (settings.policyEvidence) {
        const policy = retrievalBudget({ taskType: 'research' }), policyStarted = performance.now();
        const pool = await index.search({ query: query.text, scopeKeys, ...vectorOptions(vector),
          limit: policy.fusedCandidates, channelCandidates: policy.channelCandidates });
        const selected = selectCandidates(modelBudgetCandidates(pool.items), { query: query.text,
          limit: policy.limit, maximumTokens: policy.maximumTokens });
        const projection = projectEvidence(selected.items, 65536,
          { maximumTokens: policy.maximumTokens, assessment: selected.evidenceAssessment });
        policyEvidence = { policy, returnedCandidates: pool.items.length, selection: selected.selection,
          projection: projection.audit, selectedDocumentIds: projection.items.map(item => item.locator.beirDocumentId),
          metrics: evidenceMetrics(projection.items.map(item => item.locator.beirDocumentId), relevance),
          simulationOnly: true, durationMs: duration(policyStarted) };
      }
      if (reranker) {
        const startedRerank = performance.now();
        const candidates = selectCandidates(modelBudgetCandidates(hybrid.items.slice(0, RETRIEVAL_CANDIDATE_LIMIT)),
          { query: query.text, limit: 20, maximumTokens: 16384, lambda: 1 }).items;
        const reranked = await reranker.rerank({ query: query.text, candidates, limit: 20 });
        const selected = selectCandidates(reranked.items, { query: query.text, limit: 6, maximumTokens: 2048 });
        rerankRecord = { documents: documentRanking(reranked.items), chunks: chunkRanking(reranked.items),
          metrics: scoreRanking(documentRanking(reranked.items), relevance),
          selectedEvidence6: { metrics: evidenceMetrics(selected.items.map(item => item.locator.beirDocumentId), relevance),
            selection: selected.selection, evidenceAssessment: selected.evidenceAssessment, chunks: chunkRanking(selected.items) },
          evidenceBudgetSweep: evidenceBudgetSweep(reranked.items, query.text, relevance),
          model: { profileId: reranked.profileId, modelVersion: reranked.modelVersion,
            truncatedInputsCount: reranked.truncatedInputsCount }, durationMs: duration(startedRerank), candidateCount: candidates.length };
      }
      rankings.push({ queryId: query.id, originalQuery: query.text,
        judgments: [...relevance].map(([documentId, grade]) => ({ documentId, grade })),
        lexical: { documents: documentRanking(lexical.items), chunks: chunkRanking(lexical.items),
          metrics: scoreRanking(documentRanking(lexical.items), relevance), evidence6: coverageOnly(retrievalMetrics(lexical.items.slice(0, 6).map(item => item.locator.beirDocumentId), relevance, 6)) },
        hybrid: { documents: documentRanking(hybrid.items), chunks: chunkRanking(hybrid.items),
          metrics: scoreRanking(documentRanking(hybrid.items), relevance), evidence6: coverageOnly(retrievalMetrics(hybrid.items.slice(0, 6).map(item => item.locator.beirDocumentId), relevance, 6)) },
        denseAblation: { documents: dense.slice(0, 60), totalDocumentsScored: dense.length,
          metrics: scoreRanking(dense.map(item => item.documentId), relevance) },
        selectedEvidence6: Object.fromEntries([['lexical', selectedLexical], ['hybrid', selectedHybrid]].map(([mode, selected]) =>
          [mode, { metrics: evidenceMetrics(selected.items.map(item => item.locator.beirDocumentId), relevance),
            selection: selected.selection, evidenceAssessment: selected.evidenceAssessment, chunks: chunkRanking(selected.items) }])),
        evidenceBudgetSweep: Object.fromEntries([['lexical', lexical.items], ['hybrid', hybrid.items]].map(([mode, items]) =>
          [mode, evidenceBudgetSweep(items.slice(0, RETRIEVAL_CANDIDATE_LIMIT), query.text, relevance)])),
        ...(rerankRecord ? { neuralRerank: rerankRecord } : {}), ...(policyEvidence ? { policyEvidence } : {}) });
      appendFileSync(join(output, 'rankings.jsonl'), JSON.stringify(rankings.at(-1)) + '\n');
      progress('score-query', { completed: rankings.length, total: sample.queries.length, queryId: query.id });
    }
    const latency = { lexical: [], queryEmbedding: [], hybridSearchWithPrecomputedVector: [], hybridEndToEnd: [], denseAblation: [] };
    for (let repeat = 0; repeat < settings.repeats; repeat++) for (const query of sample.queries) {
      started = performance.now();
      await index.search({ query: query.text, scopeKeys, limit: 60 });
      latency.lexical.push(duration(started));
      const hybridStarted = performance.now();
      const vector = (await embeddings.embedQuery(query.text)).vector;
      latency.queryEmbedding.push(duration(hybridStarted));
      started = performance.now();
      await index.search({ query: query.text, scopeKeys, limit: 60, ...vectorOptions(vector) });
      latency.hybridSearchWithPrecomputedVector.push(duration(started));
      latency.hybridEndToEnd.push(duration(hybridStarted));
      started = performance.now();
      denseDocumentRanking(vector, sources);
      latency.denseAblation.push(duration(started));
      progress('warm-latency-query', { repeat: repeat + 1, completed: sample.queries.indexOf(query) + 1, total: sample.queries.length });
    }
    // Unique absent identifiers diagnose forced nearest-neighbour results; they do not score generated answers.
    // 全语料不存在的唯一标识诊断强制近邻返回；它们不评价生成答案。
    const nonceQueries = Array.from({ length: 5 }, (_, index) => ({ id: `diagnostic-no-gold-${index + 1}`,
      text: `What is the launch credential for synthetic project zzragunavailablequartz${settings.seed}${index}zz?` }));
    const allOriginalText = [...corpus.values.values()].map(document => document.title + '\n' + document.text).join('\n').toLowerCase();
    const diagnostics = [];
    for (const query of nonceQueries) {
      const nonce = `zzragunavailablequartz${settings.seed}${diagnostics.length}zz`;
      assert.equal(allOriginalText.includes(nonce), false, 'the synthetic diagnostic identifier does not occur in the public corpus');
      const vector = (await embeddings.embedQuery(query.text)).vector;
      const lexical = await index.search({ query: query.text, scopeKeys, limit: 6 });
      const hybrid = await index.search({ query: query.text, scopeKeys, limit: 6, ...vectorOptions(vector) });
      const selected = selectCandidates(modelBudgetCandidates(hybrid.items), { query: query.text, limit: 6, maximumTokens: 2048 });
      diagnostics.push({ ...query, diagnosticOnly: true, scoredAgainstPublicQrels: false, goldCount: 0,
        lexical: { nonempty: lexical.items.length > 0, returnedChunks: lexical.items.length, documentIds: documentRanking(lexical.items) },
        hybrid: { nonempty: hybrid.items.length > 0, returnedChunks: hybrid.items.length, documentIds: documentRanking(hybrid.items) },
        selectedEvidence: { returnedChunks: selected.items.length, selection: selected.selection,
          evidenceAssessment: selected.evidenceAssessment, documentIds: documentRanking(selected.items) },
        denseAblation: { nonempty: true, bestDocuments: denseDocumentRanking(vector, sources).slice(0, 3) } });
    }
    await writeFile(join(output, 'no-gold-diagnostics.json'), JSON.stringify({ note:
      'Synthetic no-gold retrieval diagnostics only; no model answers were generated, so this is not a hallucination rate.', queries: diagnostics }, null, 2) + '\n');
    const metrics = Object.fromEntries(['lexical', 'hybrid', 'denseAblation', ...(reranker ? ['neuralRerank'] : [])].map(mode => [mode,
      Object.fromEntries(RETRIEVAL_CUTOFFS.map(cutoff => [cutoff, meanMetrics(rankings.map(record => record[mode].metrics[cutoff]))]))]));
    const indexStatus = await index.status();
    const results = { schemaVersion: 2, label: manifest.label,
      evidence6Policy: 'legacy-six-fragment-diagnostic; not current production evidence limits',
      ...(settings.policyEvidence ? { policyEvidence: { policy: 'research', simulationOnly: true,
        metrics: meanEvidenceMetrics(rankings.map(record => record.policyEvidence.metrics)) } } : {}),
      sample: { queries: rankings.length, documents: sources.length, chunks: chunks.length,
      seed: settings.seed, goldDocumentCount: sample.goldIds.length, allSampleGoldIncluded: true }, metrics,
      productionEvidence6Chunks: Object.fromEntries(['lexical', 'hybrid'].map(mode => [mode, meanCoverage(rankings.map(record => record[mode].evidence6))])),
      selectedEvidence6Chunks: Object.fromEntries(['lexical', 'hybrid', ...(reranker ? ['neuralRerank'] : [])].map(mode =>
        [mode, meanEvidenceMetrics(rankings.map(record => mode === 'neuralRerank' ? record.neuralRerank.selectedEvidence6.metrics
          : record.selectedEvidence6[mode].metrics))])),
      evidenceBudgetSweep: Object.fromEntries(['lexical', 'hybrid', ...(reranker ? ['neuralRerank'] : [])].map(mode =>
        [mode, Object.fromEntries(EVIDENCE_TOKEN_BUDGETS.map(budget => [budget, meanEvidenceMetrics(rankings.map(record =>
          mode === 'neuralRerank' ? record.neuralRerank.evidenceBudgetSweep[budget].metrics
            : record.evidenceBudgetSweep[mode][budget].metrics))]))])),
      vectorCache: { document: documentCache.stats, query: queryCache.stats },
      ...(reranker ? { reranking: { model: reranker.status(), latency: latencySummary(rankings.map(record => record.neuralRerank.durationMs)),
        candidateLimit: 20, truncatedInputsCount: rankings.reduce((sum, record) => sum + record.neuralRerank.model.truncatedInputsCount, 0) } } : {}),
      timings, warmLatency: Object.fromEntries(Object.entries(latency).map(([mode, samples]) => [mode, latencySummary(samples)])),
      memory: { baselineRssBytes: rssStartBytes, peakRssBytes, endRssBytes: process.memoryUsage().rss, samplingIntervalMs: 20,
        scope: 'This benchmark Node process and its SQLite/embedding worker threads only; desktop and other processes excluded' },
      index: indexStatus, embedding: embeddings.status(), indexStorage: await indexStorageFootprint(dataRoot),
      noGoldDiagnosticQueries: diagnostics.length,
      limitations: [isFull ? 'Full public SciFact corpus/test split, evaluated through KYNXA chunking rather than an official BEIR leaderboard run'
        : 'Reduced corpus and sampled test queries, not the official full BEIR score',
        'English scientific claims only, not Chinese, code, web, multi-turn memory or end-to-end answer quality',
        'Unjudged distractors are treated as nonrelevant, following the public qrels',
        'Dense-only is an explicit diagnostic cosine/max-chunk ablation, not a production mode',
        'Raw ranking and first-six chunks are separate from the production selected-six projection',
        'Latency excludes model answer generation, web requests, coordinator preparation and source scanning',
        'Warm repeats reuse loaded models and SQLite pages, but recompute query embeddings without an answer/vector cache',
        'Synthetic no-gold nonempty results do not measure hallucination because no answers are generated'] };
    scoredResults = results;
    await writeFile(join(output, 'results.json'), JSON.stringify(results, null, 2) + '\n');
    await writeFile(join(output, 'latency-samples.json'), JSON.stringify(latency) + '\n');
    progress('scores-saved', { output });
    process.stdout.write(JSON.stringify({ output, sample: results.sample, metrics: results.metrics,
      selectedEvidence6Chunks: results.selectedEvidence6Chunks, timings: results.timings,
      warmLatency: results.warmLatency }, null, 2) + '\n');
  } finally {
    clearInterval(rssTimer);
    // Retire every owned native resource even if one independent shutdown rejects.
    // 任一独立资源释放失败时，仍观察并释放全部自有原生资源，不保留孤立 worker。
    const closed = await Promise.allSettled([embeddings.close(), index.close(), ...(reranker ? [reranker.close()] : [])]);
    const failures = closed.filter(result => result.status === 'rejected').map(result => result.reason);
    if (failures.length) throw new AggregateError(failures, 'Benchmark resources could not confirm safe retirement.');
    if (scoredResults) {
      scoredResults.closedIndexStorage = await indexStorageFootprint(dataRoot);
      scoredResults.closedIndexStorage.measurement = 'Main database plus WAL and SHM after successful owned index/model close';
      await writeFile(join(output, 'results.json'), JSON.stringify(scoredResults, null, 2) + '\n');
    }
  }
  progress('complete', { output });
}

try { await main(); }
catch (error) { progress('failed', { message: error.message, code: error.code }); process.exitCode = 1; }

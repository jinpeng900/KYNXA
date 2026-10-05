import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { constants, appendFileSync } from 'node:fs';
import { copyFile, mkdir, mkdtemp, readFile, readdir, stat, writeFile } from 'node:fs/promises';
import { DatabaseSync } from 'node:sqlite';
import { cpus, totalmem } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { RetrievalIndex } from '../../apps/model-gateway/data/retrieval/index.mjs';
import { matchExpression } from '../../apps/model-gateway/data/retrieval/retrieval-text.mjs';
import { EmbeddingService } from '../../apps/model-gateway/models/retrieval/embedding-service.mjs';
import { BUILTIN_EMBEDDING_PROFILE } from '../../apps/model-gateway/models/retrieval/embedding-profile.mjs';
import { RerankerService } from '../../apps/model-gateway/models/retrieval/reranker-service.mjs';
import { deduplicateCandidates } from '../../apps/model-gateway/orchestration/retrieval/candidate-selection.mjs';
import { latencySummary, meanEvidenceMetrics } from './metrics.mjs';
import { PEER_EVIDENCE_BUDGETS, meanPeerMetrics, peerDocumentMetrics, peerEvidenceProjection,
  peerChunkFromRow, verifyPeerSource } from './peer-scifact-metrics.mjs';

const REPOSITORY_ROOT = fileURLToPath(new URL('../../', import.meta.url));
const EVALUATION_ROOT = join(REPOSITORY_ROOT, 'artifacts', 'verification', 'rag-benchmark');
const DATASET_MD5 = '5f7d1de60b170fc8027bb7898e2efca1';
const ORIGINAL_FILES = Object.freeze({
  'corpus.jsonl': 'dec31c8182f3d744c7d2c09423756fd1d17cbef75808db13ba01cc0aab4d1ac6',
  'queries.jsonl': '8ff84a7c903f722981cd8d595c022660140c51867b27608a6d4910db86080313',
  'qrels/test.tsv': '0864bb985e0ca2367ba217977e72004d549054b2b06666ed9d4825ac7c21284c'
});
const VECTOR_ONLY_QUERY = 'zzkynxa6f1e857148f09d476b34c7dfbcc2e91f';
const METRIC_CUTOFF = 10;
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');
const jsonLines = bytes => bytes.toString('utf8').trim().split(/\r?\n/u).filter(Boolean).map(JSON.parse);
const documentIds = items => [...new Set(items.map(item => item.locator.beirDocumentId))];
const rankingSnapshot = items => items.map(item => ({ documentId: item.locator.beirDocumentId,
  sourceId: item.sourceId, chunkId: item.chunkId, score: item.score }));
const mean = values => values.reduce((sum, value) => sum + value, 0) / values.length;

function options() {
  const settings = { output: EVALUATION_ROOT, dataset: join(EVALUATION_ROOT, 'datasets', 'scifact'),
    repeats: 1, rerank: 'off' };
  for (let offset = 2; offset < process.argv.length; offset++) {
    if (process.argv[offset] === '--help') {
      process.stdout.write('node tests/retrieval-benchmark/run-peer-scifact.mjs --input FULL_RUN [--neural-input NN_RUN] '
        + '[--rerank off|cached|live] [--repeats 1..5] [--dataset DIR] [--output DIR]\n'
        + 'Offline full 300/5183 public component comparison; never downloads models or scores generated answers.\n');
      return null;
    }
    const flag = process.argv[offset].replace(/^--/u, ''), value = process.argv[++offset];
    assert.ok(value && ['input', 'neural-input', 'rerank', 'repeats', 'dataset', 'output'].includes(flag),
      'Unknown or incomplete peer-benchmark option.');
    const key = flag === 'neural-input' ? 'neuralInput' : flag;
    settings[key] = flag === 'repeats' ? Number(value) : flag === 'rerank' ? value : resolve(value);
  }
  assert.ok(settings.input, '--input must be a completed full public SciFact run.');
  assert.ok(Number.isSafeInteger(settings.repeats) && settings.repeats >= 1 && settings.repeats <= 5);
  assert.ok(['off', 'cached', 'live'].includes(settings.rerank));
  if (settings.rerank === 'cached') assert.ok(settings.neuralInput, '--rerank cached requires --neural-input.');
  return settings;
}

async function publicDataset(directory) {
  const files = {};
  for (const [name, expected] of Object.entries(ORIGINAL_FILES)) {
    files[name] = await readFile(join(directory, name));
    assert.equal(sha256(files[name]), expected, 'Public corpus/query/qrels bytes must match the pinned BEIR dataset.');
  }
  const corpus = new Map(jsonLines(files['corpus.jsonl']).map(row => [String(row._id), row]));
  const queries = new Map(jsonLines(files['queries.jsonl']).map(row => [String(row._id), row.text]));
  const judgments = new Map();
  const lines = files['qrels/test.tsv'].toString('utf8').trim().split(/\r?\n/u);
  assert.equal(lines.shift(), 'query-id\tcorpus-id\tscore');
  for (const line of lines) {
    const [queryId, documentId, grade] = line.split('\t');
    if (!judgments.has(queryId)) judgments.set(queryId, new Map());
    assert.ok(corpus.has(documentId));
    assert.ok(!judgments.get(queryId).has(documentId));
    judgments.get(queryId).set(documentId, Number(grade));
  }
  assert.equal(corpus.size, 5183);
  assert.equal(judgments.size, 300);
  return { corpus, queries, judgments, fileSha256: ORIGINAL_FILES };
}

/** Copy the closed public index, preserving the original run and every original inference artifact.
 * 复制已关闭的公开索引，保留原评测与真实推理文件；新 worker 只接触本轮自有副本。 */
async function isolatedIndex(input, output) {
  const folder = (await readdir(input)).find(name => name.startsWith('temporary-data-'));
  assert.ok(folder);
  const original = join(input, folder, 'Index', 'retrieval.sqlite');
  const wal = await stat(original + '-wal').catch(error => {
    if (error.code !== 'ENOENT') throw error;
    return null;
  });
  assert.ok(!wal || wal.size === 0, 'Input must be a completed closed index, not an active WAL database.');
  const root = join(output, 'public-index');
  await mkdir(join(root, 'Index'), { recursive: true });
  await mkdir(join(root, 'Retrieval'));
  await copyFile(original, join(root, 'Index', 'retrieval.sqlite'), constants.COPYFILE_EXCL);
  const identities = join(input, folder, 'Retrieval', 'source-identities.json');
  await copyFile(identities, join(root, 'Retrieval', 'source-identities.json'), constants.COPYFILE_EXCL);
  const originalHash = sha256(await readFile(original));
  assert.equal(sha256(await readFile(join(root, 'Index', 'retrieval.sqlite'))), originalHash);
  return { root, original, originalSha256: originalHash, bytes: (await stat(original)).size };
}

function documentBm25Index(corpus) {
  const database = new DatabaseSync(':memory:');
  database.exec("CREATE VIRTUAL TABLE documents USING fts5(document_id UNINDEXED,title,text,tokenize='unicode61'); BEGIN");
  const insert = database.prepare('INSERT INTO documents(document_id,title,text) VALUES (?,?,?)');
  for (const document of corpus.values()) insert.run(String(document._id), document.title, document.text);
  database.exec('COMMIT');
  const search = database.prepare('SELECT document_id,bm25(documents) AS bm25 FROM documents WHERE documents MATCH ? ORDER BY bm25,document_id LIMIT 60');
  return { database, search: query => {
    const terms = [...new Set(query.toLowerCase().match(/[\p{L}\p{N}_]+/gu) ?? [])].slice(0, 64);
    const expression = terms.map(term => `"${term.replace(/"/g, '""')}"`).join(' OR ');
    return expression ? search.all(expression) : [];
  } };
}

function markdownReport(result) {
  const rows = Object.entries(result.methods).map(([name, method]) => {
    const metric = method.documentAt10, timing = method.latency?.endToEnd;
    return `| ${name} | ${(metric.ndcg * 100).toFixed(2)} | ${(metric.recall * 100).toFixed(2)} | ${(metric.mrr * 100).toFixed(2)} | ${timing ? timing.p50Ms.toFixed(1) : '历史缓存，无本轮计时'} | ${timing ? timing.p95Ms.toFixed(1) : '—'} |`;
  });
  const budgetRows = Object.entries(result.methods).flatMap(([name, method]) => PEER_EVIDENCE_BUDGETS.map(budget => {
    const value = method.evidenceBudget[budget];
    return `| ${name} | ${budget} | ${(value.recall * 100).toFixed(2)} | ${(value.hit * 100).toFixed(2)} | ${value.selectedChunks.toFixed(2)} | ${value.meanUsedTokens.toFixed(1)} |`;
  }));
  return '# SciFact 同条件检索组件对照\n\n'
    + '完整 300 条 test 查询、5183 篇文档。分数是文档检索与公开金标覆盖，不是 Agent 总分或答案生成质量。\n\n'
    + '| 方法 | nDCG@10 % | Recall@10 % | MRR@10 % | 新测 P50 ms | 新测 P95 ms |\n|---|---:|---:|---:|---:|---:|\n'
    + rows.join('\n') + '\n\n'
    + '| 方法 | 预算 token | 最终相关文档覆盖 % | 至少一份相关文档 % | 平均分块数 | 实际启发式 token |\n|---|---:|---:|---:|---:|---:|\n'
    + budgetRows.join('\n') + '\n\n'
    + 'nDCG/MAP 使用 TREC 文档截点，排除 queryId==documentId；按返回次序归并重复文档，未经标注的文档按不相关处理。\n\n'
    + 'BM25 是 SQLite FTS5 unicode61 文档基线，不是 BEIR Elasticsearch 的同实现复现。KYNXA chunk lexical 另有精确匹配优先与 40 分块候选上限；vector-only 使用相同原文的真实 E5 向量与同一生产索引的 40 分块上限；hybrid 使用生产 RRF。\n\n'
    + '短引用投影使用实际选择与投影 helper，完整 source/chunk SHA-256 已核验；这是公开组件模拟，不声称观测了真实 provider 请求、回执授权或计费 token。所有预算采用相同回源提示与六分块上限。\n\n'
    + result.limitations.map(value => `- ${value}`).join('\n') + '\n';
}

async function main() {
  const settings = options();
  if (!settings) return;
  const manifestBytes = await readFile(join(settings.input, 'manifest.json'));
  const manifest = JSON.parse(manifestBytes);
  assert.equal(manifest.schemaVersion, 2);
  assert.equal(manifest.dataset.publishedMd5, DATASET_MD5);
  assert.equal(manifest.sample.queryCount, 300);
  assert.equal(manifest.sample.documentCount, 5183);
  assert.equal(manifest.implementation.baselineRoot, null);
  assert.equal(manifest.implementation.modelVersion, BUILTIN_EMBEDDING_PROFILE.modelVersion);
  assert.equal(manifest.implementation.modelIdentity.modelRevisionSha256,
    sha256(JSON.stringify(BUILTIN_EMBEDDING_PROFILE.files.map(file => ({ path: file.path, sha256: file.sha256 })))));
  const originalRankingBytes = await readFile(join(settings.input, 'rankings.jsonl'));
  const originals = jsonLines(originalRankingBytes);
  assert.equal(originals.length, 300);
  const publicData = await publicDataset(settings.dataset);
  assert.deepEqual(new Set(originals.map(row => row.queryId)), new Set(publicData.judgments.keys()));
  assert.deepEqual(originals.map(row => row.queryId), manifest.sample.queryIds);
  for (const record of originals) {
    assert.equal(record.originalQuery, publicData.queries.get(record.queryId));
    assert.deepEqual(new Map(record.judgments.map(row => [row.documentId, row.grade])), publicData.judgments.get(record.queryId));
  }
  let cachedNeural, cachedNeuralSummary, cachedNeuralManifest;
  if (settings.rerank === 'cached') {
    cachedNeuralManifest = JSON.parse(await readFile(join(settings.neuralInput, 'manifest.json'), 'utf8'));
    assert.equal(cachedNeuralManifest.originalManifestSha256, sha256(manifestBytes));
    assert.equal(cachedNeuralManifest.originalRankingsSha256, sha256(originalRankingBytes));
    cachedNeural = jsonLines(await readFile(join(settings.neuralInput, 'rankings.jsonl')));
    assert.deepEqual(cachedNeural.map(row => row.queryId), originals.map(row => row.queryId));
    cachedNeuralSummary = JSON.parse(await readFile(join(settings.neuralInput, 'results.json'), 'utf8'));
  }
  await mkdir(settings.output, { recursive: true });
  const output = await mkdtemp(join(settings.output, 'scifact-peer-q300-d5183-v1-'));
  const writeNew = (name, value) => writeFile(join(output, name), typeof value === 'string' ? value : JSON.stringify(value, null, 2) + '\n', { flag: 'wx' });
  const progress = (phase, details = {}) => {
    const line = JSON.stringify({ time: new Date().toISOString(), phase, ...details }) + '\n';
    appendFileSync(join(output, 'progress.jsonl'), line);
    if (!details.completed || details.completed % 20 === 0 || details.completed === details.total) process.stderr.write(line);
  };
  const indexCopy = await isolatedIndex(settings.input, output);
  const reader = new DatabaseSync(join(indexCopy.root, 'Index', 'retrieval.sqlite'), { readOnly: true });
  reader.exec('PRAGMA query_only=ON');
  const sources = new Map(), sourceReceipts = [];
  for (const row of reader.prepare('SELECT * FROM sources').all()) {
    const document = publicData.corpus.get(JSON.parse(row.locator).beirDocumentId);
    sourceReceipts.push(verifyPeerSource(row, document));
    sources.set(row.source_id, row);
  }
  assert.equal(sources.size, 5183);
  const chunkRows = reader.prepare('SELECT chunk_id,source_id,chunk_index,text,chunk_hash,start_offset,end_offset,start_line,end_line,dimensions,embedding_model_version,length(vector) AS vector_bytes FROM chunks ORDER BY source_id,chunk_index').all();
  assert.equal(chunkRows.length, manifest.sample.chunkCount);
  const version = manifest.implementation.indexedEmbeddingModelVersion ?? manifest.implementation.modelVersion;
  const itemsById = new Map(), firstChunkBySource = new Map();
  for (const row of chunkRows) {
    assert.equal(row.dimensions, 384);
    assert.equal(row.vector_bytes, 384 * 4);
    assert.equal(row.embedding_model_version, version);
    const item = peerChunkFromRow(row, sources.get(row.source_id));
    itemsById.set(item.chunkId, item);
    if (!firstChunkBySource.has(item.sourceId)) firstChunkBySource.set(item.sourceId, item);
  }
  const bestChunks = reader.prepare('SELECT c.chunk_id,c.source_id,bm25(chunk_fts) AS lexical_rank FROM chunk_fts JOIN chunks c ON c.id=chunk_fts.rowid WHERE chunk_fts MATCH ? ORDER BY lexical_rank,c.chunk_id');
  const reconstruct = saved => {
    const item = itemsById.get(saved.chunkId);
    assert.ok(item);
    assert.equal(item.sourceId, saved.sourceId);
    assert.equal(item.locator.beirDocumentId, saved.documentId);
    return { ...item, score: saved.score, ...(saved.rerankScore === undefined ? {} : { rerankScore: saved.rerankScore }) };
  };
  const bm25 = documentBm25Index(publicData.corpus);
  const index = new RetrievalIndex({ root: indexCopy.root });
  const embedding = new EmbeddingService({ cpuThreads: 2 });
  const reranker = settings.rerank === 'live' ? new RerankerService({ cpuThreads: 2 }) : null;
  const hashes = {};
  for (const path of ['tests/retrieval-benchmark/run-peer-scifact.mjs', 'tests/retrieval-benchmark/peer-scifact-metrics.mjs',
    'apps/model-gateway/data/retrieval/index-worker.mjs', 'apps/model-gateway/data/retrieval/evidence-references.mjs',
    'apps/model-gateway/orchestration/retrieval/candidate-selection.mjs', 'apps/model-gateway/orchestration/retrieval/source-projection.mjs'])
    hashes[path] = sha256(await readFile(join(REPOSITORY_ROOT, path)));
  await writeNew('manifest.json', { schemaVersion: 1, label: 'Full public SciFact peer component benchmark',
    createdAt: new Date().toISOString(), originalManifestSha256: sha256(manifestBytes), originalRankingsSha256: sha256(originalRankingBytes),
    dataset: manifest.dataset, sample: manifest.sample, originalIndex: indexCopy, sourceReceipts,
    sourceReceiptsSha256: sha256(JSON.stringify(sourceReceipts)), componentSha256: hashes,
    embedding: manifest.implementation.modelIdentity, neural: cachedNeuralManifest?.model ?? reranker?.status() ?? null,
    settings: { repeats: settings.repeats, rerank: settings.rerank, cpuThreads: 2, metricCutoff: 10,
      ignoreIdenticalIds: true, evidenceBudgets: PEER_EVIDENCE_BUDGETS, projectionCharacterBudget: 10000,
      chunkLimit: 6, candidateLimit: 48, requiresSourceRead: true, existingContext: [] },
    answerGeneration: false, actualProviderRequestObserved: false, agentOverallScore: false, network: false,
    system: { node: process.version, platform: process.platform, arch: process.arch, cpu: cpus()[0]?.model,
      totalMemoryBytes: totalmem() } });
  progress('verified-public-index', { output, sourceCount: sources.size, chunkCount: chunkRows.length });
  const methodNames = ['documentBm25', 'chunkLexical', 'vectorOnly', 'hybrid', ...(settings.rerank !== 'off' ? ['neuralRerank'] : [])];
  const records = [], timings = Object.fromEntries(methodNames.map(name => [name, { retrieval: [], endToEnd: [], projection: [], cpuMs: [] }]));
  const embeddingTimes = [];
  let peakRssBytes = process.memoryUsage().rss;
  const sampler = setInterval(() => { peakRssBytes = Math.max(peakRssBytes, process.memoryUsage().rss); }, 20);
  sampler.unref();
  const resourceStarted = process.cpuUsage(), runStarted = performance.now();
  try {
    assert.equal(embedding.status().state, 'ready');
    const warmingStarted = performance.now();
    const warmVector = await embedding.embedQuery(originals[0].originalQuery);
    const vectorOptions = vector => ({ queryVector: vector, embeddingProfileId: 'builtin-multilingual', embeddingModelVersion: version });
    await index.search({ query: originals[0].originalQuery, scopeKeys: ['user'], limit: 60, ...vectorOptions(warmVector.vector) });
    await index.search({ query: VECTOR_ONLY_QUERY, scopeKeys: ['user'], limit: 60, ...vectorOptions(warmVector.vector) });
    bm25.search(originals[0].originalQuery);
    if (reranker) await reranker.rerank({ query: originals[0].originalQuery,
      candidates: originals[0].hybrid.chunks.slice(0, 2).map(reconstruct), limit: 2 });
    const coldWarmupMs = performance.now() - warmingStarted;
    progress('warmup-complete', { coldWarmupMs, rerank: settings.rerank });
    for (let repeat = 0; repeat < settings.repeats; repeat++) for (const [queryIndex, original] of originals.entries()) {
      const query = original.originalQuery, relevance = publicData.judgments.get(original.queryId);
      const embeddedStarted = performance.now(), embedded = await embedding.embedQuery(query);
      const embeddingMs = performance.now() - embeddedStarted;
      embeddingTimes.push(embeddingMs);
      const outputs = {}, methodMs = {};
      // Rotate retrieval order to reduce an accidental always-first warm-cache advantage.
      // 轮转检索方法顺序，减少固定先执行的方法获得偶然热缓存优势。
      const modes = methodNames.slice(0, 4), rotation = (queryIndex + repeat) % modes.length;
      const order = [...modes.slice(rotation), ...modes.slice(0, rotation)];
      for (const mode of order) {
        const started = performance.now(), cpuStarted = process.cpuUsage();
        if (mode === 'documentBm25') {
          const documents = bm25.search(query), bestBySource = new Map();
          const expression = matchExpression(query);
          if (expression) for (const row of bestChunks.all(expression))
            if (!bestBySource.has(row.source_id)) bestBySource.set(row.source_id, itemsById.get(row.chunk_id));
          outputs[mode] = { documents: documents.map(row => row.document_id), items: documents.map(row => {
            const sourceId = `beir:scifact:${row.document_id}`;
            const item = bestBySource.get(sourceId) ?? firstChunkBySource.get(sourceId);
            assert.ok(item);
            return { ...item, score: -row.bm25 };
          }) };
        } else {
          const response = await index.search({ query: mode === 'vectorOnly' ? VECTOR_ONLY_QUERY : query,
            scopeKeys: ['user'], limit: 60, ...(mode === 'chunkLexical' ? {} : vectorOptions(embedded.vector)) });
          for (const item of response.items) {
            const actual = itemsById.get(item.chunkId);
            assert.equal(actual?.contentHash, item.contentHash);
            assert.equal(actual?.chunkHash, item.chunkHash);
            assert.equal(actual?.excerpt, item.excerpt);
          }
          if (mode === 'vectorOnly') assert.ok(response.items.every(item => !item.lexicalRank && item.vectorRank > 0),
            'The vector-only diagnostic must not receive any lexical hits from its sentinel query.');
          if (mode === 'chunkLexical') assert.deepEqual(rankingSnapshot(response.items), original.lexical.chunks);
          if (mode === 'hybrid') assert.deepEqual(rankingSnapshot(response.items), original.hybrid.chunks);
          outputs[mode] = { documents: documentIds(response.items), items: response.items };
        }
        const elapsed = performance.now() - started, cpu = process.cpuUsage(cpuStarted);
        methodMs[mode] = elapsed;
        timings[mode].retrieval.push(elapsed);
        timings[mode].endToEnd.push(elapsed + (['vectorOnly', 'hybrid'].includes(mode) ? embeddingMs : 0));
        timings[mode].cpuMs.push((cpu.user + cpu.system) / 1000);
      }
      if (settings.rerank === 'cached') {
        const prior = cachedNeural[queryIndex];
        assert.equal(prior.originalQuery, query);
        assert.deepEqual(prior.judgments, original.judgments);
        outputs.neuralRerank = { documents: prior.documents, items: prior.chunks.map(reconstruct) };
      } else if (reranker) {
        const cpuStarted = process.cpuUsage(), started = performance.now();
        const candidates = deduplicateCandidates(outputs.hybrid.items.slice(0, 48)).items.slice(0, 20);
        const reranked = await reranker.rerank({ query, candidates, limit: 20 });
        const elapsed = performance.now() - started, cpu = process.cpuUsage(cpuStarted);
        outputs.neuralRerank = { documents: documentIds(reranked.items), items: reranked.items };
        timings.neuralRerank.retrieval.push(elapsed);
        timings.neuralRerank.endToEnd.push(embeddingMs + methodMs.hybrid + elapsed);
        timings.neuralRerank.cpuMs.push((cpu.user + cpu.system) / 1000);
      }
      const record = { queryId: original.queryId, repeat: repeat + 1, query, methods: {} };
      for (const mode of methodNames) {
        const retrieved = outputs[mode], started = performance.now();
        const evidenceBudget = Object.fromEntries(PEER_EVIDENCE_BUDGETS.map(budget => [budget,
          peerEvidenceProjection(retrieved.items, query, relevance, budget, { queryId: original.queryId })]));
        timings[mode].projection.push(performance.now() - started);
        record.methods[mode] = { documents: retrieved.documents,
          selfIdsRemoved: retrieved.documents.filter(id => id === original.queryId).length,
          documentAt10: peerDocumentMetrics(retrieved.documents, relevance, { queryId: original.queryId }),
          legacyNoSelfIdExclusionAt10: peerDocumentMetrics(retrieved.documents, relevance,
            { queryId: original.queryId, ignoreIdenticalIds: false }), evidenceBudget };
        const retained = [...new Set(retrieved.documents)].filter(id => id !== original.queryId);
        if (repeat === 0) appendFileSync(join(output, `${mode}.trec.tsv`), retained.map((documentId, rank) =>
          `${original.queryId}\tQ0\t${documentId}\t${rank + 1}\t${1 / (rank + 1)}\tKYNXA-${mode}`).join('\n') + '\n');
      }
      records.push(record);
      appendFileSync(join(output, 'rankings.jsonl'), JSON.stringify(record) + '\n');
      peakRssBytes = Math.max(peakRssBytes, process.memoryUsage().rss);
      progress('query-scored', { repeat: repeat + 1, completed: queryIndex + 1, total: originals.length });
    }
    assert.equal(records.length, settings.repeats * 300);
    const cpu = process.cpuUsage(resourceStarted);
    const methods = Object.fromEntries(methodNames.map(name => [name, {
      documentAt10: meanPeerMetrics(records.map(record => record.methods[name].documentAt10)),
      legacyNoSelfIdExclusionAt10: meanPeerMetrics(records.map(record => record.methods[name].legacyNoSelfIdExclusionAt10)),
      selfIdsRemoved: records.reduce((sum, record) => sum + record.methods[name].selfIdsRemoved, 0),
      evidenceBudget: Object.fromEntries(PEER_EVIDENCE_BUDGETS.map(budget => [budget, {
        ...meanEvidenceMetrics(records.map(record => record.methods[name].evidenceBudget[budget].metrics)),
        meanUsedTokens: mean(records.map(record => record.methods[name].evidenceBudget[budget].usedTokens)),
        maximumUsedTokens: Math.max(...records.map(record => record.methods[name].evidenceBudget[budget].usedTokens)) }])),
      latency: timings[name].retrieval.length ? { retrieval: latencySummary(timings[name].retrieval),
        endToEnd: latencySummary(timings[name].endToEnd), projectionAllBudgets: latencySummary(timings[name].projection),
        cpu: latencySummary(timings[name].cpuMs), sequentialQueriesPerSecond: 1000 / mean(timings[name].endToEnd) } : null,
      ...(name === 'neuralRerank' && cachedNeuralSummary ? { computation: 'Historical actual BGE logits and candidates reused; no new neural inference',
        historicalLatency: cachedNeuralSummary.latency, historicalModel: cachedNeuralManifest.model } : {}) }]));
    const result = { schemaVersion: 1, label: 'Full SciFact public retrieval components; not Agent or answer-generation score',
      createdAt: new Date().toISOString(), sample: { queries: 300, documents: 5183, chunks: chunkRows.length, repeats: settings.repeats },
      methods, queryEmbedding: latencySummary(embeddingTimes), coldWarmupMs,
      resource: { durationMs: performance.now() - runStarted, processCpuMs: (cpu.user + cpu.system) / 1000,
        peakRssBytes, rssSamplingIntervalMs: 20, originalIndexBytes: indexCopy.bytes,
        scope: 'This process, public in-memory BM25 index, production retrieval/E5/optional BGE worker threads; not gateway or whole OS' },
      protocol: { metricCutoff: METRIC_CUTOFF, duplicateDocuments: 'First occurrence in returned chunk order',
        ignoreIdenticalIds: true, ndcgGain: 'Linear TREC gain; SciFact judgments are binary', mapDenominator: 'All positive qrels',
        trecRunScore: 'Strict monotone ordinal scores preserving actual document order; original chunk scores stay in the source run',
        officialEvaluationReference: 'https://github.com/beir-cellar/beir/blob/main/beir/retrieval/evaluation.py',
        originalSourceAndChunkHashesVerified: true, shortReferences: 'ev1, exactly 29 characters; public projection simulation only',
        providerTokenizer: false, actualProviderRequestObserved: false, freshQueryInference: true,
        documentVectorsRecomputed: 0, inputArtifactsModified: false },
      limitations: ['A full SciFact component test is not full BEIR, Agent task success, generated-answer faithfulness or performance across languages',
        'SQLite document BM25 uses unicode61 with no stemming, equal title/body weights and OR queries; not the official Elasticsearch baseline',
        'Document BM25 evidence uses the best matching existing chunk per retrieved document, falling back to its first chunk; no gold data enters retrieval',
        'Vector-only is an explicit production-index diagnostic using no lexical hits and a 40-chunk cap; it is not the earlier unlimited max-chunk document diagnostic',
        'All evidence budgets use the same 48-candidate cap, six chunks, 10000-character cap and surrounding-source notice',
        'Cached BGE quality is historical actual inference; its timing is not part of this new same-machine latency comparison',
        'Component timing excludes dataset validation, index copying, cold warmup, conversation authority snapshots, web, generation and approvals',
        'Each vector mode includes one fresh shared E5 query inference cost; no mode computes document vectors',
        'Projection token counts use the conservative repository heuristic, not a provider tokenizer or billed tokens',
        'Unrelated user and OS workloads are neither controlled nor stopped; no system-wide exclusivity guarantee'] };
    assert.equal(sha256(await readFile(indexCopy.original)), indexCopy.originalSha256, 'The original public index remains byte-for-byte unchanged.');
    await writeNew('results.json', result);
    await writeNew('latency-samples.json', { embedding: embeddingTimes, methods: timings });
    await writeNew('report.md', markdownReport(result));
    progress('complete', { output });
    process.stdout.write(JSON.stringify({ output, methods: Object.fromEntries(Object.entries(methods)
      .map(([name, method]) => [name, { documentAt10: method.documentAt10, latency: method.latency }])) }) + '\n');
  } finally {
    clearInterval(sampler);
    reader.close();
    bm25.database.close();
    const closed = await Promise.allSettled([index.close(), embedding.close(), ...(reranker ? [reranker.close()] : [])]);
    const failures = closed.filter(item => item.status === 'rejected').map(item => item.reason);
    if (failures.length) throw new AggregateError(failures, 'Peer benchmark workers did not retire cleanly.');
  }
}

try { await main(); }
catch (error) { process.stderr.write(`Peer component benchmark failed: ${error.message}\n`); process.exitCode = 1; }

import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { appendFileSync } from 'node:fs';
import { mkdir, mkdtemp, readFile, readdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { RetrievalIndex } from '../../apps/model-gateway/data/retrieval/index.mjs';
import { EmbeddingService } from '../../apps/model-gateway/models/retrieval/embedding-service.mjs';
import { latencySummary } from './metrics.mjs';

const REPOSITORY_ROOT = fileURLToPath(new URL('../../', import.meta.url));
const EVALUATION_ROOT = join(REPOSITORY_ROOT, 'artifacts', 'verification', 'rag-benchmark');
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');
const chunkRanking = items => items.map(item => ({ documentId: item.locator.beirDocumentId, sourceId: item.sourceId,
  chunkId: item.chunkId, score: item.score }));

function options() {
  const settings = { inputs: [], output: EVALUATION_ROOT, repeats: 1 };
  for (let offset = 2; offset < process.argv.length; offset++) {
    const name = process.argv[offset].replace(/^--/u, ''), value = process.argv[++offset];
    if (!value || !['input', 'output', 'repeats'].includes(name)) throw new Error('Unknown or incomplete latency argument.');
    if (name === 'input') settings.inputs.push(resolve(value));
    else if (name === 'repeats') settings.repeats = Number(value);
    else settings.output = resolve(value);
  }
  assert.ok(settings.inputs.length > 0);
  assert.ok(Number.isSafeInteger(settings.repeats) && settings.repeats >= 1 && settings.repeats <= 10);
  return settings;
}

/** Isolate warm query latency from document indexing and other evaluation workloads.
 * 独立测量热态查询耗时，不重复文档建索引，并与其他评测负载错开。
 */
async function main() {
  const settings = options();
  await mkdir(settings.output, { recursive: true });
  const output = await mkdtemp(join(settings.output, 'scifact-clean-latency-'));
  const progress = (phase, details = {}) => {
    const line = JSON.stringify({ time: new Date().toISOString(), phase, ...details }) + '\n';
    appendFileSync(join(output, 'progress.jsonl'), line);
    if (!details.completed || details.completed % 20 === 0 || details.completed === details.total) process.stderr.write(line);
  };
  const embedding = new EmbeddingService({ cpuThreads: 2 });
  const cases = [];
  let originalQueryIds;
  try {
    for (const input of settings.inputs) {
      const manifestBytes = await readFile(join(input, 'manifest.json'));
      const manifest = JSON.parse(manifestBytes);
      assert.equal(manifest.dataset.publishedMd5, '5f7d1de60b170fc8027bb7898e2efca1');
      await readFile(join(input, 'results.json'));
      const rankings = (await readFile(join(input, 'rankings.jsonl'), 'utf8')).trim().split(/\r?\n/u).map(JSON.parse);
      const queryIds = rankings.map(record => record.queryId);
      if (originalQueryIds) assert.deepEqual(queryIds, originalQueryIds, 'Latency cases must use exactly the same query order.');
      else originalQueryIds = queryIds;
      const indexModule = manifest.implementation.baselineRoot
        ? await import(pathToFileURL(join(manifest.implementation.baselineRoot, 'index.mjs')).href) : { RetrievalIndex };
      const dataFolder = (await readdir(input)).find(name => name.startsWith('temporary-data-'));
      assert.ok(dataFolder);
      const index = new indexModule.RetrievalIndex({ root: join(input, dataFolder) });
      const samples = { lexical: [], queryEmbedding: [], hybridSearchWithPrecomputedVector: [], hybridEndToEnd: [] };
      try {
        const sources = await index.listSources({ scopeKeys: ['user'] });
        assert.equal(sources.length, manifest.sample.documentCount);
        assert.ok(sources.every(source => source.sourceType === 'public-benchmark' && source.sourceId.startsWith('beir:scifact:')));
        const version = manifest.implementation.indexedEmbeddingModelVersion ?? manifest.implementation.modelVersion;
        const vectorOptions = vector => ({ queryVector: vector, embeddingProfileId: 'builtin-multilingual', embeddingModelVersion: version });
        const warm = await embedding.embedQuery(rankings[0].originalQuery);
        await index.search({ query: rankings[0].originalQuery, scopeKeys: ['user'], limit: 60 });
        await index.search({ query: rankings[0].originalQuery, scopeKeys: ['user'], limit: 60, ...vectorOptions(warm.vector) });
        progress('case-warmed', { input, queryCount: rankings.length });
        for (let repeat = 0; repeat < settings.repeats; repeat++) for (const [queryIndex, record] of rankings.entries()) {
          const query = record.originalQuery;
          let started = performance.now();
          const lexical = await index.search({ query, scopeKeys: ['user'], limit: 60 });
          samples.lexical.push(performance.now() - started);
          assert.deepEqual(chunkRanking(lexical.items), record.lexical.chunks);
          const hybridStarted = performance.now();
          // Recompute every query vector through the real loaded E5 worker; no cached vector short-cut.
          // 每次查询都通过已加载的真实 E5 worker 重算，不用向量缓存缩短计时。
          const embedded = await embedding.embedQuery(query);
          samples.queryEmbedding.push(performance.now() - hybridStarted);
          started = performance.now();
          const hybrid = await index.search({ query, scopeKeys: ['user'], limit: 60, ...vectorOptions(embedded.vector) });
          samples.hybridSearchWithPrecomputedVector.push(performance.now() - started);
          samples.hybridEndToEnd.push(performance.now() - hybridStarted);
          assert.deepEqual(chunkRanking(hybrid.items), record.hybrid.chunks);
          progress('latency-query', { input, repeat: repeat + 1, completed: queryIndex + 1, total: rankings.length });
        }
        cases.push({ input, originalManifestSha256: sha256(manifestBytes), sample: manifest.sample,
          latency: Object.fromEntries(Object.entries(samples).map(([name, values]) => [name, latencySummary(values)])), samples });
      } finally {
        await index.close();
      }
    }
  } finally {
    await embedding.close();
  }
  const summary = { schemaVersion: 1, label: 'Independent same-index warm-query retest; original run observations remain unchanged',
    createdAt: new Date().toISOString(), repeats: settings.repeats, cpuThreads: 2, freshQueryVectors: true,
    network: false, answerGeneration: false, previousDocumentInferenceInThisProcess: false,
    noOtherOwnedNativeBenchmarkWorkloads: true, cases,
    limitations: ['No system-wide exclusivity guarantee; unrelated user/OS processes remain untouched',
      'No document scanning, generation, evidence selection, web or Agent task time is included',
      'Model cold initialization and each index first-query warmup are excluded'] };
  await writeFile(join(output, 'results.json'), JSON.stringify(summary, null, 2) + '\n');
  progress('complete', { output });
  process.stdout.write(JSON.stringify({ output, cases: cases.map(result => ({ input: result.input,
    queryCount: result.sample.queryCount, documentCount: result.sample.documentCount, latency: result.latency })) }) + '\n');
}

try { await main(); }
catch (error) { process.stderr.write(`Warm component retest failed: ${error.message}\n`); process.exitCode = 1; }

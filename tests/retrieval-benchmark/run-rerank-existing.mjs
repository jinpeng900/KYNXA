import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { appendFileSync } from 'node:fs';
import { mkdir, mkdtemp, readFile, readdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { RetrievalIndex } from '../../apps/model-gateway/data/retrieval/index.mjs';
import { hashText } from '../../apps/model-gateway/data/retrieval/retrieval-contracts.mjs';
import { EmbeddingService } from '../../apps/model-gateway/models/retrieval/embedding-service.mjs';
import { RerankerService } from '../../apps/model-gateway/models/retrieval/reranker-service.mjs';
import { BUILTIN_RERANKER_PROFILE } from '../../apps/model-gateway/models/retrieval/reranker-profile.mjs';
import { selectCandidates, RETRIEVAL_CANDIDATE_LIMIT } from '../../apps/model-gateway/orchestration/retrieval/candidate-selection.mjs';
import { BenchmarkVectorCache } from './vector-cache.mjs';
import { RETRIEVAL_CUTOFFS, evidenceMetrics, meanEvidenceMetrics, retrievalMetrics, meanMetrics, latencySummary } from './metrics.mjs';

const REPOSITORY_ROOT = fileURLToPath(new URL('../../', import.meta.url));
const EVALUATION_ROOT = join(REPOSITORY_ROOT, 'artifacts', 'verification', 'rag-benchmark');
const BUDGETS = Object.freeze([2048, 4096, 6144, 8192]);
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const chunkRanking = items => items.map(item => ({ documentId: item.locator.beirDocumentId, sourceId: item.sourceId,
  chunkId: item.chunkId, score: item.score }));
const documentRanking = items => [...new Set(items.map(item => item.locator.beirDocumentId))];

function options() {
  const settings = { vectorCache: join(EVALUATION_ROOT, 'vectors'), output: EVALUATION_ROOT };
  for (let offset = 2; offset < process.argv.length; offset++) {
    const name = process.argv[offset].replace(/^--/u, ''), value = process.argv[++offset];
    const key = ({ input: 'input', output: 'output', 'vector-cache': 'vectorCache', 'reranker-root': 'rerankerRoot' })[name];
    if (!key || !value) throw new Error('Unknown or incomplete rerank-evaluation argument.');
    settings[key] = resolve(value);
  }
  if (!settings.input) throw new Error('Use --input to select a completed public SciFact evaluation.');
  return settings;
}

/** Reuse a completed public component run; never reconstruct production data or rerun document inference.
 * 复用已完成的公开组件评测，不重建用户正式数据，也不重新推理所有文档。
 */
async function main() {
  const settings = options();
  const manifestBytes = await readFile(join(settings.input, 'manifest.json'));
  const manifest = JSON.parse(manifestBytes);
  assert.equal(manifest.schemaVersion, 2);
  assert.equal(manifest.dataset.publishedMd5, '5f7d1de60b170fc8027bb7898e2efca1');
  assert.equal(manifest.implementation.baselineRoot, null,
    'Use a completed production-index run; preserved legacy baseline indexes are not migrated by this entry.');
  const originalResults = JSON.parse(await readFile(join(settings.input, 'results.json'), 'utf8'));
  const originalRankingBytes = await readFile(join(settings.input, 'rankings.jsonl'));
  const originalRankings = originalRankingBytes.toString('utf8').trim().split(/\r?\n/u).map(JSON.parse);
  assert.equal(originalRankings.length, manifest.sample.queryCount);
  assert.equal(originalResults.sample.queries, originalRankings.length);
  const dataFolder = (await readdir(settings.input)).find(name => name.startsWith('temporary-data-'));
  assert.ok(dataFolder, 'The independent public evaluation index must still be present.');
  const index = new RetrievalIndex({ root: join(settings.input, dataFolder) });
  const embedding = new EmbeddingService({ cpuThreads: 2 });
  const reranker = new RerankerService({ modelRoot: settings.rerankerRoot, cpuThreads: 2 });
  const queryCache = new BenchmarkVectorCache({ root: settings.vectorCache,
    modelIdentity: manifest.implementation.modelIdentity, inputVersion: 'original-query-v1', dimensions: 384 });
  await mkdir(settings.output, { recursive: true });
  const output = await mkdtemp(join(settings.output, `scifact-neural-q${originalRankings.length}-d${manifest.sample.documentCount}-`));
  const progress = (phase, details = {}) => {
    const line = JSON.stringify({ time: new Date().toISOString(), phase, ...details }) + '\n';
    appendFileSync(join(output, 'progress.jsonl'), line);
    if (!details.completed || details.completed % 20 === 0 || details.completed === details.total) process.stderr.write(line);
  };
  const sourceFiles = ['reranker-service.mjs', 'reranker-worker.mjs', 'reranker-profile.mjs'];
  const componentHashes = Object.fromEntries(await Promise.all(sourceFiles.map(async name =>
    [name, hash(await readFile(join(REPOSITORY_ROOT, 'apps/model-gateway/models/retrieval', name)))])));
  await writeFile(join(output, 'manifest.json'), JSON.stringify({ schemaVersion: 1,
    label: 'Actual offline neural reranking ablation of a completed public retrieval-component run',
    originalRun: settings.input, originalManifestSha256: hash(manifestBytes), originalRankingsSha256: hash(originalRankingBytes),
    dataset: manifest.dataset, sample: manifest.sample, model: BUILTIN_RERANKER_PROFILE,
    componentSha256: componentHashes, candidateLimit: 20, evidenceChunkLimit: 6, evidenceTokenBudgets: BUDGETS,
    cpuThreads: 2, network: false, answerGeneration: false, agentOverallScore: false,
    queryCacheModelIdentity: manifest.implementation.modelIdentity }, null, 2) + '\n');
  progress('starting', { output, originalRun: settings.input, queries: originalRankings.length });
  const records = [], timings = { queryPreparation: [], rerank: [], rerankAndSelection: [] };
  let peakRssBytes = process.memoryUsage().rss;
  const rssTimer = setInterval(() => { peakRssBytes = Math.max(peakRssBytes, process.memoryUsage().rss); }, 20);
  rssTimer.unref();
  try {
    assert.equal(reranker.status().state, 'ready', 'Real fixed reranker assets must be available; no fallback score is accepted.');
    const available = await index.listSources({ scopeKeys: ['user'] });
    assert.equal(available.length, manifest.sample.documentCount);
    assert.ok(available.every(source => source.sourceType === 'public-benchmark' &&
      source.sourceId.startsWith('beir:scifact:')), 'Only public benchmark sources may be searched.');
    for (const original of originalRankings) {
      const startedPreparation = performance.now(), query = original.originalQuery;
      const [vector] = await queryCache.getOrCompute([{ input: query, originalSha256: hashText(query), kind: 'original-public-query' }],
        async inputs => [(await embedding.embedQuery(inputs[0])).vector]);
      const raw = await index.search({ query, scopeKeys: ['user'], queryVector: vector,
        embeddingProfileId: 'builtin-multilingual', embeddingModelVersion:
          manifest.implementation.indexedEmbeddingModelVersion ?? manifest.implementation.modelVersion, limit: 60 });
      // The reranker sees the exact original candidates. A stale or differently tokenized index is rejected.
      // 重排必须接收原评测的同一批候选；索引过期或分词版本不同导致排序变化时明确拒绝。
      assert.deepEqual(chunkRanking(raw.items), original.hybrid.chunks);
      const candidates = selectCandidates(raw.items.slice(0, RETRIEVAL_CANDIDATE_LIMIT),
        { query, limit: 20, maximumTokens: 16384, lambda: 1 }).items;
      timings.queryPreparation.push(performance.now() - startedPreparation);
      const startedRerank = performance.now();
      const reranked = await reranker.rerank({ query, candidates, limit: 20 });
      timings.rerank.push(performance.now() - startedRerank);
      const relevance = new Map(original.judgments.map(judgment => [judgment.documentId, judgment.grade]));
      const selected = Object.fromEntries(BUDGETS.map(maximumTokens => {
        const projection = selectCandidates(reranked.items, { query, limit: 6, maximumTokens });
        return [maximumTokens, { metrics: evidenceMetrics(projection.items.map(item => item.locator.beirDocumentId), relevance),
          selection: projection.selection, evidenceAssessment: projection.evidenceAssessment, chunks: chunkRanking(projection.items) }];
      }));
      timings.rerankAndSelection.push(performance.now() - startedRerank);
      records.push({ queryId: original.queryId, originalQuery: query, judgments: original.judgments,
        candidateCount: candidates.length, documents: documentRanking(reranked.items),
        chunks: reranked.items.map(item => ({ ...chunkRanking([item])[0], rerankScore: item.rerankScore, rerankRank: item.rerankRank })),
        metrics: Object.fromEntries(RETRIEVAL_CUTOFFS.map(cutoff => [cutoff, retrievalMetrics(documentRanking(reranked.items), relevance, cutoff)])),
        selectedEvidence: selected, truncatedInputsCount: reranked.truncatedInputsCount });
      appendFileSync(join(output, 'rankings.jsonl'), JSON.stringify(records.at(-1)) + '\n');
      progress('neural-query-scored', { completed: records.length, total: originalRankings.length, queryId: original.queryId });
    }
    const summary = { schemaVersion: 1, label: 'Agent retrieval-component ablation; not Agent overall performance or answer quality',
      originalRun: settings.input, sample: originalResults.sample,
      baselineHybrid: originalResults.metrics.hybrid,
      neuralRerank: Object.fromEntries(RETRIEVAL_CUTOFFS.map(cutoff => [cutoff, meanMetrics(records.map(record => record.metrics[cutoff]))])),
      evidenceBudgetSweep: Object.fromEntries(BUDGETS.map(budget => [budget, meanEvidenceMetrics(records.map(record => record.selectedEvidence[budget].metrics))])),
      latency: Object.fromEntries(Object.entries(timings).map(([name, samples]) => [name, latencySummary(samples)])),
      memory: { peakRssBytes, samplingIntervalMs: 20, scope: 'This owned benchmark process and native worker threads only' },
      model: reranker.status(), queryCache: queryCache.stats,
      truncatedInputsCount: records.reduce((sum, record) => sum + record.truncatedInputsCount, 0),
      limitations: ['Public English scientific claims only', 'Reranks at most 20 initial hybrid chunks, not the complete corpus',
        'Cold model initialization is included in the first rerank observation; all observations are reported',
        'No chat answers, browser actions, real task success, approvals or recovery are scored'] };
    await writeFile(join(output, 'results.json'), JSON.stringify(summary, null, 2) + '\n');
    await writeFile(join(output, 'latency-samples.json'), JSON.stringify(timings) + '\n');
  } catch (error) {
    progress('failed', { code: error.code, message: error.message });
    throw error;
  } finally {
    clearInterval(rssTimer);
    const closed = await Promise.allSettled([index.close(), embedding.close(), reranker.close()]);
    const failures = closed.filter(result => result.status === 'rejected').map(result => result.reason);
    if (failures.length) throw new AggregateError(failures, 'Neural benchmark resources could not confirm safe retirement.');
  }
  progress('complete', { output });
  process.stdout.write(JSON.stringify({ output, queries: records.length }) + '\n');
}

try { await main(); }
catch (error) { process.stderr.write(`Neural component evaluation failed: ${error.message}\n`); process.exitCode = 1; }

import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { mkdir, mkdtemp, readdir, readFile, rm, stat, writeFile } from 'node:fs/promises';
import { tmpdir, cpus, totalmem } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';
import { RetrievalIndex } from '../data/retrieval/index.mjs';
import { hashText } from '../data/retrieval/retrieval-contracts.mjs';
import { DEFAULT_RETRIEVAL_SETTINGS } from '../data/retrieval/settings.mjs';
import { SourceSyncService } from '../orchestration/retrieval/source-sync.mjs';
import { sourceIdentity } from '../orchestration/retrieval/source-projection.mjs';

const DIMENSIONS = 384, UNITS_PER_FILE = 10, CLUSTERS = 256;
const EXPANSION_SEARCH = DEFAULT_RETRIEVAL_SETTINGS.local.ann.expansionSearch;
const PROJECT_ID = 'scale-repo', SCOPE = `project:${PROJECT_ID}`;
const PROFILE = 'synthetic-scale-v1';
const SPACE_ID = hashText('synthetic-scale-space-v1');

function randomSequence(seed) {
  let state = seed >>> 0;
  return () => { state ^= state << 13; state ^= state >>> 17; state ^= state << 5; return (state >>> 0) / 4294967296; };
}

function normalized(vector) {
  const norm = Math.sqrt(vector.reduce((sum, value) => sum + value * value, 0));
  return Float32Array.from(vector, value => value / norm);
}

function fixtureCentres() {
  return Array.from({ length: CLUSTERS }, (_, index) => {
    const random = randomSequence(index + 42);
    return normalized(Float32Array.from({ length: DIMENSIONS }, () => random() - 0.5));
  });
}

function fixtureVector(centres, cluster, seed) {
  const random = randomSequence(seed + 117);
  return normalized(Float32Array.from(centres[cluster], value => value +
    (random() + random() + random() + random() - 2) * 0.035));
}

function fixtureSource(workspace, number, centres) {
  const filename = `module-${String(number).padStart(5, '0')}.mjs`, path = join(workspace, filename);
  const chunks = [], vectors = [];
  let text = '';
  for (let unit = 0; unit < UNITS_PER_FILE; unit++) {
    const name = `compute_${number}_${unit}`, body = `export function ${name}() { return "scale receipt ${number}:${unit}"; }\n`;
    const startOffset = text.length, endOffset = startOffset + body.length;
    chunks.push({ chunkIndex: unit, chunkId: `scale-${number}-${unit}`, chunkHash: hashText(body), text: body,
      startOffset, endOffset, startLine: unit + 1, endLine: unit + 2,
      structure: { domain: 'code', language: 'javascript', kind: 'function', symbolName: name,
        qualifiedName: name, parseStatus: 'parsed', unitStartOffset: startOffset, unitEndOffset: endOffset } });
    vectors.push(fixtureVector(centres, (number * UNITS_PER_FILE + unit) % CLUSTERS, number * UNITS_PER_FILE + unit + 1));
    text += body;
  }
  return { sourceId: sourceIdentity('work-file', PROJECT_ID, workspace, filename), scopeKey: SCOPE,
    sourceType: 'work-file', title: filename, locator: { root: workspace, path, relativePath: filename }, text,
    contentHash: hashText(text), bindingRevision: 1, chunks, vectors, embeddingProfileId: PROFILE,
    embeddingModelVersion: 'fixture-v1', embeddingSpaceId: SPACE_ID,
    structure: { domain: 'code', language: 'javascript', parserVersion: 'scale-known-boundaries-v1', parseStatus: 'parsed', diagnosticCodes: [] } };
}

async function directoryBytes(directory) {
  let bytes = 0;
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    bytes += entry.isDirectory() ? await directoryBytes(path) : (await stat(path)).size;
  }
  return bytes;
}

function latencySummary(values) {
  const sorted = [...values].sort((left, right) => left - right);
  return { p50Ms: sorted[Math.floor(sorted.length / 2)], p95Ms: sorted[Math.min(sorted.length - 1, Math.ceil(sorted.length * 0.95) - 1)],
    minimumMs: sorted[0], maximumMs: sorted.at(-1) };
}

// This is a large component acceptance test, not model quality or an Agent task success score.
// 这是大规模组件验收，不测模型质量，也不能当成 Agent 任务成功率；默认回归不自动构建十万向量。
test('ten thousand source files and one hundred thousand chunks retain local ANN recall, persistence and incremental reads',
  { skip: process.env.KYNXA_TEST_RETRIEVAL_SCALE !== '1', timeout: 900000 }, async t => {
    const root = await mkdtemp(join(tmpdir(), 'kynxa-large-retrieval-'));
    const workspace = join(root, '测评仓库'), data = join(root, '本地数据');
    await mkdir(workspace); await mkdir(data);
    let index = new RetrievalIndex({ root: data }), sync;
    t.after(async () => {
      sync?.close(); await index.close();
      const suffix = relative(resolve(tmpdir()), resolve(root));
      assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
      await rm(root, { recursive: true, force: true });
    });
    let peakRssBytes = process.memoryUsage().rss;
    const sampleMemory = () => { peakRssBytes = Math.max(peakRssBytes, process.memoryUsage().rss); };
    const timer = setInterval(sampleMemory, 25); t.after(() => clearInterval(timer));
    const centres = fixtureCentres(), fileCount = 10000;
    const preparedAt = performance.now();
    for (let offset = 0; offset < fileCount; offset += 100) {
      const batch = Array.from({ length: Math.min(100, fileCount - offset) }, (_, index) => fixtureSource(workspace, offset + index, centres));
      await Promise.all(batch.map(source => writeFile(source.locator.path, source.text)));
      await index.upsertSources(batch);
      sampleMemory();
      if (offset % 2000 === 0) process.stdout.write(`# scale publication ${offset + batch.length}/${fileCount}\n`);
    }
    const corpusPublicationMs = performance.now() - preparedAt;
    const indexed = await index.status();
    assert.equal(indexed.sources, fileCount); assert.equal(indexed.chunks, fileCount * UNITS_PER_FILE);
    assert.equal(indexed.vectorChunks, fileCount * UNITS_PER_FILE);
    const queries = Array.from({ length: 32 }, (_, number) => fixtureVector(centres, (number * 29) % CLUSTERS, 1000001 + number));
    const search = (vector, mode) => index.search({ query: 'zzindependentvectorquery', scopeKeys: [SCOPE], queryVector: vector,
      embeddingProfileId: PROFILE, embeddingModelVersion: 'fixture-v1', embeddingSpaceId: SPACE_ID,
      retrievalIntent: { domain: 'code' }, limit: 20, ann: { mode, expansionSearch: EXPANSION_SEARCH } });
    const coldAt = performance.now(), cold = await search(queries[0], 'ann');
    const coldAnnBuildAndQueryMs = performance.now() - coldAt;
    assert.equal(cold.semanticBackend, 'ann', JSON.stringify(cold.degradedReason));
    const exactTimes = [], annTimes = [], recalls = [];
    for (const vector of queries) {
      let started = performance.now(); const exact = await search(vector, 'exact'); exactTimes.push(performance.now() - started);
      started = performance.now(); const approximate = await search(vector, 'ann'); annTimes.push(performance.now() - started);
      assert.equal(exact.semanticBackend, 'exact'); assert.equal(approximate.semanticBackend, 'ann');
      assert.equal(exact.items.length, 20); assert.equal(approximate.items.length, 20);
      const exactIds = new Set(exact.items.map(item => item.chunkId));
      recalls.push(approximate.items.filter(item => exactIds.has(item.chunkId)).length / 20);
    }
    const annBeforeRestart = (await index.status()).ann;
    await index.close(); index = new RetrievalIndex({ root: data });
    const restartAt = performance.now(), restored = await search(queries[0], 'ann');
    const restartLoadAndQueryMs = performance.now() - restartAt;
    assert.equal(restored.semanticBackend, 'ann');
    const annAfterRestart = (await index.status()).ann;
    assert.ok(annAfterRestart.loaded > 0, 'Unicode-path graph must load, not rebuild');
    const settings = { ...structuredClone(DEFAULT_RETRIEVAL_SETTINGS), projectId: PROJECT_ID,
      projectIndexing: { mountedFolder: true, bindingRevision: 1 } };
    const createSync = () => new SourceSyncService({ library: { root: data }, getProject: async () => ({ FolderPath: workspace }),
      watchFactory: () => Object.assign(new EventEmitter(), { close() {} }) });
    sync = createSync();
    let started = performance.now(), firstScan = await sync.mountedSnapshot(PROJECT_ID, settings);
    const firstScanMs = performance.now() - started;
    assert.equal(firstScan.sources.length, fileCount);
    sync.close(); sync = createSync();
    started = performance.now(); const reused = await sync.mountedSnapshot(PROJECT_ID, settings);
    const resumedScanMs = performance.now() - started;
    assert.equal(reused.scan.fileReads, 0, 'restart metadata reconciliation must not reread the whole corpus');
    const changed = reused.sources[0];
    await writeFile(changed.locator.path, 'export function changedOnly() { return "new independent receipt"; }\n');
    sync.markChanged(PROJECT_ID, changed.locator.relativePath);
    started = performance.now(); const updated = await sync.mountedSnapshot(PROJECT_ID, settings);
    const updateScanMs = performance.now() - started;
    assert.equal(updated.scan.fileReads, 1);
    sampleMemory();
    const result = { benchmark: 'local-scale-acceptance-v1', date: '2026-10-07',
      scope: 'synthetic component acceptance: exact-vs-ANN plus physical source scanning; no model API or Agent answer scoring',
      corpus: { files: fileCount, chunks: fileCount * UNITS_PER_FILE, dimensions: DIMENSIONS, queries: queries.length,
        embeddings: 'deterministic normalized clustered fixture vectors; not learned model embeddings', seed: 42,
        chunking: 'known literal function boundaries, independent of parser quality tests' },
      searchConfiguration: { ...DEFAULT_RETRIEVAL_SETTINGS.local.ann, expansionSearch: EXPANSION_SEARCH, limit: 20 },
      hardware: { platform: process.platform, arch: process.arch, node: process.version, cpu: cpus()[0]?.model,
        logicalCpus: cpus().length, totalMemoryBytes: totalmem() },
      indexing: { corpusPublicationMs, coldAnnBuildAndQueryMs, restartLoadAndQueryMs, diskBytes: await directoryBytes(data),
        peakParentRssBytes: peakRssBytes,
        helperObservedPeakRssBytes: Math.max(annBeforeRestart.helperObservedPeakRssBytes ?? 0, annAfterRestart.helperObservedPeakRssBytes ?? 0),
        memoryMeasurement: 'parent RSS sampled every 25 ms; helper RSS sampled after operations, not an OS-enforced total peak',
        annBeforeRestart, annAfterRestart },
      retrieval: { recallAt20RelativeToExact: recalls.reduce((sum, value) => sum + value, 0) / recalls.length,
        minimumQueryRecallAt20: Math.min(...recalls), exact: latencySummary(exactTimes), ann: latencySummary(annTimes) },
      sourceScan: { firstScanMs, resumedScanMs, updateScanMs, first: firstScan.scan, resumed: reused.scan, changed: updated.scan },
      paidApiUsed: false, personalDataUsed: false };
    const artifactDirectory = fileURLToPath(new URL('../../../artifacts/verification/rag-scale-20261007/', import.meta.url));
    await mkdir(artifactDirectory, { recursive: true });
    await writeFile(join(artifactDirectory, 'scale-benchmark.json'), JSON.stringify(result, null, 2));
    assert.ok(result.retrieval.recallAt20RelativeToExact >= 0.95, 'ANN recall target must be measured against exact search');
  });

import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { test } from 'node:test';
import { SourceIndexer, embeddingChunkBatch } from '../orchestration/retrieval/source-indexer.mjs';
import { IndexJobService } from '../orchestration/retrieval/index-job-service.mjs';
import { SourceCoverageStore } from '../data/retrieval/source-coverage.mjs';
import { RetrievalJobStore } from '../data/retrieval/job-store.mjs';
import { hashText } from '../data/retrieval/retrieval-contracts.mjs';
import { DEFAULT_RETRIEVAL_SETTINGS, validateIndexingLimits } from '../data/retrieval/settings.mjs';
import { documentExtractionCoverage } from '../tools/retrieval/document-extraction.mjs';
import { DOCUMENT_EXTRACTION_LIMITS } from '../tools/retrieval/document-extraction-contracts.mjs';

async function temporaryRoot(t) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-indexing-capacity-'));
  t.after(async () => {
    const suffix = relative(resolve(tmpdir()), root);
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(root, { recursive: true, force: true });
  });
  return root;
}

function chunkFixture(count, characters = 40, tokenCount = 20) {
  const text = `${'x'.repeat(characters - 1)}\n`;
  const chunks = Array.from({ length: count }, (_, index) => ({ chunkIndex: index,
    chunkId: `capacity:${index}`, chunkHash: hashText(text), text,
    startOffset: index * text.length, endOffset: (index + 1) * text.length,
    startLine: index + 1, endLine: index + 2,
    embeddingProjection: { context: '', tokenCount, maxInputTokens: 512, version: 'fixture-fit-v1' } }));
  const source = { sourceId: 'capacity-source', scopeKey: 'user', sourceType: 'knowledge',
    title: 'Capacity fixture', locator: { relativePath: 'capacity.txt' }, text: chunks.map(chunk => chunk.text).join('') };
  source.contentHash = hashText(source.text);
  return { source, chunks };
}

test('resource suggestions reach 32/64/128 request tiers while token and UTF-8 byte demand remain bounded', () => {
  const fixture = chunkFixture(300), status = { maxInputTokens: 512 };
  assert.equal(embeddingChunkBatch(fixture.source, fixture.chunks, 0, status).length, 32);
  for (const batchSize of [32, 64, 128]) assert.equal(embeddingChunkBatch(fixture.source, fixture.chunks, 0,
    { ...status, batchSuggestions: { batchSize, batchTokenBudget: 65536 } }).length, batchSize);
  assert.equal(embeddingChunkBatch(fixture.source, fixture.chunks, 0,
    { ...status, batchSuggestions: { batchSize: 128, batchTokenBudget: 512 } }).length, 25);
  const medium = chunkFixture(128, 3000, 100), large = chunkFixture(128, 9000, 100);
  assert.equal(embeddingChunkBatch(medium.source, medium.chunks, 0,
    { ...status, batchSuggestions: { batchSize: 128, batchTokenBudget: 65536 } }).length, 64);
  const largeBatch = embeddingChunkBatch(large.source, large.chunks, 0,
    { ...status, batchSuggestions: { batchSize: 128, batchTokenBudget: 65536 } });
  assert.ok(largeBatch.length < 32);
  assert.ok(largeBatch.reduce((sum, chunk) => sum + Buffer.byteLength(chunk.text) + 64, 0) <= 256 * 1024);
});

test('upstream sends a real 128-item request and rereads reduced suggestions before the next request', async () => {
  const { source, chunks } = chunkFixture(210), sizes = [];
  const settings = structuredClone(DEFAULT_RETRIEVAL_SETTINGS);
  const embeddings = { status: () => ({ state: 'ready', modelVersion: 'fixture', dimensions: 2, maxInputTokens: 512,
    batchSuggestions: { batchSize: sizes.length ? 32 : 128, batchTokenBudget: 65536 } }),
  embedDocuments: async texts => { sizes.push(texts.length); return { vectors: texts.map(() => [1, 0]), modelVersion: 'fixture', dimensions: 2 }; } };
  const publications = [];
  const indexer = new SourceIndexer({ embeddings, library: { readSource: async () => source },
    index: { upsertSources: async sources => { publications.push(...sources); return { sources: sources.map(item => ({ sourceId: item.sourceId })) }; } },
    structures: { version: () => 'fixture-structure', parse: async () => ({ chunks }) },
    effectiveSettings: async () => settings, serialize: operation => operation() });
  const report = await indexer.upsert([source], settings);
  assert.deepEqual(sizes, [128, 32, 32, 18]);
  assert.equal(publications[0].vectors.length, 210);
  assert.equal(report.semantic.vectorChunks, 210);
  assert.equal(report.coverage.complete, true);
});

test('CPU indexing policy reaches status, token fitting and embedding requests without reverting to auto', async () => {
  const { source, chunks } = chunkFixture(2), requests = [];
  const settings = structuredClone(DEFAULT_RETRIEVAL_SETTINGS);
  settings.local.embeddingDevicePolicy = 'cpu';
  const metadata = { state: 'ready', profileId: settings.local.embeddingProfileId,
    modelVersion: 'fixture', dimensions: 2, maxInputTokens: 512, fittingVersion: 'fixture-fit-v1' };
  const embeddings = { status: (profileId, options) => { requests.push(['status', { profileId, ...options }]); return metadata; },
    fitDocuments: async (documents, options) => { requests.push(['fit', options]); return { ...metadata,
      documents: documents.map(document => ({ tokenCount: 20,
        segments: [{ start: 0, end: document.text.length, tokenCount: 20 }] })) }; },
    embedDocuments: async (texts, options) => { requests.push(['embed', options]);
      return { ...metadata, vectors: texts.map(() => [1, 0]) }; } };
  const indexer = new SourceIndexer({ embeddings, library: { readSource: async () => source },
    index: { upsertSources: async inputs => ({ sources: inputs.map(item => ({ sourceId: item.sourceId })) }) },
    structures: { version: () => 'fixture-structure', parse: async () => ({ chunks }) },
    effectiveSettings: async () => settings, serialize: operation => operation() });
  const report = await indexer.upsert([source], settings);
  assert.equal(report.semantic.vectorChunks, 2);
  assert.ok(requests.some(([kind]) => kind === 'fit')); assert.ok(requests.some(([kind]) => kind === 'embed'));
  assert.ok(requests.every(([, options]) => options.profileId === settings.local.embeddingProfileId && options.devicePreference === 'cpu'));
});

test('cancelling enlarged batches drains inference and keeps only acknowledged publications and cache receipts', async t => {
  const root = await temporaryRoot(t), jobs = new RetrievalJobStore(root);
  const { source, chunks } = chunkFixture(210), sources = [source, { ...source, sourceId: 'capacity-second' }];
  const settings = structuredClone(DEFAULT_RETRIEVAL_SETTINGS);
  settings.local.indexing.batchSize = 1;
  let resolveEntered, resolveInference;
  const entered = new Promise(resolve => { resolveEntered = resolve; });
  const inference = new Promise(resolve => { resolveInference = resolve; });
  const sizes = [], published = [], coverage = [];
  const embeddings = { status: () => ({ state: 'ready', modelVersion: 'fixture', dimensions: 2, maxInputTokens: 512,
    batchSuggestions: { batchSize: 128, batchTokenBudget: 65536 } }),
  embedDocuments: async texts => {
    sizes.push(texts.length);
    if (sizes.length === 3) { resolveEntered(); await inference; }
    return { vectors: texts.map(() => [1, 0]), modelVersion: 'fixture', dimensions: 2 };
  } };
  const indexer = new SourceIndexer({ embeddings, library: { readSource: async id => sources.find(item => item.sourceId === id) },
    index: { upsertSources: async inputs => { published.push(...inputs); return { sources: inputs.map(item => ({ sourceId: item.sourceId })) }; },
      recordCoverage: async ({ entries }) => { coverage.push(...entries); } },
    structures: { version: () => 'fixture-structure', parse: async () => ({ chunks }) },
    effectiveSettings: async () => settings, serialize: operation => operation() });
  const lifecycle = new IndexJobService({ jobs, prepareSources: async () => ({ sources, settings }),
    publishSources: (...args) => indexer.upsert(...args) });
  const admitted = await lifecycle.rebuild({});
  await entered;
  let acknowledged = false;
  const cancellation = lifecycle.cancelJob(admitted.jobId).then(receipt => { acknowledged = true; return receipt; });
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(acknowledged, false, 'cancellation cannot finish while owned inference is still pending');
  resolveInference();
  const receipt = await cancellation;
  assert.equal(receipt.status, 'cancelled'); assert.equal(receipt.completedSources, 1);
  assert.deepEqual(sizes, [128, 82, 128]);
  assert.deepEqual(published.map(item => item.sourceId), ['capacity-source']);
  assert.equal(coverage.length, 1); assert.equal(coverage[0].status, 'ready');
  assert.ok(indexer.fingerprints.has('capacity-source:semantic'));
  assert.equal(indexer.fingerprints.has('capacity-second:semantic'), false);
});

test('new failed PDFs retain bounded page diagnostics across SQLite coverage migration and restart', async t => {
  const root = await temporaryRoot(t), filename = join(root, 'coverage.sqlite');
  const database = new DatabaseSync(filename);
  database.exec(`CREATE TABLE source_coverage (scope_key TEXT NOT NULL,source_id TEXT NOT NULL,relative_path TEXT NOT NULL,
    status TEXT NOT NULL,lexical TEXT NOT NULL,semantic TEXT NOT NULL,parser TEXT NOT NULL,error_code TEXT,
    source_revision TEXT,updated_at TEXT NOT NULL,PRIMARY KEY(scope_key,source_id));
    CREATE TABLE sources (source_id TEXT,scope_key TEXT,source_revision TEXT);
    CREATE TABLE chunks (source_id TEXT,vector BLOB);`);
  const store = new SourceCoverageStore(database), documentCoverage = documentExtractionCoverage('pdf', DOCUMENT_EXTRACTION_LIMITS,
    'DOCUMENT_PAGE_LIMIT', hashText('synthetic PDF bytes'), { pageCount: 125, processedPages: 0,
      limit: { dimension: 'maximumPages', limit: 100, observed: 125 } });
  const indexer = new SourceIndexer({ embeddings: { status: () => ({ state: 'unavailable' }) },
    index: { recordCoverage: async ({ entries, scopeKeys }) => store.record(entries, scopeKeys) } });
  const report = await indexer.upsert([], structuredClone(DEFAULT_RETRIEVAL_SETTINGS), undefined, undefined,
    { sourceFailures: [{ sourceId: 'failed-document', scopeKey: 'project:fixture', relativePath: 'manual.pdf',
      errorCode: 'DOCUMENT_PAGE_LIMIT', documentCoverage }] });
  assert.equal(report.coverage.discovered, 1); assert.equal(report.coverage.failed, 1);
  assert.equal(report.coverage.complete, false);
  database.close();
  const reopened = new DatabaseSync(filename);
  try {
    const item = new SourceCoverageStore(reopened).query(['project:fixture']).items[0];
    assert.equal(item.sourceRevision, null, 'failed diagnostics do not invent a published source version');
    assert.equal(item.documentCoverage.publishedPages, 0);
    assert.deepEqual(item.documentCoverage.uncoveredRanges, [{ startPage: 1, endPage: 125 }]);
    assert.deepEqual(item.documentCoverage.readback, { kind: 'original-document', startPage: 1, endPage: 125 });
  } finally { reopened.close(); }
});

test('scan capacity gaps persist as partial jobs with effective decoder limits instead of invalid-update failures', async t => {
  const root = await temporaryRoot(t), jobs = new RetrievalJobStore(root);
  const lifecycle = new IndexJobService({ jobs, validateProject: async () => {}, prepareSources: async () => ({ sources: [], settings: {},
    sourceScan: { coverage: { discovered: 7, failed: 0, skipped: 6, complete: false,
      limit: { dimension: 'files', limit: 1, observed: 2, unvisitedCountKnown: false },
      effectiveLimits: { maximumFiles: 1, maximumSourceBytes: 33554432, maximumBytes: 536870912, maximumEntries: 200000,
        maximumDocumentInputBytes: 33554432, maximumDocumentOutputBytes: 2097152, maximumPdfPages: 100 } } } }),
    publishSources: async () => ({ coverage: { discovered: 0, lexical: 0, semantic: 0, failed: 0, skipped: 0, partial: 0, complete: true, sources: [] } }) });
  const admitted = await lifecycle.rebuild({ projectId: 'fixture' });
  await lifecycle.active.get(admitted.jobId)?.promise;
  const job = await new RetrievalJobStore(root).get(admitted.jobId);
  assert.equal(job.status, 'partial'); assert.equal(job.coverage.complete, false);
  assert.equal(job.coverage.limit.dimension, 'files'); assert.equal(job.coverage.effectiveLimits.maximumPdfPages, 100);
});

test('document decoder limits default independently and reject values above the fixed offline safety caps', () => {
  const limits = validateIndexingLimits({ maximumSourceBytes: 256 * 1024 * 1024 });
  assert.equal(limits.maximumDocumentInputBytes, 32 * 1024 * 1024);
  assert.equal(limits.maximumDocumentOutputBytes, 2 * 1024 * 1024);
  assert.equal(limits.maximumPdfPages, 100);
  for (const patch of [{ maximumDocumentInputBytes: 32 * 1024 * 1024 + 1 },
    { maximumDocumentOutputBytes: 2 * 1024 * 1024 + 1 }, { maximumPdfPages: 101 }])
    assert.throws(() => validateIndexingLimits(patch));
});

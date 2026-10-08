import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { test } from 'node:test';
import { EmbeddingService } from '../models/retrieval/embedding-service.mjs';
import { RerankerService } from '../models/retrieval/reranker-service.mjs';
import { BUILTIN_EMBEDDING_PROFILE } from '../models/retrieval/embedding-profile.mjs';
import { BUILTIN_RERANKER_PROFILE } from '../models/retrieval/reranker-profile.mjs';
import { listRetrievalModelProfiles, resolveRetrievalModelProfile } from '../models/retrieval/model-registry.mjs';
import { SourceIndexer } from '../orchestration/retrieval/source-indexer.mjs';
import { hashText } from '../data/retrieval/retrieval-contracts.mjs';
import { RetrievalIndex } from '../data/retrieval/index.mjs';
import { parseDocumentStructure } from '../data/retrieval/document-structure.mjs';
import { chunkStructuredSource, embeddingTextForChunk, STRUCTURED_CHUNKER_VERSION,
  STRUCTURED_EMBEDDING_TEXT_VERSION } from '../data/retrieval/retrieval-text.mjs';

test('local model registry describes only executable pinned implementations and stable vector spaces', () => {
  const profiles = listRetrievalModelProfiles();
  assert.equal(profiles.length, 4);
  const embedding = resolveRetrievalModelProfile('embedding', 'builtin-multilingual');
  const reranker = resolveRetrievalModelProfile('reranker', 'builtin-multilingual-reranker');
  assert.equal(embedding.modelVersion, BUILTIN_EMBEDDING_PROFILE.modelVersion);
  assert.equal(reranker.modelVersion, BUILTIN_RERANKER_PROFILE.modelVersion);
  assert.equal(embedding.dimensions, 384);
  assert.equal(embedding.maxInputTokens, 512);
  assert.equal(embedding.inputProjection.queryPrefix, 'query: ');
  assert.equal(embedding.inputProjection.documentPrefix, 'passage: ');
  assert.equal(embedding.inputProjection.pooling, 'mean');
  assert.equal(embedding.inputProjection.normalize, true);
  assert.match(embedding.embeddingSpaceId, /^[a-f0-9]{64}$/);
  assert.match(embedding.modelAssetSignature, /^[a-f0-9]{64}$/);
  assert.match(embedding.assetSignature, /^[a-f0-9]{64}$/);
  assert.equal(embedding.embeddingSpaceId, resolveRetrievalModelProfile('embedding', embedding.id).embeddingSpaceId);
  assert.ok(profiles.every(profile => profile.local && profile.network === false && Object.isFrozen(profile)));
  assert.equal(reranker.embeddingSpaceId, undefined, 'ranking scores are not embedding vectors');
  assert.throws(() => { embedding.inputProjection.queryPrefix = 'changed'; }, TypeError);
  profiles.pop();
  assert.equal(listRetrievalModelProfiles().length, 4, 'caller changes cannot remove registered implementations');
});

test('audited GPU profiles share pinned assets but never share the legacy CPU vector space', () => {
  const cpu = resolveRetrievalModelProfile('embedding', 'builtin-multilingual');
  const gpu = resolveRetrievalModelProfile('embedding', 'builtin-multilingual-dml-q8');
  assert.equal(cpu.embeddingSpaceId, '6e803abd83b13b2c491ccfafd8723aab46672ad6a985da4cc19a103a2ffc24a2');
  assert.notEqual(gpu.embeddingSpaceId, cpu.embeddingSpaceId);
  assert.equal(gpu.modelAssetSignature, cpu.modelAssetSignature);
  assert.equal(gpu.requiredDevice, 'dml');
  assert.equal(gpu.dtype, cpu.dtype);
  assert.throws(() => new EmbeddingService({ profileId: gpu.id, devicePreference: 'cpu' }), { code: 'EMBEDDING_GPU_REQUIRED' });
  assert.throws(() => new RerankerService({ profileId: 'builtin-multilingual-reranker-dml-q8', devicePreference: 'cpu' }), { code: 'RERANK_GPU_REQUIRED' });
});

test('unknown, wrong-kind and null profiles cannot silently become the default model', async () => {
  const invalid = { code: 'RETRIEVAL_MODEL_PROFILE_UNSUPPORTED' };
  for (const profileId of ['unimplemented-remote-model', null, 'builtin-multilingual-reranker'])
    assert.throws(() => new EmbeddingService({ profileId }), invalid);
  assert.throws(() => new RerankerService({ profileId: 'builtin-multilingual' }), invalid);
  const embedding = new EmbeddingService();
  const reranker = new RerankerService();
  try {
    await assert.rejects(embedding.embedQuery('source', { profileId: 'other' }), invalid);
    await assert.rejects(embedding.embedDocuments([], { profileId: null }), invalid);
    await assert.rejects(embedding.embedQuery('source', { profileId: 'builtin-multilingual-dml-q8' }), invalid);
    await assert.rejects(embedding.fitDocuments([], { profileId: 'builtin-multilingual-dml-q8' }), invalid);
    await assert.rejects(reranker.rerank({ query: 'source', candidates: [], profileId: 'other' }), invalid);
    await assert.rejects(reranker.rerank({ query: 'source', candidates: [], profileId: 'builtin-multilingual-reranker-dml-q8' }), invalid);
    assert.deepEqual(embedding.status('other'), { profileId: 'other', kind: 'embedding', state: 'unavailable',
      loaded: false, supported: false, local: true, network: false, errorCode: invalid.code });
    assert.equal(reranker.status(null).supported, false);
    assert.equal(embedding.status().workerPhase, 'stopped');
    assert.equal(reranker.status().workerPhase, 'stopped');
  } finally { await Promise.all([embedding.close(), reranker.close()]); }
});

test('explicit registered profile metadata survives empty work without loading native assets', async () => {
  const embedding = new EmbeddingService({ profileId: 'builtin-multilingual' });
  const reranker = new RerankerService({ profileId: 'builtin-multilingual-reranker' });
  try {
    const result = await embedding.embedDocuments([], { profileId: 'builtin-multilingual' });
    assert.equal(result.embeddingSpaceId, embedding.status('builtin-multilingual').embeddingSpaceId);
    assert.equal(result.inputProjectionVersion, 'e5-prefixed-mean-l2-v1');
    const ranking = await reranker.rerank({ query: 'source', candidates: [], profileId: 'builtin-multilingual-reranker' });
    assert.equal(ranking.inputProjectionVersion, 'query-passage-pair-sigmoid-v1');
    assert.equal(embedding.status().loaded, false);
    assert.equal(reranker.status().loaded, false);
  } finally { await Promise.all([embedding.close(), reranker.close()]); }
  assert.equal(embedding.status().errorCode, 'EMBEDDING_CLOSED');
  assert.equal(reranker.status().errorCode, 'RERANK_CLOSED');
});

async function syntheticSourcePublication({ status, receipt }) {
  const publications = [];
  const source = { sourceId: 'model-contract-fixture', scopeKey: 'user', sourceType: 'knowledge',
    sourceRevision: 1, title: 'Public fixture', locator: {}, text: 'public fixture evidence '.repeat(680) };
  const settings = { local: { semantic: 'auto', embeddingProfileId: 'builtin-multilingual' } };
  let calls = 0;
  const indexer = new SourceIndexer({ embeddings: { status: () => ({ state: 'ready', ...status }),
    embedDocuments: async texts => ({ vectors: texts.map(() => [1, 0]), ...receipt(++calls, texts) }) },
    library: { readSource: async () => ({ ...source, contentHash: hashText(source.text) }) },
    index: { upsertSources: async sources => { publications.push(...sources);
      return { sources: sources.map(source => ({ sourceId: source.sourceId })) }; } }, serialize: operation => operation(),
    effectiveSettings: async () => settings, getProject: async () => null });
  const progress = [];
  const report = await indexer.upsert([source], settings, undefined, (count, value) => progress.push({ count, report: value }));
  return { publications, indexer, source, settings, report, progress, get calls() { return calls; } };
}

// These are the application consumer's compatibility checks, independent of native model accuracy.
// 这些测试验证应用消费模型合同的兼容规则，与原生模型准确率无关。
test('source publication rejects model, projection and dimension metadata that contradicts the selected instance', async () => {
  for (const [field, expected, actual] of [['modelVersion', 'weights-v1', 'weights-v2'],
    ['inputProjectionVersion', 'projection-v1', 'projection-v2'], ['dimensions', 2, 384]]) {
    const result = await syntheticSourcePublication({ status: { [field]: expected },
      receipt: () => ({ [field]: actual }) });
    assert.equal(result.calls, 1);
    assert.equal(result.indexer.lastEmbeddingError, 'EMBEDDING_PROFILE_MISMATCH', field);
    assert.equal(result.publications.length, 1, 'valid lexical source remains available');
    assert.equal(result.publications[0].vectors, undefined, field);
    assert.equal(result.publications[0].embeddingModelVersion, undefined, field);
  }
});

test('different receipts across batches discard the whole source vector set rather than publishing mixed spaces', async () => {
  for (const field of ['modelVersion', 'inputProjectionVersion', 'embeddingSpaceId']) {
    const result = await syntheticSourcePublication({ status: {}, receipt: call => ({
      [field]: field === 'embeddingSpaceId' ? hashText(`space-${call}`) : `version-${call}` }) });
    assert.equal(result.calls, 2, 'fixture spans multiple embedding batches');
    assert.equal(result.indexer.lastEmbeddingError, 'EMBEDDING_PROFILE_MISMATCH', field);
    assert.equal(result.publications[0].vectors, undefined, field);
    assert.equal(result.publications[0].embeddingSpaceId, undefined, field);
  }
});

test('legacy model seams without optional metadata retain their consistent vectors', async () => {
  const result = await syntheticSourcePublication({ status: {}, receipt: () => ({}) });
  assert.ok(result.calls > 1);
  assert.equal(result.indexer.lastEmbeddingError, undefined);
  assert.ok(result.publications[0].vectors.every(vector => vector.length === 2));
  assert.equal(result.publications[0].embeddingProfileId, 'builtin-multilingual');
});

test('semantic publication reports partial coverage and the actual failed batch without claiming full success', async () => {
  const result = await syntheticSourcePublication({ status: {}, receipt: call => {
    if (call === 2) throw Object.assign(new Error('Synthetic second batch failure.'), { code: 'EMBEDDING_TIMEOUT' });
    return {};
  } });
  const { semantic } = result.report;
  assert.equal(semantic.state, 'partial');
  assert.equal(semantic.requested, true);
  assert.equal(semantic.profileId, 'builtin-multilingual');
  assert.equal(semantic.vectorChunks, 32);
  assert.ok(semantic.totalChunks > semantic.vectorChunks);
  assert.deepEqual(semantic.diagnosticCodes, ['EMBEDDING_TIMEOUT']);
  assert.deepEqual(result.progress[0].report.semantic, result.report.semantic, 'committed progress includes its scoped semantic coverage');
  assert.deepEqual(result.progress[0].report.coverage, result.report.coverage, 'committed progress includes its scoped source coverage');
  assert.equal(result.progress[0].report.checkpointSources[0].vectorChunks, 32, 'the checkpoint records only acknowledged vectors');
});

test('semantic reports are request-scoped and never repeat a prior job error after recovery', async () => {
  let shouldFail = true;
  const result = await syntheticSourcePublication({ status: {}, receipt: () => {
    if (shouldFail) throw Object.assign(new Error('Synthetic first job failure.'), { code: 'EMBEDDING_TIMEOUT' });
    return {};
  } });
  assert.equal(result.report.semantic.state, 'unavailable');
  assert.deepEqual(result.report.semantic.diagnosticCodes, ['EMBEDDING_TIMEOUT']);
  shouldFail = false;
  const recovered = await result.indexer.upsert([result.source], result.settings);
  assert.equal(recovered.semantic.state, 'complete');
  assert.equal(recovered.semantic.vectorChunks, recovered.semantic.totalChunks);
  assert.deepEqual(recovered.semantic.diagnosticCodes, []);
  assert.equal(result.indexer.lastEmbeddingError, 'EMBEDDING_TIMEOUT', 'legacy debugging field is not authoritative job state');
});

test('validated fingerprint reuse reports cached vectors, but revoked sources cannot count as successful coverage', async () => {
  const result = await syntheticSourcePublication({ status: {}, receipt: () => ({}) });
  const calls = result.calls;
  const cached = await result.indexer.upsert([result.source], result.settings);
  assert.equal(cached.semantic.state, 'complete');
  assert.equal(cached.semantic.cachedChunks, cached.semantic.totalChunks);
  assert.equal(cached.semantic.vectorChunks, cached.semantic.totalChunks);
  assert.equal(result.calls, calls);
  result.indexer.library.readSource = async () => null;
  const revoked = await result.indexer.upsert([result.source], result.settings);
  assert.equal(revoked.semantic.state, 'unavailable');
  assert.equal(revoked.semantic.vectorChunks, 0);
  assert.equal(revoked.semantic.cachedChunks, 0);
  assert.equal(revoked.semantic.skippedSources, 1);
  assert.equal(revoked.semantic.skippedChunks, revoked.semantic.totalChunks);
  assert.deepEqual(revoked.semantic.diagnosticCodes, ['STALE_RETRIEVAL_SOURCE']);
});

test('a newly embedded source rejected at publication never counts uncommitted vectors in its report', async () => {
  const result = await syntheticSourcePublication({ status: {}, receipt: () => ({}) });
  result.indexer.invalidate(result.source.sourceId);
  result.indexer.library.readSource = async () => null;
  const skipped = await result.indexer.upsert([result.source], result.settings);
  assert.equal(skipped.semantic.vectorChunks, 0);
  assert.equal(skipped.semantic.skippedChunks, skipped.semantic.totalChunks);
  assert.equal(result.publications.length, 1, 'freshness rejection does not publish another source');
});

test('missing or incomplete publication receipts never report committed coverage or populate reuse caches', async () => {
  const result = await syntheticSourcePublication({ status: {}, receipt: () => ({}) });
  for (const receipt of [undefined, { sources: [] }, { sources: [{ sourceId: 'another-source' }] }]) {
    result.indexer.invalidate(result.source.sourceId);
    result.indexer.index.upsertSources = async () => receipt;
    const unverified = await result.indexer.upsert([result.source], result.settings);
    assert.equal(unverified.semantic.vectorChunks, 0);
    assert.equal(unverified.coverage.lexical, 0);
    assert.equal(unverified.coverage.partial, 1);
    assert.equal(unverified.coverage.complete, false);
    assert.equal(unverified.coverage.sources[0].lexical, 'unverified');
    assert.equal(unverified.coverage.sources[0].errorCode, 'RETRIEVAL_PUBLICATION_UNVERIFIED');
    assert.equal(result.indexer.fingerprints.size, 0);
  }
});

test('disabled semantics and unsupported profiles report their actual states without leaking arbitrary error text', async () => {
  const result = await syntheticSourcePublication({ status: { state: 'unavailable', errorCode: 'RETRIEVAL_MODEL_PROFILE_UNSUPPORTED' },
    receipt: () => { throw new Error('Unavailable profiles cannot execute.'); } });
  assert.equal(result.calls, 0);
  assert.equal(result.report.semantic.state, 'unavailable');
  assert.deepEqual(result.report.semantic.diagnosticCodes, ['RETRIEVAL_MODEL_PROFILE_UNSUPPORTED']);
  result.settings.local.embeddingProfileId = null;
  const disabled = await result.indexer.upsert([result.source], result.settings);
  assert.equal(disabled.semantic.requested, false);
  assert.equal(disabled.semantic.state, 'disabled');
  assert.deepEqual(disabled.semantic.diagnosticCodes, []);
  result.settings.local.embeddingProfileId = 'builtin-multilingual';
  result.indexer.embeddings.status = () => ({ state: 'ready' });
  result.indexer.embeddings.embedDocuments = async () => {
    throw Object.assign(new Error('Synthetic unexpected path-bearing failure.'), { code: 'private/path/value' });
  };
  const failed = await result.indexer.upsert([result.source], result.settings);
  assert.deepEqual(failed.semantic.diagnosticCodes, ['EMBEDDING_FAILED']);
});

test('semantic diagnostics stay bounded across many independent source failures', async () => {
  const result = await syntheticSourcePublication({ status: {}, receipt: () => ({}) });
  const sources = Array.from({ length: 10 }, (_, index) => ({ ...result.source,
    sourceId: `bounded-diagnostic-${index}`, text: `public fixture ${index}` }));
  result.indexer.library.readSource = async sourceId => {
    const source = sources.find(item => item.sourceId === sourceId);
    return source ? { ...source, contentHash: hashText(source.text) } : null;
  };
  let calls = 0;
  result.indexer.embeddings.embedDocuments = async () => {
    throw Object.assign(new Error('Synthetic independent failure.'), { code: `EMBEDDING_FIXTURE_${++calls}` });
  };
  const report = await result.indexer.upsert(sources, result.settings);
  assert.equal(calls, 10);
  assert.equal(report.semantic.state, 'unavailable');
  assert.equal(report.semantic.totalChunks, 10);
  assert.equal(report.semantic.diagnosticCodes.length, 8);
  assert.equal(report.semantic.vectorChunks, 0);
});

test('parser failures retain honest raw context without stale versions or fictitious Markdown chapters in code', async () => {
  const text = 'const content = `\n' + 'String content filler line.\n'.repeat(35) + '# Fake chapter\nbody\n`;\n';
  const source = { sourceId: 'parser-fallback-fixture', scopeKey: 'user', sourceType: 'knowledge', sourceRevision: 1,
    title: 'sample.js', locator: { relativePath: 'sample.js' }, text,
    parserVersion: 'previous-ast-v1', chunkerVersion: 'previous-units-v1', embeddingInputVersion: 'previous-context-v1' };
  const publications = [];
  const indexer = new SourceIndexer({ structures: { parse: async () => {
    throw Object.assign(new Error('Synthetic timeout.'), { code: 'STRUCTURE_PARSE_TIMED_OUT' });
  } }, embeddings: { status: () => ({ state: 'unavailable' }) },
  index: { upsertSources: async sources => publications.push(...sources) } });
  await indexer.upsert([source], { local: { semantic: 'off', embeddingProfileId: null } }, undefined, undefined, { semantic: false });
  assert.equal(publications.length, 1);
  const published = publications[0];
  assert.equal(published.parserVersion, 'plain-text-v1');
  assert.equal(published.chunkerVersion, STRUCTURED_CHUNKER_VERSION);
  assert.equal(published.embeddingInputVersion, STRUCTURED_EMBEDDING_TEXT_VERSION);
  assert.equal(published.text, text);
  assert.equal(published.contentHash, hashText(text));
  assert.ok(published.chunks.every(chunk => chunk.structure.kind === 'context' && chunk.structure.domain === 'code' &&
    chunk.structure.parseStatus === 'unavailable' && chunk.text === text.slice(chunk.startOffset, chunk.endOffset)));
  for (const chunk of published.chunks) assert.doesNotMatch(embeddingTextForChunk(published, chunk), /Section:|Symbol:/u);
});

test('changed lexical fallback clears real vectors and semantic recovery republishes rather than reusing stale fingerprints', async t => {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-model-structure-recovery-'));
  const index = new RetrievalIndex({ root, vectorEnabled: false });
  t.after(async () => {
    await index.close();
    const suffix = relative(resolve(tmpdir()), resolve(root));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(root, { recursive: true, force: true });
  });
  const source = { sourceId: 'structure-recovery-fixture', scopeKey: 'user', sourceType: 'knowledge', sourceRevision: 1,
    title: 'sample.md', locator: { relativePath: 'sample.md' }, text: '# Genuine section\nSome public synthetic content.\n' };
  const settings = { local: { semantic: 'auto', embeddingProfileId: 'builtin-multilingual' } };
  let parserFails = false, embeddingCalls = 0;
  const metadata = { state: 'ready', profileId: 'builtin-multilingual', modelVersion: 'synthetic-model-v1',
    dimensions: 2, inputProjectionVersion: 'synthetic-projection-v1', embeddingSpaceId: hashText('synthetic-space-v1') };
  const indexer = new SourceIndexer({ index, library: { readSource: async () => ({ ...source, contentHash: hashText(source.text) }) },
    serialize: operation => operation(), effectiveSettings: async () => settings,
    embeddings: { status: () => metadata, embedDocuments: async texts => {
      embeddingCalls++; return { ...metadata, vectors: texts.map(() => [1, 0]) };
    } }, structures: { parse: async input => {
      if (parserFails) throw Object.assign(new Error('Synthetic timeout.'), { code: 'STRUCTURE_PARSE_TIMED_OUT' });
      const { units, ...structure } = parseDocumentStructure(input);
      return { structure, parserVersion: structure.parserVersion, chunkerVersion: STRUCTURED_CHUNKER_VERSION,
        embeddingInputVersion: STRUCTURED_EMBEDDING_TEXT_VERSION,
        chunks: chunkStructuredSource(input, { ...structure, units }) };
    } } });
  const initial = await indexer.upsert([source], settings);
  const successfulCalls = embeddingCalls;
  assert.equal((await index.status()).vectorChunks, initial.semantic.totalChunks);
  await indexer.upsert([source], settings, undefined, undefined, { semantic: false });
  const unchanged = await indexer.upsert([source], settings);
  assert.equal(unchanged.semantic.cachedChunks, initial.semantic.totalChunks);
  assert.equal(embeddingCalls, successfulCalls, 'a proven unchanged lexical receipt retains semantic reuse');
  parserFails = true;
  await indexer.upsert([source], settings, undefined, undefined, { semantic: false });
  assert.equal((await index.status()).vectorChunks, 0);
  assert.equal((await index.read({ sourceId: source.sourceId, scopeKeys: ['user'] })).text, source.text);
  parserFails = false;
  const recovered = await indexer.upsert([source], settings);
  assert.equal(recovered.semantic.state, 'complete');
  assert.equal(recovered.semantic.cachedChunks, 0);
  assert.ok(embeddingCalls > successfulCalls, 'lost vectors must be regenerated');
  assert.equal((await index.status()).vectorChunks, recovered.semantic.totalChunks);
  assert.equal((await index.listSources({ scopeKeys: ['user'] }))[0].structure.parseStatus, 'parsed');
});

test('legacy index seams without unchanged publication receipts cannot preserve a semantic fingerprint', async () => {
  const result = await syntheticSourcePublication({ status: {}, receipt: () => ({}) });
  const before = result.calls;
  await result.indexer.upsert([result.source], result.settings, undefined, undefined, { semantic: false });
  const refreshed = await result.indexer.upsert([result.source], result.settings);
  assert.equal(refreshed.semantic.cachedChunks, 0);
  assert.ok(result.calls > before);
});

function preparedDocument(input, parserVersion) {
  const { units, ...descriptor } = parseDocumentStructure(input);
  const structure = { ...descriptor, parserVersion: parserVersion ?? descriptor.parserVersion };
  return { structure, parserVersion: structure.parserVersion, chunkerVersion: STRUCTURED_CHUNKER_VERSION,
    embeddingInputVersion: STRUCTURED_EMBEDDING_TEXT_VERSION,
    chunks: chunkStructuredSource(input, { ...structure, units }) };
}

test('a scope larger than the parser LRU skips unchanged preparation and refreshes revisions or parser identities', async () => {
  const sources = Array.from({ length: 96 }, (_, number) => ({ sourceId: `large-preparation-${number}`, scopeKey: 'user',
    sourceType: 'knowledge', title: `Document ${number}`, text: `# Chapter ${number}\nPublic body ${number}.\n`,
    sourceRevision: 1, locator: { relativePath: `document-${number}.md` } }));
  let version = 'synthetic-document-units-v1', parseCalls = 0, publications = 0;
  const parser = () => ({ version: () => version, parse: async input => { parseCalls++; return preparedDocument(input, version); } });
  const indexer = new SourceIndexer({ structures: parser(), embeddings: { status: () => ({ state: 'unavailable' }) },
    index: { upsertSources: async batch => { publications += batch.length; return { sources: batch.map(source => ({ sourceId: source.sourceId })) }; } } });
  const settings = { local: { semantic: 'off', embeddingProfileId: null } };
  await indexer.upsert(sources, settings, undefined, undefined, { semantic: false });
  assert.equal(parseCalls, 96);
  await indexer.upsert(sources, settings, undefined, undefined, { semantic: false });
  assert.equal(parseCalls, 96, 'second full-scope pass does not thrash a 64-entry derived-text LRU');
  assert.equal(publications, 96);
  sources[2] = { ...sources[2], sourceRevision: 2 };
  await indexer.upsert(sources, settings, undefined, undefined, { semantic: false });
  assert.equal(parseCalls, 97, 'a source revision is still inspected without requiring new raw text');
  sources[2] = { ...sources[2], parserVersion: 'explicit-source-parser-change' };
  await indexer.upsert(sources, settings, undefined, undefined, { semantic: false });
  assert.equal(parseCalls, 98);
  version = 'synthetic-document-units-v2';
  await indexer.upsert(sources, settings, undefined, undefined, { semantic: false });
  assert.equal(parseCalls, 194, 'a declared service version refreshes all derived sources');
  indexer.structures = parser();
  await indexer.upsert(sources, settings, undefined, undefined, { semantic: false });
  assert.equal(parseCalls, 290, 'a replacement parser object cannot inherit another instance preparation cache');
  let actualParser = parser();
  indexer.structures = { version: () => actualParser.version(), identity: () => actualParser,
    parse: (...args) => actualParser.parse(...args) };
  await indexer.upsert(sources, settings, undefined, undefined, { semantic: false });
  assert.equal(parseCalls, 386);
  actualParser = parser();
  await indexer.upsert(sources, settings, undefined, undefined, { semantic: false });
  assert.equal(parseCalls, 482, 'a stable adapter still tracks its actual parser instance');
  await indexer.upsert(sources, settings, undefined, undefined, { semantic: false });
  assert.equal(parseCalls, 482);
  assert.equal(indexer.fingerprints.size, sources.length);
  for (const entry of indexer.fingerprints.values()) {
    assert.equal(entry.preparation.text, undefined);
    assert.equal(entry.preparation.chunks, undefined);
  }
});

test('semantic preparation reuse checks the current model and source revision before skipping parsing', async () => {
  let source = { sourceId: 'model-preparation-fixture', scopeKey: 'user', sourceType: 'knowledge', sourceRevision: 1,
    title: 'Notes', text: '# Section\nPublic evidence.\n', locator: { relativePath: 'notes.md' } };
  const settings = { local: { semantic: 'auto', embeddingProfileId: 'builtin-multilingual' } };
  let parseCalls = 0, embeddingCalls = 0;
  const metadata = { state: 'ready', modelVersion: 'synthetic-model-v1', dimensions: 2,
    inputProjectionVersion: 'synthetic-projection-v1', embeddingSpaceId: hashText('preparation-space-v1') };
  const indexer = new SourceIndexer({ structures: { derivationVersion: 'synthetic-parser-v1',
    parse: async input => { parseCalls++; return preparedDocument(input); } },
  embeddings: { status: () => metadata, embedDocuments: async texts => {
    embeddingCalls++; return { ...metadata, vectors: texts.map(() => Array(metadata.dimensions).fill(1)) };
  } }, library: { readSource: async () => ({ ...source, contentHash: hashText(source.text) }) },
  effectiveSettings: async () => settings, serialize: operation => operation(),
  index: { upsertSources: async sources => ({ sources: sources.map(item => ({ sourceId: item.sourceId })) }) } });
  await indexer.upsert([source], settings);
  const cached = await indexer.upsert([source], settings);
  assert.equal(parseCalls, 1);
  assert.equal(embeddingCalls, 1);
  assert.equal(cached.semantic.cachedChunks, cached.semantic.totalChunks);
  metadata.modelVersion = 'synthetic-model-v2';
  await indexer.upsert([source], settings);
  assert.equal(parseCalls, 2);
  assert.equal(embeddingCalls, 2);
  metadata.inputProjectionVersion = 'synthetic-projection-v2';
  metadata.dimensions = 3;
  await indexer.upsert([source], settings);
  assert.equal(parseCalls, 3);
  assert.equal(embeddingCalls, 3);
  source = { ...source, sourceRevision: 2 };
  await indexer.upsert([source], settings);
  assert.equal(parseCalls, 4);
  assert.equal(embeddingCalls, 4);
});

test('unavailable parsing never becomes a permanent preparation hit and undeclared parser versions stay conservative', async () => {
  const source = { sourceId: 'unavailable-preparation-fixture', scopeKey: 'user', sourceType: 'knowledge', sourceRevision: 1,
    title: 'Notes', text: '# Section\nPublic evidence.\n', locator: { relativePath: 'notes.md' } };
  const settings = { local: { semantic: 'off', embeddingProfileId: null } };
  let state = 'unavailable', parseCalls = 0;
  const structures = { derivationVersion: 'synthetic-parser-v1', status: () => ({ state }), parse: async input => {
    parseCalls++;
    if (state === 'unavailable') throw Object.assign(new Error('Synthetic parser unavailable.'), { code: 'STRUCTURE_WORKER_FAILED' });
    return preparedDocument(input);
  } };
  const indexer = new SourceIndexer({ structures, embeddings: { status: () => ({ state: 'unavailable' }) },
    index: { upsertSources: async sources => ({ sources: sources.map(item => ({ sourceId: item.sourceId })) }) } });
  await indexer.upsert([source], settings, undefined, undefined, { semantic: false });
  await indexer.upsert([source], settings, undefined, undefined, { semantic: false });
  assert.equal(parseCalls, 2);
  state = 'idle';
  await indexer.upsert([source], settings, undefined, undefined, { semantic: false });
  await indexer.upsert([source], settings, undefined, undefined, { semantic: false });
  assert.equal(parseCalls, 3, 'a recovered successful parse can then use the lightweight preparation receipt');
  indexer.structures = { parse: structures.parse };
  await indexer.upsert([source], settings, undefined, undefined, { semantic: false });
  await indexer.upsert([source], settings, undefined, undefined, { semantic: false });
  assert.equal(parseCalls, 5, 'an unknown parser derivation version is never assumed stable');
});

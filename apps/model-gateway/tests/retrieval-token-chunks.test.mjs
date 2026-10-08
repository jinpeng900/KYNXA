import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { test } from 'node:test';
import { chunkSource, RetrievalIndex } from '../data/retrieval/index.mjs';
import { hashText, validateSource } from '../data/retrieval/retrieval-contracts.mjs';
import { applyTokenFit, embeddingInputForChunk } from '../data/retrieval/token-chunks.mjs';
import { embeddingProjectionForChunk } from '../data/retrieval/retrieval-text.mjs';
import { deriveSourceVersion } from '../data/retrieval/derivation-version.mjs';
import { SourceIndexer } from '../orchestration/retrieval/source-indexer.mjs';
import { EmbeddingService } from '../models/retrieval/embedding-service.mjs';

const source = validateSource({ sourceId: 'fit-source', scopeKey: 'user', sourceType: 'document',
  title: '分块验证', locator: { relativePath: 'docs/token.md' }, text: '😀 原文第一段。\nSecond source line.\n' });
const documents = [{ tokenCount: 20, segments: [{ start: 0, end: 3, tokenCount: 7 },
  { start: 3, end: source.text.length, tokenCount: 18 }] }];
const receipt = { documents, maxInputTokens: 512, fittingVersion: 'fixture-tokenizer-v1' };

test('token fitting preserves every original character, structured range and actual embedding projection identity', () => {
  const chunks = chunkSource(source), projections = chunks.map(chunk => embeddingProjectionForChunk(source, chunk));
  chunks[0].structure = { domain: 'knowledge', language: 'text', kind: 'paragraph', parseStatus: 'parsed',
    unitStartOffset: 0, unitEndOffset: source.text.length };
  const fitted = applyTokenFit(source, chunks, projections, receipt);
  assert.equal(fitted.map(chunk => chunk.text).join(''), source.text);
  assert.equal(fitted[1].startOffset, 3);
  assert.equal(fitted[1].endLine, 3);
  assert.equal(fitted[1].structure.unitEndOffset, source.text.length);
  assert.equal(embeddingInputForChunk(source, fitted[1]), projections[0].context + fitted[1].text);
  const original = deriveSourceVersion(source, fitted);
  const changed = deriveSourceVersion(source, fitted.map(chunk => ({ ...chunk,
    embeddingProjection: { ...chunk.embeddingProjection, context: '' } })));
  assert.notEqual(original.embeddingInputSignature, changed.embeddingInputSignature);
});

test('invalid tokenizer receipts cannot omit, overlap or split source codepoints or forge a fitting budget', () => {
  const chunks = chunkSource(source), projections = chunks.map(chunk => embeddingProjectionForChunk(source, chunk));
  const invalidRanges = [
    [{ start: 1, end: source.text.length, tokenCount: 8 }],
    [{ start: 0, end: 1, tokenCount: 8 }, { start: 1, end: source.text.length, tokenCount: 8 }],
    [{ start: 0, end: 4, tokenCount: 8 }, { start: 3, end: source.text.length, tokenCount: 8 }],
    [{ start: 0, end: source.text.length - 1, tokenCount: 8 }],
    [{ start: 0, end: source.text.length, tokenCount: 513 }],
  ];
  for (const segments of invalidRanges) assert.throws(() => applyTokenFit(source, chunks, projections,
    { ...receipt, documents: [{ segments }] }), { code: 'INVALID_RETRIEVAL_TOKEN_FIT' });
});

test('fitted projection persists across SQLite restart and lexical refresh without losing vectors or old original text', async t => {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-token-index-'));
  const index = new RetrievalIndex({ root, vectorEnabled: false });
  let restarted;
  t.after(async () => {
    await index.close(); await restarted?.close();
    const suffix = relative(resolve(tmpdir()), root);
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(root, { recursive: true, force: true });
  });
  const chunks = chunkSource(source), projections = chunks.map(chunk => embeddingProjectionForChunk(source, chunk));
  const fitted = applyTokenFit(source, chunks, projections, receipt);
  const input = { ...source, chunks: fitted, vectors: fitted.map(() => [1, 0]), embeddingProfileId: 'fixture',
    embeddingModelVersion: 'v1', embeddingSpaceId: hashText('fixture-space'),
    chunkerVersion: fitted[0].chunkerVersion, embeddingInputVersion: 'fixture-tokenizer-v1' };
  await index.upsertSources([input]);
  const citation = (await index.search({ query: 'Second', scopeKeys: ['user'] })).items[0];
  assert.equal((await index.read({ sourceRef: citation.sourceRef, scopeKeys: ['user'] })).text, source.text);
  await index.close();
  const database = new DatabaseSync(join(root, 'Index', 'retrieval.sqlite'));
  try {
    assert.equal(JSON.parse(database.prepare('SELECT embedding_projection FROM chunks WHERE chunk_index=1').get().embedding_projection).tokenCount, 18);
    database.exec("UPDATE retrieval_metadata SET value='han-bigram-code-v1' WHERE key='tokenizer_version'; UPDATE chunks SET tokenizer_version='han-bigram-code-v1';");
  } finally { database.close(); }
  restarted = new RetrievalIndex({ root, vectorEnabled: false });
  assert.equal((await restarted.status()).vectorChunks, fitted.length);
  const same = await restarted.upsertSources([{ ...input, vectors: undefined }]);
  assert.equal(same.sources[0].unchanged, true);
});

test('indexer uses the same fitted ranges for foreground and background and explicitly retries an oversized context', async () => {
  const projectionsUsed = [], diagnostics = [];
  const embeddings = { status: () => ({ state: 'ready', fittingVersion: 'fixture-tokenizer-v1', maxInputTokens: 512 }),
    fitDocuments: async projections => {
      projectionsUsed.push(structuredClone(projections));
      if (projections[0].context) throw Object.assign(new Error('Context too long.'), {
        code: 'EMBEDDING_CONTEXT_TOO_LONG', details: { index: 0 } });
      return { ...receipt, profileId: 'fixture' };
    } };
  const indexer = new SourceIndexer({ embeddings });
  const result = await indexer.fitSourceChunks(source, chunkSource(source), embeddings.status(), { profileId: 'fixture' }, code => diagnostics.push(code));
  assert.equal(result.chunks.map(chunk => chunk.text).join(''), source.text);
  assert.equal(result.chunks[0].embeddingProjection.context, '');
  assert.ok(result.source.embeddingInputVersion.endsWith('fixture-tokenizer-v1'));
  assert.deepEqual(diagnostics, ['EMBEDDING_CONTEXT_OMITTED']);
  assert.equal(projectionsUsed[0][0].text, projectionsUsed[1][0].text);
});

async function outageFixture(t, initialState = 'ready') {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-token-outage-'));
  const index = new RetrievalIndex({ root, vectorEnabled: false });
  t.after(async () => {
    await index.close();
    const suffix = relative(resolve(tmpdir()), root);
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(root, { recursive: true, force: true });
  });
  let current = validateSource({ ...source, sourceType: 'knowledge', sourceRevision: 1,
    locator: { ...source.locator, documentId: 'fixture' } }), state = initialState, fittingCalls = 0;
  const metadata = { profileId: 'fixture', modelVersion: 'v1', inputProjectionVersion: 'fixture-v1',
    dimensions: 2, embeddingSpaceId: hashText('token-outage-space'), fittingVersion: 'fixture-tokenizer-v1', maxInputTokens: 512 };
  const embeddings = { status: () => ({ ...metadata, state }), fitDocuments: async projections => {
    fittingCalls++;
    return { ...metadata, documents: projections.map(({ text }) => ({ tokenCount: 40,
      segments: [{ start: 0, end: 3, tokenCount: 7 }, { start: 3, end: text.length, tokenCount: 39 }] })) };
  }, embedDocuments: async texts => ({ ...metadata, vectors: texts.map(() => [1, 0]) }) };
  const settings = { local: { semantic: 'auto', embeddingProfileId: 'fixture' } };
  const options = { index, embeddings, serialize: operation => operation(), effectiveSettings: async () => settings,
    library: { readSource: async () => current } };
  return { index, options, settings, input: () => current, state: value => { state = value; },
    fittingCalls: () => fittingCalls,
    reorderLocator: () => { current = { ...current, locator: Object.fromEntries(Object.entries(current.locator).reverse()) }; },
    change: text => { current = validateSource({ ...current, text, contentHash: undefined, sourceRevision: 2 }); } };
}

test('unavailable fitting is never cached as completed and the first ready refresh fits the source', async t => {
  const fixture = await outageFixture(t, 'unavailable'), indexer = new SourceIndexer(fixture.options);
  const unavailable = await indexer.upsert([fixture.input()], fixture.settings, undefined, undefined, { semantic: false });
  assert.ok(unavailable.semantic.diagnosticCodes.includes('EMBEDDING_PROFILE_UNAVAILABLE'));
  assert.equal(indexer.fingerprints.size, 1, 'only a non-prepared fingerprint may exist for the lexical fallback');
  assert.equal(indexer.fingerprints.values().next().value.preparation, undefined);
  fixture.state('ready');
  await indexer.upsert([fixture.input()], fixture.settings, undefined, undefined, { semantic: false });
  assert.equal(fixture.fittingCalls(), 1);
  assert.equal((await fixture.index.listSources({ scopeKeys: ['user'], sourceId: source.sourceId }))[0].chunkCount, 2);
});

test('a cold foreground during a tokenizer outage retains unchanged vectors, then refits after recovery', async t => {
  const fixture = await outageFixture(t), originalIndexer = new SourceIndexer(fixture.options);
  await originalIndexer.upsert([fixture.input()], fixture.settings);
  assert.equal((await fixture.index.status()).vectorChunks, 2);
  fixture.state('error');
  fixture.reorderLocator();
  const coldIndexer = new SourceIndexer(fixture.options);
  const outage = await coldIndexer.upsert([fixture.input()], fixture.settings, undefined, undefined, { semantic: false });
  assert.ok(outage.semantic.diagnosticCodes.includes('EMBEDDING_PROFILE_UNAVAILABLE'));
  assert.equal((await fixture.index.status()).vectorChunks, 2);
  assert.equal(coldIndexer.fingerprints.size, 0, 'preserving an old publication does not pretend a fitting operation completed');
  fixture.state('ready');
  await coldIndexer.upsert([fixture.input()], fixture.settings, undefined, undefined, { semantic: false });
  assert.equal(fixture.fittingCalls(), 2);
  assert.equal((await fixture.index.status()).vectorChunks, 2);
});

test('changed original text still replaces stale vectors during an outage and recovers with fresh embeddings', async t => {
  const fixture = await outageFixture(t), indexer = new SourceIndexer(fixture.options);
  await indexer.upsert([fixture.input()], fixture.settings);
  fixture.state('error');
  fixture.change('😀 已经变化的新资料，必须替换旧版本。\n');
  await indexer.upsert([fixture.input()], fixture.settings, undefined, undefined, { semantic: false });
  assert.equal((await fixture.index.status()).vectorChunks, 0);
  assert.equal((await fixture.index.read({ sourceId: source.sourceId, scopeKeys: ['user'] })).text, fixture.input().text);
  fixture.state('ready');
  const recovered = await indexer.upsert([fixture.input()], fixture.settings);
  assert.equal(recovered.semantic.state, 'complete');
  assert.equal((await fixture.index.status()).vectorChunks, 2);
});

// Exercise the actual tokenizer, inference process, SQLite publication and checkpoint restore with existing offline assets.
// 使用现有离线资产验证真实分词、推理进程、SQLite 发布和检查点恢复，不访问产品资料或下载模型。
test('native fitted publication survives foreground refresh and checkpoint recovery with unchanged original text',
  { skip: process.env.KYNXA_TEST_NATIVE_EMBEDDING_FIT !== '1', timeout: 45000 }, async t => {
    const root = await mkdtemp(join(tmpdir(), 'kynxa-token-publication-'));
    const index = new RetrievalIndex({ root, vectorEnabled: false }), embeddings = new EmbeddingService();
    t.after(async () => {
      await embeddings.close(); await index.close();
      const suffix = relative(resolve(tmpdir()), root);
      assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
      await rm(root, { recursive: true, force: true });
    });
    assert.equal(embeddings.status().state, 'ready', 'Pinned local model assets must already exist.');
    const input = validateSource({ sourceId: 'native-fit-publication', scopeKey: 'user', sourceType: 'knowledge',
      title: '真实发布验收', sourceRevision: 1, locator: { relativePath: 'budget.md' },
      text: '中文说明取消重试权限索引并发异步路径恢复😀🙂𠀋。\n'.repeat(35) });
    const settings = { local: { semantic: 'auto', embeddingProfileId: embeddings.status().profileId } };
    let parseCalls = 0;
    const structures = { version: () => 'fixture-whole-body-v1', parse: async value => {
      parseCalls++;
      return { parserVersion: 'fixture-whole-body-v1', chunks: chunkSource(value, { maxChars: 2000 }) };
    } };
    const options = { index, embeddings, structures, serialize: operation => operation(),
      effectiveSettings: async () => settings, library: { readSource: async () => input } };
    const indexer = new SourceIndexer(options), records = [];
    const initial = await indexer.upsert([input], settings, undefined,
      (_completed, details) => records.push(...(details.checkpointSources ?? [])));
    assert.equal(initial.semantic.state, 'complete');
    assert.ok(initial.semantic.totalChunks > 1, 'the original single large body must be token-fitted');
    assert.equal(initial.semantic.vectorChunks, initial.semantic.totalChunks);
    assert.equal((await index.read({ sourceId: input.sourceId, scopeKeys: ['user'] })).text, input.text);
    await indexer.upsert([input], settings, undefined, undefined, { semantic: false });
    assert.equal((await index.status()).vectorChunks, initial.semantic.vectorChunks);
    const cached = await indexer.upsert([input], settings);
    assert.equal(cached.semantic.cachedChunks, initial.semantic.totalChunks);
    const parsedBeforeRestore = parseCalls, restartedIndexer = new SourceIndexer(options);
    await restartedIndexer.restore([input], settings, records);
    const restored = await restartedIndexer.upsert([input], settings);
    assert.equal(restored.semantic.cachedChunks, initial.semantic.totalChunks);
    assert.equal(parseCalls, parsedBeforeRestore, 'valid fitted checkpoints avoid another parse or native embedding');
    assert.ok(records[0].preparationVersion.includes(embeddings.status().fittingVersion));
    assert.equal((await index.status()).vectorChunks, initial.semantic.totalChunks);
    t.diagnostic(`${initial.semantic.totalChunks} fitted source blocks persisted and recovered on ${process.platform} ${process.version}.`);
  });

import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { test } from 'node:test';
import { DatabaseSync } from 'node:sqlite';
import { RetrievalIndex, chunkSource } from '../data/retrieval/index.mjs';
import { hashText, RETRIEVAL_SCHEMA_VERSION, RETRIEVAL_INDEX_SCHEMA_VERSION, sourceReference, validateSource } from '../data/retrieval/retrieval-contracts.mjs';
import { deriveSourceVersion } from '../data/retrieval/derivation-version.mjs';

async function fixture(t, options = {}) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-retrieval-derivation-'));
  const index = new RetrievalIndex({ root, ...options });
  t.after(async () => {
    await index.close().catch(() => {});
    const suffix = relative(resolve(tmpdir()), resolve(root));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(root, { recursive: true, force: true });
  });
  return { root, index };
}

function document(extra = {}) {
  return { sourceId: 'stable-document', scopeKey: 'project:one', sourceRevision: 1,
    title: 'Indexed source', sourceType: 'document', locator: { relativePath: 'notes/design.md' },
    text: '# Design\n' + 'durable retrieval evidence alpha beta gamma. '.repeat(28), ...extra };
}

function chunkVectorSource(source, { maxChars = 256, ...extra } = {}) {
  const chunks = chunkSource(source, { maxChars });
  return { ...source, chunks, vectors: chunks.map(() => [1, 0]), embeddingProfileId: 'fixture-profile',
    embeddingModelVersion: 'weights-v1', embeddingSpaceId: hashText('fixture-space-v1'), ...extra };
}

async function indexedRows(root) {
  const database = new DatabaseSync(join(root, 'Index', 'retrieval.sqlite'), { readOnly: true });
  try { return database.prepare('SELECT * FROM chunks ORDER BY chunk_index').all(); }
  finally { database.close(); }
}

test('same text and revision with changed chunk boundaries rederives, invalidates vectors and old citations', async t => {
  const { index } = await fixture(t);
  const original = chunkVectorSource(document());
  const first = await index.upsertSources([original]);
  const citation = (await index.search({ query: 'retrieval', scopeKeys: ['project:one'] })).items[0];
  const changedChunks = chunkSource(original, { maxChars: 96 });
  assert.notEqual(changedChunks.length, original.chunks.length);
  const updated = await index.upsertSources([{ ...original, chunks: changedChunks, vectors: undefined }]);
  assert.ok(updated.generation > first.generation);
  assert.equal((await index.status()).chunks, changedChunks.length);
  assert.equal((await index.status()).vectorChunks, 0);
  await assert.rejects(index.read({ sourceRef: citation.sourceRef, scopeKeys: ['project:one'] }), { code: 'STALE_RETRIEVAL_SOURCE' });
  assert.ok((await index.search({ query: 'retrieval', scopeKeys: ['project:one'] })).items.length > 0);
});

test('identical derivation retains vectors; lexical-only version updates keep compatible vector bytes', async t => {
  const { root, index } = await fixture(t);
  const original = chunkVectorSource(document());
  const first = await index.upsertSources([original]);
  const before = await indexedRows(root);
  const same = await index.upsertSources([{ ...original, vectors: undefined }]);
  assert.equal(same.generation, first.generation);
  assert.equal(same.sources[0].unchanged, true);
  const changed = await index.upsertSources([{ ...original, vectors: undefined, tokenizerVersion: 'lexical-policy-v3',
    chunks: original.chunks.map(chunk => ({ ...chunk, tokenizerVersion: 'lexical-policy-v3' })) }]);
  assert.ok(changed.generation > first.generation);
  const after = await indexedRows(root);
  assert.equal(after.length, before.length);
  assert.ok(after.every((row, index) => row.tokenizer_version === 'lexical-policy-v3' &&
    Buffer.from(row.vector).equals(Buffer.from(before[index].vector)) && row.embedding_space_id === hashText('fixture-space-v1')));
  assert.equal((await index.status()).vectorChunks, before.length);
});

test('derivation changes invalidate vectors while space transitions retain only old compatible tuples', async t => {
  const { index } = await fixture(t);
  for (const [name, extra] of Object.entries({ parser: { parserVersion: 'markdown-parser-v2' },
    chunker: { chunkerVersion: 'structure-lines-v2' }, projection: { embeddingInputVersion: 'source-context-v2' },
    space: { embeddingSpaceId: hashText('fixture-space-v2') }, profile: { embeddingProfileId: 'different-profile' },
    model: { embeddingModelVersion: 'weights-v2' } })) {
    const original = chunkVectorSource(document({ sourceId: `version-${name}` }));
    const first = await index.upsertSources([original]);
    const updated = await index.upsertSources([{ ...original, ...extra, vectors: undefined }]);
    assert.ok(updated.generation > first.generation, name);
    const result = await index.search({ query: 'unmatchedterm', scopeKeys: ['project:one'], queryVector: [1, 0],
      embeddingProfileId: original.embeddingProfileId, embeddingModelVersion: original.embeddingModelVersion,
      embeddingSpaceId: original.embeddingSpaceId });
    if (['parser', 'chunker', 'projection', 'model'].includes(name))
      assert.ok(!result.items.some(item => item.sourceId === original.sourceId), name);
    else {
      // An unchanged embedding input may keep the old tuple for migration fallback, never for the unpublished new tuple.
      // 嵌入输入未变时可为迁移回退保留旧元组，不能把旧向量借给尚未发布的新配置元组。
      assert.ok(result.items.some(item => item.sourceId === original.sourceId), `${name}: old compatible tuple stays readable`);
      const unpublished = await index.search({ query: 'unmatchedterm', scopeKeys: ['project:one'], queryVector: [1, 0],
        embeddingProfileId: extra.embeddingProfileId ?? original.embeddingProfileId,
        embeddingModelVersion: extra.embeddingModelVersion ?? original.embeddingModelVersion,
        embeddingSpaceId: extra.embeddingSpaceId ?? original.embeddingSpaceId });
      assert.ok(!unpublished.items.some(item => item.sourceId === original.sourceId), `${name}: new tuple cannot borrow old vectors`);
    }
  }
  assert.equal((await index.status()).vectorChunks, 0);
});

test('exact embedding space queries exclude untagged legacy vectors and other spaces', async t => {
  const { index } = await fixture(t);
  const tagged = chunkVectorSource(document({ sourceId: 'tagged' }));
  const untagged = chunkVectorSource(document({ sourceId: 'legacy' }), { embeddingSpaceId: undefined });
  const other = chunkVectorSource(document({ sourceId: 'other-space', scopeKey: 'project:two' }), { embeddingSpaceId: hashText('fixture-space-v2') });
  await index.upsertSources([tagged, untagged, other]);
  const result = await index.search({ query: 'unmatchedterm', scopeKeys: ['project:one'], queryVector: [1, 0],
    embeddingProfileId: 'fixture-profile', embeddingSpaceId: hashText('fixture-space-v1'), limit: 60 });
  assert.ok(result.items.length > 0);
  assert.deepEqual([...new Set(result.items.map(item => item.sourceId))], ['tagged']);
});

test('malformed identities, order, lines, versions and float32 vectors fail without publishing rows', async t => {
  const { index } = await fixture(t);
  const original = chunkVectorSource(document());
  const first = await index.upsertSources([original]);
  const invalid = [
    { chunks: original.chunks.map((chunk, index) => index === 1 ? { ...chunk, chunkId: original.chunks[0].chunkId } : chunk) },
    { chunks: original.chunks.map((chunk, index) => index === 1 ? { ...chunk, chunkIndex: 0 } : chunk) },
    { chunks: original.chunks.map((chunk, index) => index === 0 ? { ...chunk, startLine: chunk.startLine + 1 } : chunk) },
    { chunks: original.chunks.map((chunk, index) => index === 0 ? { ...chunk, chunkerVersion: '' } : chunk) },
    { chunks: original.chunks.map((chunk, index) => index === 0 ? { ...chunk, chunkId: 'invalid\nidentifier' } : chunk) },
    { parserVersion: 'invalid\nversion' },
    { embeddingSpaceId: '' },
    { vectors: original.chunks.map(() => [Number.MAX_VALUE, 0]) }
  ];
  for (const extra of invalid) {
    await assert.rejects(index.upsertSources([{ ...original, ...extra }]));
    assert.equal((await index.status()).generation, first.generation);
    assert.equal((await index.status()).chunks, original.chunks.length);
  }
  await assert.rejects(index.upsertSources([original, original]));
  assert.equal((await index.read({ sourceRef: first.sources[0].sourceRef, scopeKeys: ['project:one'] })).text, original.text);
});

test('failed batch rolls back updated chunks, generation, FTS and existing canonical references', async t => {
  const { index } = await fixture(t);
  const original = chunkVectorSource(document());
  const first = await index.upsertSources([original]);
  const collision = chunkVectorSource(document({ sourceId: 'collision' }));
  const updated = { ...original, sourceRevision: 2, text: original.text + ' new published state' };
  updated.chunks = chunkSource(updated, { maxChars: 256 });
  const bad = { ...collision, chunks: collision.chunks.map((chunk, index) => index === 0
    ? { ...chunk, chunkId: updated.chunks[0].chunkId } : chunk) };
  await assert.rejects(index.upsertSources([{ ...updated, vectors: undefined }, bad]));
  assert.equal((await index.status()).generation, first.generation);
  assert.equal((await index.status()).sources, 1);
  assert.equal((await index.read({ sourceRef: first.sources[0].sourceRef, scopeKeys: ['project:one'] })).text, original.text);
  assert.equal((await index.search({ query: 'new published state', scopeKeys: ['project:one'] })).items.length, 0);
});

test('additive legacy schema migration retains identities, vectors, epochs, tombstones and old references', async t => {
  const { root, index } = await fixture(t);
  const original = chunkVectorSource(document());
  const revoked = document({ sourceId: 'revoked', text: 'revoked original document' });
  await index.upsertSources([original, revoked]);
  await index.removeSource('revoked', { scopeKeys: ['project:one'] });
  const snapshot = await index.status();
  const oldReference = sourceReference(validateSource(original));
  const registry = await readFile(join(root, 'Retrieval', 'source-identities.json'), 'utf8');
  await index.close();
  const database = new DatabaseSync(join(root, 'Index', 'retrieval.sqlite'));
  database.exec("DROP INDEX chunks_space; ALTER TABLE sources DROP COLUMN derivation_signature; ALTER TABLE sources DROP COLUMN embedding_input_signature;\n" +
    "ALTER TABLE sources DROP COLUMN parser_version; ALTER TABLE sources DROP COLUMN embedding_input_version;\n" +
    "ALTER TABLE sources DROP COLUMN chunker_version; ALTER TABLE sources DROP COLUMN tokenizer_version;\n" +
    "ALTER TABLE chunks DROP COLUMN embedding_space_id; PRAGMA user_version = 1;");
  database.close();
  const reopened = new RetrievalIndex({ root });
  try {
    const status = await reopened.status();
    assert.equal(status.schemaVersion, RETRIEVAL_INDEX_SCHEMA_VERSION);
    assert.equal(status.indexSnapshotId, snapshot.indexSnapshotId);
    assert.equal(status.vectorChunks, original.chunks.length);
    assert.equal((await reopened.read({ sourceRef: oldReference, scopeKeys: ['project:one'] })).text, original.text);
    assert.equal(await readFile(join(root, 'Retrieval', 'source-identities.json'), 'utf8'), registry);
    await assert.rejects(reopened.upsertSources([revoked]), { code: 'RETRIEVAL_SOURCE_REVOKED' });
    const rederived = await reopened.upsertSources([{ ...original, vectors: undefined }]);
    assert.ok(rederived.generation > snapshot.generation);
    assert.equal((await reopened.status()).vectorChunks, 0);
  } finally { await reopened.close(); }
  assert.equal(RETRIEVAL_SCHEMA_VERSION, 1);
});

test('known lexical migration keeps durable signatures aligned with migrated chunk versions', async t => {
  const { root, index } = await fixture(t);
  const original = chunkVectorSource(document());
  const oldTokenizer = 'han-bigram-code-v1';
  original.tokenizerVersion = oldTokenizer;
  original.chunks = original.chunks.map(chunk => ({ ...chunk, tokenizerVersion: oldTokenizer }));
  const initial = await index.upsertSources([original]);
  await index.close();
  const reopened = new RetrievalIndex({ root });
  try {
    const listed = (await reopened.listSources({ scopeKeys: ['project:one'] }))[0];
    const rows = await indexedRows(root);
    const migratedChunks = rows.map(chunk => ({ chunkId: chunk.chunk_id, chunkIndex: chunk.chunk_index,
      chunkHash: chunk.chunk_hash, text: chunk.text, startOffset: chunk.start_offset, endOffset: chunk.end_offset,
      startLine: chunk.start_line, endLine: chunk.end_line, chunkerVersion: chunk.chunker_version,
      tokenizerVersion: chunk.tokenizer_version }));
    const migrated = deriveSourceVersion(validateSource({ ...original, ...listed }), migratedChunks);
    assert.equal(listed.derivationSignature, migrated.derivationSignature);
    assert.equal((await reopened.status()).vectorChunks, original.chunks.length);
    await assert.rejects(reopened.read({ sourceRef: initial.sources[0].sourceRef, scopeKeys: ['project:one'] }), { code: 'STALE_RETRIEVAL_SOURCE' });
    const current = await reopened.upsertSources([{ ...original, ...listed, chunks: migratedChunks, vectors: undefined }]);
    assert.equal(current.sources[0].unchanged, true);
  } finally { await reopened.close(); }
});

test('metadata-only verification refuses same-text rederivation, missing chunks and revoked evidence', async t => {
  const { index } = await fixture(t);
  const original = chunkVectorSource(document());
  const first = await index.upsertSources([original]);
  const citation = (await index.search({ query: 'durable retrieval', scopeKeys: ['project:one'] })).items[0];
  assert.equal((await index.verifyReference({ sourceRef: citation.sourceRef, scopeKeys: ['project:one'] })).current, true);
  const missing = sourceReference(validateSource(original), { chunkId: 'missing-chunk', chunkHash: hashText('missing') });
  assert.deepEqual(await index.verifyReference({ sourceRef: missing, scopeKeys: ['project:one'] }),
    { current: false, reason: 'STALE_RETRIEVAL_SOURCE' });
  await assert.rejects(index.verifyReference({ sourceRef: citation.sourceRef, scopeKeys: ['project:two'] }), { code: 'RETRIEVAL_SOURCE_NOT_FOUND' });
  const legacy = sourceReference(validateSource(original));
  await index.upsertSources([{ ...original, parserVersion: 'parser-v2', vectors: undefined }]);
  assert.deepEqual(await index.verifyReference({ sourceRef: first.sources[0].sourceRef, scopeKeys: ['project:one'] }),
    { current: false, reason: 'STALE_RETRIEVAL_SOURCE' });
  assert.equal((await index.verifyReference({ sourceRef: legacy, scopeKeys: ['project:one'] })).current, true);
  const current = (await index.search({ query: 'durable retrieval', scopeKeys: ['project:one'] })).items[0];
  await index.removeSource(original.sourceId, { scopeKeys: ['project:one'] });
  assert.deepEqual(await index.verifyReference({ sourceRef: current.sourceRef, scopeKeys: ['project:one'] }),
    { current: false, reason: 'RETRIEVAL_SOURCE_NOT_FOUND' });
});

test('derivation signatures ignore metadata key ordering, distinguish projection and check cancellation', () => {
  const first = validateSource(document({ locator: { relativePath: 'notes/design.md', metadata: { language: 'en', section: 1 } } }));
  const second = validateSource({ ...first, locator: { metadata: { section: 1, language: 'en' }, relativePath: 'notes/design.md' } });
  const chunks = chunkSource(first);
  assert.equal(deriveSourceVersion(first, chunks).derivationSignature, deriveSourceVersion(second, chunks).derivationSignature);
  assert.notEqual(deriveSourceVersion(first, chunks).embeddingInputSignature,
    deriveSourceVersion({ ...first, embeddingInputVersion: 'source-context-v2' }, chunks).embeddingInputSignature);
  assert.notEqual(deriveSourceVersion(first, chunks).derivationSignature,
    deriveSourceVersion(first, chunks, { embeddingMaxCharacters: 128 }).derivationSignature);
  assert.throws(() => deriveSourceVersion(first, chunks, { checkCancelled() {
    throw Object.assign(new Error('cancelled'), { name: 'AbortError' });
  } }), { name: 'AbortError' });
});

test('in-flight cancellation rejects uncommitted preparation and leaves searchable rows unchanged', async t => {
  const { index } = await fixture(t, { vectorEnabled: false });
  const original = document({ text: 'original committed source' });
  const first = await index.upsertSources([original]);
  const controller = new AbortController();
  const large = document({ sourceId: 'cancelled-large', text: ('# Heading\n' + 'cancel candidate evidence. '.repeat(12)).repeat(4800) });
  assert.ok(large.text.length < 2 * 1024 * 1024);
  const publication = index.upsertSources([large], { signal: controller.signal });
  const timer = setTimeout(() => controller.abort(), 2);
  try { await assert.rejects(publication, { name: 'AbortError' }); }
  finally { clearTimeout(timer); }
  assert.equal((await index.status()).generation, first.generation);
  assert.equal((await index.status()).sources, 1);
  assert.equal((await index.read({ sourceRef: first.sources[0].sourceRef, scopeKeys: ['project:one'] })).contentHash, hashText(original.text));
});

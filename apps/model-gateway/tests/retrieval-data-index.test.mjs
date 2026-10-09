import assert from 'node:assert/strict';
import { mkdtemp, readFile, rename, rm, stat, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { test } from 'node:test';
import { DatabaseSync } from 'node:sqlite';
import { RetrievalIndex, chunkSource } from '../data/retrieval/index.mjs';
import { hashText } from '../data/retrieval/retrieval-contracts.mjs';
import { EMBEDDING_TEXT_VERSION, TOKENIZER_VERSION, embeddingTextForChunk, lexicalTerms, lexicalText, matchExpression } from '../data/retrieval/retrieval-text.mjs';

async function cleanup(root) {
  const suffix = relative(resolve(tmpdir()), resolve(root));
  assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
  await rm(root, { recursive: true, force: true });
}

async function fixture(t, options = {}) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-retrieval-index-'));
  const index = new RetrievalIndex({ root, ...options });
  t.after(async () => { await index.close().catch(() => {}); await cleanup(root); });
  return { root, index };
}

const source = (sourceId, scopeKey, text, extra = {}) => ({ sourceId, scopeKey, text,
  sourceRevision: 1, title: sourceId, sourceType: 'document', locator: { relativePath: `${sourceId}.md` }, ...extra });

test('Chinese bigrams and code identifiers retrieve bounded original chunks with safe FTS syntax', async t => {
  const { index } = await fixture(t);
  const document = source('notes', 'project:a', '# 项目设计\n我们设计项目记忆系统。\n\n' + '其他无关段落。'.repeat(120));
  const code = source('code', 'project:a', 'export function readProjectMemory() { return memoryEntries; } // SQLite索引');
  await index.upsertSources([document, code]);
  const chinese = await index.search({ query: '记忆', scopeKeys: ['project:a'] });
  assert.ok(chinese.items.some(item => item.sourceId === 'notes' && item.excerpt.includes('记忆')));
  assert.ok(chinese.items.every(item => item.excerpt.length <= 385 && item.sourceRef.startsWith('rag1:')));
  assert.ok(chinese.items[0].locator.startLine >= 1);
  const identifier = await index.search({ query: 'readProjectMemory', scopeKeys: ['project:a'] });
  assert.equal(identifier.items[0].sourceId, 'code');
  assert.equal((await index.search({ query: '索引', scopeKeys: ['project:a'] })).items[0].sourceId, 'code');
  assert.ok((await index.search({ query: '" OR memory:*', scopeKeys: ['project:a'] })).items.length > 0);
  assert.equal((await index.read({ sourceRef: chinese.items[0].sourceRef, scopeKeys: ['project:a'], limit: 10 })).text.length, 10);
});

test('lexical and vector channels pre-filter scope and reject unauthorized read or deletion', async t => {
  const { index } = await fixture(t);
  await index.upsertSources([
    source('allowed', 'project:a', '本项目合同资料', { embeddingProfileId: 'fixture', vectors: [[0, 1]] }),
    source('forbidden', 'project:b', '本项目合同资料另一个项目秘密', { embeddingProfileId: 'fixture', vectors: [[1, 0]] })
  ]);
  const result = await index.search({ query: '合同资料', scopeKeys: ['project:a'], queryVector: [1, 0], embeddingProfileId: 'fixture' });
  assert.deepEqual(result.items.map(item => item.sourceId), ['allowed']);
  assert.equal(result.strategy, 'hybrid');
  await assert.rejects(() => index.read({ sourceId: 'forbidden', scopeKeys: ['project:a'] }), { code: 'RETRIEVAL_SOURCE_NOT_FOUND' });
  assert.equal((await index.removeSource('forbidden', { scopeKeys: ['project:a'] })).removed, false);
  assert.equal((await index.read({ sourceId: 'forbidden', scopeKeys: ['project:b'] })).text, '本项目合同资料另一个项目秘密');
  assert.throws(() => index.search({ query: '资料', scopeKeys: [] }), { code: 'RETRIEVAL_SCOPE_REQUIRED' });
  const mismatch = await index.search({ query: '找不到的字词', scopeKeys: ['project:a'], queryVector: [1, 0], embeddingProfileId: 'other' });
  assert.equal(mismatch.strategy, 'lexical');
  assert.equal(mismatch.items.length, 0);
});

test('idempotent refresh preserves vectors, updates generation, and refuses stale revisions', async t => {
  const { index } = await fixture(t);
  const first = source('stable', 'user', '保存正式记忆', { embeddingProfileId: 'fixture', vectors: [[1, 0]] });
  const published = await index.upsertSources([first]);
  const original = (await index.search({ query: '正式记忆', scopeKeys: ['user'] })).items[0];
  assert.equal((await index.upsertSources([first])).generation, published.generation);
  assert.equal((await index.upsertSources([{ ...first, vectors: undefined }])).generation, published.generation);
  assert.equal((await index.status()).vectorChunks, 1);
  const updated = await index.upsertSources([{ ...first, text: '新的正式记忆', sourceRevision: 2 }]);
  assert.ok(updated.generation > published.generation);
  await assert.rejects(() => index.read({ sourceRef: original.sourceRef, scopeKeys: ['user'] }), { code: 'STALE_RETRIEVAL_SOURCE' });
  await assert.rejects(() => index.upsertSources([first]), { code: 'STALE_RETRIEVAL_SOURCE' });
  await assert.rejects(() => index.upsertSources([{ ...first, sourceRevision: 2 }]), { code: 'RETRIEVAL_SOURCE_CHANGED' });
  assert.equal((await index.read({ sourceId: 'stable', scopeKeys: ['user'] })).text, '新的正式记忆');
  await index.invalidateScope('user');
  assert.equal((await index.search({ query: '正式记忆', scopeKeys: ['user'] })).items.length, 0);
});

test('source references survive index rebuild and Data relocation; close checkpoints WAL', async t => {
  const { root, index } = await fixture(t);
  const document = source('persistent', 'chat:one', '聊天原文中的可回查资料');
  await index.upsertSources([document]);
  const original = (await index.search({ query: '资料', scopeKeys: ['chat:one'] })).items[0];
  const registry = JSON.parse(await readFile(join(root, 'Retrieval', 'source-identities.json'), 'utf8'));
  assert.equal(Object.values(registry.sources)[0].sourceId, document.sourceId);
  assert.equal((await index.close()).checkpointed, true);
  const wal = await stat(join(root, 'Index', 'retrieval.sqlite-wal')).catch(error => error.code === 'ENOENT' ? null : Promise.reject(error));
  assert.ok(!wal || wal.size === 0);
  await rm(join(root, 'Index', 'retrieval.sqlite'));
  const rebuilt = new RetrievalIndex({ root });
  await rebuilt.upsertSources([document]);
  assert.equal((await rebuilt.search({ query: '资料', scopeKeys: ['chat:one'] })).items[0].sourceRef, original.sourceRef);
  await rebuilt.close();
  const relocated = `${root}-moved`;
  await rename(root, relocated);
  t.after(() => cleanup(relocated));
  const moved = new RetrievalIndex({ root: relocated });
  try { assert.equal((await moved.read({ sourceRef: original.sourceRef, scopeKeys: ['chat:one'] })).text, document.text); }
  finally { await moved.close(); }
});

test('cheap scoped versions track publication and rebuild without including unrelated scope metadata', async t => {
  const { root, index } = await fixture(t, { vectorEnabled: false });
  const initial = await index.scopeVersion({ scopeKeys: ['user', 'project:one'] });
  assert.ok(initial.indexEpoch);
  assert.deepEqual(initial.scopes, [{ scopeKey: 'user', generation: 0, corpusGeneration: 0 },
    { scopeKey: 'project:one', generation: 0, corpusGeneration: 0 }]);
  await index.upsertSources([source('scoped-version', 'project:one', 'project corpus')]);
  const published = await index.scopeVersion({ scopeKeys: ['user', 'project:one'] });
  assert.equal(published.indexEpoch, initial.indexEpoch);
  assert.ok(published.scopes[1].generation > 0);
  await index.upsertSources([source('other-version', 'project:other', 'separate corpus')]);
  assert.deepEqual(await index.scopeVersion({ scopeKeys: ['user', 'project:one'] }), published);
  await index.removeSource('scoped-version', { scopeKeys: ['project:one'] });
  assert.ok((await index.scopeVersion({ scopeKeys: ['project:one'] })).scopes[0].generation > published.scopes[1].generation);
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(() => index.scopeVersion({ scopeKeys: ['user'], signal: controller.signal }), { name: 'AbortError' });
  assert.throws(() => index.scopeVersion({ scopeKeys: [] }), { code: 'RETRIEVAL_SCOPE_REQUIRED' });
  await index.close();
  await rm(join(root, 'Index', 'retrieval.sqlite'));
  const rebuilt = new RetrievalIndex({ root, vectorEnabled: false });
  try {
    const fresh = await rebuilt.scopeVersion({ scopeKeys: ['user', 'project:one'] });
    assert.notEqual(fresh.indexEpoch, initial.indexEpoch);
    assert.deepEqual(fresh.scopes, initial.scopes);
  } finally { await rebuilt.close(); }
});

test('corpus generations preserve body caches across vector and conversation publication but track real derivations', async t => {
  const { root, index } = await fixture(t);
  const knowledge = source('corpus-body', 'project:one', 'stable evidence', { sourceType: 'knowledge' });
  const mounted = source('mounted-body', 'project:one', 'mounted evidence', { sourceType: 'work-file' });
  await index.upsertSources([knowledge, mounted]);
  const version = async () => (await index.scopeVersion({ scopeKeys: ['project:one'] })).scopes[0];
  const initial = await version();
  assert.ok(initial.corpusGeneration > 0);
  // Genuine vector attachment changes the retrieval graph while preserving the source-body projection.
  // 真实追加向量改变检索图版本，但不改变原文投影或其缓存代次。
  await index.upsertSources([{ ...knowledge, embeddingProfileId: 'fixture', vectors: [[1, 0]] }]);
  const vectorAttached = await version();
  assert.ok(vectorAttached.generation > initial.generation);
  assert.equal(vectorAttached.corpusGeneration, initial.corpusGeneration);
  assert.equal((await index.listSources({ scopeKeys: ['project:one'] })).find(row => row.sourceId === knowledge.sourceId).vectorChunks, 1);
  await index.upsertSources([source('conversation-memory', 'project:one', 'new memory', { sourceType: 'memory' }),
    source('conversation-message', 'project:one', 'new message', { sourceType: 'message' })]);
  const conversationPublished = await version();
  assert.ok(conversationPublished.generation > vectorAttached.generation);
  assert.equal(conversationPublished.corpusGeneration, initial.corpusGeneration);
  await index.removeSource('conversation-memory', { scopeKeys: ['project:one'] });
  assert.equal((await version()).corpusGeneration, initial.corpusGeneration);
  const projection = { ...knowledge, parserVersion: 'plain-text-v2' };
  await index.upsertSources([projection]);
  const rederived = await version();
  assert.ok(rederived.corpusGeneration > conversationPublished.corpusGeneration);
  await index.upsertSources([{ ...projection, title: 'updated source title' }]);
  const retitled = await version();
  assert.ok(retitled.corpusGeneration > rederived.corpusGeneration);
  await index.upsertSources([{ ...projection, title: 'updated source title', text: 'changed evidence', sourceRevision: 2 }]);
  const changed = await version();
  assert.ok(changed.corpusGeneration > retitled.corpusGeneration);
  await index.removeSource(mounted.sourceId, { scopeKeys: ['project:one'] });
  const removed = await version();
  assert.ok(removed.corpusGeneration > changed.corpusGeneration);
  await index.close();
  const reopened = new RetrievalIndex({ root });
  try {
    assert.deepEqual((await reopened.scopeVersion({ scopeKeys: ['project:one'] })).scopes[0], removed);
    await reopened.invalidateScope('project:one');
    assert.ok((await reopened.scopeVersion({ scopeKeys: ['project:one'] })).scopes[0].corpusGeneration > removed.corpusGeneration);
  } finally { await reopened.close(); }
});

test('existing corpus generation metadata migrates conservatively and lexical rebuild advances only corpus scopes', async t => {
  const { root, index } = await fixture(t, { vectorEnabled: false });
  await index.upsertSources([source('legacy-corpus', 'project:corpus', 'repeated repeated', { sourceType: 'knowledge' }),
    source('legacy-memory', 'project:memory', 'repeated memory', { sourceType: 'memory' })]);
  const before = await index.scopeVersion({ scopeKeys: ['project:corpus', 'project:memory'] });
  await index.close();
  const database = new DatabaseSync(join(root, 'Index', 'retrieval.sqlite'));
  database.prepare("DELETE FROM retrieval_metadata WHERE key=?").run('corpus_generation:project:corpus');
  database.prepare('UPDATE chunks SET tokenizer_version=?').run('han-bigram-code-v1');
  database.close();
  const migrated = new RetrievalIndex({ root, vectorEnabled: false });
  try {
    const after = await migrated.scopeVersion({ scopeKeys: ['project:corpus', 'project:memory'] });
    assert.equal(after.indexEpoch, before.indexEpoch);
    assert.ok(after.scopes[0].generation > before.scopes[0].generation);
    assert.ok(after.scopes[0].corpusGeneration > before.scopes[0].generation);
    assert.ok(after.scopes[1].generation > before.scopes[1].generation);
    assert.equal(after.scopes[1].corpusGeneration, 0);
  } finally { await migrated.close(); }
});

test('missing vector extension provides lexical fallback and does not mix dimensions or invalid chunks', async t => {
  const { index } = await fixture(t, { vectorEnabled: false });
  await index.upsertSources([source('lexical', 'user', '中文检索仍然可用', { embeddingProfileId: 'fixture', vectors: [[1, 0]] })]);
  const result = await index.search({ query: '检索', scopeKeys: ['user'], queryVector: [1, 0], embeddingProfileId: 'fixture' });
  assert.equal(result.strategy, 'lexical');
  assert.equal(result.degradedReason, 'RETRIEVAL_VECTOR_DISABLED');
  assert.equal(result.items[0].sourceId, 'lexical');
  const malformed = source('invalid', 'user', '原文资料');
  const chunks = chunkSource(malformed);
  chunks[0].text = '篡改后的分块';
  await assert.rejects(() => index.upsertSources([{ ...malformed, chunks }]), { code: 'RETRIEVAL_SOURCE_CHANGED' });
  await assert.rejects(() => index.upsertSources([source('invalid', 'user', '原文资料', { contentHash: hashText('其他资料') })]), { code: 'RETRIEVAL_SOURCE_CHANGED' });
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(() => index.upsertSources([source('cancelled', 'user', '不应写入')], { signal: controller.signal }), { name: 'AbortError' });
  assert.equal((await index.status()).sources, 1);
});

test('unknown future SQLite schema is preserved and unsafe junctions are refused', async t => {
  const { root, index } = await fixture(t);
  await index.status();
  await index.close();
  const filename = join(root, 'Index', 'retrieval.sqlite');
  const database = new DatabaseSync(filename);
  database.exec('PRAGMA user_version = 99');
  database.close();
  const future = new RetrievalIndex({ root });
  await assert.rejects(() => future.status(), { code: 'UNSUPPORTED_RETRIEVAL_INDEX_VERSION' });
  await future.close().catch(() => {});
  const verification = new DatabaseSync(filename, { readOnly: true });
  assert.equal(verification.prepare('PRAGMA user_version').get().user_version, 99);
  verification.close();
  const unsafe = await mkdtemp(join(tmpdir(), 'kynxa-retrieval-junction-'));
  t.after(() => cleanup(unsafe));
  try { await symlink(join(root, 'Index'), join(unsafe, 'Index'), process.platform === 'win32' ? 'junction' : 'dir'); }
  catch (error) { if (error.code === 'EPERM') return; throw error; }
  const linked = new RetrievalIndex({ root: unsafe });
  await assert.rejects(() => linked.status(), { code: 'UNSAFE_RETRIEVAL_PATH' });
  await linked.close().catch(() => {});
});

test('chunking preserves Unicode and exact offsets with a linear line-number mapping', () => {
  const document = source('unicode', 'user', '前言\n\n# 资料\n' + ('😀中文代码 camelCase_value\n'.repeat(1000)));
  const chunks = chunkSource(document, { maxChars: 384 });
  assert.ok(chunks.length > 20);
  for (const chunk of chunks) {
    assert.equal(document.text.slice(chunk.startOffset, chunk.endOffset), chunk.text);
    assert.ok(!/^[\uDC00-\uDFFF]/.test(chunk.text) && !/[\uD800-\uDBFF]$/.test(chunk.text));
    assert.equal(chunk.startLine, 1 + (document.text.slice(0, chunk.startOffset).match(/\n/g)?.length ?? 0));
    assert.equal(chunk.endLine, 1 + (document.text.slice(0, chunk.endOffset).match(/\n/g)?.length ?? 0));
  }
});

test('explicit tombstones reject late publication across rebuild while temporary pruning is reversible', async t => {
  const { root, index } = await fixture(t);
  const document = source('revoked', 'project:a', '用户已撤销的资料');
  const temporary = source('temporary', 'project:a', '可以重新建立的索引');
  await index.upsertSources([document, temporary]);
  assert.equal((await index.listSources({ scopeKeys: ['project:a'], sourceType: 'document' })).length, 2);
  assert.equal((await index.listSources({ scopeKeys: ['project:b'] })).length, 0);
  await index.removeSource(document.sourceId, { scopeKeys: ['project:a'] });
  await assert.rejects(() => index.upsertSources([document]), { code: 'RETRIEVAL_SOURCE_REVOKED' });
  await index.removeSource(temporary.sourceId, { scopeKeys: ['project:a'], permanent: false });
  await index.upsertSources([temporary]);
  const before = await index.status();
  await index.close();
  await rm(join(root, 'Index', 'retrieval.sqlite'));
  const rebuilt = new RetrievalIndex({ root });
  try {
    await assert.rejects(() => rebuilt.upsertSources([document]), { code: 'RETRIEVAL_SOURCE_REVOKED' });
    await rebuilt.upsertSources([temporary]);
    assert.notEqual((await rebuilt.status()).indexSnapshotId, before.indexSnapshotId);
    assert.equal((await rebuilt.search({ query: '索引', scopeKeys: ['project:a'] })).items[0].sourceId, 'temporary');
  } finally { await rebuilt.close(); }
});

test('model versions isolate same-profile vectors and partially embedded chunks retain lexical retrieval', async t => {
  const { index } = await fixture(t);
  const document = source('versioned', 'user', '跨语言向量资料', {
    embeddingProfileId: 'fixture', embeddingModelVersion: 'weights-v1', vectors: [[1, 0]] });
  await index.upsertSources([document]);
  const old = await index.search({ query: '没有词法命中', scopeKeys: ['user'], queryVector: [1, 0],
    embeddingProfileId: 'fixture', embeddingModelVersion: 'weights-v2' });
  assert.equal(old.items.length, 0);
  const current = await index.search({ query: '没有词法命中', scopeKeys: ['user'], queryVector: [1, 0],
    embeddingProfileId: 'fixture', embeddingModelVersion: 'weights-v1' });
  assert.equal(current.items[0].sourceId, 'versioned');
  const partial = source('partial', 'user', '第一段原文。'.repeat(60) + '\n\n第二段包含特有关键词：部分嵌入。');
  const chunks = chunkSource(partial);
  assert.ok(chunks.length > 1);
  await index.upsertSources([{ ...partial, chunks, vectors: chunks.map((_, i) => i === 0 ? [0, 1] : null),
    embeddingProfileId: 'fixture', embeddingModelVersion: 'weights-v1' }]);
  assert.ok((await index.search({ query: '部分嵌入', scopeKeys: ['user'] })).items.some(item => item.sourceId === partial.sourceId));
});

test('non-finite native cosine distances keep lexical evidence and cannot displace normal semantic candidates', async t => {
  const { index } = await fixture(t);
  const tiny = source('tiny-distance', 'user', 'lexical source with extremely small vector', { embeddingProfileId: 'fixture', vectors: [[1e-40, 0]] });
  await index.upsertSources([tiny]);
  const invalid = await index.search({ query: 'lexical', scopeKeys: ['user'], queryVector: [1e-40, 0], embeddingProfileId: 'fixture' });
  assert.equal(invalid.strategy, 'lexical');
  assert.equal(invalid.degradedReason, 'RETRIEVAL_VECTOR_DISTANCE_INVALID');
  assert.equal(invalid.items[0].sourceId, tiny.sourceId);
  assert.ok(invalid.items.every(item => item.vectorRank === undefined && item.distance === undefined));
  const invalidRows = Array.from({ length: 45 }, (_, index) => source(`invalid-distance-${index}`, 'user', 'unrelated stored text',
    { embeddingProfileId: 'fixture', vectors: [[1e-40, 0]] }));
  const normal = source('normal-distance', 'user', 'normal semantic source', { embeddingProfileId: 'fixture', vectors: [[1, 0]] });
  await index.upsertSources([...invalidRows, normal]);
  const valid = await index.search({ query: 'unmatchedterm', scopeKeys: ['user'], queryVector: [1, 0], embeddingProfileId: 'fixture' });
  assert.equal(valid.strategy, 'hybrid');
  assert.equal(valid.degradedReason, 'RETRIEVAL_VECTOR_DISTANCE_INVALID');
  assert.deepEqual(valid.items.map(item => item.sourceId), [normal.sourceId]);
  assert.equal(valid.items[0].vectorRank, 1);
  assert.equal(valid.items[0].distance, 0);
});

test('document lexical text preserves occurrences while bounded query terms remain unique', () => {
  assert.deepEqual(lexicalText('alpha alpha camelCase camelCase').split(' '),
    ['alpha', 'alpha', 'camelcase', 'camel', 'case', 'camelcase', 'camel', 'case']);
  assert.deepEqual(lexicalTerms('alpha '.repeat(100) + 'beta', 2), ['alpha', 'beta']);
  assert.equal(matchExpression('alpha alpha'), matchExpression('alpha'));
  assert.equal(lexicalText('中文中文').split(' ').filter(term => term === '中文').length, 2);
});

test('embedding context uses actual metadata within bounds and preserves citation identity', () => {
  const document = source('context', 'user', '# Opening\n' + 'intro '.repeat(90) + '\n\n## Evidence\n' + '😀supported detail '.repeat(60),
    { title: 'Actual document title', locator: { relativePath: 'papers/result.md' } });
  const chunks = chunkSource(document);
  const chunk = chunks.find(value => value.startOffset > document.text.indexOf('## Evidence') + 20);
  assert.ok(chunk);
  const original = structuredClone(chunk);
  const text = embeddingTextForChunk(document, chunk);
  assert.ok(text.includes('Title: Actual document title'));
  assert.ok(text.includes('Path: papers/result.md'));
  assert.ok(text.includes('Section: Evidence'));
  assert.ok(text.endsWith(chunk.text));
  assert.ok(text.length <= 512);
  const narrow = embeddingTextForChunk(document, chunk, { maxChars: 65 });
  assert.ok(narrow.length <= 65 && !/[\uD800-\uDBFF]$/u.test(narrow));
  assert.deepEqual(chunk, original);
  assert.equal(chunk.chunkHash, hashText(document.text.slice(chunk.startOffset, chunk.endOffset)));
  assert.equal(EMBEDDING_TEXT_VERSION, 'source-context-v1');
});

test('tokenizer migration changes only derived lexical fields and retains vectors, references and tombstones', async t => {
  const { root, index } = await fixture(t);
  const document = source('migration', 'user', 'repeated repeated repeated 本文本文', {
    embeddingProfileId: 'fixture', embeddingModelVersion: 'weights-v1', vectors: [[1, 0]] });
  await index.upsertSources([document, source('deleted', 'user', 'permanently removed')]);
  await index.removeSource('deleted', { scopeKeys: ['user'] });
  const original = (await index.search({ query: 'repeated', scopeKeys: ['user'] })).items[0];
  const originalRegistry = await readFile(join(root, 'Retrieval', 'source-identities.json'), 'utf8');
  const generation = (await index.status()).generation;
  await index.close();
  const filename = join(root, 'Index', 'retrieval.sqlite');
  const old = new DatabaseSync(filename);
  old.prepare('UPDATE chunks SET lexical_text=?,tokenizer_version=?').run(
    lexicalTerms(`${document.title} ${document.locator.relativePath} ${document.text}`).join(' '), 'han-bigram-code-v1');
  const before = old.prepare('SELECT * FROM chunks').get();
  old.close();
  const migrated = new RetrievalIndex({ root });
  try {
    assert.ok((await migrated.status()).generation > generation);
    assert.equal((await migrated.search({ query: 'repeated', scopeKeys: ['user'] })).items[0].sourceRef, original.sourceRef);
    assert.equal((await migrated.read({ sourceRef: original.sourceRef, scopeKeys: ['user'] })).text, document.text);
    await assert.rejects(() => migrated.upsertSources([source('deleted', 'user', 'permanently removed')]), { code: 'RETRIEVAL_SOURCE_REVOKED' });
  } finally { await migrated.close(); }
  assert.equal(await readFile(join(root, 'Retrieval', 'source-identities.json'), 'utf8'), originalRegistry);
  const verification = new DatabaseSync(filename, { readOnly: true });
  try {
    const after = verification.prepare('SELECT * FROM chunks').get();
    assert.equal(after.tokenizer_version, TOKENIZER_VERSION);
    assert.equal(after.lexical_text.split(' ').filter(term => term === 'repeated').length, 3);
    delete before.lexical_text; delete before.tokenizer_version;
    delete after.lexical_text; delete after.tokenizer_version;
    assert.deepEqual(after, before);
  } finally { verification.close(); }
});

test('narrow lexical candidates preserve reference ordering and filter scopes before the candidate limit', async t => {
  const { root, index } = await fixture(t);
  const documents = Array.from({ length: 90 }, (_, position) => source(`candidate-${position}`, position < 60 ? 'project:b' : 'project:a',
    position % 3 === 0 ? 'alpha beta ' + 'alpha '.repeat(position % 7) : 'beta filler alpha filler'));
  await index.upsertSources(documents);
  const database = new DatabaseSync(join(root, 'Index', 'retrieval.sqlite'), { readOnly: true });
  try {
    // An independent FTS corpus is the oracle for scoped BM25, not the polluted shared corpus.
    // 以独立 FTS 语料作为范围 BM25 的参考，不能继续以受无关资料污染的共享统计作为标准。
    database.exec(`CREATE VIRTUAL TABLE temp.authorized_fts USING fts5(lexical_text,tokenize='unicode61');
      INSERT INTO temp.authorized_fts(rowid,lexical_text) SELECT c.id,c.lexical_text FROM chunks c
      JOIN sources s ON s.source_id=c.source_id WHERE s.scope_key='project:a'`);
    const reference = database.prepare(`SELECT c.chunk_id,bm25(authorized_fts) AS lexical_rank FROM authorized_fts
      JOIN chunks c ON c.id=authorized_fts.rowid
      WHERE authorized_fts MATCH ?
      ORDER BY CASE WHEN instr(lower(c.text),lower(?)) > 0 THEN 0 ELSE 1 END,lexical_rank,c.chunk_id LIMIT 40`);
    for (const query of ['alpha beta', 'alpha', '" OR alpha:*']) {
      const expected = reference.all(matchExpression(query), query);
      const actual = await index.search({ query, scopeKeys: ['project:a'], limit: 60 });
      assert.deepEqual(actual.items.map(item => item.chunkId), expected.map(row => row.chunk_id));
      for (const [position, item] of actual.items.entries())
        assert.ok(Math.abs(item.lexicalScore - expected[position].lexical_rank) < 1e-18);
      assert.ok(actual.items.every(item => item.scopeKey === 'project:a'));
      assert.deepEqual(actual.items.map(item => item.lexicalRank), expected.map((_, position) => position + 1));
    }
    const controller = new AbortController();
    const cancelledSearch = index.search({ query: 'alpha', scopeKeys: ['project:a'], signal: controller.signal });
    controller.abort();
    await assert.rejects(cancelledSearch, { name: 'AbortError' });
    assert.equal((await index.search({ query: 'alpha', scopeKeys: ['project:a'], limit: 60 })).items.length, 30);
  } finally { database.close(); }
});

test('unrelated scope publications cannot change authorized BM25 scores or candidate ordering', async t => {
  const { index } = await fixture(t, { vectorEnabled: false });
  await index.upsertSources([
    source('scoped-alpha', 'project:isolated', 'alpha '.repeat(18) + 'beta'),
    source('scoped-beta', 'project:isolated', 'beta '.repeat(18) + 'alpha'),
    source('scoped-filler', 'project:isolated', 'unrelated filler '.repeat(25))
  ]);
  const search = () => index.search({ query: 'alpha OR beta', scopeKeys: ['project:isolated'] });
  const ranked = result => result.items.map(item => [item.chunkId, item.lexicalRank, item.lexicalScore]);
  const original = ranked(await search());
  await index.upsertSources(Array.from({ length: 45 }, (_, position) =>
    source(`unrelated-${position}`, 'project:unrelated', 'alpha '.repeat(40) + 'irrelevant')));
  assert.deepEqual(ranked(await search()), original);
  await index.invalidateScope('project:unrelated');
  assert.deepEqual(ranked(await search()), original);
  // Authorized edits invalidate statistics even when an earlier request populated the cache.
  // 已授权语料编辑仍须使统计缓存失效，不能因之前查询过而保留旧平均长度和词频。
  await index.upsertSources([source('scoped-filler', 'project:isolated', 'beta '.repeat(80), { sourceRevision: 2 })]);
  assert.notDeepEqual(ranked(await search()), original);
});

test('vector publication retains scoped lexical statistics while real conversation text invalidates them', async t => {
  const { root, index } = await fixture(t);
  const document = source('lexical-stats-corpus', 'project:stats', 'Stable evidence original', { sourceType: 'knowledge' });
  await index.upsertSources([document]);
  const database = new DatabaseSync(join(root, 'Index', 'retrieval.sqlite'), { readOnly: true });
  try {
    const lexicalGeneration = () => database.prepare('SELECT value FROM retrieval_metadata WHERE key=?').get('lexical_generation:project:stats').value;
    const original = lexicalGeneration();
    await index.search({ query: 'evidence', scopeKeys: ['project:stats'] });
    await index.upsertSources([{ ...document, embeddingProfileId: 'fixture', vectors: [[1, 0]] }]);
    assert.equal(lexicalGeneration(), original);
    await index.upsertSources([source('lexical-stats-message', 'project:stats', 'New evidence from conversation', { sourceType: 'message' })]);
    assert.notEqual(lexicalGeneration(), original);
    assert.ok((await index.search({ query: 'evidence', scopeKeys: ['project:stats'] })).items.some(item => item.sourceId === 'lexical-stats-message'));
  } finally { database.close(); }
});

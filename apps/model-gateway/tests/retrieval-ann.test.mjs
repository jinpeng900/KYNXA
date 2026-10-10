import assert from 'node:assert/strict';
import { mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { test } from 'node:test';
import { RetrievalIndex, chunkSource } from '../data/retrieval/index.mjs';
import { LocalAnnStore, validateAnnOptions } from '../data/retrieval/ann-store.mjs';
import { RetrievalVectorSearch } from '../data/retrieval/vector-search.mjs';
import { DatabaseSync } from 'node:sqlite';
import { load as loadSqliteVec } from 'sqlite-vec';
import { setTimeout as delay } from 'node:timers/promises';

async function fixture(t, options = {}) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-ann-'));
  const index = new RetrievalIndex({ root, ...options });
  t.after(async () => {
    await index.close().catch(() => {});
    const suffix = relative(resolve(tmpdir()), resolve(root));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(root, { recursive: true, force: true });
  });
  return { root, index };
}

function source(sourceId, vector, extra = {}) {
  return { sourceId, scopeKey: 'project:one', sourceType: 'code', title: sourceId,
    text: `export const ${sourceId.replace(/-/gu, '_')} = true;`, sourceRevision: 1,
    locator: { relativePath: `${sourceId}.mjs` }, embeddingProfileId: 'fixture',
    embeddingModelVersion: 'v1', embeddingSpaceId: 'a'.repeat(64), vectors: [vector], ...extra };
}

const search = (index, extra = {}) => index.search({ query: 'querynotincorpus', scopeKeys: ['project:one'],
  queryVector: [1, 0], embeddingProfileId: 'fixture', embeddingModelVersion: 'v1', embeddingSpaceId: 'a'.repeat(64),
  ann: { mode: 'ann' }, limit: 20, ...extra });

test('local ANN prefilters scope, embedding space, model version and domain before top candidates', async t => {
  const { index } = await fixture(t);
  const forbidden = Array.from({ length: 60 }, (_, number) => source(`private-${number}`, [1, 0], { scopeKey: 'project:other' }));
  await index.upsertSources([...forbidden, source('allowed', [0.8, 0.2]),
    source('wrong-space', [1, 0], { embeddingSpaceId: 'b'.repeat(64) }),
    source('wrong-version', [1, 0], { embeddingModelVersion: 'v2' }),
    source('wrong-domain', [1, 0], { sourceType: 'document' })]);
  const result = await search(index, { retrievalIntent: { domain: 'code' } });
  assert.equal(result.semanticBackend, 'ann');
  assert.deepEqual(result.items.map(item => item.sourceId), ['allowed']);
  assert.equal((await index.status()).ann.built, 1);
  assert.equal(result.degradedReason, undefined);
});

test('ANN mutation updates loaded graphs and formal deletion prevents stale native results', async t => {
  const { index } = await fixture(t);
  const first = source('nearest', [1, 0]), second = source('second', [0, 1]);
  await index.upsertSources([first, second]);
  const initial = await search(index);
  assert.equal(initial.items[0].sourceId, 'nearest');
  const built = (await index.status()).ann.built;
  await index.upsertSources([{ ...first, text: 'export const nearest = false;', sourceRevision: 2, vectors: [[-1, 0]] }]);
  assert.equal((await search(index)).items[0].sourceId, 'second');
  assert.equal((await index.status()).ann.built, built);
  assert.ok((await index.status()).ann.updated >= 1);
  assert.ok((await index.status()).ann.descriptorCache.invalidated >= 1);
  await assert.rejects(() => index.read({ sourceRef: initial.items[0].sourceRef, scopeKeys: ['project:one'] }), { code: 'STALE_RETRIEVAL_SOURCE' });
  await index.removeSource('second', { scopeKeys: ['project:one'] });
  assert.deepEqual((await search(index)).items.map(item => item.sourceId), ['nearest']);
  await index.invalidateScope('project:one');
  assert.equal((await search(index)).items.length, 0);
  assert.equal((await index.status()).ann.cachedShards, 0);
});

test('persisted ANN caches reload only for current epoch, graph hash, space and generation', async t => {
  const { root, index } = await fixture(t);
  await index.upsertSources([source('persistent', [1, 0]), source('other', [0, 1])]);
  await search(index);
  await index.close();
  const reopened = new RetrievalIndex({ root });
  try {
    assert.equal((await search(reopened)).items[0].sourceId, 'persistent');
    assert.equal((await reopened.status()).ann.loaded, 1);
    assert.equal((await reopened.status()).ann.built, 0);
  } finally { await reopened.close(); }
  const files = await readdir(join(root, 'Index', 'ann'));
  const graph = join(root, 'Index', 'ann', files.find(name => name.endsWith('.usearch')));
  const bytes = await readFile(graph);
  bytes[bytes.length - 1] ^= 1;
  await writeFile(graph, bytes);
  const rebuilt = new RetrievalIndex({ root });
  try {
    assert.equal((await search(rebuilt)).items[0].sourceId, 'persistent');
    assert.equal((await rebuilt.status()).ann.loaded, 0);
    assert.equal((await rebuilt.status()).ann.built, 1);
    await rebuilt.upsertSources([source('persistent', [0, 1], { sourceRevision: 2, text: 'changed contents' })]);
  } finally { await rebuilt.close(); }
  const changed = new RetrievalIndex({ root });
  try {
    const result = await search(changed);
    assert.ok(result.items.every(item => item.sourceId !== 'persistent' || item.excerpt === 'changed contents'));
    assert.equal((await changed.status()).ann.loaded, 1);
  } finally { await changed.close(); }
});

test('auto routing stays exact for small shards, activates ANN for large shards and opt-out is honored', async t => {
  const { index } = await fixture(t, { ann: { threshold: 3 } });
  await index.upsertSources([source('one', [1, 0]), source('two', [0, 1])]);
  assert.equal((await search(index, { ann: { mode: 'auto' } })).semanticBackend, 'exact');
  await index.upsertSources([source('three', [0.3, 0.7]), source('four', [-1, 0])]);
  assert.equal((await search(index, { ann: { mode: 'auto' } })).semanticBackend, 'ann');
  assert.equal((await search(index, { ann: { mode: 'off' } })).semanticBackend, 'exact');
  assert.equal((await search(index, { ann: { mode: 'exact' } })).semanticBackend, 'exact');
  const disabled = await fixture(t, { vectorEnabled: false });
  await disabled.index.upsertSources([source('disabled', [1, 0])]);
  const fallback = await search(disabled.index);
  assert.equal(fallback.semanticBackend, null);
  assert.equal(fallback.degradedReason, 'RETRIEVAL_VECTOR_DISABLED');
  assert.equal((await disabled.index.status()).ann.built, 0);
});

test('bounded graph cache evicts owned native shards while unauthorized data never occupies a query graph', async t => {
  const { index } = await fixture(t, { ann: { maxCachedShards: 1 } });
  await index.upsertSources([source('one', [1, 0]), source('two', [0, 1], { scopeKey: 'project:two' })]);
  await search(index, { ann: { mode: 'ann', maxCachedShards: 1 } });
  await search(index, { scopeKeys: ['project:two'], ann: { mode: 'ann', maxCachedShards: 1 } });
  assert.equal((await index.status()).ann.cachedShards, 1);
  assert.equal((await index.status()).ann.cachedVectors, 1);
  assert.deepEqual((await search(index, { ann: { mode: 'ann', maxCachedShards: 1 } })).items.map(item => item.sourceId), ['one']);
  assert.ok((await index.status()).ann.loaded >= 1);
});

test('ordinary cache replacement reaps its actual native process across repeated project switches', async t => {
  const { index } = await fixture(t, { ann: { maxCachedShards: 1 } });
  await index.upsertSources([source('one', [1, 0]), source('two', [0, 1], { scopeKey: 'project:two' })]);
  let previousPid;
  for (const scope of ['project:one', 'project:two', 'project:one', 'project:two']) {
    const result = await search(index, { scopeKeys: [scope], ann: { mode: 'ann', maxCachedShards: 1 } });
    assert.ok(result.items.every(item => item.scopeKey === scope));
    const status = (await index.status()).ann;
    assert.equal(status.cachedShards, 1);
    if (previousPid) {
      assert.notEqual(status.helperPid, previousPid);
      assert.throws(() => process.kill(previousPid, 0), { code: 'ESRCH' });
    }
    previousPid = status.helperPid;
  }
  assert.equal((await index.status()).ann.recycled, 3);
});

test('cold graph building returns pending immediately and observes generation changes before publication', async t => {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-ann-background-'));
  const database = new DatabaseSync(':memory:');
  database.exec(`CREATE TABLE scope_snapshots(scope_key TEXT,generation INTEGER);
    INSERT INTO scope_snapshots VALUES('project:one',1);
    CREATE TABLE sources(source_id TEXT,scope_key TEXT,structure_json TEXT,source_type TEXT);
    INSERT INTO sources VALUES('cold','project:one','','code');
    CREATE TABLE chunks(id INTEGER,source_id TEXT,vector BLOB,embedding_profile_id TEXT,dimensions INTEGER,
      embedding_model_version TEXT,embedding_space_id TEXT,structure_domain TEXT);`);
  const insert = database.prepare('INSERT INTO chunks VALUES(?,?,?,?,?,?,?,?)');
  for (let id = 1; id <= 1280; id++) insert.run(id, 'cold', new Uint8Array(new Float32Array([1, 0]).buffer),
    'fixture', 2, 'v1', 'a'.repeat(64), 'code');
  const ann = new LocalAnnStore({ database, directory: root, epoch: 'fixture' });
  t.after(async () => { await ann.close(); database.close();
    const suffix = relative(resolve(tmpdir()), resolve(root));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(root, { recursive: true, force: true });
  });
  const descriptor = { scope_key: 'project:one', embedding_profile_id: 'fixture', dimensions: 2,
    embedding_model_version: 'v1', embedding_space_id: 'a'.repeat(64), domain: 'code', count: 1280, generation: 1 };
  const options = validateAnnOptions({ threshold: 1024, mode: 'ann' });
  let releaseBatch, firstBatchStarted;
  const batchStarted = new Promise(resolveBatch => { firstBatchStarted = resolveBatch; });
  const batchGate = new Promise(resolveBatch => { releaseBatch = resolveBatch; });
  const originalRequest = ann._request.bind(ann);
  ann._request = async (method, input) => {
    if (method === 'add') { firstBatchStarted(); await batchGate; }
    return originalRequest(method, input);
  };
  await assert.rejects(ann.search(descriptor, [1, 0], 4, options, () => {}), { code: 'RETRIEVAL_ANN_BUILD_PENDING' });
  assert.equal(ann.status().pendingBuilds, 1);
  await batchStarted;
  // The SQLite owner is usable while a native batch is paused; stale work cannot publish afterward.
  // 原生批次暂停期间 SQLite 所有者仍可用，随后过期的建图作业不能发布。
  assert.equal(database.prepare('SELECT count(*) AS count FROM chunks').get().count, 1280);
  database.exec('UPDATE scope_snapshots SET generation=2');
  ann.builds.cancelScopes(['project:one']);
  releaseBatch();
  await ann.builds.drain();
  assert.equal(ann.status().cachedShards, 0);
  assert.equal(ann.status().helperPid, null);
  const updated = { ...descriptor, generation: 2 };
  ann.warm(updated, options);
  const deadline = performance.now() + 5000;
  while (ann.status().pendingBuilds && performance.now() < deadline) await delay(10);
  assert.equal(ann.status().pendingBuilds, 0);
  assert.equal(ann.status().failedBuilds, 0);
  assert.equal(ann.status().cachedVectors, 1280);
  assert.equal((await ann.search(updated, [1, 0], 4, options, () => {})).length, 4);
});

test('automatic prebuild preserves foreground lexical evidence and becomes queryable after a bounded job', async t => {
  const { index } = await fixture(t, { ann: { threshold: 1024 } });
  const document = source('cold-auto', null);
  document.text = 'export const lexicalEvidence = true;\n'.repeat(3500);
  document.chunks = chunkSource(document, { maxChars: 80 });
  document.vectors = document.chunks.map(() => [1, 0]);
  await index.upsertSources([document]);
  const disabled = await index.prepareVectors({ scopeKeys: ['project:one'], ann: { mode: 'off' } });
  assert.equal(disabled.pendingBuilds, 0);
  const prepared = await index.prepareVectors({ scopeKeys: ['project:one'] });
  assert.equal(prepared.pendingBuilds, 1);
  const first = await search(index, { query: 'lexicalEvidence', ann: { mode: 'auto' } });
  assert.ok(first.items.length > 0);
  if (first.semanticBackend === null) assert.equal(first.degradedReason, 'RETRIEVAL_ANN_BUILD_PENDING');
  const deadline = performance.now() + 5000;
  let status;
  do { status = (await index.status()).ann; if (status.pendingBuilds) await delay(10); }
  while (status.pendingBuilds && performance.now() < deadline);
  assert.equal(status.pendingBuilds, 0);
  assert.equal(status.failedBuilds, 0);
  assert.equal((await search(index, { ann: { mode: 'auto' } })).semanticBackend, 'ann');
});

async function pendingVectorFixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-ann-pending-'));
  const index = new RetrievalIndex({ root });
  let database, vectors;
  t.after(async () => {
    await vectors?.close(); database?.close(); await index.close().catch(() => {});
    const suffix = relative(resolve(tmpdir()), resolve(root));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(root, { recursive: true, force: true });
  });
  await index.upsertSources([source('allowed-nearest', [1, 0]), source('allowed-second', [0, 1]),
    source('forbidden-nearest', [1, 0], { scopeKey: 'project:other' }),
    source('different-space', [1, 0], { embeddingSpaceId: 'b'.repeat(64) }),
    source('different-model', [1, 0], { embeddingModelVersion: 'v2' }),
    source('different-domain', [1, 0], { sourceType: 'document' })]);
  await index.close();
  database = new DatabaseSync(join(root, 'Index', 'retrieval.sqlite'), { allowExtension: true });
  loadSqliteVec(database);
  database.enableLoadExtension(false);
  vectors = new RetrievalVectorSearch({ database, directory: join(root, 'Index'), epoch: 'pending-fixture',
    options: { mode: 'auto', threshold: 1, adaptive: false } });
  // Only graph readiness is controlled; exact vectors and all scope/model/domain filtering use real SQLite.
  // 仅控制图未就绪这一条件；精确向量与范围、模型、领域过滤均通过真实 SQLite 验证。
  const attempts = [];
  vectors.ann.search = async descriptor => {
    attempts.push(descriptor);
    throw Object.assign(new Error('Synthetic pending graph.'), { code: 'RETRIEVAL_ANN_BUILD_PENDING' });
  };
  const input = { scopeKeys: ['project:one'], queryVector: [1, 0], embeddingProfileId: 'fixture',
    embeddingModelVersion: 'v1', embeddingSpaceId: 'a'.repeat(64), requestedDomain: 'code', channelCandidates: 4 };
  return { vectors, input, attempts };
}

test('pending automatic ANN preserves existing bounded exact vectors without exposing another scope or claiming a ready graph', async t => {
  const { vectors, input, attempts } = await pendingVectorFixture(t);
  const result = await vectors.search(input, () => {});
  assert.equal(result.semanticBackend, 'exact');
  assert.equal(result.degradedReason, null);
  assert.deepEqual(result.items.map(item => item.source_id), ['allowed-nearest', 'allowed-second']);
  assert.ok(result.items.every(item => item.scope_key === 'project:one' && item.actual_domain === 'code'));
  assert.equal(attempts.length, 1);
  assert.equal(attempts[0].count, 2);
  assert.equal(attempts[0].scope_key, 'project:one');
  assert.equal(vectors.status().built, 0);
  assert.equal(vectors.status().cachedShards, 0);
  assert.equal(vectors.status().helperPid, null);
});

test('pending ANN exact fallback retains both the configured scan limit and the nonadaptive fifty-thousand ceiling', async t => {
  const { vectors, input, attempts } = await pendingVectorFixture(t);
  const configured = await vectors.search({ ...input, ann: { exactScanLimit: 1 } }, () => {});
  assert.deepEqual(configured.items, []);
  assert.equal(configured.semanticBackend, null);
  assert.equal(configured.degradedReason, 'RETRIEVAL_VECTOR_SCAN_LIMIT');
  const descriptors = vectors._descriptors.bind(vectors);
  // Descriptor-only scale exercises refusal before any large scan, without allocating fifty thousand vectors.
  // 仅放大标量目录数量，验证大扫描开始前的拒绝；不分配五万份向量。
  vectors._descriptors = options => descriptors(options).map(descriptor => ({ ...descriptor, count: 50001 }));
  const bounded = await vectors.search({ ...input, ann: { exactScanLimit: 1000000 } }, () => {});
  assert.deepEqual(bounded.items, []);
  assert.equal(bounded.semanticBackend, null);
  assert.equal(bounded.degradedReason, 'RETRIEVAL_VECTOR_SCAN_LIMIT');
  assert.ok(attempts.every(descriptor => descriptor.scope_key === 'project:one' && descriptor.domain === 'code'));
  assert.equal(vectors.status().built, 0);
  assert.equal(vectors.status().cachedShards, 0);
  assert.equal(vectors.status().helperPid, null);
});

test('ANN resource failures preserve lexical results and return truthful degradation without executing native builds', async t => {
  const { index } = await fixture(t);
  const document = source('bounded', null, { text: 'bounded lexical evidence\n'.repeat(300), vectors: undefined });
  document.chunks = chunkSource(document, { maxChars: 80 });
  document.vectors = document.chunks.map(() => Array.from({ length: 4096 }, (_, number) => number === 0 ? 1 : 0));
  await index.upsertSources([document]);
  const result = await search(index, { query: 'lexical evidence', queryVector: document.vectors[0],
    ann: { mode: 'ann', adaptive: false, maxShardBytes: 1024 * 1024, exactScanLimit: 1 } });
  assert.ok(result.items.length > 0);
  assert.equal(result.strategy, 'lexical');
  assert.equal(result.degradedReason, 'RETRIEVAL_ANN_RESOURCE_LIMIT');
  assert.equal((await index.status()).ann.built, 0);
  const metadata = (await index.listSources({ scopeKeys: ['project:one'] }))[0];
  assert.equal(metadata.chunkCount, document.chunks.length);
  assert.equal(metadata.vectorChunks, document.chunks.length);
  assert.equal(metadata.vectorDimensions, 4096);
  assert.equal(metadata.embeddingSpaceId, 'a'.repeat(64));
});

test('ANN rejects malformed options before starting a worker or crossing the persistence boundary', async t => {
  assert.throws(() => validateAnnOptions({ mode: 'cloud' }), { code: 'INVALID_RETRIEVAL_ANN' });
  assert.throws(() => validateAnnOptions({ threshold: 0 }), { code: 'INVALID_RETRIEVAL_ANN' });
  assert.throws(() => validateAnnOptions({ dependencyPath: 'malicious.dll' }), { code: 'INVALID_RETRIEVAL_ANN' });
  const { index } = await fixture(t);
  assert.throws(() => search(index, { ann: { maxCachedShards: -1 } }), { code: 'INVALID_RETRIEVAL_ANN' });
  assert.throws(() => search(index, { ann: null }), { code: 'INVALID_RETRIEVAL_ANN' });
  assert.throws(() => search(index, { ann: [] }), { code: 'INVALID_RETRIEVAL_ANN' });
  assert.equal(index.worker, null);
});

test('cancelled cold ANN builds release partial graph ownership and later retrieval can recover', async t => {
  const { index } = await fixture(t);
  const rows = Array.from({ length: 70 }, (_, number) => source(`cancel-${number}`, [Math.cos(number), Math.sin(number)]));
  await index.upsertSources(rows);
  const controller = new AbortController();
  const pending = search(index, { signal: controller.signal });
  const timer = setTimeout(() => controller.abort(), 20);
  try { await assert.rejects(pending, { name: 'AbortError' }); }
  finally { clearTimeout(timer); }
  assert.equal((await index.status()).ann.cachedShards, 0);
  assert.equal((await search(index)).semanticBackend, 'ann');
  assert.equal((await index.status()).ann.cachedShards, 1);
});

test('owned native helper crash degrades to authorized exact retrieval without losing stored vectors', async t => {
  const { index } = await fixture(t);
  await index.upsertSources([source('crash-proof', [1, 0])]);
  await search(index);
  const status = await index.status();
  assert.ok(status.ann.helperPid > 0 && status.ann.helperPid !== process.pid);
  assert.ok(status.ann.helperRssBytes > 0);
  process.kill(status.ann.helperPid);
  await new Promise(resolveExit => setTimeout(resolveExit, 60));
  const result = await search(index);
  assert.equal(result.semanticBackend, 'exact');
  assert.equal(result.degradedReason, 'RETRIEVAL_ANN_WORKER_EXITED');
  assert.equal(result.items[0].sourceId, 'crash-proof');
  assert.equal((await index.status()).vectorChunks, 1);
  assert.equal((await index.status()).ann.state, 'unavailable');
});

test('SQLite rebuild changes the epoch so an older persisted ANN graph cannot be revived', async t => {
  const { root, index } = await fixture(t);
  await index.upsertSources([source('epoch-target', [1, 0]), source('epoch-second', [0, 1])]);
  await search(index);
  await index.close();
  await rm(join(root, 'Index', 'retrieval.sqlite'));
  const rebuilt = new RetrievalIndex({ root });
  try {
    await rebuilt.upsertSources([source('epoch-target', [-1, 0]), source('epoch-second', [0, 1])]);
    assert.equal((await search(rebuilt)).items[0].sourceId, 'epoch-second');
    assert.equal((await rebuilt.status()).ann.loaded, 0);
    assert.equal((await rebuilt.status()).ann.built, 1);
    assert.equal((await rebuilt.status()).ann.descriptorCache.hits, 0);
    assert.equal((await rebuilt.status()).ann.descriptorCache.misses, 1);
  } finally { await rebuilt.close(); }
});

test('scalar shard descriptors reuse only current authorized scope, domain and model identity', async t => {
  const { index } = await fixture(t);
  await index.upsertSources([source('allowed', [1, 0]),
    source('private', [1, 0], { scopeKey: 'project:other' }),
    source('space-other', [1, 0], { embeddingSpaceId: 'b'.repeat(64) }),
    source('version-other', [1, 0], { embeddingModelVersion: 'v2' }),
    source('document-other', [1, 0], { sourceType: 'document' })]);
  const codeQuery = { retrievalIntent: { domain: 'code' } };
  assert.deepEqual((await search(index, codeQuery)).items.map(item => item.sourceId), ['allowed']);
  assert.equal((await index.status()).ann.descriptorCache.misses, 1);
  assert.deepEqual((await search(index, { ...codeQuery, ann: { mode: 'exact' } })).items.map(item => item.sourceId), ['allowed']);
  assert.equal((await index.status()).ann.descriptorCache.hits, 1);
  assert.deepEqual((await search(index, { ...codeQuery, scopeKeys: ['project:other'] })).items.map(item => item.sourceId), ['private']);
  assert.deepEqual((await search(index, { ...codeQuery, embeddingSpaceId: 'b'.repeat(64) })).items.map(item => item.sourceId), ['space-other']);
  assert.deepEqual((await search(index, { ...codeQuery, embeddingModelVersion: 'v2' })).items.map(item => item.sourceId), ['version-other']);
  assert.deepEqual((await search(index, { retrievalIntent: { domain: 'knowledge' } })).items.map(item => item.sourceId), ['document-other']);
  const before = (await index.status()).ann.descriptorCache;
  await index.upsertSources([source('allowed-new', [1, 0])]);
  const updated = await search(index, codeQuery);
  assert.equal(updated.items.length, 2);
  assert.ok(updated.items.every(item => item.scopeKey === 'project:one'));
  assert.ok((await index.status()).ann.descriptorCache.misses > before.misses);
  await index.removeSource('allowed-new', { scopeKeys: ['project:one'] });
  assert.deepEqual((await search(index, codeQuery)).items.map(item => item.sourceId), ['allowed']);
  await index.invalidateScope('project:one');
  assert.equal((await search(index, codeQuery)).items.length, 0);
});

test('negative descriptor entries are bounded and cannot revive when a matching model space is published', async t => {
  const { index } = await fixture(t, { ann: { mode: 'exact' } });
  await index.upsertSources([source('existing', [1, 0])]);
  for (let number = 0; number < 70; number++)
    assert.equal((await search(index, { ann: { mode: 'exact' }, embeddingModelVersion: `missing-${number}` })).items.length, 0);
  const status = await index.status();
  assert.equal(status.ann.descriptorCache.entries, 64);
  assert.ok(status.ann.descriptorCache.payloadBytes <= 8 * 1024 * 1024);
  assert.equal((await search(index, { ann: { mode: 'exact' }, embeddingModelVersion: 'missing-69' })).items.length, 0);
  assert.equal((await index.status()).ann.descriptorCache.hits, 1);
  await index.upsertSources([source('new-model-match', [1, 0], { embeddingModelVersion: 'missing-69' })]);
  assert.deepEqual((await search(index, { ann: { mode: 'exact' }, embeddingModelVersion: 'missing-69' })).items.map(item => item.sourceId), ['new-model-match']);
  assert.equal((await index.status()).ann.descriptorCache.entries, 1);
});

test('lowering a loaded ANN shard budget releases its owned helper before exact fallback', async t => {
  const { index } = await fixture(t);
  const document = source('loaded-budget', null, { text: 'budget source contents\n'.repeat(300), vectors: undefined });
  document.chunks = chunkSource(document, { maxChars: 80 });
  const vector = Array.from({ length: 4096 }, (_, number) => number === 0 ? 1 : 0);
  document.vectors = document.chunks.map(() => vector);
  await index.upsertSources([document]);
  const normal = await search(index, { queryVector: vector });
  assert.equal(normal.semanticBackend, 'ann');
  const before = (await index.status()).ann;
  assert.ok(before.cachedVectors > 0 && before.helperPid);
  const reduced = await search(index, { queryVector: vector, ann: { mode: 'ann', adaptive: false, maxShardBytes: 1024 * 1024 } });
  assert.equal(reduced.semanticBackend, 'exact');
  assert.equal(reduced.degradedReason, 'RETRIEVAL_ANN_RESOURCE_LIMIT');
  const after = (await index.status()).ann;
  assert.equal(after.cachedShards, 0);
  assert.equal(after.cachedVectors, 0);
  assert.equal(after.helperPid, null);
  assert.equal(after.helperRssBytes, 0);
  assert.throws(() => process.kill(before.helperPid, 0), { code: 'ESRCH' });
  assert.equal((await index.status()).vectorChunks, document.chunks.length);
});

test('lowering cached shard count closes the oversized owner and reloads compliant graphs under the new limit', async t => {
  const { index } = await fixture(t);
  await index.upsertSources([source('cached-one', [1, 0]), source('cached-two', [0, 1], { scopeKey: 'project:two' })]);
  await search(index);
  await search(index, { scopeKeys: ['project:two'] });
  const before = (await index.status()).ann;
  assert.equal(before.cachedShards, 2);
  const reduced = await search(index, { scopeKeys: ['project:two'], ann: { mode: 'ann', maxCachedShards: 1 } });
  assert.equal(reduced.items[0].sourceId, 'cached-two');
  const after = (await index.status()).ann;
  assert.equal(after.cachedShards, 1);
  assert.equal(after.cachedVectors, 1);
  assert.notEqual(after.helperPid, before.helperPid);
  assert.ok(after.loaded >= 1);
  assert.throws(() => process.kill(before.helperPid, 0), { code: 'ESRCH' });
});

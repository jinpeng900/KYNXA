import assert from 'node:assert/strict';
import { test } from 'node:test';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { RetrievalIndex, chunkSource } from '../data/retrieval/index.mjs';
import { EmbeddingSpacePolicy } from '../orchestration/retrieval/embedding-space-policy.mjs';
import { resolveRetrievalModelProfile } from '../models/retrieval/model-registry.mjs';

const CPU = 'builtin-multilingual', GPU = 'builtin-multilingual-dml-q8';
const profiles = [CPU, GPU].map(id => resolveRetrievalModelProfile('embedding', id));

async function temporary(t, closers) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-spaces-'));
  t.after(async () => {
    for (const close of closers) await close();
    const suffix = relative(resolve(tmpdir()), resolve(root));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(root, { recursive: true, force: true, maxRetries: 3, retryDelay: 100 });
  });
  return root;
}

function source(id, profileId, { revision = 1, text = 'Reset the password in Account Settings.', vectors = true } = {}) {
  const profile = profiles.find(item => item.id === profileId);
  const result = { sourceId: id, scopeKey: 'project:one', sourceType: 'knowledge', title: id,
    text, sourceRevision: revision, locator: {}, embeddingProfileId: profile.id,
    embeddingModelVersion: profile.modelVersion, embeddingSpaceId: profile.embeddingSpaceId };
  result.chunks = chunkSource(result);
  if (vectors) result.vectors = result.chunks.map(() => profileId === CPU ? [1, 0] : [0, 1]);
  return result;
}

function query(index, profileId) {
  const profile = profiles.find(item => item.id === profileId);
  return index.search({ query: 'unrelated', scopeKeys: ['project:one'],
    queryVector: profileId === CPU ? [1, 0] : [0, 1], embeddingProfileId: profileId,
    embeddingModelVersion: profile.modelVersion, embeddingSpaceId: profile.embeddingSpaceId });
}

function policy(index, { gpu = true, legacy = false } = {}) {
  return new EmbeddingSpacePolicy({ index, platform: 'win32',
    resources: { snapshot: async () => ({ gpu: { state: gpu ? 'available' : 'unavailable', executionProvider: 'dml', mappingStatus: 'verified' } }) },
    embeddings: { status: id => legacy ? { state: 'ready' } : {
      profileId: id, state: 'ready', embeddingSpaceId: profiles.find(item => item.id === id).embeddingSpaceId } } });
}
const settings = device => ({ local: { semantic: 'auto', embeddingProfileId: CPU, embeddingDevicePolicy: device } });

test('partial GPU migration keeps exact CPU vectors and switches only after all current source chunks are ready', async t => {
  const closers = [], root = await temporary(t, closers), index = new RetrievalIndex({ root });
  closers.push(() => index.close());
  await index.upsertSources([source('one', CPU), source('two', CPU)]);
  await index.upsertSources([source('one', GPU)]);
  const route = policy(index), partial = await route.select(settings('auto'), ['project:one']);
  assert.equal(partial.profileId, CPU); assert.equal(partial.targetProfileId, GPU);
  assert.equal(partial.state, 'migrating-with-compatible-cpu');
  assert.equal((await query(index, CPU)).items.length, 2);
  assert.equal((await query(index, GPU)).items.length, 1);
  await index.upsertSources([source('two', GPU)]);
  const ready = await route.select(settings('auto'), ['project:one']);
  assert.equal(ready.profileId, GPU); assert.equal(ready.fallbackProfileId, CPU);
  assert.equal(ready.state, 'ready');
  await index.close();
  const reopened = new RetrievalIndex({ root }); closers.push(() => reopened.close());
  assert.equal((await query(reopened, CPU)).items.length, 2);
  assert.equal((await query(reopened, GPU)).items.length, 2);
});

test('changed embedding input discards old spaces and source deletion cascades both projections', async t => {
  const closers = [], root = await temporary(t, closers), index = new RetrievalIndex({ root }); closers.push(() => index.close());
  await index.upsertSources([source('one', CPU)]); await index.upsertSources([source('one', GPU)]);
  await index.upsertSources([source('one', GPU, { revision: 2, text: 'Use the current reset link.' })]);
  assert.equal((await query(index, CPU)).items.length, 0);
  assert.equal((await query(index, GPU)).items.length, 1);
  await index.removeSource('one', { scopeKeys: ['project:one'] });
  assert.equal((await query(index, GPU)).items.length, 0);
  assert.deepEqual((await index.vectorSpaceStatus({ scopeKeys: ['project:one'] })).scopes, []);
});

test('a version-four index upgrades existing vectors without changing source references', async t => {
  const closers = [], root = await temporary(t, closers), index = new RetrievalIndex({ root });
  closers.push(() => index.close());
  await index.upsertSources([source('legacy', CPU)]);
  const reference = (await query(index, CPU)).items[0].sourceRef;
  await index.close();
  // Emulate the previous schema using only disposable synthetic data, never production databases.
  // 仅用可丢弃合成数据模拟旧架构，不修改正式数据库，也不让旧评测结果混入新运行。
  const database = new DatabaseSync(join(root, 'Index', 'retrieval.sqlite'));
  database.exec('DROP VIEW retrieval_vector_rows; DROP TABLE chunk_vector_spaces; PRAGMA user_version=4'); database.close();
  const upgraded = new RetrievalIndex({ root }); closers.push(() => upgraded.close());
  assert.equal((await query(upgraded, CPU)).items[0].sourceRef, reference);
  assert.equal((await upgraded.vectorSpaceStatus({ scopeKeys: ['project:one'] })).scopes[0].spaces[0].complete, true);
});

test('automatic hardware policy does not reinterpret undeclared adapters and explicit CPU preference wins', async () => {
  const route = policy({}, { legacy: true });
  assert.equal(await route.target(settings('auto')), CPU);
  assert.equal(await policy({}).target(settings('cpu')), CPU);
  assert.equal(await policy({}, { gpu: false }).target(settings('auto')), CPU);
  assert.equal(await policy({}, { gpu: false }).target(settings('gpu')), GPU);
});

test('a same-name but different-space index is not considered a ready GPU migration', async () => {
  const index = { vectorSpaceStatus: async () => ({ scopes: [{ chunks: 1, spaces: [{ profileId: GPU, spaceId: 'a'.repeat(64), complete: true, vectors: 1 }] }] }) };
  const route = await policy(index).select(settings('auto'), ['project:one']);
  assert.equal(route.state, 'awaiting-index'); assert.equal(route.canMigrate, true);
  assert.equal(route.fallbackProfileId, null);
});

test('disabled semantic retrieval never inspects spaces or queues a migration', async () => {
  const index = { vectorSpaceStatus: async () => { throw new Error('Disabled semantics cannot request vector preparation.'); } };
  const configuration = settings('auto'); configuration.local.semantic = 'off';
  const route = await policy(index).select(configuration, ['project:one']);
  assert.equal(route.state, 'disabled'); assert.equal(route.canMigrate, false);
});

test('a failed automatic GPU backend retains a ready compatible CPU semantic route', async () => {
  const index = { vectorSpaceStatus: async () => ({ scopes: [{ chunks: 1,
    spaces: profiles.map(profile => ({ profileId: profile.id, spaceId: profile.embeddingSpaceId, complete: true, vectors: 1 })) }] }) };
  const route = policy(index);
  const status = route.embeddings.status;
  route.embeddings.status = id => ({ ...status(id), state: id === GPU ? 'error' : 'ready', loaded: id === CPU });
  const selected = await route.select(settings('auto'), ['project:one']);
  assert.equal(selected.profileId, CPU); assert.equal(selected.targetProfileId, CPU);
  assert.equal(selected.state, 'ready'); assert.equal(selected.canMigrate, false);
});

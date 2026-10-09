import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { setImmediate as nextTurn } from 'node:timers/promises';
import { test } from 'node:test';
import { RetrievalIndex, chunkSource } from '../data/retrieval/index.mjs';
import { LocalAnnStore, partitionAnnDescriptor, validateAnnOptions } from '../data/retrieval/ann-store.mjs';
import { RetrievalVectorSearch } from '../data/retrieval/vector-search.mjs';

const MIB = 1024 * 1024;

function resources(availableBytes) {
  const leases = new Map(), memoryRequests = [];
  return { leases, memoryRequests,
    snapshot: async () => ({ memory: { availableBytes } }),
    acquire: async request => {
      const lease = { ...request, status: 'granted', leaseId: randomUUID() };
      if (request.memoryBytes) memoryRequests.push(request.memoryBytes);
      leases.set(lease.leaseId, lease); return lease;
    },
    renew: async () => ({ status: 'renewed' }), registerExecutor: async () => ({ status: 'registered' }),
    report: async () => ({ status: 'reported' }),
    release: async leaseId => { leases.delete(leaseId); return { status: 'released' }; } };
}

async function directory(t, prefix) {
  const root = await mkdtemp(join(tmpdir(), prefix));
  t.after(async () => {
    const suffix = relative(resolve(tmpdir()), resolve(root));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(root, { recursive: true, force: true });
  });
  return root;
}

async function settled(queue) {
  const deadline = performance.now() + 10000;
  while (queue.jobs.size || queue.running) { assert.ok(performance.now() < deadline); await nextTurn(); }
}

function database(count = 3500) {
  const database = new DatabaseSync(':memory:');
  database.exec(`CREATE TABLE scope_snapshots(scope_key TEXT,generation INTEGER);
    INSERT INTO scope_snapshots VALUES('project:one',1);
    CREATE TABLE sources(source_id TEXT,scope_key TEXT,structure_json TEXT,source_type TEXT);
    INSERT INTO sources VALUES('allowed','project:one','','code'),('private','project:other','','code'),
      ('knowledge','project:one','','document');
    CREATE TABLE chunks(id INTEGER,source_id TEXT,vector BLOB,embedding_profile_id TEXT,dimensions INTEGER,
      embedding_model_version TEXT,embedding_space_id TEXT,structure_domain TEXT);`);
  const insert = database.prepare('INSERT INTO chunks VALUES(?,?,?,?,?,?,?,?)');
  const vector = new Uint8Array(new Float32Array([1, 0]).buffer);
  for (let id = 1; id <= count; id++) insert.run(id, 'allowed', vector, 'fixture', 2, 'v1', 'a'.repeat(64), 'code');
  insert.run(count + 1, 'private', vector, 'fixture', 2, 'v1', 'a'.repeat(64), 'code');
  insert.run(count + 2, 'knowledge', vector, 'fixture', 2, 'v1', 'a'.repeat(64), 'knowledge');
  insert.run(count + 3, 'allowed', vector, 'fixture', 2, 'v1', 'b'.repeat(64), 'code');
  return database;
}

const descriptor = count => ({ scope_key: 'project:one', embedding_profile_id: 'fixture', dimensions: 2,
  embedding_model_version: 'v1', embedding_space_id: 'a'.repeat(64), domain: 'code', count, generation: 1 });

test('actual resident capacity splits before ANN execution and repeat queries retain authorized results', async t => {
  let index;
  t.after(() => index?.close());
  const root = await directory(t, 'kynxa-ann-capacity-');
  const resourceService = resources(300 * MIB);
  index = new RetrievalIndex({ root, resourceService });
  const vector = Array(384).fill(0); vector[0] = 1;
  const source = { sourceId: 'capacity', scopeKey: 'project:one', sourceType: 'code', title: 'capacity',
    text: 'export const currentCapacity = true;\n'.repeat(5000), sourceRevision: 1,
    locator: { relativePath: 'capacity.mjs' }, embeddingProfileId: 'fixture',
    embeddingModelVersion: 'v1', embeddingSpaceId: 'a'.repeat(64) };
  source.chunks = chunkSource(source, { maxChars: 80 });
  source.vectors = source.chunks.map(() => vector);
  assert.ok(source.chunks.length > 2048);
  await index.upsertSources([source, { ...source, sourceId: 'private', scopeKey: 'project:other',
    chunks: undefined, text: 'private contents', vectors: [vector] }]);
  for (let attempt = 0; attempt < 2; attempt++) {
    const result = await index.search({ query: 'querynotincorpus', scopeKeys: ['project:one'], queryVector: vector,
      embeddingProfileId: 'fixture', embeddingModelVersion: 'v1', embeddingSpaceId: 'a'.repeat(64),
      ann: { mode: 'ann' }, limit: 4 });
    assert.equal(result.semanticBackend, 'ann'); assert.equal(result.degradedReason, undefined);
    assert.ok(result.items.length && result.items.every(item => item.sourceId === 'capacity'));
    const status = (await index.status()).ann;
    assert.ok(status.planningAudit.plannedShards > 1);
    assert.ok(status.planningAudit.largestShardBytes <= status.planningAudit.approved.maxShardBytes);
    assert.equal(status.planningAudit.approvedMemoryBytes, 100 * MIB);
    assert.equal(status.planningAudit.memoryAdmission, 'full-request-or-denial');
    assert.ok(status.planningAudit.configured.maxShardBytes > status.planningAudit.approved.maxShardBytes);
  }
  await index.close(); assert.equal(resourceService.leases.size, 0);
});

test('a smaller isolated build reservation creates durable subranges and avoids duplicate covering work', async t => {
  let ann, store;
  t.after(async () => { await ann?.close(); store?.close(); });
  const root = await directory(t, 'kynxa-ann-build-capacity-'); store = database();
  const resourceService = resources(291 * MIB);
  ann = new LocalAnnStore({ database: store, directory: root, epoch: 'fixture', resourceService });
  const original = descriptor(3500), options = validateAnnOptions({ mode: 'ann', threshold: 1024 });
  const originalKey = ann._entry(original, options).key;
  ann.warm(original, options);
  const active = ann.builds.jobs.get(originalKey);
  assert.ok(active);
  const child = { ...original, count: 1750, minimumId: 1, maximumId: 1750, partitioned: true };
  ann._enqueue(ann._entry(child, options));
  assert.equal(ann.builds.jobs.size, 1);
  await settled(ann.builds);
  assert.equal(ann.status().failedBuilds, 0);
  assert.equal(ann.preparedOnDisk.has(originalKey), false);
  assert.equal(ann.preparedOnDisk.size, 2);
  const parts = partitionAnnDescriptor(store, 'chunks', original, ann.adaptiveOptions);
  assert.equal(parts.reduce((sum, part) => sum + part.count, 0), 3500);
  assert.equal(parts.length, 2); assert.equal(parts[0].minimumId, 1); assert.equal(parts.at(-1).maximumId, 3500);
  for (const part of parts) assert.equal(ann.preparedOnDisk.get(ann._entry(part, ann.adaptiveOptions).key), 1);
  await ann.close(); assert.equal(resourceService.leases.size, 0);
});

test('isolated persistence failure stays retryable and never advertises an unpublished graph', async t => {
  let ann, store;
  const persist = LocalAnnStore.prototype._persist;
  t.after(async () => { LocalAnnStore.prototype._persist = persist; await ann?.close(); store?.close(); });
  const root = await directory(t, 'kynxa-ann-publish-failure-'); store = database(1280);
  let now = 100000;
  ann = new LocalAnnStore({ database: store, directory: root, epoch: 'fixture',
    resourceService: resources(4 * 1024 ** 3), now: () => now });
  LocalAnnStore.prototype._persist = async function(entry) {
    if (this.isolatedBuilder) throw Object.assign(new Error('synthetic disk failure'), { code: 'FIXTURE_DISK_FAILURE' });
    return persist.call(this, entry);
  };
  const original = descriptor(1280), options = validateAnnOptions({ mode: 'ann', threshold: 1024 });
  ann.warm(original, options); await settled(ann.builds);
  assert.equal(ann.preparedOnDisk.size, 0); assert.equal(ann.status().failedBuilds, 1);
  assert.equal(ann.status().completedBuilds, 0);
  assert.equal(ann.status().buildErrorCode, 'FIXTURE_DISK_FAILURE');
  LocalAnnStore.prototype._persist = persist; now += 1001;
  ann.warm(original, options); await settled(ann.builds);
  assert.equal(ann.status().completedBuilds, 1); assert.equal(ann.preparedOnDisk.size, 1);
});

test('cancelled reduced builds cannot register a late disk receipt and the next generation can rebuild', async t => {
  let ann, store, resume;
  const prepare = LocalAnnStore.prototype._prepareEntry;
  t.after(async () => { resume?.(); LocalAnnStore.prototype._prepareEntry = prepare; await ann?.close(); store?.close(); });
  const root = await directory(t, 'kynxa-ann-reduced-cancel-'); store = database();
  const resourceService = resources(291 * MIB);
  ann = new LocalAnnStore({ database: store, directory: root, epoch: 'fixture', resourceService });
  let signalSaved;
  const saved = new Promise(resolveSaved => { signalSaved = resolveSaved; });
  const release = new Promise(resolveResume => { resume = resolveResume; });
  LocalAnnStore.prototype._prepareEntry = async function(...args) {
    await prepare.apply(this, args);
    if (this.isolatedBuilder) { signalSaved(); await release; }
  };
  const original = descriptor(3500), options = validateAnnOptions({ mode: 'ann', threshold: 1024 });
  ann.warm(original, options);
  let timeout;
  try { await Promise.race([saved, new Promise((_, reject) => {
    timeout = setTimeout(() => reject(new Error('synthetic build must reach publication')), 5000);
  })]); } finally { clearTimeout(timeout); }
  store.exec('UPDATE scope_snapshots SET generation=2');
  ann.builds.cancelScopes(['project:one']); resume(); await settled(ann.builds);
  assert.equal(ann.preparedOnDisk.size, 0); assert.equal(ann.status().completedBuilds, 0);
  assert.equal(ann.status().failedBuilds, 0); assert.equal(resourceService.leases.size, 0);
  LocalAnnStore.prototype._prepareEntry = prepare;
  ann.warm({ ...original, generation: 2 }, options); await settled(ann.builds);
  assert.equal(ann.status().completedBuilds, 1);
  assert.ok([...ann.preparedOnDisk.values()].every(generation => generation === 2));
});

test('default auto threshold adapts to measured corpus cost and approved capacity while explicit policy and versions stay isolated', async t => {
  let search, store;
  t.after(async () => { await search?.close(); store?.close(); });
  const root = await directory(t, 'kynxa-ann-threshold-'); store = database(8);
  search = new RetrievalVectorSearch({ database: store, directory: root, epoch: 'fixture' });
  const current = descriptor(20000), options = validateAnnOptions();
  search._recordExactCost(current, 100);
  assert.equal(search._routingOptions(options, [current]).threshold, 5000);
  assert.equal(search.routingAudit.reason, 'observed-exact-cost');
  const newer = { ...current, generation: 2 };
  assert.equal(search._routingOptions(options, [newer]).threshold, 50000);
  search._recordExactCost(newer, 0.1);
  assert.equal(search._routingOptions(options, [newer]).threshold, 200000);
  assert.equal(search._routingOptions({ ...options, threshold: 3000 }, [current]).threshold, 3000);
  assert.equal(search._routingOptions({ ...options, adaptive: false }, [current]).threshold, 50000);
  assert.equal(search._routingOptions({ ...options, maxShardBytes: MIB }, [{ ...newer, dimensions: 384 }],
    { approvedCapacity: true }).threshold, 1024);
  assert.equal(search.routingAudit.reason, 'approved-shard-capacity');
  search._invalidateDescriptors(['project:one']); assert.equal(search.exactCosts.size, 0);
});

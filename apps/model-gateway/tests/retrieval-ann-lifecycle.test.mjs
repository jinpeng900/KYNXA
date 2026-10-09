import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { setImmediate as nextTurn } from 'node:timers/promises';
import { test } from 'node:test';
import { AnnBuildQueue } from '../data/retrieval/ann-build-queue.mjs';
import { LocalAnnStore, validateAnnOptions } from '../data/retrieval/ann-store.mjs';

function gate() {
  let release;
  const promise = new Promise(resolveGate => { release = resolveGate; });
  return { promise, release };
}

async function settled(queue) {
  const deadline = performance.now() + 5000;
  while (queue.jobs.size || queue.running) {
    assert.ok(performance.now() < deadline, 'owned build must settle');
    await nextTurn();
  }
}

async function fixture(t, count = 8, options = {}) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-ann-lifecycle-'));
  const database = new DatabaseSync(':memory:');
  database.exec(`CREATE TABLE scope_snapshots(scope_key TEXT,generation INTEGER);
    INSERT INTO scope_snapshots VALUES('project:fixture',1);
    CREATE TABLE sources(source_id TEXT,scope_key TEXT,structure_json TEXT,source_type TEXT);
    INSERT INTO sources VALUES('fixture','project:fixture','','code');
    CREATE TABLE chunks(id INTEGER,source_id TEXT,vector BLOB,embedding_profile_id TEXT,dimensions INTEGER,
      embedding_model_version TEXT,embedding_space_id TEXT,structure_domain TEXT);`);
  const insert = database.prepare('INSERT INTO chunks VALUES(?,?,?,?,?,?,?,?)');
  for (let id = 1; id <= count; id++) insert.run(id, 'fixture', new Uint8Array(new Float32Array([1, 0]).buffer),
    'fixture', 2, 'v1', 'a'.repeat(64), 'code');
  const ann = new LocalAnnStore({ database, directory: root, epoch: 'fixture', ...options });
  t.after(async () => {
    await ann.close(); database.close();
    const suffix = relative(resolve(tmpdir()), resolve(root));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(root, { recursive: true, force: true });
  });
  const descriptor = { scope_key: 'project:fixture', embedding_profile_id: 'fixture', dimensions: 2,
    embedding_model_version: 'v1', embedding_space_id: 'a'.repeat(64), domain: 'code', count, generation: 1 };
  return { ann, database, descriptor, options: validateAnnOptions({ mode: 'ann', threshold: 1024 }) };
}

test('cancelled generation replacement remains queued until the old owner settles', async t => {
  const queue = new AnnBuildQueue();
  t.after(() => queue.close());
  const entered = gate(), finish = gate(), observed = [];
  const first = { scope_key: 'project:fixture', generation: 1, count: 1280 };
  queue.enqueue('same-key', first, async job => { observed.push(job.generation); entered.release(); await finish.promise; });
  await entered.promise;
  queue.cancelScopes(['project:fixture']);
  assert.equal(queue.enqueue('same-key', { ...first, generation: 2 }, async job => { observed.push(job.generation); }), true);
  assert.equal(queue.status().pendingBuilds, 2);
  assert.deepEqual(queue.status().buildProgress.map(job => job.state), ['cancelling', 'queued']);
  finish.release();
  await settled(queue);
  assert.deepEqual(observed, [1, 2]);
  assert.equal(queue.completed, 1);
  assert.equal(queue.failed, 0);
});

test('failed builds back off and allow at most three attempts per minute without blocking newer versions', async t => {
  let now = 100000;
  const queue = new AnnBuildQueue({ now: () => now });
  t.after(() => queue.close());
  const descriptor = { scope_key: 'project:fixture', generation: 1, count: 1280 };
  let attempts = 0;
  const fail = async () => { attempts++; throw Object.assign(new Error('synthetic failure'), { code: 'FIXTURE_BUILD_FAILED' }); };
  queue.enqueue('same-key', descriptor, fail); await settled(queue);
  for (let request = 0; request < 20; request++) assert.equal(queue.enqueue('same-key', descriptor, fail), false);
  assert.equal(attempts, 1);
  now += 1000; assert.equal(queue.enqueue('same-key', descriptor, fail), true); await settled(queue);
  now += 2000; assert.equal(queue.enqueue('same-key', descriptor, fail), true); await settled(queue);
  now += 2000; assert.equal(queue.enqueue('same-key', descriptor, fail), false);
  assert.equal(attempts, 3);
  assert.equal(queue.failure('same-key', descriptor).retryAt, 160000);
  queue.enqueue('same-key', { ...descriptor, generation: 2 }, async () => {}); await settled(queue);
  assert.equal(queue.completed, 1);
  assert.equal(queue.status().retryingBuilds, 0);
});

test('an owned native crash recovers after backoff using verified disk data without application restart', async t => {
  let now = 100000;
  const { ann, descriptor, options } = await fixture(t, 8, { now: () => now });
  assert.equal((await ann.search(descriptor, [1, 0], 4, options, () => {})).length, 4);
  const child = ann.child, exited = new Promise(resolveExit => child.once('exit', resolveExit));
  // The fixture kills only the helper it created; no user process or application is touched.
  // 夹具仅结束自身创建的计算进程，不影响用户进程或应用。
  child.kill(); await exited;
  assert.equal(ann.status().state, 'unavailable');
  await assert.rejects(ann.search(descriptor, [1, 0], 4, options, () => {}), { code: 'RETRIEVAL_ANN_WORKER_EXITED' });
  assert.equal(ann.status().helperPid, null);
  now += 1001;
  assert.equal((await ann.search(descriptor, [1, 0], 4, options, () => {})).length, 4);
  assert.notEqual(ann.status().helperPid, child.pid);
  assert.equal(ann.status().state, 'ready');
  assert.equal(ann.status().loaded, 1);
  assert.equal(databaseVectorCount(ann.database), 8);
});

function databaseVectorCount(database) { return database.prepare('SELECT count(*) AS count FROM chunks').get().count; }

test('cancellation during graph save removes the old published entry and prewarms its replacement', async t => {
  const { ann, database, descriptor, options } = await fixture(t, 1280);
  const saved = gate(), finishSave = gate();
  t.after(() => finishSave.release());
  const originalRequest = ann._request.bind(ann);
  let firstSave = true, previousPid, replacementStarted = false;
  ann._request = async (method, input) => {
    if (method === 'create' && !firstSave && descriptor.generation === 2) {
      replacementStarted = true;
      assert.equal(ann.status().cachedShards, 0);
      assert.throws(() => process.kill(previousPid, 0), { code: 'ESRCH' });
    }
    const result = await originalRequest(method, input);
    if (method === 'save' && firstSave) {
      firstSave = false; previousPid = ann.status().helperPid;
      saved.release(); await finishSave.promise;
    }
    return result;
  };
  ann.warm(descriptor, options);
  await saved.promise;
  assert.equal(ann.status().cachedShards, 1);
  database.exec('UPDATE scope_snapshots SET generation=2');
  ann.builds.cancelScopes(['project:fixture']);
  descriptor.generation = 2;
  ann.warm({ ...descriptor }, options);
  finishSave.release();
  await settled(ann.builds);
  assert.equal(replacementStarted, true);
  assert.equal(ann.status().failedBuilds, 0);
  assert.equal(ann.status().completedBuilds, 1);
  assert.equal(ann.status().cachedVectors, 1280);
  assert.equal([...ann.shards.values()][0].generation, 2);
  assert.equal((await ann.search(descriptor, [1, 0], 4, options, () => {})).length, 4);
});

test('invalidation retires the actual native owner and keeps unaffected scoped graphs recoverable', async t => {
  const { ann, database, descriptor, options } = await fixture(t);
  database.exec(`INSERT INTO scope_snapshots VALUES('project:other',1);
    INSERT INTO sources VALUES('other','project:other','','code');`);
  database.prepare('INSERT INTO chunks VALUES(?,?,?,?,?,?,?,?)').run(9, 'other',
    new Uint8Array(new Float32Array([0, 1]).buffer), 'fixture', 2, 'v1', 'a'.repeat(64), 'code');
  const other = { ...descriptor, scope_key: 'project:other', count: 1 };
  await ann.search(descriptor, [1, 0], 4, options, () => {});
  await ann.search(other, [0, 1], 1, options, () => {});
  const previousPid = ann.status().helperPid;
  await ann.invalidateScopes(['project:fixture']);
  assert.equal(ann.status().cachedShards, 0);
  assert.equal(ann.status().helperRssBytes, 0);
  assert.throws(() => process.kill(previousPid, 0), { code: 'ESRCH' });
  assert.deepEqual((await ann.search(other, [0, 1], 1, options, () => {})).map(item => item.id), [9]);
  assert.equal(ann.status().cachedVectors, 1);
  assert.equal(databaseVectorCount(database), 9);
});

test('foreground search and invalidation serialize around a background batch and release a partial native graph', async t => {
  const { ann, database, descriptor, options } = await fixture(t, 1280);
  database.exec(`INSERT INTO scope_snapshots VALUES('project:other',1);
    INSERT INTO sources VALUES('other','project:other','','code');`);
  database.prepare('INSERT INTO chunks VALUES(?,?,?,?,?,?,?,?)').run(1281, 'other',
    new Uint8Array(new Float32Array([0, 1]).buffer), 'fixture', 2, 'v1', 'a'.repeat(64), 'code');
  const other = { ...descriptor, scope_key: 'project:other', count: 1 };
  await ann.search(other, [0, 1], 1, options, () => {});
  const paused = gate(), finishBatch = gate();
  t.after(() => finishBatch.release());
  const originalRequest = ann._request.bind(ann);
  let firstBatch = true;
  ann._request = async (method, input) => {
    const result = await originalRequest(method, input);
    if (method === 'add' && firstBatch) { firstBatch = false; paused.release(); await finishBatch.promise; }
    return result;
  };
  ann.warm(descriptor, options);
  await paused.promise;
  const previousPid = ann.status().helperPid;
  const foreground = ann.search(other, [0, 1], 1, options, () => {});
  await nextTurn();
  const invalidation = ann.invalidateScopes(['project:other']);
  finishBatch.release();
  assert.deepEqual((await foreground).map(item => item.id), [1281]);
  await invalidation;
  await settled(ann.builds);
  assert.equal(ann.status().cachedShards, 0);
  assert.equal(ann.status().helperPid, null);
  assert.equal(ann.status().failedBuilds, 0);
  assert.throws(() => process.kill(previousPid, 0), { code: 'ESRCH' });
  ann.warm(descriptor, options); await settled(ann.builds);
  assert.equal((await ann.search(descriptor, [1, 0], 4, options, () => {})).length, 4);
});

test('a rejected loaded graph retires its native allocations before rebuilding from authoritative vectors', async t => {
  const { ann, descriptor, options } = await fixture(t);
  await ann.search(descriptor, [1, 0], 4, options, () => {});
  await ann.invalidateScopes(['project:fixture']);
  const originalRequest = ann._request.bind(ann);
  let rejectedOwnerPid;
  ann._request = async (method, input) => {
    const result = await originalRequest(method, input);
    if (method === 'load') {
      rejectedOwnerPid = ann.status().helperPid;
      return { ...result, count: result.count + 1 };
    }
    return result;
  };
  assert.equal((await ann.search(descriptor, [1, 0], 4, options, () => {})).length, 4);
  assert.ok(rejectedOwnerPid);
  assert.throws(() => process.kill(rejectedOwnerPid, 0), { code: 'ESRCH' });
  assert.notEqual(ann.status().helperPid, rejectedOwnerPid);
  assert.equal(ann.status().cachedVectors, 8);
  assert.equal(ann.status().built, 2);
});

test('lowered budgets cancel an active build before acquiring its owner and exact mode releases native memory', async t => {
  const { ann, descriptor, options } = await fixture(t, 1280);
  const paused = gate(), finishBatch = gate();
  t.after(() => finishBatch.release());
  const originalRequest = ann._request.bind(ann);
  let firstBatch = true;
  ann._request = async (method, input) => {
    const result = await originalRequest(method, input);
    if (method === 'add' && firstBatch) { firstBatch = false; paused.release(); await finishBatch.promise; }
    return result;
  };
  ann.warm(descriptor, options);
  await paused.promise;
  const previousPid = ann.status().helperPid;
  const reduced = { ...options, maxShardBytes: 1024 * 1024 };
  const enforcing = ann.enforceBudget(reduced);
  finishBatch.release(); await enforcing;
  assert.equal(ann.status().pendingBuilds, 0);
  assert.equal(ann.status().helperPid, null);
  assert.throws(() => process.kill(previousPid, 0), { code: 'ESRCH' });
  ann.warm(descriptor, reduced); await settled(ann.builds);
  assert.equal([...ann.shards.values()][0].options.maxShardBytes, reduced.maxShardBytes);
  const rebuiltPid = ann.status().helperPid;
  await ann.enforceBudget({ ...reduced, mode: 'exact' });
  assert.equal(ann.status().cachedShards, 0);
  assert.equal(ann.status().helperRssBytes, 0);
  assert.throws(() => process.kill(rebuiltPid, 0), { code: 'ESRCH' });
});

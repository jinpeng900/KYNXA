import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { Worker } from 'node:worker_threads';
import { test } from 'node:test';
import { RetrievalIndex } from '../data/retrieval/index.mjs';
import { runResourceTask } from '../platform/resources/resource-task.mjs';

async function fixture(t, { recoverWithRealWorker = false, realWorker = false, resources } = {}) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-search-deadline-'));
  if (recoverWithRealWorker) {
    const original = new RetrievalIndex({ root, vectorEnabled: false });
    await original.upsertSources([{ sourceId: 'original', scopeKey: 'user', title: 'Original committed source',
      sourceType: 'document', sourceRevision: 1, locator: {}, text: 'alpha committed evidence is retained' }]);
    await original.close();
  }
  let starts = 0;
  const index = new RetrievalIndex({ root, vectorEnabled: false, resourceService: resources,
    workerFactory: (url, options) => {
      starts++;
      return new Worker(realWorker || recoverWithRealWorker && starts > 1 ? url :
        new URL('./fixtures/retrieval-deadline-worker.mjs', import.meta.url), options);
    } });
  t.after(async () => { await index.close(); await rm(root, { recursive: true, force: true }); });
  await index.status();
  return { root, index, starts: () => starts };
}

test('a native-only search really exits at its deadline before releasing resources and recovers without replaying', async t => {
  const releases = [], resources = { acquire: async () => ({ leaseId: 'probe-lease' }),
    renew: async () => {}, report: async () => {}, release: async () => { releases.push(performance.now()); } };
  const { index, starts } = await fixture(t, { recoverWithRealWorker: true, resources });
  let exitTime;
  index.worker.once('exit', () => { exitTime = performance.now(); });
  const started = performance.now();
  await assert.rejects(runResourceTask(resources, {}, () => index.search({ query: 'alpha', scopeKeys: ['user'], timeoutMs: 100 })),
    { code: 'RETRIEVAL_SEARCH_TIMEOUT', statusCode: 504, timeoutMs: 100 });
  assert.ok(exitTime !== undefined);
  assert.ok(performance.now() - started < 3000, 'Native work terminates rather than running its full recursive SQL.');
  assert.equal(index.worker, null);
  assert.equal(releases.length, 1); assert.ok(releases[0] >= exitTime);
  const recovered = await index.search({ query: 'alpha', scopeKeys: ['user'], timeoutMs: 15000 });
  assert.equal(starts(), 2);
  assert.equal(recovered.items[0].sourceId, 'original');
  const status = await index.status();
  assert.equal(status.searchTimeouts, 1); assert.equal(status.hardSearchRetirements, 1);
  assert.equal(status.searchDeadlinePolicy.nativeSqliteProgressHandler, false);
});

test('queued search timeout never terminates an uncommitted write and preserves its real receipt', async t => {
  const { root, index, starts } = await fixture(t);
  const opened = new Promise(resolveOpened => {
    const listener = message => {
      if (message.type !== 'probe_transaction_open') return;
      index.worker.removeListener('message', listener); resolveOpened();
    };
    index.worker.on('message', listener);
  });
  const write = index.upsertSources([]); await opened;
  const search = assert.rejects(index.search({ query: 'alpha', scopeKeys: ['user'], timeoutMs: 50 }),
    { code: 'RETRIEVAL_SEARCH_TIMEOUT' });
  assert.deepEqual(await write, { committed: true }); await search;
  assert.equal(index.hardSearchRetirements, 0); assert.equal(starts(), 1);
  const database = new DatabaseSync(join(root, 'deadline-probe.sqlite'));
  try { assert.equal(database.prepare('SELECT COUNT(*) AS count FROM markers').get().count, 1); }
  finally { database.close(); }
});

test('user cancellation before the deadline retains its cancellation category after native retirement', async t => {
  const { index } = await fixture(t), controller = new AbortController();
  const search = assert.rejects(index.search({ query: 'alpha', scopeKeys: ['user'], timeoutMs: 100,
    signal: controller.signal }), { name: 'AbortError', code: 'ABORT_ERR' });
  const timer = setTimeout(() => controller.abort(), 30);
  try { await search; } finally { clearTimeout(timer); }
  assert.equal(index.searchTimeouts, 0);
  assert.equal(index.hardSearchRetirements, 1);
});

test('a write queued behind a blocked read prevents unsafe hard retirement', async t => {
  const { root, index } = await fixture(t);
  const search = assert.rejects(index.search({ query: 'alpha', scopeKeys: ['user'], timeoutMs: 50 }),
    { code: 'RETRIEVAL_SEARCH_TIMEOUT' });
  const write = index.upsertSources([]);
  await search; assert.deepEqual(await write, { committed: true });
  assert.equal(index.hardSearchRetirements, 0);
  const database = new DatabaseSync(join(root, 'deadline-probe.sqlite'));
  try { assert.equal(database.prepare('SELECT COUNT(*) AS count FROM markers').get().count, 1); }
  finally { database.close(); }
});

test('a stale true retirement hint cannot make an active vector request safe to terminate', async t => {
  const { index } = await fixture(t);
  // The probe deliberately advertises canRetire=true; the owner's vector contract must override this stale hint.
  // 探针故意宣称可退役，拥有者必须依据向量请求契约拒绝这一过期线索，不能等待撤销消息后才生效。
  await assert.rejects(index.search({ query: 'alpha', scopeKeys: ['user'], queryVector: [1, 0],
    embeddingProfileId: 'fixture', timeoutMs: 50 }), { code: 'RETRIEVAL_SEARCH_TIMEOUT' });
  assert.equal(index.hardSearchRetirements, 0);
  assert.notEqual(index.worker, null);
});

test('a queued vector request blocks retirement of the preceding lexical phase', async t => {
  const { index } = await fixture(t);
  const lexical = assert.rejects(index.search({ query: 'alpha', scopeKeys: ['user'], timeoutMs: 50 }),
    { code: 'RETRIEVAL_SEARCH_TIMEOUT' });
  const vector = index.search({ query: 'alpha', scopeKeys: ['user'], queryVector: [1, 0],
    embeddingProfileId: 'fixture', timeoutMs: 15000 });
  await lexical; assert.deepEqual(await vector, { items: [] });
  assert.equal(index.hardSearchRetirements, 0);
});

test('the actual worker denies vector retirement from its initial operation message', async t => {
  const { index } = await fixture(t, { realWorker: true });
  const guards = [];
  const observe = message => { if (message.type === 'operation_started') guards.push(message.canRetire); };
  index.worker.on('message', observe);
  await index.search({ query: 'alpha', scopeKeys: ['user'], queryVector: [1, 0], embeddingProfileId: 'fixture' });
  await index.search({ query: 'alpha', scopeKeys: ['user'] });
  index.worker.removeListener('message', observe);
  assert.deepEqual(guards, [false, true]);
});

test('search deadline validation rejects invalid values without dispatch and exposes the bounded default', async t => {
  const { index } = await fixture(t);
  for (const timeoutMs of [0, -1, 120001, '15000', NaN, Infinity])
    assert.throws(() => index.search({ query: 'alpha', scopeKeys: ['user'], timeoutMs }),
      { code: 'INVALID_RETRIEVAL_INPUT' });
  const status = await index.status();
  assert.equal(status.searchDeadlinePolicy.defaultTimeoutMs, 15000);
  assert.equal(status.searchDeadlinePolicy.maximumTimeoutMs, 120000);
});

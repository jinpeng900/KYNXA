import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { randomUUID } from 'node:crypto';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { setImmediate as nextTurn, setTimeout as delay } from 'node:timers/promises';
import { test } from 'node:test';
import { LocalAnnStore, validateAnnOptions } from '../data/retrieval/ann-store.mjs';

class ControlledHelper extends EventEmitter {
  constructor() {
    super(); this.pid = process.pid; this.exitCode = null; this.signalCode = null;
    this.channel = { ref() {}, unref() {} }; this.requests = []; this.killCalls = 0;
  }
  ref() {} unref() {}
  kill() { this.killCalls++; return true; }
  send(request, callback) {
    this.requests.push(request);
    queueMicrotask(() => {
      callback?.(null);
      if (this.delayResponse || request.method === 'close' && this.delayClose) return;
      this.emit('message', { id: request.id, result: { count: 1, closed: request.method === 'close' } });
      if (request.method === 'close') this.exit();
    });
  }
  exit(code = 0) { this.exitCode = code; this.emit('exit', code); }
}

async function fixture(t, requestTimeoutMs = 30000) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-ann-ownership-'));
  const leases = new Map(), released = [], helpers = [];
  let now = 100000;
  const resources = {
    snapshot: async () => ({ memory: { availableBytes: 4 * 1024 ** 3 } }),
    acquire: async options => {
      const lease = { status: 'granted', ...options, leaseId: randomUUID() };
      leases.set(lease.leaseId, lease); return lease;
    },
    renew: async () => ({ status: 'renewed' }), report: async () => ({ status: 'reported' }),
    registerExecutor: async () => ({ status: 'registered' }),
    release: async leaseId => { released.push(leaseId); leases.delete(leaseId); return { status: 'released' }; }
  };
  const ann = new LocalAnnStore({ directory: root, epoch: 'fixture', database: {}, resourceService: resources,
    requestTimeoutMs, now: () => now, processFactory: () => { const child = new ControlledHelper(); helpers.push(child); return child; } });
  const options = validateAnnOptions({ mode: 'ann' });
  const descriptor = { scope_key: 'project:fixture', count: 1, dimensions: 2 };
  await ann.resourceOptions(options, descriptor, 'foreground');
  t.after(async () => {
    for (const helper of helpers) if (helper.exitCode === null) helper.exit();
    await ann.close();
    const suffix = relative(resolve(tmpdir()), resolve(root));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(root, { recursive: true, force: true });
  });
  return { ann, leases, released, helpers, options, descriptor, advance: ms => { now += ms; } };
}

test('an already exited helper releases its captured memory lease and a fresh caller gets a distinct lease', async t => {
  const { ann, leases, released, helpers, advance } = await fixture(t);
  await ann._request('create', { key: 'a'.repeat(64), dimensions: 2 });
  const previousLease = ann.resourceLease.leaseId;
  helpers[0].exit(1);
  await ann.retiring;
  assert.equal(leases.size, 0);
  assert.equal(released.filter(id => id === previousLease).length, 1);
  advance(1001);
  await ann._request('create', { key: 'b'.repeat(64), dimensions: 2 });
  const replacementLease = ann.resourceLease.leaseId;
  assert.notEqual(replacementLease, previousLease);
  helpers[0].emit('exit', 1);
  assert.equal(leases.has(replacementLease), true);
  assert.equal(helpers.length, 2);
});

test('failed native work retains compute and memory leases until actual exit and never replays the request', async t => {
  const { ann, leases, helpers, advance } = await fixture(t);
  await ann._request('create', { key: 'a'.repeat(64), dimensions: 2 });
  helpers[0].delayResponse = true;
  const failed = ann._request('add', { keys: [1] });
  const failureAssertion = assert.rejects(failed, { code: 'FIXTURE_SEND_FAILED' });
  await nextTurn();
  helpers[0].emit('error', Object.assign(new Error('synthetic send failure'), { code: 'FIXTURE_SEND_FAILED' }));
  await nextTurn();
  assert.equal(leases.size, 2);
  assert.equal(helpers[0].killCalls, 1);
  advance(1001);
  const replacement = ann._request('create', { key: 'b'.repeat(64), dimensions: 2 });
  await nextTurn();
  assert.equal(helpers.length, 1);
  helpers[0].exit(1);
  await failureAssertion; await replacement;
  assert.equal(helpers.length, 2);
  assert.equal(helpers[0].requests.filter(request => request.method === 'add').length, 1);
  assert.equal(helpers[1].requests.filter(request => request.method === 'add').length, 0);
  assert.equal(leases.size, 1);
});

test('a timed out computation holds reservations until the killed process confirms exit', async t => {
  const { ann, leases, helpers } = await fixture(t, 10);
  await ann._request('create', { key: 'a'.repeat(64), dimensions: 2 });
  helpers[0].delayResponse = true;
  const failureAssertion = assert.rejects(ann._request('add', { keys: [1] }), { code: 'RETRIEVAL_ANN_TIMEOUT' });
  await delay(25);
  assert.equal(helpers[0].killCalls, 1);
  assert.equal(leases.size, 2);
  helpers[0].exit(1);
  await failureAssertion;
  assert.equal(leases.size, 0);
});

test('normal helper recycling reacquires memory before starting the replacement owner', async t => {
  const { ann, leases, helpers } = await fixture(t);
  await ann._request('create', { key: 'a'.repeat(64), dimensions: 2 });
  const previousLease = ann.resourceLease.leaseId;
  await ann._releaseHelper();
  assert.equal(leases.size, 0);
  await ann._request('create', { key: 'b'.repeat(64), dimensions: 2 });
  assert.equal(helpers.length, 2);
  assert.ok(ann.resourceLease?.leaseId);
  assert.notEqual(ann.resourceLease.leaseId, previousLease);
  assert.equal(leases.has(ann.resourceLease.leaseId), true);
});

test('a helper that never obtains a process identity cannot wait forever for a nonexistent exit', async t => {
  const { ann, leases, helpers, advance } = await fixture(t);
  const factory = ann.processFactory;
  ann.processFactory = () => {
    const helper = factory(); helper.pid = undefined;
    queueMicrotask(() => helper.emit('error', Object.assign(new Error('synthetic spawn failure'), { code: 'ENOENT' })));
    return helper;
  };
  await assert.rejects(ann._request('create', { key: 'a'.repeat(64), dimensions: 2 }), { code: 'ENOENT' });
  assert.equal(leases.size, 0);
  assert.equal(ann.retiring, null);
  ann.processFactory = factory; advance(1001);
  await ann._request('create', { key: 'b'.repeat(64), dimensions: 2 });
  assert.equal(helpers.length, 2);
  assert.equal(leases.size, 1);
});

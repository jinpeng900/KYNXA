import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, relative, resolve, sep } from 'node:path';
import { test } from 'node:test';
import { EmbeddingRouter } from '../models/retrieval/embedding-router.mjs';
import { EmbeddingService } from '../models/retrieval/embedding-service.mjs';
import { RerankerService } from '../models/retrieval/reranker-service.mjs';
import { resolveRetrievalModelProfile } from '../models/retrieval/model-registry.mjs';

const windowsOnly = { skip: process.platform !== 'win32' };

function deferred() {
  let resolvePromise;
  const promise = new Promise(resolveResult => { resolvePromise = resolveResult; });
  return { promise, resolve: resolvePromise };
}

async function until(predicate) {
  for (let attempt = 0; attempt < 100; attempt++) {
    if (predicate()) return;
    await new Promise(resolveResult => setImmediate(resolveResult));
  }
  assert.fail('Owned fixture reaches the expected asynchronous boundary.');
}

function resourceFixture(gpu) {
  const leases = new Map(), requests = [], releases = [];
  return { leases, requests, releases, gate: undefined,
    async acquire(request) {
      requests.push(request);
      const leaseId = `owned-${requests.length}`;
      if (this.gate) await this.gate.promise;
      if (request.gpuMemoryBytes && !gpu) return { status: 'denied', reason: 'GPU_UNAVAILABLE' };
      const lease = { status: 'granted', leaseId, ...request,
        mode: 'fixture', deviceId: 0, executionProvider: 'dml', executionDeviceId: 0 };
      leases.set(lease.leaseId, lease);
      return lease;
    },
    async renew(leaseId) { return { status: leases.has(leaseId) ? 'renewed' : 'denied' }; },
    async release(leaseId) { releases.push(leaseId); leases.delete(leaseId); return { status: 'released' }; },
  };
}

// This fixture owns every simulated executor; no installed model or user process is touched.
// 此夹具拥有所有模拟执行器，不加载实际模型，也不接触用户进程。
class OwnedWorker extends EventEmitter {
  constructor(kind, profile, { autoRetire = true } = {}) {
    super(); this.kind = kind; this.profile = profile; this.autoRetire = autoRetire;
    this.messages = []; this.terminations = 0; this.exited = false;
  }
  ref() {}
  unref() {}
  postMessage(message) {
    this.messages.push(message);
    if (message.type === 'embed' || message.type === 'rerank') {
      this.emit('message', { type: 'phase', phase: 'inference' });
      this.emit('message', { type: 'ready', inferenceBackend: { device: message.resourceBudget.device } });
    }
    if (message.type === 'close' && this.autoRetire) queueMicrotask(() => this.retire());
  }
  request(index = 0) { return this.messages.filter(message => ['embed', 'rerank'].includes(message.type))[index]; }
  complete(id, { settled = true, idle = true } = {}) {
    const message = this.messages.find(request => request.id === id);
    this.emit('message', this.kind === 'embedding'
      ? { type: 'result', id, vectors: message.texts.map(() => Array(this.profile.dimensions).fill(0)) }
      : { type: 'result', id, scores: message.texts.map(() => 0.75), truncatedInputsCount: 0 });
    if (settled) this.emit('message', { type: 'settled', id });
    if (idle) this.idle(id);
  }
  idle(throughId) { this.emit('message', { type: 'idle', throughId }); }
  retire(disposed = true) {
    if (this.exited) return;
    this.exited = true;
    this.emit('message', { type: 'closed', disposed });
    this.emit('exit', 0);
  }
  async terminate() { this.terminations++; this.retire(false); }
}

async function modelFixture(t, { kind = 'embedding', gpu = true, autoRetire = true } = {}) {
  const home = await mkdtemp(join(tmpdir(), 'kynxa-idle-gpu-yield-'));
  const profileId = kind === 'embedding' ? gpu ? 'builtin-multilingual-dml-q8' : 'builtin-multilingual'
    : gpu ? 'builtin-multilingual-reranker-dml-q8' : 'builtin-multilingual-reranker';
  const profile = resolveRetrievalModelProfile(kind, profileId);
  for (const asset of profile.files) {
    const filename = join(home, asset.path);
    await mkdir(dirname(filename), { recursive: true });
    await writeFile(filename, 'owned mock asset');
  }
  const resources = resourceFixture(gpu), workers = [];
  const options = { modelRoot: home, resourceService: resources, closeTimeoutMs: 1000,
    workerFactory: () => { const worker = new OwnedWorker(kind, profile, { autoRetire }); workers.push(worker); return worker; } };
  const service = kind === 'embedding' ? new EmbeddingRouter({ factory: instanceOptions => new EmbeddingService({ ...options, ...instanceOptions,
    resourceService: resources }) }) : new RerankerService({ ...options, profileId });
  t.after(async () => {
    workers.forEach(worker => worker.retire());
    await service.close().catch(() => {});
    const suffix = relative(resolve(tmpdir()), resolve(home));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(home, { recursive: true, force: true });
  });
  const call = (signal) => kind === 'embedding' ? service.embedQuery('Owned synthetic GPU query.', { profileId, signal })
    : service.rerank({ query: 'Synthetic recovery', candidates: [{ excerpt: 'Owned passage.' }], signal });
  async function warm() {
    const pending = call();
    await until(() => workers.at(-1)?.request());
    const worker = workers.at(-1); worker.complete(worker.request().id);
    await pending;
    return worker;
  }
  return { service, resources, workers, profileId, call, warm };
}

test('CPU residency is retained and a cold profile does not start a worker just to yield', async t => {
  const fixture = await modelFixture(t, { gpu: false });
  assert.deepEqual(await fixture.service.releaseIdleGpu(), { released: false, results: [] });
  assert.equal(fixture.workers.length, 0);
  const worker = await fixture.warm();
  const result = await fixture.service.releaseIdleGpu();
  assert.equal(result.released, false);
  assert.equal(result.results[0].reason, 'no-gpu-residency');
  assert.ok(fixture.resources.leases.size > 0);
  assert.equal(worker.messages.filter(message => message.type === 'close').length, 0);
});

test('CPU memory pressure waits for owned worker exit before releasing residency and restarting the same space', async t => {
  const fixture = await modelFixture(t, { gpu: false, autoRetire: false }), worker = await fixture.warm();
  const residentLeases = [...fixture.resources.leases.values()].filter(lease => lease.memoryBytes > 0);
  assert.ok(residentLeases.length > 0);
  const yielding = fixture.service.releaseIdleResources();
  const next = fixture.call();
  await new Promise(resolveResult => setImmediate(resolveResult));
  assert.equal(fixture.workers.length, 1);
  assert.ok(residentLeases.every(lease => fixture.resources.leases.has(lease.leaseId)), 'a close request is not an exit receipt');
  worker.retire();
  const result = await yielding;
  assert.equal(result.released, true);
  assert.equal(result.results[0].reason, 'memory-pressure');
  assert.ok(result.results[0].residentMemoryBytes > 0);
  assert.ok(residentLeases.every(lease => !fixture.resources.leases.has(lease.leaseId)));
  await until(() => fixture.workers[1]?.request());
  fixture.workers[1].complete(fixture.workers[1].request().id);
  assert.equal((await next).profileId, fixture.profileId);
  assert.equal(worker.request(1), undefined, 'retirement never replays the completed query');
});

test('automatic CPU idle retirement keeps query bursts warm and resets the grace period after a new query', async t => {
  t.mock.timers.enable({ apis: ['setInterval', 'Date'], now: 1_000 });
  const fixture = await modelFixture(t, { gpu: false }), worker = await fixture.warm();
  const idleReleaseMs = fixture.service.status(fixture.profileId).idleReleaseMs;
  t.mock.timers.tick(idleReleaseMs / 2);
  await new Promise(resolveResult => setImmediate(resolveResult));
  assert.equal(worker.exited, false);
  const pending = fixture.call();
  await until(() => worker.request(1));
  worker.complete(worker.request(1).id); await pending;
  t.mock.timers.tick(idleReleaseMs / 2 + 1);
  await new Promise(resolveResult => setImmediate(resolveResult));
  assert.equal(worker.exited, false, 'the first query deadline must not retire a recently reused worker');
  t.mock.timers.tick(idleReleaseMs);
  await until(() => worker.exited && fixture.resources.leases.size === 0);
  assert.equal(fixture.workers.length, 1, 'idle maintenance must not load a replacement');
});

test('CPU cancellation keeps native work owned until matching idle, and failed retirement blocks replacement', async t => {
  const fixture = await modelFixture(t, { gpu: false, autoRetire: false }), worker = await fixture.warm();
  const controller = new AbortController(), pending = fixture.call(controller.signal);
  const rejected = assert.rejects(pending, { code: 'EMBEDDING_CANCELLED' });
  await until(() => worker.request(1));
  controller.abort(); await rejected;
  assert.equal((await fixture.service.releaseIdleResources()).results[0].reason, 'native-work-outstanding');
  worker.idle(worker.request(1).id);
  const yielding = fixture.service.releaseIdleResources();
  const failed = assert.rejects(yielding, { code: 'EMBEDDING_CLOSE_FAILED' });
  worker.retire(false); await failed;
  await assert.rejects(fixture.call(), { code: 'EMBEDDING_CLOSE_FAILED' });
  assert.equal(fixture.workers.length, 1);
});

test('CPU resource admission and queued work prevent pressure retirement, and closing drains a claimed retirement once', async t => {
  const fixture = await modelFixture(t, { gpu: false, autoRetire: false }), worker = await fixture.warm();
  fixture.resources.gate = deferred();
  const pending = fixture.call();
  await until(() => fixture.resources.requests.at(-1).cpuThreads > 0);
  assert.equal((await fixture.service.releaseIdleResources()).results[0].reason, 'native-work-outstanding');
  fixture.resources.gate.resolve(); fixture.resources.gate = undefined;
  await until(() => worker.request(1));
  assert.equal((await fixture.service.releaseIdleResources()).released, false);
  worker.complete(worker.request(1).id); await pending;
  const yielding = fixture.service.releaseIdleResources(), closing = fixture.service.close();
  worker.retire();
  await Promise.all([yielding, closing]);
  assert.equal(worker.messages.filter(message => message.type === 'close').length, 1);
  assert.equal(fixture.resources.leases.size, 0);
  await assert.rejects(fixture.call(), { code: 'EMBEDDING_CLOSED' });
});

test('idle embedding yields only after acknowledged exit, then a waiting request uses a new same-space worker', windowsOnly, async t => {
  const fixture = await modelFixture(t, { autoRetire: false });
  const worker = await fixture.warm();
  const yielding = fixture.service.releaseIdleGpu();
  const concurrentYield = fixture.service.releaseIdleGpu();
  assert.equal(worker.messages.at(-1).type, 'close');
  assert.ok([...fixture.resources.leases.values()].some(lease => lease.gpuMemoryBytes > 0));
  const next = fixture.call();
  await new Promise(resolveResult => setImmediate(resolveResult));
  assert.equal(fixture.workers.length, 1, 'replacement cannot start before confirmed retirement');
  worker.retire();
  const result = await yielding;
  assert.equal((await concurrentYield).released, true, 'another admission also waits for the existing retirement');
  assert.equal(result.released, true);
  assert.ok(result.results[0].gpuMemoryBytes > 0);
  await until(() => fixture.workers.length === 2 && fixture.workers[1].request());
  fixture.workers[1].complete(fixture.workers[1].request().id);
  const reply = await next;
  assert.equal(reply.profileId, fixture.profileId);
  assert.equal(worker.request(1), undefined, 'the retired request is never replayed');
  assert.equal(worker.terminations, 0);
});

test('cancelled embedding callers retain native tickets until settlement or a matching idle receipt', windowsOnly, async t => {
  const fixture = await modelFixture(t), worker = await fixture.warm();
  const controller = new AbortController(), pending = fixture.call(controller.signal);
  const rejected = assert.rejects(pending, { code: 'EMBEDDING_CANCELLED' });
  await until(() => worker.request(1));
  const id = worker.request(1).id;
  controller.abort(); await rejected;
  assert.equal(fixture.service.status(fixture.profileId).pendingRequests, 0);
  assert.equal(fixture.service.status(fixture.profileId).inputAdmission.activeRequests, 1);
  assert.equal((await fixture.service.releaseIdleGpu()).results[0].reason, 'native-work-outstanding');
  assert.equal(worker.messages.filter(message => message.type === 'close').length, 0);
  worker.idle(id);
  assert.equal((await fixture.service.releaseIdleGpu()).released, true);
});

test('preparing resource admission and queued GPU requests prevent retirement', windowsOnly, async t => {
  const fixture = await modelFixture(t), worker = await fixture.warm();
  fixture.resources.gate = deferred();
  const first = fixture.call();
  await until(() => fixture.resources.requests.at(-1).cpuThreads > 0);
  assert.equal((await fixture.service.releaseIdleGpu()).results[0].reason, 'native-work-outstanding');
  fixture.resources.gate.resolve(); fixture.resources.gate = undefined;
  await until(() => worker.request(1));
  const second = fixture.call();
  await until(() => worker.request(2));
  assert.equal((await fixture.service.releaseIdleGpu()).released, false);
  worker.complete(worker.request(1).id, { idle: false }); await first;
  assert.equal((await fixture.service.releaseIdleGpu()).released, false);
  worker.complete(worker.request(2).id); await second;
  assert.equal((await fixture.service.releaseIdleGpu()).released, true);
});

test('a returned embedding result alone is insufficient to prove the native worker is idle', windowsOnly, async t => {
  const fixture = await modelFixture(t), worker = await fixture.warm();
  const next = fixture.call(); await until(() => worker.request(1));
  const id = worker.request(1).id;
  worker.complete(id, { settled: false, idle: false }); await next;
  assert.equal((await fixture.service.releaseIdleGpu()).results[0].reason, 'native-work-outstanding');
  worker.emit('message', { type: 'settled', id });
  assert.equal((await fixture.service.releaseIdleGpu()).results[0].reason, 'worker-not-idle');
  worker.idle(id);
  assert.equal((await fixture.service.releaseIdleGpu()).released, true);
});

test('failed embedding retirement remains a barrier and does not create an unchecked replacement', windowsOnly, async t => {
  const fixture = await modelFixture(t, { autoRetire: false }), worker = await fixture.warm();
  const yielding = fixture.service.releaseIdleGpu();
  const rejected = assert.rejects(yielding, { code: 'EMBEDDING_CLOSE_FAILED' });
  worker.retire(false); await rejected;
  await assert.rejects(fixture.call(), { code: 'EMBEDDING_CLOSE_FAILED' });
  assert.equal(fixture.workers.length, 1);
});

test('a cancelled embedding waiter never starts another native request during retirement', windowsOnly, async t => {
  const fixture = await modelFixture(t, { autoRetire: false }), worker = await fixture.warm();
  const yielding = fixture.service.releaseIdleGpu();
  const controller = new AbortController(), next = fixture.call(controller.signal);
  const rejected = assert.rejects(next, { code: 'EMBEDDING_CANCELLED' });
  controller.abort(); await rejected;
  assert.equal(fixture.workers.length, 1);
  worker.retire(); await yielding;
  assert.equal(fixture.workers.length, 1);
});

test('idle reranker releases owned residency and lazily reconstructs after its retirement barrier', windowsOnly, async t => {
  const fixture = await modelFixture(t, { kind: 'reranker', autoRetire: false }), worker = await fixture.warm();
  const yielding = fixture.service.releaseIdleGpu();
  assert.equal(fixture.service.releaseIdleGpu(), yielding, 'concurrent yields share retirement');
  const next = fixture.call();
  await new Promise(resolveResult => setImmediate(resolveResult));
  assert.equal(fixture.workers.length, 1);
  worker.retire(); assert.equal((await yielding).released, true);
  await until(() => fixture.workers.length === 2 && fixture.workers[1].request());
  fixture.workers[1].complete(fixture.workers[1].request().id);
  assert.equal((await next).profileId, fixture.profileId);
  assert.equal(worker.terminations, 0);
});

test('cancelled reranker native work blocks yielding even after the caller was rejected', windowsOnly, async t => {
  const fixture = await modelFixture(t, { kind: 'reranker' }), worker = await fixture.warm();
  const controller = new AbortController(), pending = fixture.call(controller.signal);
  const rejected = assert.rejects(pending, { code: 'RERANK_CANCELLED' });
  await until(() => worker.request(1));
  controller.abort(); await rejected;
  assert.equal(fixture.service.status().pendingRequests, 0);
  assert.equal((await fixture.service.releaseIdleGpu()).reason, 'native-work-outstanding');
  worker.idle(worker.request(1).id);
  assert.equal((await fixture.service.releaseIdleGpu()).released, true);
});

test('application shutdown during reranker yield prevents resurrection or queued dispatch', windowsOnly, async t => {
  const fixture = await modelFixture(t, { kind: 'reranker', autoRetire: false }), worker = await fixture.warm();
  const yielding = fixture.service.releaseIdleGpu(), closing = fixture.service.close();
  assert.equal(fixture.service.close(), closing);
  const next = fixture.call(), rejected = assert.rejects(next, { code: 'RERANK_CLOSED' });
  worker.retire(); await yielding; await closing; await rejected;
  assert.equal(fixture.workers.length, 1);
  assert.equal(fixture.resources.leases.size, 0);
  await assert.rejects(fixture.call(), { code: 'RERANK_CLOSED' });
});

test('reranker CPU residency is kept and a pre-cancelled yield never retires the worker', async t => {
  const fixture = await modelFixture(t, { kind: 'reranker', gpu: false }), worker = await fixture.warm();
  assert.equal((await fixture.service.releaseIdleGpu()).reason, 'no-gpu-residency');
  const controller = new AbortController(); controller.abort();
  await assert.rejects(fixture.service.releaseIdleGpu({ signal: controller.signal }), { code: 'RERANK_CANCELLED' });
  assert.equal(worker.messages.filter(message => message.type === 'close').length, 0);
});

test('reranker preparing admission and unacknowledged result settlement remain protected', windowsOnly, async t => {
  const fixture = await modelFixture(t, { kind: 'reranker' }), worker = await fixture.warm();
  fixture.resources.gate = deferred();
  const next = fixture.call();
  await until(() => fixture.resources.requests.at(-1).cpuThreads > 0);
  assert.equal((await fixture.service.releaseIdleGpu()).reason, 'native-work-outstanding');
  fixture.resources.gate.resolve(); fixture.resources.gate = undefined;
  await until(() => worker.request(1));
  const id = worker.request(1).id;
  worker.complete(id, { settled: false, idle: false }); await next;
  assert.equal((await fixture.service.releaseIdleGpu()).reason, 'native-work-outstanding');
  worker.emit('message', { type: 'settled', id });
  assert.equal((await fixture.service.releaseIdleGpu()).reason, 'worker-not-idle');
  worker.idle(id);
  assert.equal((await fixture.service.releaseIdleGpu()).released, true);
});

test('a failed reranker yield remains closed rather than silently restarting its backend', windowsOnly, async t => {
  const fixture = await modelFixture(t, { kind: 'reranker', autoRetire: false }), worker = await fixture.warm();
  const yielding = fixture.service.releaseIdleGpu(), rejected = assert.rejects(yielding, { code: 'RERANK_CLOSE_FAILED' });
  worker.retire(false); await rejected;
  await assert.rejects(fixture.call(), { code: 'RERANK_CLOSED' });
  assert.equal(fixture.workers.length, 1);
});

test('a cancelled reranker waiter exits without dispatch while owned retirement continues', windowsOnly, async t => {
  const fixture = await modelFixture(t, { kind: 'reranker', autoRetire: false }), worker = await fixture.warm();
  const yielding = fixture.service.releaseIdleGpu(), controller = new AbortController();
  const next = fixture.call(controller.signal), rejected = assert.rejects(next, { code: 'RERANK_CANCELLED' });
  controller.abort(); await rejected;
  worker.retire(); assert.equal((await yielding).released, true);
  assert.equal(fixture.workers.length, 1);
});

test('embedding router shutdown during GPU yield prevents a queued replacement', windowsOnly, async t => {
  const fixture = await modelFixture(t, { autoRetire: false }), worker = await fixture.warm();
  const yielding = fixture.service.releaseIdleGpu(), closing = fixture.service.close();
  const next = fixture.call(), rejected = assert.rejects(next, { code: 'EMBEDDING_CLOSED' });
  worker.retire(); await yielding; await closing; await rejected;
  assert.equal(fixture.workers.length, 1);
  assert.equal(fixture.resources.leases.size, 0);
});

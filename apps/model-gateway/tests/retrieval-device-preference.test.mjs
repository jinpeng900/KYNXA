import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, relative, resolve, sep } from 'node:path';
import { test } from 'node:test';
import { EmbeddingRouter } from '../models/retrieval/embedding-router.mjs';
import { EmbeddingService } from '../models/retrieval/embedding-service.mjs';
import { RerankerRouter } from '../models/retrieval/reranker-router.mjs';
import { RerankerService } from '../models/retrieval/reranker-service.mjs';
import { resolveRetrievalModelProfile } from '../models/retrieval/model-registry.mjs';

function deferred() {
  let resolvePromise;
  const promise = new Promise(resolveResult => { resolvePromise = resolveResult; });
  return { promise, resolve: resolvePromise };
}

async function until(predicate) {
  for (let attempt = 0; attempt < 200; attempt++) {
    if (predicate()) return;
    await new Promise(resolveResult => setImmediate(resolveResult));
  }
  assert.fail('The owned mock reaches its asynchronous boundary.');
}

// Every asset, lease and executor belongs to this synthetic fixture; no real model or user process is started.
// 所有资产、租约和执行器都归合成夹具所有，不启动真实模型或用户进程。
class OwnedWorker extends EventEmitter {
  constructor(kind, profile, options) {
    super(); this.kind = kind; this.profile = profile; this.options = options; this.messages = []; this.exited = false;
  }
  ref() {}
  unref() {}
  postMessage(message) {
    this.messages.push(message);
    if (['embed', 'rerank'].includes(message.type)) {
      this.emit('message', { type: 'phase', phase: 'inference' });
      this.emit('message', { type: 'ready', inferenceBackend: { device: message.resourceBudget.device,
        cpuThreads: message.resourceBudget.cpuThreads } });
    }
  }
  request(index = 0) { return this.messages.filter(message => ['embed', 'rerank'].includes(message.type))[index]; }
  complete(id, { settle = true } = {}) {
    const request = this.messages.find(message => message.id === id && ['embed', 'rerank'].includes(message.type));
    this.emit('message', this.kind === 'embedding'
      ? { type: 'result', id, vectors: request.texts.map(() => Array(this.profile.dimensions).fill(0)) }
      : { type: 'result', id, scores: request.texts.map(() => 0.75), truncatedInputsCount: 0 });
    if (settle) this.idle(id);
  }
  idle(id) {
    this.emit('message', { type: 'settled', id });
    this.emit('message', { type: 'idle', throughId: id });
  }
  retire(disposed = true) {
    if (this.exited) return;
    this.exited = true;
    this.emit('message', { type: 'closed', disposed }); this.emit('exit', 0);
  }
  async terminate() { this.retire(false); }
}

async function fixture(t, kind) {
  const home = await mkdtemp(join(tmpdir(), 'kynxa-device-preference-'));
  const profileId = kind === 'embedding' ? 'builtin-multilingual' : 'builtin-multilingual-reranker';
  const profile = resolveRetrievalModelProfile(kind, profileId);
  for (const asset of profile.files) {
    const path = join(home, asset.path); await mkdir(dirname(path), { recursive: true }); await writeFile(path, 'owned fixture');
  }
  const workers = [], leases = new Map(), requests = [];
  const resources = { gate: undefined,
    async acquire(request) {
      requests.push(request);
      if (this.gate) await this.gate.promise;
      const lease = { ...request, status: 'granted', leaseId: `owned-${requests.length}`,
        executionProvider: 'dml', deviceId: 0, executionDeviceId: 0, mode: 'fixture' };
      leases.set(lease.leaseId, lease); return lease;
    },
    async renew() { return { status: 'renewed' }; },
    async release(id) { leases.delete(id); return { status: 'released' }; },
  };
  const factory = options => {
    const workerFactory = (_url, workerOptions) => {
      const worker = new OwnedWorker(kind, resolveRetrievalModelProfile(kind, options.profileId), workerOptions);
      workers.push(worker); return worker;
    };
    const settings = { ...options, modelRoot: home, resourceService: resources, workerFactory, closeTimeoutMs: 1000 };
    return kind === 'embedding' ? new EmbeddingService(settings) : new RerankerService(settings);
  };
  const router = kind === 'embedding' ? new EmbeddingRouter({ factory }) : new RerankerRouter({ factory });
  const codePrefix = kind === 'embedding' ? 'EMBEDDING' : 'RERANK';
  t.after(async () => {
    const closing = router.close();
    workers.forEach(worker => worker.retire());
    await closing.catch(() => {});
    const suffix = relative(resolve(tmpdir()), resolve(home));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(home, { recursive: true, force: true });
  });
  const call = (devicePreference = 'auto', signal) => kind === 'embedding'
    ? router.embedQuery('Owned preference query.', { profileId, devicePreference, signal })
    : router.rerank({ query: 'Owned preference query.', candidates: [{ excerpt: 'Synthetic passage.' }], profileId, devicePreference, signal });
  return { router, profile, profileId, workers, requests, leases, resources, call, codePrefix };
}

for (const kind of ['embedding', 'reranker']) {
  test(`${kind} CPU preference reaches resource admission and the actual worker`, async t => {
    const owned = await fixture(t, kind), pending = owned.call('cpu');
    await until(() => owned.workers[0]?.request());
    const worker = owned.workers[0], request = worker.request();
    assert.equal(worker.options.workerData.devicePreference, 'cpu');
    assert.equal(request.resourceBudget.device, 'cpu');
    assert.equal(owned.requests.some(value => value.gpuMemoryBytes > 0), false);
    worker.complete(request.id); const result = await pending;
    assert.equal(result.profileId, owned.profileId);
    assert.equal(owned.router.status(owned.profileId).devicePreference, 'cpu');
    assert.equal(owned.router.status(owned.profileId, { devicePreference: 'auto' }).requiresReconfiguration, true);
    assert.equal(owned.workers.length, 1);
    if (kind === 'embedding') assert.equal(result.embeddingSpaceId, owned.profile.embeddingSpaceId);
  });

  test(`${kind} preference switch waits for accepted result and native idle before replacement`, async t => {
    const owned = await fixture(t, kind), first = owned.call();
    await until(() => owned.workers[0]?.request());
    const worker = owned.workers[0], id = worker.request().id;
    const next = owned.call('cpu');
    await until(() => owned.router.retiring.has(owned.profileId));
    const observedAfterDrain = owned.router.retiring.get(owned.profileId).completion.then(() =>
      owned.router.status(owned.profileId).devicePreference);
    assert.equal(worker.messages.some(message => message.type === 'close'), false);
    worker.complete(id, { settle: false });
    assert.equal((await first).profileId, owned.profileId);
    assert.equal(worker.messages.some(message => message.type === 'close'), false);
    worker.idle(id); await until(() => worker.messages.some(message => message.type === 'close'));
    assert.equal(owned.workers.length, 1);
    worker.retire(); await until(() => owned.workers[1]?.request());
    assert.equal(await observedAfterDrain, 'cpu');
    assert.equal(owned.workers[1].request().resourceBudget.device, 'cpu');
    owned.workers[1].complete(owned.workers[1].request().id); const result = await next;
    if (kind === 'embedding') assert.equal(result.embeddingSpaceId, owned.profile.embeddingSpaceId);
  });

  test(`${kind} cancelled native work retains the old policy barrier until settlement`, async t => {
    const owned = await fixture(t, kind), controller = new AbortController(), first = owned.call('auto', controller.signal);
    const rejected = assert.rejects(first, { code: `${owned.codePrefix}_CANCELLED` });
    await until(() => owned.workers[0]?.request());
    const worker = owned.workers[0], id = worker.request().id;
    controller.abort(); await rejected;
    const switching = owned.router.configure({ profileId: owned.profileId, devicePreference: 'cpu' });
    await until(() => owned.router.retiring.has(owned.profileId));
    assert.equal(worker.messages.some(message => message.type === 'close'), false);
    assert.ok(owned.leases.size > 0);
    worker.idle(id); await until(() => worker.messages.some(message => message.type === 'close'));
    worker.retire(); const selected = await switching;
    assert.equal(selected.executionIdentity, `${owned.profileId}:cpu`);
    assert.equal(owned.leases.size, 0);
  });

  test(`${kind} cancelling a policy waiter does not skip old worker disposal or create a replacement`, async t => {
    const owned = await fixture(t, kind), first = owned.call();
    await until(() => owned.workers[0]?.request());
    const worker = owned.workers[0], id = worker.request().id;
    worker.complete(id); await first;
    const controller = new AbortController();
    const switching = owned.router.configure({ profileId: owned.profileId, devicePreference: 'cpu', signal: controller.signal });
    const rejected = assert.rejects(switching, { code: `${owned.codePrefix}_CANCELLED` });
    await until(() => worker.messages.some(message => message.type === 'close'));
    controller.abort(); await rejected;
    worker.retire(); await owned.router.configuration;
    assert.equal(owned.workers.length, 1); assert.equal(owned.router.instances.size, 0); assert.equal(owned.leases.size, 0);
  });

  test(`${kind} accepted resource preparation finishes before a changed preference retires its worker`, async t => {
    const owned = await fixture(t, kind);
    owned.resources.gate = deferred(); const first = owned.call();
    await until(() => owned.requests.length > 0);
    const next = owned.call('cpu'); await until(() => owned.router.retiring.has(owned.profileId));
    owned.resources.gate.resolve(); owned.resources.gate = undefined;
    await until(() => owned.workers[0]?.request());
    const worker = owned.workers[0];
    assert.equal(worker.messages.some(message => message.type === 'close'), false);
    worker.complete(worker.request().id); await first;
    await until(() => worker.messages.some(message => message.type === 'close'));
    worker.retire(); await until(() => owned.workers[1]?.request());
    owned.workers[1].complete(owned.workers[1].request().id); await next;
  });

  test(`${kind} failed retirement stays a barrier and strict GPU profile refuses CPU relabeling`, async t => {
    const owned = await fixture(t, kind), first = owned.call();
    await until(() => owned.workers[0]?.request());
    const worker = owned.workers[0]; worker.complete(worker.request().id); await first;
    const switching = owned.router.configure({ profileId: owned.profileId, devicePreference: 'cpu' });
    const rejected = assert.rejects(switching, { code: `${owned.codePrefix}_CLOSE_FAILED` });
    await until(() => worker.messages.some(message => message.type === 'close'));
    worker.retire(false); await rejected;
    await assert.rejects(owned.call('cpu'), { code: `${owned.codePrefix}_CLOSE_FAILED` });
    assert.equal(owned.workers.length, 1);
    const gpuId = kind === 'embedding' ? 'builtin-multilingual-dml-q8' : 'builtin-multilingual-reranker-dml-q8';
    assert.throws(() => owned.router.configure({ profileId: gpuId, devicePreference: 'cpu' }), { code: `${owned.codePrefix}_GPU_REQUIRED` });
  });

  test(`${kind} disabled-policy retirement unloads without constructing another model`, async t => {
    const owned = await fixture(t, kind), first = owned.call('cpu');
    await until(() => owned.workers[0]?.request());
    const worker = owned.workers[0]; worker.complete(worker.request().id); await first;
    const retiring = owned.router.retire();
    await until(() => worker.messages.some(message => message.type === 'close'));
    worker.retire(); assert.equal((await retiring).retired, true);
    assert.equal(owned.router.instances.size, 0); assert.equal(owned.workers.length, 1); assert.equal(owned.leases.size, 0);
  });

  test(`${kind} application close interrupts accepted work and settles a pending preference drain`, async t => {
    const owned = await fixture(t, kind), first = owned.call();
    const rejected = assert.rejects(first, { code: `${owned.codePrefix}_CLOSED` });
    await until(() => owned.workers[0]?.request());
    const worker = owned.workers[0];
    const next = owned.call('cpu'), nextRejected = assert.rejects(next, { code: `${owned.codePrefix}_CLOSED` });
    await until(() => owned.router.retiring.has(owned.profileId));
    const closing = owned.router.close();
    await until(() => worker.messages.some(message => message.type === 'close'));
    worker.retire(); await closing; await rejected; await nextRejected;
    assert.equal(owned.workers.length, 1); assert.equal(owned.leases.size, 0);
  });

  test(`${kind} overlapping opposite policy waiters never bypass the owned retirement barrier`, async t => {
    const owned = await fixture(t, kind), signal = new AbortController().signal;
    let completed = 0;
    const cpu = owned.call('cpu', signal).then(value => { completed++; return value; });
    const automatic = owned.call('auto', signal).then(value => { completed++; return value; });
    const results = Promise.all([cpu, automatic]), settled = new Set();
    for (let attempt = 0; attempt < 200 && completed < 2; attempt++) {
      for (const [index, worker] of owned.workers.entries()) {
        if (index > 0) assert.equal(owned.workers[index - 1].exited, true, 'replacement starts only after its predecessor exits');
        const request = worker.request();
        if (request && !settled.has(worker)) {
          if (worker.options.workerData.devicePreference === 'cpu') assert.equal(request.resourceBudget.device, 'cpu');
          settled.add(worker); worker.complete(request.id);
        }
        if (worker.messages.some(message => message.type === 'close')) worker.retire();
      }
      await new Promise(resolveResult => setImmediate(resolveResult));
    }
    assert.equal(completed, 2);
    for (const result of await results) assert.equal(result.profileId, owned.profileId);
  });
}

test('reranker preference retirement during GPU yield settles without resurrecting an executor',
  { skip: process.platform !== 'win32' }, async t => {
    const owned = await fixture(t, 'reranker'), first = owned.call();
    await until(() => owned.workers[0]?.request());
    const worker = owned.workers[0]; worker.complete(worker.request().id); await first;
    const yielding = owned.router.releaseIdleGpu();
    await until(() => worker.messages.some(message => message.type === 'close'));
    const switching = owned.router.configure({ profileId: owned.profileId, devicePreference: 'cpu' });
    await until(() => owned.router.retiring.has(owned.profileId));
    worker.retire(); await yielding; await switching;
    assert.equal(owned.workers.length, 1); assert.equal(owned.leases.size, 0);
    assert.equal(owned.router.status(owned.profileId).devicePreference, 'cpu');
  });

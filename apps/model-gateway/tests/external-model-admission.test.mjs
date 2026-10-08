import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { test } from 'node:test';
import { ExternalModelAdmission } from '../orchestration/external-model-admission.mjs';
import { ModelRuntime } from '../orchestration/runtime.mjs';

const MIB = 1024 ** 2;
const connection = { providerId: 'ollama', baseUrl: 'http://127.0.0.1:11434/v1', apiKey: 'synthetic-unused' };
const modelId = 'qwen-synthetic';
const coldSnapshot = { backend: 'ollama', loaded: false, runtimeContextTokens: null,
  configuredContextTokens: 8192, modelMaximumContextTokens: 131072,
  estimatedWeightBytes: 512 * MIB, kvBytesPerToken: 16384,
  observedMemoryBytes: null, observedGpuMemoryBytes: null, observedAt: 1234 };
const hardware = { mode: 'rust', memory: { availableBytes: 16 * 1024 ** 3 },
  gpu: { state: 'available', mappingStatus: 'verified', executionProvider: 'dml',
    executionDeviceId: 0, availableMemoryBytes: 8 * 1024 ** 3 } };

function deferred() {
  let resolveResult;
  const promise = new Promise(resolve => { resolveResult = resolve; });
  return { promise, resolve: resolveResult };
}

async function until(predicate) {
  for (let attempt = 0; attempt < 100; attempt++) {
    if (predicate()) return;
    await new Promise(resolveResult => setImmediate(resolveResult));
  }
  assert.fail('The mocked resource lifecycle reaches its expected boundary.');
}

async function waitGate(gate, signal, { ignoreCancellation = false } = {}) {
  if (!gate) return;
  if (!signal || ignoreCancellation) return gate.promise;
  signal.throwIfAborted();
  let abort;
  try {
    await Promise.race([gate.promise, new Promise((resolve, reject) => {
      abort = () => reject(signal.reason);
      signal.addEventListener('abort', abort, { once: true });
    })]);
  } finally { signal.removeEventListener('abort', abort); }
}

// All observations and leases are synthetic; this test never contacts or controls a user model server.
// 所有观察与租约均为合成数据，测试不连接或控制用户本机模型服务。
function admissionFixture(t, { snapshot = coldSnapshot } = {}) {
  const fixture = { snapshot: { ...snapshot }, events: [], states: [], requests: [], released: [],
    observations: [], leases: new Map(), handles: [], acquireGate: undefined, releaseGate: undefined,
    observationGate: undefined, ignoreAcquireCancellation: false, denyAcquire: false,
    renewResult: { status: 'renewed', mode: 'rust' } };
  const observer = { async observe(selectedConnection, options = {}) {
    fixture.events.push('observe'); fixture.observations.push({ connection: selectedConnection, ...options });
    await waitGate(fixture.observationGate, options.signal);
    options.signal?.throwIfAborted();
    return { ...fixture.snapshot };
  } };
  const resources = {
    async snapshot({ signal } = {}) { signal?.throwIfAborted(); fixture.events.push('hardware'); return structuredClone(hardware); },
    async acquire(request, { signal } = {}) {
      fixture.events.push('reserve'); fixture.requests.push(request);
      if (fixture.denyAcquire) return { status: 'denied', reason: 'RESOURCE_PRESSURE', mode: 'rust' };
      const leaseId = `synthetic-rust-${fixture.requests.length}`;
      await waitGate(fixture.acquireGate, signal, { ignoreCancellation: fixture.ignoreAcquireCancellation });
      if (!fixture.ignoreAcquireCancellation) signal?.throwIfAborted();
      const lease = { status: 'granted', mode: 'rust', leaseId, ...request };
      fixture.leases.set(leaseId, lease); return lease;
    },
    async renew(leaseId) { fixture.events.push('renew'); assert.ok(fixture.leases.has(leaseId)); return fixture.renewResult; },
    async release(leaseId) {
      fixture.events.push('release-start');
      await waitGate(fixture.releaseGate);
      fixture.released.push(leaseId); fixture.leases.delete(leaseId); fixture.events.push('release-end');
      return { status: 'released', leaseId };
    },
  };
  fixture.admission = new ExternalModelAdmission({ resources, observer,
    yieldIdleGpu: async ({ signal } = {}) => { signal?.throwIfAborted(); fixture.events.push('yield'); return { released: true }; },
    onState: (selectedConnection, selectedModelId, state) => {
      fixture.events.push(`state:${state.state}`); fixture.states.push(state);
    } });
  fixture.acquire = async signal => {
    const handle = await fixture.admission.acquire(connection, { modelId, signal });
    fixture.handles.push(handle); return handle;
  };
  t.after(async () => {
    fixture.acquireGate?.resolve(); fixture.releaseGate?.resolve(); fixture.observationGate?.resolve();
    for (const handle of fixture.handles) { handle.settled(); await handle.release(); }
    await fixture.admission.close();
  });
  return fixture;
}

test('cold Ollama increment is admitted through Rust after idle yielding and before model dispatch', async t => {
  const fixture = admissionFixture(t), handle = await fixture.acquire();
  assert.deepEqual(fixture.events.slice(0, 4), ['observe', 'hardware', 'yield', 'reserve']);
  assert.equal(fixture.requests.length, 1);
  const request = fixture.requests[0];
  assert.equal(request.kind, 'foreground');
  assert.equal(request.cpuThreads, 0);
  assert.equal(request.memoryBytes, 0, 'uncertain GPU upload staging is not a second full RAM residency');
  assert.ok(request.gpuMemoryBytes > coldSnapshot.estimatedWeightBytes);
  assert.equal(fixture.states.at(-1).state, 'admitted-predicted-increment');
  handle.dispatched(); fixture.events.push('dispatch');
  assert.ok(fixture.events.indexOf('reserve') < fixture.events.indexOf('dispatch'));
  handle.settled(); await handle.release();
  assert.equal(fixture.leases.size, 0);
  assert.equal(fixture.admission.entries.size, 0);
  assert.equal(fixture.states.at(-1).externalResidentOwnership, false);
});

test('already loaded target context never reserves observed external weights or KV again', async t => {
  const fixture = admissionFixture(t, { snapshot: { ...coldSnapshot, loaded: true, runtimeContextTokens: 8192,
    observedGpuMemoryBytes: 768 * MIB, observedMemoryBytes: 1024 * MIB } });
  const handle = await fixture.acquire();
  assert.equal(handle.plan.requiresAdmission, false);
  assert.equal(handle.plan.gpuMemoryBytes, 0);
  assert.equal(fixture.requests.length, 0);
  assert.equal(fixture.events.includes('yield'), false);
  handle.dispatched(); handle.settled(); await handle.release();
  assert.equal(fixture.released.length, 0);
});

test('concurrent callers for the same external model share one pending increment fence', async t => {
  const fixture = admissionFixture(t); fixture.acquireGate = deferred();
  const first = fixture.acquire(), second = fixture.acquire();
  await until(() => fixture.requests.length === 1 && [...fixture.admission.entries.values()][0]?.waiters === 2);
  fixture.acquireGate.resolve();
  const [left, right] = await Promise.all([first, second]);
  assert.equal(fixture.requests.length, 1);
  assert.equal(fixture.observations.length, 1);
  left.dispatched(); right.dispatched(); left.settled(); await left.release();
  assert.equal(fixture.leases.size, 1, 'another admitted generation still owns the prediction fence');
  right.settled(); await right.release();
  assert.equal(fixture.released.length, 1);
});

test('one waiting caller cancellation cannot abort the other callers shared preparation', async t => {
  const fixture = admissionFixture(t); fixture.acquireGate = deferred();
  const controller = new AbortController(), first = fixture.acquire(controller.signal), second = fixture.acquire();
  const rejected = assert.rejects(first, { name: 'AbortError' });
  await until(() => [...fixture.admission.entries.values()][0]?.waiters === 2);
  controller.abort(); await rejected;
  assert.equal(fixture.observations[0].signal.aborted, false);
  fixture.acquireGate.resolve();
  const handle = await second;
  assert.equal(fixture.requests.length, 1);
  await handle.release();
  assert.equal(fixture.leases.size, 0);
});

test('last waiter cancellation aborts owned preparation without keeping a phantom forecast', async t => {
  const fixture = admissionFixture(t); fixture.acquireGate = deferred();
  const controller = new AbortController(), pending = fixture.acquire(controller.signal);
  const rejected = assert.rejects(pending, { name: 'AbortError' });
  await until(() => fixture.requests.length === 1);
  controller.abort(); await rejected;
  assert.equal(fixture.observations[0].signal.aborted, true);
  assert.equal(fixture.leases.size, 0);
  assert.equal(fixture.admission.entries.size, 0);
});

test('a late grant after last-caller cancellation is owned and released before cancellation finishes', async t => {
  const fixture = admissionFixture(t); fixture.acquireGate = deferred(); fixture.ignoreAcquireCancellation = true;
  const controller = new AbortController(), pending = fixture.acquire(controller.signal);
  const rejected = assert.rejects(pending, { name: 'AbortError' });
  await until(() => fixture.requests.length === 1);
  controller.abort();
  await until(() => fixture.observations[0].signal.aborted);
  fixture.acquireGate.resolve(); await rejected;
  assert.equal(fixture.released.length, 1);
  assert.equal(fixture.leases.size, 0);
  assert.equal(fixture.admission.entries.size, 0);
});

test('a new caller waits for prior release then observes and prepares a new fence', async t => {
  const fixture = admissionFixture(t), first = await fixture.acquire();
  fixture.releaseGate = deferred();
  const releasing = first.release();
  await until(() => fixture.events.includes('release-start'));
  const next = fixture.acquire();
  await new Promise(resolveResult => setImmediate(resolveResult));
  assert.equal(fixture.observations.length, 1);
  assert.equal(fixture.requests.length, 1);
  fixture.releaseGate.resolve(); await releasing;
  const second = await next;
  assert.equal(fixture.observations.length, 2);
  assert.equal(fixture.requests.length, 2);
  assert.ok(fixture.events.lastIndexOf('reserve') > fixture.events.indexOf('release-end'));
  await second.release(); assert.equal(fixture.leases.size, 0);
});

test('cancelling a caller awaiting another callers release does not wait for the release operation', async t => {
  const fixture = admissionFixture(t), first = await fixture.acquire();
  fixture.releaseGate = deferred();
  const releasing = first.release();
  await until(() => fixture.events.includes('release-start'));
  const controller = new AbortController();
  let cancelled = false;
  const next = fixture.acquire(controller.signal).then(() => ({ completed: true }), error => {
    cancelled = true; return { error };
  });
  await new Promise(resolveResult => setImmediate(resolveResult));
  controller.abort();
  try {
    await new Promise(resolveResult => setImmediate(resolveResult));
    assert.equal(cancelled, true, 'caller cancellation is independent of the prior release completion');
    assert.equal(fixture.observations.length, 1, 'a cancelled waiter cannot start another preparation');
  } finally { fixture.releaseGate.resolve(); }
  await releasing;
  assert.equal((await next).error.name, 'AbortError');
});

test('a new caller never joins aborted last-waiter preparation or loses its fence to the old finish', async t => {
  const fixture = admissionFixture(t); fixture.acquireGate = deferred(); fixture.ignoreAcquireCancellation = true;
  const controller = new AbortController(), first = fixture.acquire(controller.signal);
  const rejected = assert.rejects(first, { name: 'AbortError' });
  await until(() => fixture.requests.length === 1);
  controller.abort(); await until(() => fixture.observations[0].signal.aborted);
  const next = fixture.acquire();
  await new Promise(resolveResult => setImmediate(resolveResult));
  assert.equal(fixture.observations.length, 1, 'new observation waits until the abandoned preparation is retired');
  fixture.acquireGate.resolve();
  await rejected;
  const handle = await next;
  assert.equal(fixture.observations.length, 2, 'the next caller receives its own current preparation');
  assert.equal(fixture.requests.length, 2);
  assert.equal(fixture.leases.size, 1, 'old finish cannot release the newly admitted callers fence');
  assert.equal(fixture.released.length, 1);
  handle.dispatched(); handle.settled(); await handle.release();
  assert.equal(fixture.leases.size, 0);
});

test('denied renewal creates actual quarantine and blocks subsequent dispatch until an outcome is known', async t => {
  const fixture = admissionFixture(t), handle = await fixture.acquire();
  handle.dispatched();
  fixture.renewResult = { status: 'denied', reason: 'RESOURCE_LEASE_UNKNOWN' };
  const entry = [...fixture.admission.entries.values()][0];
  await fixture.admission._renew(entry);
  assert.equal(entry.quarantined, true);
  assert.equal(fixture.states.at(-1).reason, 'resource-renewal-unconfirmed');
  assert.equal(fixture.leases.size, 1);
  await assert.rejects(fixture.acquire(), { code: 'RESOURCE_EXTERNAL_ALLOCATION_UNCONFIRMED' });
  handle.settled();
  assert.throws(() => handle.dispatched(), { code: 'RESOURCE_EXTERNAL_ALLOCATION_UNCONFIRMED' });
  await handle.release(); assert.equal(fixture.leases.size, 0);
});

test('fallback renewal is not a confirmed Rust GPU reservation', async t => {
  const fixture = admissionFixture(t), handle = await fixture.acquire();
  fixture.renewResult = { status: 'renewed', mode: 'fallback' };
  const entry = [...fixture.admission.entries.values()][0];
  await fixture.admission._renew(entry);
  assert.equal(entry.quarantined, true);
  assert.throws(() => handle.dispatched(), { code: 'RESOURCE_EXTERNAL_ALLOCATION_UNCONFIRMED' });
  await handle.release();
});

test('cancelled dispatched allocation remains reserved until the target loaded context is observed', async t => {
  const fixture = admissionFixture(t), handle = await fixture.acquire();
  handle.dispatched();
  await handle.release();
  assert.equal(fixture.leases.size, 1);
  assert.equal(fixture.admission.entries.size, 1);
  assert.equal(fixture.states.at(-1).reason, 'external-request-outcome-unconfirmed');
  await assert.rejects(fixture.acquire(), { code: 'RESOURCE_EXTERNAL_ALLOCATION_UNCONFIRMED' });
  fixture.snapshot = { ...coldSnapshot, loaded: true, runtimeContextTokens: 4096 };
  await fixture.admission.reconcile();
  assert.equal(fixture.leases.size, 1, 'a smaller loaded context does not fulfill the pending target');
  fixture.snapshot = { ...coldSnapshot, loaded: true, runtimeContextTokens: 8192 };
  await fixture.admission.reconcile();
  assert.equal(fixture.leases.size, 0);
  assert.equal(fixture.states.at(-1).reason, 'pending-allocation-now-observed');
  assert.equal(fixture.states.at(-1).externalResidentOwnership, false);
  const next = await fixture.acquire();
  assert.equal(next.plan.requiresAdmission, false);
  assert.equal(fixture.requests.length, 1);
  await next.release();
});

test('partial cold weight forecasts reconcile after observed loading without claiming unknown KV coverage', async t => {
  const fixture = admissionFixture(t, { snapshot: { ...coldSnapshot, configuredContextTokens: null,
    runtimeContextTokens: null, kvBytesPerToken: null } });
  const handle = await fixture.acquire();
  assert.equal(handle.plan.partialCoverage, true);
  assert.equal(handle.plan.breakdown.targetContextTokens, null);
  assert.equal(handle.plan.breakdown.weightIncrementBytes, coldSnapshot.estimatedWeightBytes);
  assert.equal(handle.plan.breakdown.kvIncrementBytes, null);
  assert.deepEqual(handle.plan.unknownComponents, ['kv-cache']);
  handle.dispatched(); await handle.release();
  assert.equal(fixture.leases.size, 1, 'a cancelled cold dispatch still needs observed allocation evidence');
  assert.equal(fixture.admission.entries.size, 1);
  fixture.snapshot = { ...fixture.snapshot, loaded: true, runtimeContextTokens: null };
  await fixture.admission.reconcile();
  assert.equal(fixture.leases.size, 1, 'an incomplete running model observation does not fulfill the weight forecast');
  fixture.snapshot = { ...fixture.snapshot, runtimeContextTokens: 4096, observedGpuMemoryBytes: 640 * MIB };
  const next = await fixture.acquire();
  assert.equal(fixture.released.length, 1);
  assert.equal(fixture.requests.length, 1, 'observed loaded weights must not be reserved again');
  assert.equal(fixture.leases.size, 0);
  assert.equal(next.plan.requiresAdmission, false);
  assert.equal(next.plan.breakdown.weightIncrementBytes, 0);
  const releasedState = fixture.states.find(state => state.reason === 'pending-allocation-now-observed');
  assert.ok(releasedState);
  assert.equal(releasedState.externalResidentOwnership, false);
  assert.equal(handle.plan.partialCoverage, true);
  assert.deepEqual(handle.plan.unknownComponents, ['kv-cache'], 'retiring a weight fence does not certify past KV sufficiency');
  await next.release();
});

test('missing resource APIs stay explicitly observation-only without loading or fabricating a fence', async t => {
  const fixture = admissionFixture(t, { snapshot: { backend: 'external-openai', loaded: null } });
  const handle = await fixture.acquire();
  assert.equal(handle.plan, undefined);
  assert.equal(fixture.requests.length, 0);
  assert.equal(fixture.events.includes('hardware'), false);
  assert.equal(fixture.events.includes('yield'), false);
  handle.dispatched(); handle.settled(); await handle.release();
});

async function mockUpstream(t, fixture, { bodyGate } = {}) {
  const requests = [];
  const upstream = createServer(async (request, response) => {
    let text = '';
    for await (const chunk of request) text += chunk;
    requests.push(JSON.parse(text)); fixture.events.push('http-dispatch');
    response.setHeader('Content-Type', 'application/json');
    response.write('{"ready":');
    await bodyGate?.promise;
    response.end('true}');
  });
  await new Promise(resolveResult => upstream.listen(0, '127.0.0.1', resolveResult));
  t.after(() => {
    bodyGate?.resolve();
    const closed = new Promise(resolveResult => upstream.close(resolveResult));
    upstream.closeAllConnections?.();
    return closed;
  });
  return { requests, connection: { ...connection, baseUrl: `http://127.0.0.1:${upstream.address().port}/v1` } };
}

test('real HTTP transport receives no request when external resource admission is denied', async t => {
  const fixture = admissionFixture(t), upstream = await mockUpstream(t, fixture);
  fixture.denyAcquire = true;
  let generations = 0, bodyReads = 0;
  const runtime = { externalAdmissions: fixture.admission,
    beginLocalGeneration: () => { generations++; return () => {}; } };
  await assert.rejects(ModelRuntime.prototype.consumeModelResponse.call(runtime,
    { connection: upstream.connection }, modelId, { path: '/chat/completions', body: { model: modelId } },
    new AbortController().signal, async response => { bodyReads++; return response.json(); }), { code: 'RESOURCE_PRESSURE' });
  assert.equal(upstream.requests.length, 0);
  assert.equal(generations, 0);
  assert.equal(bodyReads, 0);
  assert.equal(fixture.leases.size, 0);
});

test('real HTTP body consumption keeps the forecast fence until the full response is read', async t => {
  const fixture = admissionFixture(t), bodyGate = deferred(), upstream = await mockUpstream(t, fixture, { bodyGate });
  const runtime = { externalAdmissions: fixture.admission,
    beginLocalGeneration: () => { fixture.events.push('generation-start'); return () => fixture.events.push('generation-stop'); } };
  const pending = ModelRuntime.prototype.consumeModelResponse.call(runtime,
    { connection: upstream.connection }, modelId, { path: '/chat/completions', body: { model: modelId } },
    new AbortController().signal, async response => {
      fixture.events.push('body-reading');
      const body = await response.json(); fixture.events.push('body-read'); return body;
    });
  await until(() => fixture.events.includes('body-reading'));
  assert.equal(upstream.requests.length, 1);
  assert.equal(fixture.leases.size, 1);
  assert.equal(fixture.released.length, 0, 'received headers alone do not settle model execution');
  assert.ok(fixture.events.indexOf('state:admitted-predicted-increment') < fixture.events.indexOf('http-dispatch'));
  bodyGate.resolve();
  assert.deepEqual(await pending, { ready: true });
  assert.equal(fixture.released.length, 1);
  assert.equal(fixture.leases.size, 0);
  assert.equal(fixture.admission.entries.size, 0);
  assert.ok(fixture.events.indexOf('body-read') < fixture.events.indexOf('release-start'));
  assert.equal(fixture.states.at(-1).state, 'prediction-fence-released');
});

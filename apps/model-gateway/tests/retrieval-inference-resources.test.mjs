import assert from 'node:assert/strict';
import { test } from 'node:test';
import { EventEmitter } from 'node:events';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { InferenceResourceReservation, inferenceBatchBudget, inferenceResourceRequest } from '../models/retrieval/inference-resources.mjs';
import { executeInferenceBatches, inferenceSessionOptions, loadVerifiedInferenceBackend } from '../models/retrieval/inference-backend.mjs';
import { EmbeddingService } from '../models/retrieval/embedding-service.mjs';
import { BUILTIN_EMBEDDING_PROFILE } from '../models/retrieval/embedding-profile.mjs';
import { InferenceAdmission } from '../models/retrieval/inference-admission.mjs';
import { auditGpuPartition, qualifiedGpuOutputCompatibility } from '../models/retrieval/inference-backend.mjs';

function resourceFixture({ gpu = false, deny = false } = {}) {
  const leases = new Map(), requests = [], releases = [];
  return { leases, requests, releases,
    async acquire(request) {
      requests.push(request);
      if (deny || request.gpuMemoryBytes && !gpu) return { status: 'denied', reason: 'RESOURCE_GPU_UNKNOWN' };
      const lease = { status: 'granted', leaseId: `fixture-${requests.length}`, cpuThreads: Math.min(6, request.cpuThreads),
        memoryBytes: request.memoryBytes, gpuMemoryBytes: request.gpuMemoryBytes, expiresAt: Date.now() + 30_000, mode: 'fixture',
        deviceId: 0, executionProvider: 'dml', executionDeviceId: 0 };
      leases.set(lease.leaseId, lease); return lease;
    },
    async renew(leaseId) { return { status: leases.has(leaseId) ? 'renewed' : 'denied' }; },
    async release(leaseId) { releases.push(leaseId); leases.delete(leaseId); return { status: 'released' }; },
  };
}

test('approved threads replace fixed two-thread/four-item settings without changing pinned model identity', async () => {
  const resourceService = resourceFixture();
  const reservation = new InferenceResourceReservation({ profile: BUILTIN_EMBEDDING_PROFILE, cpuThreads: 12, resourceService });
  const grant = await reservation.acquire();
  assert.equal(grant.cpuThreads, 6);
  assert.ok(grant.batchSize > 4 && grant.batchSize <= 128);
  assert.equal(grant.device, 'cpu');
  assert.equal(grant.diagnostic.code, 'GPU_RESOURCE_UNAVAILABLE');
  await reservation.idle();
  assert.equal(reservation.status().cpuThreads, 0);
  assert.ok(reservation.status().residentMemoryBytes > 0);
  assert.equal(resourceService.leases.size, 1, 'resident memory remains reserved while the session is idle');
  await reservation.acquire();
  assert.equal(resourceService.requests.filter(request => request.gpuMemoryBytes > 0).length, 1, 'unknown GPU is not reprobed on every query');
  await reservation.close();
  assert.equal(resourceService.leases.size, 0);
});

test('thread and backend audits distinguish requested resources from approved and session-configured values', async () => {
  const resourceService = resourceFixture();
  const reservation = new InferenceResourceReservation({ profile: BUILTIN_EMBEDDING_PROFILE, cpuThreads: 12, resourceService });
  assert.equal(reservation.status().cpuThreadAudit.requested, 12);
  assert.equal(reservation.status().cpuThreadAudit.configuredInSession, null);
  const grant = await reservation.acquire();
  await reservation.backend({ device: 'cpu', cpuThreads: grant.cpuThreads, gpuValidated: false,
    diagnostic: { code: 'GPU_BACKEND_NOT_BUNDLED', requestedDevice: 'dml' } });
  assert.deepEqual(reservation.status().cpuThreadAudit, { requested: 12, granted: 6, configuredInSession: 6,
    source: 'worker-ready-session-options', physicalActiveThreadsMeasured: false });
  assert.equal(reservation.status().appliedBackend.device, 'cpu');
  assert.equal(reservation.status().appliedBackend.diagnostic.code, 'GPU_BACKEND_NOT_BUNDLED');
  await reservation.idle();
  assert.equal(reservation.status().cpuThreadAudit.granted, 0, 'idle releases active execution capacity');
  assert.equal(reservation.status().cpuThreadAudit.configuredInSession, 6, 'resident session configuration remains observable');
  await reservation.close();
});

test('owned native hot batches expose padding measurements without admitting parent or cold-load samples', async () => {
  const resourceService = resourceFixture();
  resourceService.registerExecutor = async () => ({ status: 'registered' });
  const reservation = new InferenceResourceReservation({ profile: BUILTIN_EMBEDDING_PROFILE, resourceService });
  await reservation.acquire(); await reservation.registerExecutor(7654321);
  const sample = { phase: 'hot-inference', backend: 'cpu', processId: 7654321,
    sequenceTokens: 64, batchSize: 2, inputTokens: 96, paddedTokens: 128, bucketMinimumTokens: 32, latencyMs: 10 };
  await reservation.report({ ...sample, processId: process.pid });
  assert.equal(reservation.status().lastBatchMeasurement, undefined);
  await reservation.report({ ...sample, phase: 'cold-load' });
  assert.equal(reservation.status().lastBatchMeasurement, undefined);
  await reservation.report(sample);
  assert.equal(reservation.status().lastBatchMeasurement.paddingTokens, 32);
  assert.equal(reservation.status().lastBatchMeasurement.source, 'owned-worker-hot-inference');
  await reservation.close();
});

test('GPU batch capacity follows memory and tokens independently from CPU thread grants', () => {
  const memory = { memoryBytes: 1024 ** 3, gpuMemoryBytes: 4 * 1024 ** 3, device: 'dml' };
  const oneThread = inferenceBatchBudget({ ...memory, cpuThreads: 1 });
  const manyThreads = inferenceBatchBudget({ ...memory, cpuThreads: 24 });
  assert.deepEqual(oneThread, manyThreads);
  assert.ok(oneThread.batchSize > 4);
  assert.ok(oneThread.batchTokenBudget <= 65_536);
  assert.ok(inferenceBatchBudget({ cpuThreads: 1, memoryBytes: 1024 ** 3 }).batchSize < oneThread.batchSize);
});

test('GPU attention scratch admission uses actual padded sequence lengths without changing result order', async () => {
  const counts = [];
  const output = await executeInferenceBatches(['long-a', 'short-a', 'long-b', 'short-b'], {
    batchSize: 64, batchTokenBudget: 65_536, tokenLengths: [512, 32, 512, 32],
    hiddenSize: 384, attentionHeads: 12, activationMemoryBytes: 20 * 1024 ** 2,
    infer: async values => { counts.push(values.length); return values; },
  });
  assert.deepEqual(output, ['long-a', 'short-a', 'long-b', 'short-b']);
  assert.deepEqual(counts, [2, 1, 1], 'short inputs share a batch while long attention buffers stay inside the grant');
});

test('isolated workload peaks refine model/dtype/backend costs without treating parent RSS as model memory', async () => {
  const resourceService = resourceFixture(), reports = [];
  resourceService.registerExecutor = async () => ({ status: 'registered' });
  resourceService.report = async (leaseId, feedback) => { reports.push(feedback); return { status: 'reported', feedback: { backgroundFraction: 0.5 } }; };
  const profile = { ...BUILTIN_EMBEDDING_PROFILE, modelVersion: 'synthetic-memory-cost-history' };
  const reservation = new InferenceResourceReservation({ profile, resourceService, cpuThreads: 8 });
  const first = await reservation.acquire();
  await reservation.registerExecutor(987654);
  await reservation.report({ phase: 'cold-load', backend: 'cpu', processId: process.pid,
    memoryMeasurementSource: 'isolated-native-process-delta', workloadMemoryDeltaBytes: 2 * 1024 ** 3 });
  assert.equal(reservation.status().memoryEstimate.sampleCount, 0);
  await reservation.report({ phase: 'cold-load', backend: 'cpu', processId: 987654,
    memoryMeasurementSource: 'isolated-native-process-delta', workloadMemoryDeltaBytes: 1024 ** 3 });
  assert.equal(reports.length, 0, 'cold load does not train the hot-throughput controller');
  assert.equal(reservation.status().memoryEstimate.sampleCount, 1);
  await reservation.idle();
  const grown = await reservation.acquire();
  assert.ok(grown.memoryBytes > first.memoryBytes);
  await reservation.report({ phase: 'hot-inference', unit: 'tokens', backend: 'cpu', processId: 987654,
    memoryMeasurementSource: 'isolated-native-process-delta', workloadMemoryDeltaBytes: 2 * 1024 ** 3,
    sequenceTokens: 256, batchSize: 8, throughputPerSecond: 5000 });
  await reservation.idle();
  const grownAgain = await reservation.acquire();
  assert.ok(grownAgain.memoryBytes > grown.memoryBytes);
  assert.equal(reports[0].unit, 'tokens');
  assert.equal(inferenceResourceRequest({ ...profile, dtype: 'fp32' }, 8).memoryEstimate.sampleCount, 0);
  assert.equal(inferenceResourceRequest(profile, 8, { backend: 'dml' }).memoryEstimate.sampleCount, 0);
  await reservation.close();
  assert.equal(resourceService.leases.size, 0, 'every growth lease is released rather than losing an earlier upgrade');
});

test('operator audits require actual GPU matrix compute and ignore unrelated or shape-only events', () => {
  const node = (provider, op_name, dur = 10) => ({ cat: 'Node', dur, args: { provider, op_name } });
  assert.equal(auditGpuPartition([node('DmlExecutionProvider', 'Shape')]).usefulGpuCompute, false);
  const audit = auditGpuPartition([node('DmlExecutionProvider', 'MatMulIntegerToFloat'), node('DmlExecutionProvider', 'FusedMatMul'),
    node('CPUExecutionProvider', 'Gather'), { cat: 'Session', dur: 1000, args: { provider: 'DmlExecutionProvider', op_name: 'MatMul' } }]);
  assert.equal(audit.usefulGpuCompute, true);
  assert.equal(audit.gpuComputeKernelCount, 2);
  assert.equal(audit.cpuKernelCount, 1);
  assert.equal(qualifiedGpuOutputCompatibility([1, 0, 0, 1], [1, 0, 0, 1], 2).embeddingSpaceQualified, true);
  assert.equal(qualifiedGpuOutputCompatibility([1, 0, 0, 1], [0, 1, 1, 0], 2), undefined);
});

test('configured document, UTF-8, token and native queue budgets accept more than legacy 64 rows without becoming unbounded', () => {
  const queue = new InferenceAdmission({ maxBatchDocuments: 128, maxPendingRequests: 2, maxRequestBytes: 100_000,
    maxQueuedBytes: 200_000, maxQueuedEstimatedTokens: 200_000 });
  const first = queue.reserve(Array(100).fill('短句'));
  assert.ok(queue.status().queuedInputBytes > 100 * 6);
  const second = queue.reserve(['Another passage.']);
  assert.throws(() => queue.reserve(['Third']), { code: 'INFERENCE_INPUT_BACKPRESSURE' });
  queue.release(first); queue.release(second);
  assert.equal(queue.status().activeRequests, 0);
  assert.throws(() => queue.reserve(Array(129).fill('a')), { code: 'INFERENCE_INPUT_BACKPRESSURE' });
  assert.throws(() => new InferenceAdmission({ maxPendingRequests: Infinity }), TypeError);
});

test('GPU resident reservation survives idle and is released only after confirmed CPU fallback or retirement', async () => {
  const resourceService = resourceFixture({ gpu: true });
  const reservation = new InferenceResourceReservation({ profile: BUILTIN_EMBEDDING_PROFILE, cpuThreads: 8, resourceService });
  const grant = await reservation.acquire();
  assert.ok(grant.gpuMemoryBytes > 0);
  await reservation.idle();
  assert.equal(resourceService.leases.size, 2);
  await reservation.backend({ device: 'cpu', diagnostic: { code: 'GPU_MODEL_PROBE_FAILED' } });
  assert.equal(resourceService.leases.size, 1);
  const fallback = await reservation.acquire();
  assert.equal(fallback.device, 'cpu');
  assert.equal(fallback.diagnostic.code, 'GPU_MODEL_PROBE_FAILED');
  await reservation.close();
  assert.equal(resourceService.leases.size, 0);
});

test('denied resources do not start or fabricate inference and remain recoverable', async () => {
  const resourceService = resourceFixture({ deny: true });
  const reservation = new InferenceResourceReservation({ profile: BUILTIN_EMBEDDING_PROFILE, resourceService });
  await assert.rejects(reservation.acquire(), { code: 'INFERENCE_RESOURCE_BUSY' });
  assert.equal(resourceService.leases.size, 0);
  await reservation.close();
  assert.deepEqual(inferenceBatchBudget({ cpuThreads: 1, memoryBytes: 1 }), { batchSize: 1, batchTokenBudget: 512 });
});

test('cold document fitting reserves only tokenizer memory and never requests GPU until actual embedding', async () => {
  const resourceService = resourceFixture({ gpu: true });
  const reservation = new InferenceResourceReservation({ profile: BUILTIN_EMBEDDING_PROFILE, resourceService });
  const fitting = await reservation.acquire({ tokenizerOnly: true });
  const tokenizerMemoryBytes = fitting.memoryBytes;
  assert.equal(fitting.device, 'cpu');
  assert.equal(resourceService.requests.filter(request => request.gpuMemoryBytes > 0).length, 0);
  await reservation.idle();
  const embedding = await reservation.acquire();
  assert.ok(embedding.memoryBytes > tokenizerMemoryBytes);
  assert.ok(embedding.gpuMemoryBytes > 0);
  assert.equal(resourceService.requests.filter(request => request.memoryBytes > 0).reduce((sum, request) => sum + request.memoryBytes, 0), embedding.memoryBytes);
  await reservation.close();
  assert.equal(resourceService.leases.size, 0);
});

test('executor registration and measured feedback update later batch grants through the shared resource policy', async () => {
  const resourceService = resourceFixture(), registrations = [], reports = [];
  resourceService.registerExecutor = async (leaseId, executor) => { registrations.push({ leaseId, executor }); return { status: 'registered' }; };
  resourceService.report = async (leaseId, feedback) => {
    reports.push({ leaseId, feedback }); return { status: 'reported', feedback: { backgroundFraction: 0.5, reports: reports.length } };
  };
  const reservation = new InferenceResourceReservation({ profile: BUILTIN_EMBEDDING_PROFILE, resourceService, cpuThreads: 12 });
  const first = await reservation.acquire();
  await reservation.registerExecutor(1234);
  await reservation.report({ throughputPerSecond: 30, latencyMs: 200, queueDepth: 4 });
  await reservation.idle();
  const second = await reservation.acquire();
  assert.ok(second.batchSize < first.batchSize);
  assert.equal(registrations.length, 1);
  assert.equal(registrations[0].executor.processId, 1234);
  assert.equal(reports[0].feedback.throughputPerSecond, 30);
  await reservation.close(); assert.equal(resourceService.leases.size, 0);
});

test('a GPU-qualified vector profile refuses CPU-only resource grants instead of emitting legacy vectors', async t => {
  const resourceService = resourceFixture(); let workers = 0;
  const root = await mkdtemp(join(tmpdir(), 'kynxa-gpu-admission-'));
  for (const asset of BUILTIN_EMBEDDING_PROFILE.files) {
    await mkdir(dirname(join(root, asset.path)), { recursive: true }); await writeFile(join(root, asset.path), 'Synthetic admission fixture.');
  }
  t.after(() => rm(root, { recursive: true, force: true }));
  const service = new EmbeddingService({ modelRoot: root, profileId: 'builtin-multilingual-dml-q8', resourceService,
    workerFactory: () => { workers++; throw new Error('GPU denial must be checked before process creation.'); } });
  try {
    await assert.rejects(service.embedQuery('Synthetic GPU-required query.'), { code: 'EMBEDDING_GPU_REQUIRED' });
    assert.equal(workers, 0);
    assert.equal(service.status().inputAdmission.activeRequests, 0);
  } finally { await service.close(); }
  assert.equal(resourceService.leases.size, 0);
});

test('cancelling one shared admission does not cancel another caller or leak a late CPU grant', async () => {
  const resourceService = resourceFixture();
  const acquire = resourceService.acquire;
  resourceService.acquire = async request => { await new Promise(resolve => setImmediate(resolve)); return acquire(request); };
  const reservation = new InferenceResourceReservation({ profile: BUILTIN_EMBEDDING_PROFILE, resourceService });
  const controller = new AbortController();
  const cancelled = reservation.acquire({ signal: controller.signal });
  const surviving = reservation.acquire();
  controller.abort();
  await assert.rejects(cancelled, { name: 'AbortError' });
  assert.ok((await surviving).cpuThreads > 0);
  await reservation.idle();
  assert.equal(resourceService.leases.size, 1);
  const second = new AbortController();
  const cancelledOnly = reservation.acquire({ signal: second.signal });
  second.abort();
  await assert.rejects(cancelledOnly, { name: 'AbortError' });
  await reservation.idle();
  assert.equal(reservation.status().cpuThreads, 0, 'late admission drains before idle releases its CPU');
  await reservation.close();
  assert.equal(resourceService.leases.size, 0);
});

test('GPU ready requires an actual strict model probe matching the unchanged CPU q8 output', async () => {
  const loads = [], disposed = [];
  const backend = await loadVerifiedInferenceBackend({ device: 'dml', cpuThreads: 6, dtype: 'q8' }, {
    load: async options => {
      loads.push(options);
      return { device: options.device, dispose: async () => { disposed.push(options.device); } };
    },
    probe: async () => [0.5, -0.2, 0.7],
    listProviders: async () => [{ name: 'cpu', bundled: true }, { name: 'dml', bundled: true }],
  });
  assert.equal(backend.status.gpuValidated, true);
  assert.equal(backend.status.cpuOperatorFallback, false);
  assert.deepEqual(loads.map(options => options.dtype), ['q8', 'q8']);
  assert.equal(loads[1].session_options.extra.session.disable_cpu_ep_fallback, '1');
  assert.equal(loads[1].session_options.enableMemPattern, false);
  assert.deepEqual(disposed, ['cpu']);
  await backend.model.dispose();
});

test('unsupported GPU operators and incompatible outputs explicitly retain the original CPU model', async () => {
  for (const incompatible of [false, true]) {
    let gpuAttempts = 0, cpuDisposals = 0, gpuDisposals = 0;
    const backend = await loadVerifiedInferenceBackend({ device: 'dml', cpuThreads: 4, dtype: 'q8' }, {
      load: async options => {
        if (options.device === 'dml') {
          gpuAttempts++;
          if (!incompatible) throw new Error('Unsupported quantized model operator');
        }
        return { device: options.device, dispose: async () => { if (options.device === 'cpu') cpuDisposals++; else gpuDisposals++; } };
      },
      probe: async model => model.device === 'cpu' ? [0.5, 0.2] : [0.1, 0.9],
      listProviders: async () => [{ name: 'dml', bundled: true }],
    });
    assert.equal(backend.status.device, 'cpu');
    assert.equal(backend.status.gpuValidated, false);
    assert.equal(backend.status.diagnostic.code, incompatible ? 'GPU_EMBEDDING_INCOMPATIBLE' : 'GPU_MODEL_PROBE_FAILED');
    assert.equal(gpuAttempts, 1); assert.equal(cpuDisposals, 0);
    assert.equal(gpuDisposals, incompatible ? 1 : 0);
    await backend.model.dispose();
  }
});

test('token-aware inference batches reduce padding, preserve input order and shrink boundedly on memory pressure', async () => {
  const attempted = [], pressure = [];
  const values = await executeInferenceBatches(['long', 'short', 'medium'], {
    batchSize: 8, batchTokenBudget: 2048, tokenLengths: [500, 10, 250],
    infer: async batch => {
      attempted.push(batch);
      if (batch.length > 1) throw new Error('out of memory');
      return batch.map(text => text.toUpperCase());
    },
    onPressure: state => pressure.push(state),
  });
  assert.deepEqual(values, ['LONG', 'SHORT', 'MEDIUM']);
  assert.deepEqual(attempted[0], ['short'], 'short inputs do not share padding with much longer sequences');
  assert.deepEqual(attempted[1], ['medium', 'long']);
  assert.equal(pressure.length, 1);
  assert.equal(pressure[0].batchSize, 1);
  await assert.rejects(executeInferenceBatches(['single'], { tokenLengths: [10], infer: async () => { throw new Error('out of memory'); } }), /out of memory/u);
});

test('length buckets preserve result mapping and report actual padding while single inputs respect the batch grant', async () => {
  const batches = [], measurements = [];
  const result = await executeInferenceBatches(['long-a', 'short-a', 'medium', 'long-b', 'short-b'], {
    batchSize: 8, batchTokenBudget: 2048, tokenLengths: [500, 16, 200, 480, 24],
    infer: async values => { batches.push(values); return values.map(value => `${value}-done`); },
    onMeasurement: value => measurements.push(value) });
  assert.deepEqual(result, ['long-a-done', 'short-a-done', 'medium-done', 'long-b-done', 'short-b-done']);
  assert.deepEqual(batches, [['short-a', 'short-b'], ['medium'], ['long-b', 'long-a']]);
  assert.ok(measurements.every(value => value.sequenceTokens <= value.bucketMinimumTokens * 2));
  assert.equal(measurements[0].paddingTokens, 8);
  assert.equal(measurements[0].paddedTokens, 48);
  let executions = 0;
  await assert.rejects(executeInferenceBatches(['too-long'], { tokenLengths: [128], batchTokenBudget: 64,
    infer: async () => { executions++; return ['unexpected']; } }), { code: 'INFERENCE_RESOURCE_BUSY' });
  assert.equal(executions, 0);
  await assert.rejects(executeInferenceBatches(['invalid'], { tokenLengths: [NaN], infer: async () => [] }),
    { code: 'INFERENCE_INVALID_BATCH' });
});

test('cancelled batches never retry or return a partial result', async () => {
  const controller = new AbortController(); let calls = 0;
  await assert.rejects(executeInferenceBatches(['one', 'two'], { tokenLengths: [10, 10],
    checkCancelled: () => { if (controller.signal.aborted) throw Object.assign(new Error('cancelled'), { name: 'AbortError' }); },
    infer: async () => { calls++; controller.abort(); throw new Error('out of memory'); },
  }), { name: 'AbortError' });
  assert.equal(calls, 1);
  assert.equal(inferenceSessionOptions('cpu', 6).intraOpNumThreads, 6);
});

test('embedding cancellation retains execution reservation until the worker confirms idle; close waits for exit', async t => {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-inference-budget-'));
  const resourceService = resourceFixture();
  for (const asset of BUILTIN_EMBEDDING_PROFILE.files) {
    await mkdir(dirname(join(root, asset.path)), { recursive: true }); await writeFile(join(root, asset.path), 'Synthetic transport fixture.');
  }
  const worker = new EventEmitter();
  worker.ref = worker.unref = () => {};
  const messages = [];
  worker.postMessage = message => {
    messages.push(message);
    if (message.type === 'close') setImmediate(() => { worker.emit('message', { type: 'closed', disposed: true }); worker.emit('exit', 0); });
  };
  const service = new EmbeddingService({ modelRoot: root, workerFactory: () => worker, resourceService, cpuThreads: 12,
    requestLimits: { maxPendingRequests: 1 } });
  t.after(async () => { await service.close(); await rm(root, { recursive: true, force: true }); });
  const controller = new AbortController();
  const pending = service.embedQuery('Synthetic text', { signal: controller.signal });
  for (let attempt = 0; attempt < 100 && !messages.length; attempt++) await new Promise(resolve => setImmediate(resolve));
  assert.equal(messages[0].resourceBudget.cpuThreads, 6);
  controller.abort();
  await assert.rejects(pending, { code: 'EMBEDDING_CANCELLED' });
  assert.equal(service.status().inputAdmission.activeRequests, 1, 'native payload remains counted after its caller cancels');
  await assert.rejects(service.embedQuery('Cannot bypass the native input queue.'), { code: 'EMBEDDING_BUSY' });
  assert.equal(resourceService.leases.size, 2, 'cancelling a promise cannot release resources used by native inference');
  worker.emit('message', { type: 'idle', throughId: messages[0].id });
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(service.status().inputAdmission.activeRequests, 0);
  assert.equal(resourceService.leases.size, 1);
  assert.ok(service.status().resourceReservation.residentMemoryBytes > 0);
  await service.close();
  assert.equal(resourceService.leases.size, 0);
});

test('a native GPU crash disables that backend and a new caller can restart once on CPU', async t => {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-inference-crash-'));
  const resourceService = resourceFixture({ gpu: true });
  for (const asset of BUILTIN_EMBEDDING_PROFILE.files) {
    await mkdir(dirname(join(root, asset.path)), { recursive: true }); await writeFile(join(root, asset.path), 'Synthetic transport fixture.');
  }
  let starts = 0;
  const budgets = [];
  const workerFactory = () => {
    const generation = ++starts, worker = new EventEmitter();
    worker.ref = worker.unref = () => {};
    worker.postMessage = message => {
      if (message.type === 'close') {
        setImmediate(() => { worker.emit('message', { type: 'closed', disposed: true }); worker.emit('exit', 0); }); return;
      }
      if (message.type !== 'embed') return;
      budgets.push(message.resourceBudget);
      setImmediate(() => {
        if (generation === 1) { worker.emit('exit', 23); return; }
        worker.emit('message', { type: 'ready', inferenceBackend: { device: 'cpu', cpuThreads: message.resourceBudget.cpuThreads, dtype: 'q8', gpuValidated: false } });
        worker.emit('message', { type: 'result', id: message.id, vectors: message.texts.map(() => Array(384).fill(0.125)) });
        worker.emit('message', { type: 'idle', throughId: message.id });
      });
    };
    return worker;
  };
  const service = new EmbeddingService({ modelRoot: root, resourceService, workerFactory });
  t.after(async () => { await service.close(); await rm(root, { recursive: true, force: true }); });
  await assert.rejects(service.embedQuery('Synthetic GPU crash.'), { code: 'EMBEDDING_WORKER_FAILED' });
  assert.equal(starts, 1, 'a failed caller is not automatically replayed');
  assert.equal((await service.embedQuery('New synthetic caller.')).vector.length, 384);
  assert.equal(starts, 2);
  assert.notEqual(budgets[0].device, 'cpu');
  assert.equal(budgets[1].device, 'cpu');
  assert.equal(budgets[1].diagnostic.code, 'GPU_WORKER_FAILED');
  assert.equal(resourceService.requests.filter(request => request.gpuMemoryBytes > 0).length, 1);
  await service.close(); assert.equal(resourceService.leases.size, 0);
});

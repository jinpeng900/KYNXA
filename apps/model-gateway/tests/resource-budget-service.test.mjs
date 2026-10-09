import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { ResourceBudgetService, sharedResourceBudget } from '../platform/resources/resource-client.mjs';

const hardware = () => ({ cpu: { logicalCores: 8, usagePercent: 10 },
  memory: { totalBytes: 16 * 2 ** 30, availableBytes: 8 * 2 ** 30 },
  gpu: { state: 'unknown', availableMemoryBytes: null } });
const allocation = (taskId, cpuThreads, memoryBytes = 0) => ({ taskId, cpuThreads, memoryBytes, ttlMs: 1000 });

test('fallback reservations are atomic across clients and expiry remains quarantined until release', async () => {
  let now = 1000;
  const service = new ResourceBudgetService({ executablePath: null, sampler: hardware, clock: () => now });
  try {
    const [first, second] = await Promise.all([service.acquire(allocation('embedding', 4)), service.acquire(allocation('rerank', 4))]);
    assert.equal(first.status, 'granted'); assert.equal(second.status, 'denied');
    now += 2000;
    assert.equal((await service.snapshot()).quarantinedLeases, 1);
    assert.equal((await service.acquire(allocation('next', 1))).status, 'denied');
    await service.renew(first.leaseId, { ttlMs: 1000 });
    assert.equal((await service.snapshot()).activeLeases, 1);
    await service.release(first.leaseId);
    assert.equal((await service.acquire(allocation('next', 1))).status, 'granted');
  } finally { await service.close(); }
});

test('resident memory and active CPU have separate lifetimes and unknown GPU cannot be approved', async () => {
  const service = new ResourceBudgetService({ executablePath: null, sampler: hardware });
  try {
    const resident = await service.acquire(allocation('model-resident', 0, 500 * 2 ** 20));
    assert.equal(resident.cpuThreads, 0); assert.equal(resident.status, 'granted');
    const compute = await service.acquire(allocation('query', 2));
    await service.release(compute.leaseId);
    assert.equal((await service.snapshot()).budget.reservedMemoryBytes, 500 * 2 ** 20);
    assert.equal((await service.acquire({ taskId: 'gpu', gpuMemoryBytes: 1 })).reason, 'RESOURCE_GPU_UNKNOWN');
    assert.equal((await service.acquire(allocation('too-large', 0, 20 * 2 ** 30))).status, 'denied');
  } finally { await service.close(); }
});

test('measured 5.27 GiB free RAM permits E5 and reranker residents but real pressure still blocks expansion', async () => {
  let availableBytes = Math.floor(5.27 * 2 ** 30);
  const sample = () => ({ ...hardware(), memory: { totalBytes: 16 * 2 ** 30, availableBytes } });
  const service = new ResourceBudgetService({ executablePath: null, sampler: sample });
  try {
    const e5 = await service.acquire(allocation('e5-resident', 0, 556 * 2 ** 20));
    const reranker = await service.acquire(allocation('reranker-resident', 0, 1039 * 2 ** 20));
    assert.equal(e5.status, 'granted'); assert.equal(reranker.status, 'granted');
    availableBytes = 1024 * 2 ** 20;
    assert.equal((await service.acquire(allocation('pressure', 0, 128 * 2 ** 20))).status, 'denied');
    assert.equal((await service.snapshot()).budget.reservedMemoryBytes, (556 + 1039) * 2 ** 20);
  } finally { await service.close(); }
});

test('cancellation does not create a reservation and close rejects further work', async () => {
  const service = new ResourceBudgetService({ executablePath: null, sampler: hardware });
  await assert.rejects(service.acquire(allocation('cancelled', 1), { signal: AbortSignal.abort() }), { name: 'AbortError' });
  assert.equal((await service.snapshot()).activeLeases, 0);
  await service.close(); await service.close();
  await assert.rejects(service.acquire(allocation('closed', 1)), { code: 'RESOURCE_CLOSED' });
});

test('real fallback sampling reports available hardware while the first CPU reading remains unknown', async () => {
  const service = new ResourceBudgetService({ executablePath: null });
  try {
    const snapshot = await service.snapshot();
    assert.ok(snapshot.cpu.logicalCores > 0); assert.equal(snapshot.cpu.usagePercent, null);
    assert.ok(snapshot.memory.totalBytes > 0); assert.ok(snapshot.memory.availableBytes > 0);
    assert.equal(snapshot.gpu.state, 'unknown'); assert.equal(snapshot.gpu.availableMemoryBytes, null);
  } finally { await service.close(); }
});

test('owned native service timeout is bounded and preserves earlier reservations', async () => {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-resources-'));
  const fixture = join(root, 'native-fixture.cjs');
  await writeFile(fixture, `const readline=require('node:readline');
let count=0;
readline.createInterface({input:process.stdin}).on('line',line=>{
 const request=JSON.parse(line); let result;
 if(request.method==='health') result={mode:'rust',cpu:{logicalCores:8},memory:{availableBytes:8589934592}};
 else if(request.method==='acquire' && count++===0) result={status:'granted',mode:'rust',device:'cpu',leaseId:'owned-native',cpuThreads:4,memoryBytes:0,gpuMemoryBytes:0,expiresAt:Date.now()+30000};
 else return;
 process.stdout.write(JSON.stringify({id:request.id,result})+'\\n');
}).on('close',()=>process.exit(0));`);
  const service = new ResourceBudgetService({ executablePath: process.execPath, executableArgs: [fixture], timeoutMs: 300, sampler: hardware });
  try {
    const first = await service.acquire(allocation('first', 4)); assert.equal(first.mode, 'rust');
    const started = performance.now();
    assert.equal((await service.acquire(allocation('hang', 1))).reason, 'RESOURCE_SERVICE_LOST');
    assert.ok(performance.now() - started < 2000);
    assert.equal((await service.snapshot()).budget.reservedCpuThreads, 4);
    assert.equal((await service.acquire(allocation('fallback', 1))).status, 'denied');
    await service.release(first.leaseId);
    assert.equal((await service.acquire(allocation('fallback', 1))).status, 'granted');
  } finally { await service.close(); await rm(root, { recursive: true, force: true }); }
});

test('default gateway singleton shares its allocation authority and can be recreated after close', async () => {
  const first = sharedResourceBudget(); assert.equal(first, sharedResourceBudget());
  await first.close(); const next = sharedResourceBudget(); assert.notEqual(first, next); await next.close();
});

test('bounded waiting prefers foreground and grants aged background without resuming cancelled requests', async () => {
  let now = 1000;
  const service = new ResourceBudgetService({ executablePath: null, sampler: hardware, clock: () => now });
  try {
    const owner = await service.acquire(allocation('owner', 4));
    const background = service.acquire({ ...allocation('background', 4), waitMs: 5000 });
    const foreground = service.acquire({ ...allocation('foreground', 4), kind: 'foreground', waitMs: 5000 });
    await new Promise(resolve => setTimeout(resolve, 5)); now += 3000;
    await service.release(owner.leaseId);
    const aged = await background; assert.equal(aged.status, 'granted');
    assert.equal((await service.snapshot()).queuedRequests, 1);
    await service.release(aged.leaseId); const interactive = await foreground; assert.equal(interactive.status, 'granted');
    const controller = new AbortController();
    const cancelled = service.acquire({ ...allocation('cancelled', 4), waitMs: 1000 }, { signal: controller.signal });
    await new Promise(resolve => setTimeout(resolve, 5)); controller.abort();
    await assert.rejects(cancelled, { name: 'AbortError' });
    await service.release(interactive.leaseId);
    assert.equal((await service.snapshot()).activeLeases, 0); assert.equal((await service.snapshot()).queuedRequests, 0);
  } finally { await service.close(); }
});

test('feedback shrinks actual background grants and resource advice never exceeds approved memory', async () => {
  const service = new ResourceBudgetService({ executablePath: null, sampler: hardware });
  try {
    const initial = await service.acquire(allocation('feedback', 4, 128 * 2 ** 20));
    const reported = await service.report(initial.leaseId, { allocationFailure: true });
    assert.equal(reported.feedback.throughputPerSecond, null);
    await service.release(initial.leaseId);
    const smaller = await service.acquire(allocation('smaller', 4, 128 * 2 ** 20));
    assert.equal(smaller.cpuThreads, 2); assert.equal(smaller.suggestions.batchMultiplier, 0.5);
    assert.ok(smaller.suggestions.annCacheBytes <= smaller.memoryBytes);
    assert.equal((await service.registerExecutor(smaller.leaseId, { processId: process.pid })).executor.memoryBytes, null);
    await assert.rejects(service.report(smaller.leaseId, { inventedMetric: true }), { code: 'RESOURCE_INVALID_FEEDBACK' });
  } finally { await service.close(); }
});

test('waiting has a finite deadline rather than failing the monitor itself', async () => {
  const service = new ResourceBudgetService({ executablePath: null, sampler: hardware });
  try {
    await service.acquire(allocation('owner', 4));
    const result = await service.acquire({ ...allocation('deadline', 4), waitMs: 20 });
    assert.equal(result.reason, 'RESOURCE_WAIT_TIMEOUT'); assert.equal(service.status().mode, 'fallback');
  } finally { await service.close(); }
});

test('idle advice and comparable hot throughput explore beyond startup while pressure cuts approved capacity immediately', async () => {
  let now = 1000;
  const service = new ResourceBudgetService({ executablePath: null, sampler: hardware, clock: () => now });
  try {
    const initial = await service.acquire(allocation('adaptive', 1, 8 * 2 ** 20));
    assert.equal(initial.suggestions.batchProbeMultiplier, 2);
    assert.equal(initial.suggestions.rerankCandidateLimit, 60);
    const feedback = { phase: 'hot-inference', unit: 'tokens', backend: 'cpu', sequenceTokens: 64,
      inputTokens: 64, latencyMs: 10, throughputPerSecond: 6400, queueDepth: 128, batchSize: 1, cpuThreads: 1 };
    await service.report(initial.leaseId, feedback);
    now += 6000;
    const grown = await service.report(initial.leaseId, { ...feedback, inputTokens: 128, latencyMs: 15, throughputPerSecond: 128000 / 15, batchSize: 2 });
    assert.equal(grown.feedback.backgroundFraction, 2, 'larger batches compare cost per token rather than whole-batch latency');
    assert.equal(grown.feedback.latencyMs, 11.25, 'public latency remains milliseconds per batch');
    now += 6000;
    const separated = await service.report(initial.leaseId, { ...feedback, sequenceTokens: 512, inputTokens: 512,
      latencyMs: 5, throughputPerSecond: 102400 });
    assert.equal(separated.feedback.backgroundFraction, 2, 'different sequence classes cannot manufacture a throughput gain');
    now += 6000;
    const peak = await service.report(initial.leaseId, { ...feedback, throughputPerSecond: 12800, latencyMs: 5 });
    assert.equal(peak.feedback.backgroundFraction, 4);
    const pressure = await service.report(initial.leaseId, { backend: 'cpu', foregroundLatencyMs: 300 });
    assert.equal(pressure.feedback.backgroundFraction, 0.5);
    await service.release(initial.leaseId);
    const smaller = await service.acquire(allocation('adaptive', 4, 128 * 2 ** 20));
    assert.equal(smaller.cpuThreads, 2);
    assert.equal(smaller.suggestions.rerankCandidateLimit, 20);
    assert.equal(smaller.suggestions.batchProbeMultiplier, 0.5);
    await assert.rejects(service.report(smaller.leaseId, { sequenceTokens: 513 }), { code: 'RESOURCE_INVALID_FEEDBACK' });
  } finally { await service.close(); }
});

test('a one-thread search lease plans reranking from remaining authority headroom and actual pressure', async () => {
  let usagePercent = 10, availableBytes = 8 * 2 ** 30;
  const service = new ResourceBudgetService({ executablePath: null,
    sampler: () => ({ ...hardware(), cpu: { logicalCores: 8, usagePercent }, memory: { totalBytes: 16 * 2 ** 30, availableBytes } }) });
  try {
    for (const [cpuThreads, expected] of [[1, 60], [2, 40], [4, 20]]) {
      const lease = await service.acquire(allocation('reranker', cpuThreads));
      assert.equal(lease.suggestions.rerankCandidates, expected); await service.release(lease.leaseId);
    }
    availableBytes = 1024 ** 3;
    const constrained = await service.acquire(allocation('reranker', 1, 8 * 2 ** 20));
    assert.equal(constrained.suggestions.rerankCandidateLimit, 20); await service.release(constrained.leaseId);
    usagePercent = 86;
    const pressure = await service.acquire(allocation('reranker', 4));
    assert.equal(pressure.suggestions.rerankCandidates, 20);
  } finally { await service.close(); }
});

import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm, access } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { spawn } from 'node:child_process';
import { ResourceBudgetService } from '../platform/resources/resource-client.mjs';

const hardware = () => ({ cpu: { logicalCores: 8, usagePercent: 10 },
  memory: { totalBytes: 16 * 2 ** 30, availableBytes: 8 * 2 ** 30 }, gpu: { state: 'unknown' } });
const allocation = taskId => ({ taskId, cpuThreads: 4, memoryBytes: 0 });

test('feedback histories isolate device, phase and throughput unit while cold loading cannot grow hot batches', async () => {
  let now = 1000;
  const service = new ResourceBudgetService({ executablePath: null, sampler: hardware, clock: () => now });
  try {
    const first = await service.acquire(allocation('same-task'));
    await service.report(first.leaseId, { backend: 'cpu', allocationFailure: true });
    const report = (phase, unit, throughputPerSecond, backend = 'cpu') => service.report(first.leaseId,
      { phase, unit, backend, throughputPerSecond, latencyMs: 10, queueDepth: 10 });
    await report('cold-load', 'documents', 10); now = 10000;
    assert.equal((await report('cold-load', 'documents', 100)).feedback.adjustmentReason, 'non-hot-sample-held');
    await report('hot-inference', 'tokens', 100000); now = 11000;
    await report('hot-inference', 'tokens', 200000, 'dml');
    await service.release(first.leaseId);
    const isolated = await service.acquire(allocation('same-task'));
    assert.equal(isolated.cpuThreads, 2); assert.equal(isolated.suggestions.batchMultiplier, 0.5);
    await service.report(isolated.leaseId, { phase: 'hot-inference', unit: 'tokens', backend: 'cpu',
      throughputPerSecond: 200000, latencyMs: 9, queueDepth: 10 });
    await service.release(isolated.leaseId);
    const grown = await service.acquire(allocation('same-task'));
    assert.equal(grown.suggestions.batchMultiplier, 0.625);
    await assert.rejects(service.report(grown.leaseId, { backend: 'imaginary-gpu' }), { code: 'RESOURCE_INVALID_FEEDBACK' });
  } finally { await service.close(); }
});

test('monitor restart restores the held ledger before new admission and retains debt after failed recovery', async () => {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-resource-recovery-'));
  const fixture = join(root, 'monitor.cjs');
  await writeFile(fixture, `const readline=require('node:readline');
let granted=false,restored=[];
readline.createInterface({input:process.stdin}).on('line',line=>{
 const request=JSON.parse(line);let result;
 if(request.method==='health'||request.method==='snapshot')result={mode:'rust',cpu:{logicalCores:8},memory:{availableBytes:8589934592}};
 else if(request.method==='restore'){if(process.argv.includes('reject-restore'))result={status:'denied',reason:'RESOURCE_RESTORE_INVALID'};
   else {restored=request.params.leases;result={status:'restored',restoredLeases:restored.length};}}
 else if(request.method==='reconcile')result={mode:'rust',status:'reconciled',budget:{reservedCpuThreads:restored.reduce((n,l)=>n+l.cpuThreads,0)}};
 else if(request.method==='release'){restored=restored.filter(l=>l.leaseId!==request.params.leaseId);result={status:'released'};}
 else if(request.method==='acquire'&&!granted&&restored.length===0){granted=true;result={status:'granted',mode:'rust',device:'cpu',leaseId:'owned-native',cpuThreads:4,memoryBytes:0,gpuMemoryBytes:0,expiresAt:Date.now()+30000};}
 else if(request.method==='acquire'&&restored.length)result={status:'denied',reason:'RESOURCE_PRESSURE'};
 else if(request.method==='acquire'){process.exit(1);return;}
 else return;
 process.stdout.write(JSON.stringify({id:request.id,result})+'\\n');
}).on('close',()=>process.exit(0));`);
  // Startup competes with other test files; await the actual handshake rather than treating a fallback grant as native.
  // 启动可能与其他测试文件竞争；等待实际握手，不把 CPU 降级批准当成原生批准。
  const service = new ResourceBudgetService({ executablePath: process.execPath, executableArgs: [fixture], timeoutMs: 5000, sampler: hardware });
  try {
    assert.equal((await service.snapshot()).mode, 'rust');
    const lease = await service.acquire(allocation('owned'));
    assert.equal(lease.mode, 'rust');
    assert.equal((await service.acquire(allocation('hang'))).reason, 'RESOURCE_SERVICE_LOST');
    const isolated = await service.snapshot();
    assert.equal(isolated.quarantinedLeases, 1); assert.equal(isolated.accounting.observedMaterializedMemoryBytes, 0);
    const restored = await service.reconcile({ restartService: true });
    assert.equal(restored.status, 'reconciled'); assert.equal(restored.budget.reservedCpuThreads, 4);
    assert.equal((await service.acquire(allocation('must-wait'))).reason, 'RESOURCE_PRESSURE');
    await service.release(lease.leaseId);
    assert.equal((await service.acquire(allocation('after-release'))).status, 'granted');
    const failing = new ResourceBudgetService({ executablePath: process.execPath, executableArgs: [fixture, 'reject-restore'],
      timeoutMs: 5000, sampler: hardware });
    try {
      assert.equal((await failing.snapshot()).mode, 'rust');
      assert.equal((await failing.acquire(allocation('owned'))).mode, 'rust');
      assert.equal((await failing.acquire(allocation('hang'))).reason, 'RESOURCE_SERVICE_LOST');
      assert.equal((await failing.reconcile({ restartService: true })).reason, 'RESOURCE_RESTORE_FAILED');
      assert.equal((await failing.snapshot()).budget.reservedCpuThreads, 4);
      assert.equal((await failing.acquire(allocation('still-fenced'))).status, 'denied');
    } finally { await failing.close(); }
  } finally { await service.close(); await rm(root, { recursive: true, force: true }); }
});

test('real Windows accounting credits owned RSS growth once across shared-PID leases', async context => {
  if (process.platform !== 'win32') return context.skip('Windows native accounting only.');
  const executablePath = resolve('artifacts/runtime/resources/win-x64/kynxa-resource-service.exe');
  try { await access(executablePath); } catch { return context.skip('Prepared Windows resource runtime is required.'); }
  const service = new ResourceBudgetService({ executablePath, timeoutMs: 5000 });
  let retainedBuffer;
  try {
    assert.equal((await service.snapshot()).mode, 'rust');
    const first = await service.acquire({ taskId: 'rss-first', memoryBytes: 96 * 2 ** 20 });
    const second = await service.acquire({ taskId: 'rss-second', memoryBytes: 96 * 2 ** 20 });
    assert.equal(first.status, 'granted'); assert.equal(second.status, 'granted');
    assert.equal((await service.registerExecutor(first.leaseId, { processId: process.pid })).status, 'registered');
    assert.equal((await service.registerExecutor(second.leaseId, { processId: process.pid })).status, 'registered');
    retainedBuffer = Buffer.alloc(96 * 2 ** 20, 3);
    const snapshot = await service.reconcile();
    assert.equal(snapshot.mode, 'rust'); assert.equal(snapshot.executors.processes.length, 1);
    assert.ok(snapshot.accounting.observedMaterializedMemoryBytes >= 64 * 2 ** 20);
    assert.ok(snapshot.accounting.observedMaterializedMemoryBytes <= 192 * 2 ** 20);
    assert.equal(snapshot.accounting.observedMaterializedMemoryBytes + snapshot.accounting.unmaterializedMemoryBytes,
      snapshot.budget.reservedMemoryBytes);
    assert.equal(retainedBuffer[0], 3);
    await service.release(first.leaseId); await service.release(second.leaseId);
    assert.equal((await service.reconcile()).budget.reservedMemoryBytes, 0);
  } finally { retainedBuffer = undefined; await service.close(); }
});

test('real Windows recovery retires a confirmed exited owned helper without relying on TTL or recycled PID', async context => {
  if (process.platform !== 'win32') return context.skip('Windows native accounting only.');
  const executablePath = resolve('artifacts/runtime/resources/win-x64/kynxa-resource-service.exe');
  try { await access(executablePath); } catch { return context.skip('Prepared Windows resource runtime is required.'); }
  const service = new ResourceBudgetService({ executablePath, timeoutMs: 5000 });
  const helper = spawn(process.execPath, ['-e', 'setInterval(()=>{},1000)'], { windowsHide: true, stdio: 'ignore' });
  const helperExit = new Promise(resolveExit => helper.once('close', resolveExit));
  try {
    assert.equal((await service.snapshot()).mode, 'rust');
    const lease = await service.acquire({ taskId: 'recovery-helper', kind: 'foreground', cpuThreads: 1, memoryBytes: 32 * 2 ** 20 });
    assert.equal(lease.status, 'granted');
    assert.equal(lease.mode, 'rust');
    assert.equal((await service.registerExecutor(lease.leaseId, { processId: helper.pid })).status, 'registered');
    // Only this test's owned monitor and helper are stopped; a user application is never inspected or killed.
    // 只结束本夹具创建的监控与子进程，不检查或结束用户软件。
    const monitorProcessId = service.status().processId;
    assert.ok(Number.isSafeInteger(monitorProcessId) && monitorProcessId > 0);
    process.kill(monitorProcessId);
    helper.kill(); await helperExit;
    const unavailable = await service.snapshot();
    assert.equal(unavailable.mode, 'fallback'); assert.equal(unavailable.budget.reservedMemoryBytes, 32 * 2 ** 20);
    const recovered = await service.reconcile({ restartService: true });
    assert.equal(recovered.mode, 'rust'); assert.equal(recovered.budget.reservedMemoryBytes, 0);
    assert.equal(service.status().activeReservations, 0);
  } finally { helper.kill(); await helperExit; await service.close(); }
});

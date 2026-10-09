import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { setImmediate as nextTurn } from 'node:timers/promises';
import { test } from 'node:test';
import { ResourceBudgetService } from '../platform/resources/resource-client.mjs';

const MIB = 1024 * 1024;
const hardware = () => ({ cpu: { logicalCores: 8, usagePercent: 10 },
  memory: { totalBytes: 16 * 1024 ** 3, availableBytes: 8 * 1024 ** 3 }, gpu: { state: 'unknown' } });

class ControlledMonitor extends EventEmitter {
  constructor(state, number) {
    super(); this.state = state; this.pid = 700000 + number; this.requests = []; this.leases = new Map(); this.closed = false;
    this.killCalls = 0; this.stdout = new EventEmitter(); this.stdin = new EventEmitter();
    this.stdin.write = (frame, callback) => {
      const request = JSON.parse(frame.toString()); this.requests.push(request);
      queueMicrotask(async () => { callback?.(null); await this.handle(request); });
    };
    this.stdin.destroy = () => {};
    this.stdin.end = () => this.finish();
  }
  ref() {} unref() {}
  kill() { this.killCalls++; if (!this.state.holdExit) this.finish(); return true; }
  finish() { if (!this.closed) { this.closed = true; queueMicrotask(() => this.emit('close', 0)); } }
  snapshot() {
    const leases = [...this.leases.values()];
    return { mode: 'rust', cpu: { logicalCores: 8 }, memory: { availableBytes: 8 * 1024 ** 3 },
      budget: { reservedCpuThreads: leases.reduce((sum, lease) => sum + lease.cpuThreads, 0),
        reservedMemoryBytes: leases.reduce((sum, lease) => sum + lease.memoryBytes, 0),
        reservedGpuMemoryBytes: leases.reduce((sum, lease) => sum + lease.gpuMemoryBytes, 0) } };
  }
  async handle(request) {
    const { method, params } = request;
    let result;
    if (method === 'acquire' && params.taskId === 'lose') { this.emit('error', new Error('synthetic lost monitor')); return; }
    if (method === 'health' || method === 'snapshot') result = this.snapshot();
    else if (method === 'restore') {
      this.state.restoreStarted?.(); await this.state.restoreGate;
      if (this.state.rejectRestore) result = { status: 'denied', reason: 'RESOURCE_RESTORE_INVALID' };
      else { this.leases = new Map(params.leases.map(lease => [lease.leaseId, lease]));
        result = { status: 'restored', restoredLeases: this.leases.size }; }
    } else if (method === 'registerExecutor') result = { status: 'registered', executor: params };
    else if (method === 'recoverExecutor') result = this.state.rejectExecutor
      ? { status: 'denied', reason: 'RESOURCE_EXECUTOR_INVALID' } : { status: 'registered', executor: params };
    else if (method === 'reconcile') result = { ...this.snapshot(), status: 'reconciled' };
    else if (method === 'acquire') {
      const leaseId = `controlled-${this.pid}-${this.leases.size}`;
      result = { ...params, leaseId, status: 'granted', mode: 'rust', expiresAt: Date.now() + params.ttlMs,
        device: params.gpuMemoryBytes ? 'gpu' : 'cpu', ...(params.gpuMemoryBytes ?
          { deviceId: 0, executionProvider: 'dml', executionDeviceId: 0 } : {}) };
      this.leases.set(leaseId, result);
    } else if (method === 'release') { this.leases.delete(params.leaseId); result = { status: 'released' }; }
    else if (method === 'renew') result = { status: 'renewed', expiresAt: Date.now() + params.ttlMs };
    else result = { status: 'reported' };
    if (!this.closed) this.stdout.emit('data', Buffer.from(JSON.stringify({ id: request.id, result }) + '\n'));
  }
}

function fixture(t, state = {}) {
  let now = 100000;
  const monitors = [];
  const service = new ResourceBudgetService({ executablePath: process.execPath, timeoutMs: 50, sampler: hardware,
    clock: () => now, processFactory: () => { const monitor = new ControlledMonitor(state, monitors.length);
      monitors.push(monitor); return monitor; } });
  t.after(async () => { for (const monitor of monitors) monitor.finish(); await service.close(); });
  return { service, monitors, state, advance: ms => { now += ms; } };
}

async function ownedLease(service, extra = {}) {
  return service.acquire({ taskId: 'owned', cpuThreads: 1, memoryBytes: 128 * MIB, gpuMemoryBytes: 256 * MIB, ...extra });
}

test('fresh concurrent requests recover once, restore all debt and birth identities before new admission, and ignore late old IPC', async t => {
  const { service, monitors, advance } = fixture(t);
  const lease = await ownedLease(service);
  await service.registerExecutor(lease.leaseId, { processId: 7654321, startTimeMs: 123000 });
  assert.equal((await service.acquire({ taskId: 'lose', cpuThreads: 1 })).reason, 'RESOURCE_SERVICE_LOST');
  const unavailable = await service.snapshot();
  assert.equal(unavailable.mode, 'fallback'); assert.equal(unavailable.budget.reservedGpuMemoryBytes, 256 * MIB);
  assert.equal(service.status().recoveryState, 'backoff');
  advance(1001);
  const [snapshot, next] = await Promise.all([service.snapshot(), service.acquire({ taskId: 'fresh', cpuThreads: 1 })]);
  assert.equal(snapshot.mode, 'rust'); assert.equal(next.mode, 'rust'); assert.equal(monitors.length, 2);
  const requests = monitors[1].requests, restore = requests.find(request => request.method === 'restore');
  assert.equal(restore.params.leases.length, 1); assert.equal(restore.params.leases[0].gpuMemoryBytes, 256 * MIB);
  assert.equal(restore.params.leases[0].memoryBytes, 128 * MIB);
  assert.equal(requests.find(request => request.method === 'recoverExecutor').params.startTimeMs, 123000);
  assert.ok(requests.findIndex(request => request.method === 'recoverExecutor') < requests.findIndex(request => request.method === 'acquire'));
  monitors[0].stdin.emit('error', new Error('late old pipe error'));
  monitors[0].stdout.emit('data', Buffer.from('invalid old frame\n'));
  assert.equal((await service.snapshot()).mode, 'rust');
  assert.equal(service.status().automaticRecovery.attempts, 1);
  await service.release(lease.leaseId); await service.release(next.leaseId);
});

test('failed restoration backs off boundedly while RAM and unknown GPU reservations remain fenced', async t => {
  const { service, monitors, state, advance } = fixture(t, { rejectRestore: true });
  await ownedLease(service);
  await service.acquire({ taskId: 'lose', cpuThreads: 1 });
  advance(1001);
  const first = await service.snapshot();
  assert.equal(first.mode, 'fallback'); assert.equal(first.budget.reservedMemoryBytes, 128 * MIB);
  assert.equal(first.budget.reservedGpuMemoryBytes, 256 * MIB);
  assert.equal(service.status().automaticRecovery.reason, 'RESOURCE_RESTORE_FAILED');
  for (let attempt = 0; attempt < 3; attempt++) await service.snapshot();
  assert.equal(monitors.length, 2);
  advance(1001); await service.snapshot();
  assert.equal(service.status().automaticRecovery.consecutiveFailures, 2);
  assert.equal(service.status().automaticRecovery.retryAt, 104002);
  state.rejectRestore = false; await service.snapshot(); assert.equal(monitors.length, 3);
  advance(2001); const recovered = await service.snapshot();
  assert.equal(recovered.mode, 'rust'); assert.equal(recovered.budget.reservedGpuMemoryBytes, 256 * MIB);
  assert.equal(service.status().automaticRecovery.consecutiveFailures, 0);
});

test('unconfirmed executor restoration cannot reopen native admission', async t => {
  const { service, state, advance } = fixture(t, { rejectExecutor: true });
  const lease = await ownedLease(service);
  await service.registerExecutor(lease.leaseId, { processId: 7654321, startTimeMs: 123000 });
  await service.acquire({ taskId: 'lose', cpuThreads: 1 }); advance(1001);
  const failed = await service.snapshot();
  assert.equal(failed.mode, 'fallback'); assert.equal(failed.budget.reservedMemoryBytes, 128 * MIB);
  assert.equal(service.status().errorCode, 'RESOURCE_RESTORE_FAILED');
  state.rejectExecutor = false; advance(1001);
  assert.equal((await service.snapshot()).mode, 'rust');
});

test('a still alive retired monitor blocks replacement until its actual close receipt', async t => {
  const { service, monitors, state, advance } = fixture(t, { holdExit: true });
  await ownedLease(service);
  await service.acquire({ taskId: 'lose', cpuThreads: 1 }); advance(1001);
  const blocked = await service.snapshot();
  assert.equal(monitors.length, 1); assert.equal(blocked.mode, 'fallback');
  assert.equal(blocked.budget.reservedGpuMemoryBytes, 256 * MIB);
  assert.equal(service.status().automaticRecovery.reason, 'RESOURCE_SERVICE_RETIREMENT_PENDING');
  monitors[0].finish(); state.holdExit = false; advance(1001);
  assert.equal((await service.snapshot()).mode, 'rust'); assert.equal(monitors.length, 2);
});

test('closing during restoration retires the new monitor and does not keep automatic recovery alive', async t => {
  let restoreStarted, releaseRestore;
  const entered = new Promise(resolve => { restoreStarted = resolve; });
  const restoreGate = new Promise(resolve => { releaseRestore = resolve; });
  const { service, monitors, advance } = fixture(t, { restoreStarted, restoreGate });
  t.after(() => releaseRestore());
  await ownedLease(service); await service.acquire({ taskId: 'lose', cpuThreads: 1 }); advance(1001);
  const recovering = service.snapshot(); await entered;
  const closing = service.close(); releaseRestore();
  await assert.rejects(recovering, { code: 'RESOURCE_CLOSED' }); await closing;
  assert.equal(service.status().mode, 'closed'); assert.equal(service.status().activeReservations, 0);
  assert.equal(service.status().automaticRecovery.retryAt, null);
  assert.ok(monitors.every(monitor => monitor.closed));
});

test('fallback waiters return through native admission after recovery rather than creating an untracked fallback lease', async t => {
  const { service, monitors, advance } = fixture(t);
  await ownedLease(service, { cpuThreads: 4 });
  await service.acquire({ taskId: 'lose', cpuThreads: 1 });
  const waiting = service.acquire({ taskId: 'queued', cpuThreads: 1, waitMs: 3000 });
  await nextTurn(); assert.equal((await service.snapshot()).queuedRequests, 1);
  advance(1001); assert.equal((await service.snapshot()).mode, 'rust');
  const granted = await waiting;
  assert.equal(granted.mode, 'rust');
  assert.ok(monitors[1].requests.some(request => request.method === 'acquire' && request.params.taskId === 'queued'));
  assert.equal((await service.snapshot()).budget.reservedCpuThreads, 5);
});

test('missing bundled runtime stays on explicit fallback without launching retry processes', async () => {
  const service = new ResourceBudgetService({ executablePath: null, sampler: hardware,
    processFactory: () => { throw new Error('missing runtime must not spawn'); } });
  try {
    for (let attempt = 0; attempt < 3; attempt++) {
      assert.equal((await service.snapshot()).mode, 'fallback');
      const lease = await service.acquire({ taskId: 'fallback', cpuThreads: 1 }); await service.release(lease.leaseId);
    }
    assert.equal(service.status().recoveryState, 'native-monitor-not-bundled');
    assert.equal(service.status().automaticRecovery.attempts, 0); assert.equal(service.status().automaticRecovery.retryAt, null);
  } finally { await service.close(); }
});

test('real isolated Node IPC automatically restarts and restores its held ledger through the production spawn path', async () => {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-resource-auto-ipc-'));
  const script = join(root, 'monitor.cjs'), log = join(root, 'events.jsonl');
  await writeFile(script, `const fs=require('node:fs'),readline=require('node:readline');
const leases=new Map(),log=process.argv[2];let sequence=0;
readline.createInterface({input:process.stdin}).on('line',line=>{
 const {id,method,params}=JSON.parse(line);let result;
 fs.appendFileSync(log,JSON.stringify({pid:process.pid,method,params})+'\\n');
 if(method==='acquire'&&params.taskId==='lose'){process.exit(1);return;}
 if(method==='health'||method==='snapshot'||method==='reconcile')result={mode:'rust',cpu:{logicalCores:8},
  memory:{availableBytes:8589934592},budget:{reservedMemoryBytes:[...leases.values()].reduce((sum,lease)=>sum+lease.memoryBytes,0)},
  ...(method==='reconcile'?{status:'reconciled'}:{})};
 else if(method==='restore'){for(const lease of params.leases)leases.set(lease.leaseId,lease);
  result={status:'restored',restoredLeases:leases.size};}
 else if(method==='acquire'){result={...params,leaseId:'isolated-'+process.pid+'-'+(++sequence),status:'granted',mode:'rust',
  device:'cpu',expiresAt:Date.now()+params.ttlMs};leases.set(result.leaseId,result);}
 else if(method==='release'){leases.delete(params.leaseId);result={status:'released'};}
 else result={status:'registered',executor:params};
 process.stdout.write(JSON.stringify({id,result})+'\\n');
}).on('close',()=>process.exit(0));`);
  let now = 100000;
  const service = new ResourceBudgetService({ executablePath: process.execPath, executableArgs: [script, log],
    timeoutMs: 5000, sampler: hardware, clock: () => now });
  try {
    const lease = await service.acquire({ taskId: 'owned', cpuThreads: 1, memoryBytes: 32 * MIB });
    assert.equal(lease.mode, 'rust'); const previousPid = service.status().processId;
    assert.equal((await service.acquire({ taskId: 'lose', cpuThreads: 1 })).reason, 'RESOURCE_SERVICE_LOST');
    now += 1001;
    assert.equal((await service.snapshot()).mode, 'rust'); assert.notEqual(service.status().processId, previousPid);
    const next = await service.acquire({ taskId: 'fresh', cpuThreads: 1 }); assert.equal(next.mode, 'rust');
    const events = (await readFile(log, 'utf8')).trim().split('\n').map(line => JSON.parse(line));
    const restored = events.find(event => event.method === 'restore');
    assert.equal(restored.params.leases.length, 1); assert.equal(restored.params.leases[0].memoryBytes, 32 * MIB);
    assert.ok(events.indexOf(restored) < events.findIndex(event => event.method === 'acquire' && event.params.taskId === 'fresh'));
  } finally {
    await service.close();
    const suffix = relative(resolve(tmpdir()), resolve(root));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(root, { recursive: true, force: true });
  }
});

import { spawn } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { availableParallelism, cpus, freemem, totalmem } from 'node:os';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { randomUUID } from 'node:crypto';

const MAX_FRAME_BYTES = 65_536;
const MAX_PENDING_REQUESTS = 32;
const MAX_LEASES = 128;
const DEFAULT_TTL_MS = 30_000;
const MAX_TTL_MS = 300_000;
const MAX_WAIT_MS = 60_000;
const moduleDirectory = dirname(fileURLToPath(import.meta.url));

export class ResourceBudgetError extends Error {
  constructor(code, message) { super(message); this.name = 'ResourceBudgetError'; this.code = code; }
}

function cancellationError() {
  const error = new ResourceBudgetError('RESOURCE_CANCELLED', 'Resource allocation was cancelled.');
  error.name = 'AbortError'; return error;
}

function resourceExecutablePath() {
  const executable = process.platform === 'win32' ? 'kynxa-resource-service.exe' : 'kynxa-resource-service';
  const candidates = [resolve(moduleDirectory, '../../../runtime/resource', executable),
    resolve(moduleDirectory, '../../../../artifacts/runtime/resources', process.platform === 'win32' ? 'win-x64' : `${process.platform}-${process.arch}`, executable)];
  if (existsSync(candidates[0])) return candidates[0];
  try {
    if (existsSync(candidates[1]) && readFileSync(resolve(dirname(candidates[1]), 'bundle-files.txt'), 'utf8')
        .split(/\r?\n/u).includes(executable)) return candidates[1];
  } catch { /* Invalid development caches remain on the explicit CPU fallback. 无效开发缓存明确保持 CPU 降级。 */ }
  return undefined;
}

function validRequest(request) {
  return request && typeof request.taskId === 'string' && request.taskId.length > 0 && request.taskId.length <= 128 &&
    typeof request.workspaceId === 'string' && request.workspaceId.length <= 128 && ['foreground', 'background'].includes(request.kind) &&
    Number.isSafeInteger(request.cpuThreads) && request.cpuThreads >= 0 && request.cpuThreads <= 4096 &&
    Number.isSafeInteger(request.memoryBytes) && request.memoryBytes >= 0 &&
    Number.isSafeInteger(request.gpuMemoryBytes) && request.gpuMemoryBytes >= 0 &&
    request.cpuThreads + request.memoryBytes + request.gpuMemoryBytes > 0 &&
    Number.isSafeInteger(request.ttlMs) && request.ttlMs >= 1000 && request.ttlMs <= MAX_TTL_MS &&
    Number.isSafeInteger(request.waitMs) && request.waitMs >= 0 && request.waitMs <= MAX_WAIT_MS;
}

// A single gateway owner controls bounded IPC and all fallback reservations.
// 单一网关拥有者统一管理有界 IPC 和降级预约，模型服务不得各自重复计算可分配资源。
export class ResourceBudgetService {
  #executablePath;
  #executableArgs;
  #timeoutMs;
  #child;
  #exit;
  #pending = new Map();
  #requestId = 0;
  #buffer = Buffer.alloc(0);
  #leases = new Map();
  #mode = 'uninitialized';
  #failureCode;
  #closed = false;
  #closing;
  #sampler;
  #clock;
  #lastSample;
  #lastCpuTimes;
  #starting;
  #nativeSnapshot;
  #fallbackWaiters = new Map();
  #fallbackTimer;
  #backgroundFraction = 1;
  #feedback = { throughputPerSecond: null, latencyMs: null, queueDepth: null, lastAdjustmentMs: 0, reports: 0 };
  #foregroundStreak = 0;
  #feedbackHistories = new Map();
  #feedbackControls = new Map();
  #recovering;

  constructor({ executablePath = resourceExecutablePath(), executableArgs = [], timeoutMs = 2000,
    sampler, clock = Date.now } = {}) {
    this.#executablePath = executablePath;
    this.#executableArgs = executableArgs;
    this.#timeoutMs = Math.max(50, Math.min(10_000, timeoutMs));
    this.#sampler = sampler; this.#clock = clock;
  }

  status() {
    return { mode: this.#mode, closed: this.#closed, activeReservations: this.#leases.size,
      recoveryState: this.#recovering ? 'reconciling' : this.#failureCode && this.#leases.size ? 'reservations-quarantined' : 'ready',
      ...(this.#child?.pid ? { processId: this.#child.pid } : {}),
      gpu: this.#mode === 'rust' && this.#nativeSnapshot?.gpu ? { ...this.#nativeSnapshot.gpu }
        : { state: 'unknown', availableMemoryBytes: null },
      ...(this.#failureCode ? { errorCode: this.#failureCode } : {}) };
  }

  async snapshot({ signal } = {}) {
    if (this.#recovering) await this.#recovering;
    this.#assertOpen(signal); await this.#start();
    if (this.#mode === 'rust') {
      try { this.#nativeSnapshot = await this.#request('snapshot'); return this.#nativeSnapshot; }
      catch { /* Existing reservations remain quarantined in the gateway. 既有预约继续由网关隔离保留。 */ }
    }
    this.#assertOpen(signal);
    const hardware = this.#sample(), now = this.#clock();
    const { cpuThreads, memoryBytes, gpuMemoryBytes } = this.#reserved();
    const activeLeases = [...this.#leases.values()].filter(lease => lease.expiresAt > now && lease.mode !== 'rust').length;
    return { ...hardware, mode: 'fallback', budget: { cpuThreads: this.#cpuCapacity(hardware),
      memoryBytes: this.#memoryCapacity(hardware), gpuMemoryBytes: null,
      reservedCpuThreads: cpuThreads, reservedMemoryBytes: memoryBytes, reservedGpuMemoryBytes: gpuMemoryBytes },
      accounting: { observedMaterializedMemoryBytes: 0, unmaterializedMemoryBytes: memoryBytes,
        capacityMemoryBytes: this.#memoryCapacity(hardware), maximumResidentReservationBytes: Math.floor(hardware.memory.totalBytes * 0.4),
        availableMemoryBytes: Math.max(0, this.#memoryCapacity(hardware) - memoryBytes),
        gpuMemoryState: 'unverified-reservation', gpuUnverifiedReservationBytes: gpuMemoryBytes,
        state: 'monitor-unavailable-reservations-quarantined' },
      activeLeases, quarantinedLeases: this.#leases.size - activeLeases, maxLeases: MAX_LEASES, sampledAt: now,
      queuedRequests: this.#fallbackWaiters.size, feedback: { ...this.#feedback, backgroundFraction: this.#backgroundFraction },
      executors: { measurementState: 'unknown', reason: 'RESOURCE_NATIVE_MONITOR_UNAVAILABLE', registeredLeases:
        [...this.#leases.values()].filter(lease => lease.executor).length, processes: [] },
      ...(this.#failureCode ? { errorCode: this.#failureCode } : {}) };
  }

  async acquire(options, { signal } = {}) {
    this.#assertOpen(signal);
    if (this.#recovering) await this.#recovering;
    const request = { workspaceId: '', kind: 'background', cpuThreads: 0, memoryBytes: 0,
      gpuMemoryBytes: 0, ttlMs: DEFAULT_TTL_MS, waitMs: 0, ...options };
    if (!validRequest(request)) throw new ResourceBudgetError('RESOURCE_INVALID_REQUEST', 'Invalid resource allocation request.');
    await this.#start(); this.#assertOpen(signal);
    let result;
    if (this.#mode === 'rust') {
      try { result = await this.#request('acquire', request, { signal, timeoutMs: this.#timeoutMs + request.waitMs }); }
      catch (error) { if (signal?.aborted || error.name === 'AbortError') throw cancellationError();
        return { status: 'denied', reason: 'RESOURCE_SERVICE_LOST', mode: 'fallback' }; }
    } else {
      result = this.#acquireFallback(request);
      if (result.status === 'denied' && result.reason === 'RESOURCE_PRESSURE' && request.waitMs > 0)
        result = await this.#waitFallback(request, signal);
    }
    if (result.status === 'granted') {
      if (typeof result.leaseId !== 'string' || !Number.isSafeInteger(result.cpuThreads) ||
          result.cpuThreads < 0 || result.cpuThreads > request.cpuThreads || result.memoryBytes !== request.memoryBytes ||
          result.gpuMemoryBytes !== request.gpuMemoryBytes || !Number.isSafeInteger(result.expiresAt) ||
          request.gpuMemoryBytes > 0 && (result.mode !== 'rust' || result.device !== 'gpu' || result.deviceId !== 0 ||
            result.executionProvider !== undefined && (result.executionProvider !== 'dml' ||
              !Number.isSafeInteger(result.executionDeviceId) || result.executionDeviceId < 0 || result.executionDeviceId >= 16))) {
        this.#retire('RESOURCE_INVALID_RESULT');
        throw new ResourceBudgetError('RESOURCE_INVALID_RESULT', 'Resource service returned an invalid allocation.');
      }
      this.#leases.set(result.leaseId, { ...result, taskId: request.taskId });
      if (signal?.aborted || this.#closed) { await this.release(result.leaseId); this.#assertOpen(signal); }
    }
    return result;
  }

  async renew(leaseId, { ttlMs = DEFAULT_TTL_MS, signal } = {}) {
    if (this.#recovering) await this.#recovering;
    this.#assertOpen(signal);
    if (!Number.isSafeInteger(ttlMs) || ttlMs < 1000 || ttlMs > MAX_TTL_MS)
      throw new ResourceBudgetError('RESOURCE_INVALID_TTL', 'Invalid resource lease duration.');
    const lease = this.#leases.get(leaseId);
    if (!lease) return { status: 'denied', reason: 'RESOURCE_LEASE_UNKNOWN' };
    // A lost monitor cannot expand capacity; confirmed live workers retain their reservation.
    // 监控失联不能扩大容量；调用方确认仍存活的工作进程继续保留原预约。
    let result;
    if (this.#mode === 'rust' && lease.mode === 'rust') {
      try { result = await this.#request('renew', { leaseId, ttlMs }); }
      catch { result = { status: 'renewed', leaseId, expiresAt: this.#clock() + ttlMs, mode: 'fallback' }; }
    } else result = { status: 'renewed', leaseId, expiresAt: this.#clock() + ttlMs, mode: 'fallback' };
    if (result.status === 'renewed') lease.expiresAt = result.expiresAt;
    return result;
  }

  async release(leaseId) {
    if (this.#recovering) await this.#recovering;
    const lease = this.#leases.get(leaseId);
    if (!lease) return { status: 'released', leaseId, existed: false };
    if (this.#mode === 'rust' && lease.mode === 'rust' && !this.#closed) {
      try { await this.#request('release', { leaseId }); }
      catch { /* The owned service is retired before the local reservation is removed. 先退役自有服务再移除本地预约。 */ }
    }
    this.#leases.delete(leaseId);
    this.#drainFallback();
    return { status: 'released', leaseId, existed: true };
  }

  async registerExecutor(leaseId, { processId, startTimeMs } = {}) {
    if (this.#recovering) await this.#recovering;
    this.#assertOpen();
    const lease = this.#leases.get(leaseId);
    if (!lease) return { status: 'denied', reason: 'RESOURCE_LEASE_UNKNOWN' };
    if (!Number.isSafeInteger(processId) || processId < 1 || processId > 0xffffffff ||
        startTimeMs !== undefined && (!Number.isSafeInteger(startTimeMs) || startTimeMs < 0))
      throw new ResourceBudgetError('RESOURCE_EXECUTOR_INVALID', 'Invalid owned executor identity.');
    if (this.#mode === 'rust') {
      const result = await this.#request('registerExecutor', { leaseId, processId, ...(startTimeMs !== undefined ? { startTimeMs } : {}) });
      if (result.status === 'registered') lease.executor = { processId, startTimeMs: result.executor?.startTimeMs ?? startTimeMs ?? null };
      return result;
    }
    lease.executor = { processId, startTimeMs: startTimeMs ?? null };
    return { status: 'registered', leaseId, executor: { ...lease.executor, measurementState: 'unknown', memoryBytes: null,
      peakMemoryBytes: null, reason: 'RESOURCE_NATIVE_MONITOR_UNAVAILABLE' } };
  }

  async report(leaseId, feedback = {}) {
    if (this.#recovering) await this.#recovering;
    this.#assertOpen();
    if (!this.#leases.has(leaseId)) return { status: 'denied', reason: 'RESOURCE_LEASE_UNKNOWN' };
    const dimensions = { phase: ['cold-load', 'queue', 'hot-inference', 'other'],
      unit: ['tokens', 'documents', 'pairs', 'vectors', 'operations'], backend: ['cpu', 'gpu', 'dml', 'cuda', 'host'] };
    const allowed = ['throughputPerSecond', 'latencyMs', 'queueDepth', 'allocationFailure', 'foregroundLatencyMs', 'progress', ...Object.keys(dimensions)];
    if (!feedback || typeof feedback !== 'object' || Object.entries(feedback).some(([key, value]) => !allowed.includes(key) ||
        (dimensions[key] ? !dimensions[key].includes(value) : key === 'allocationFailure' ? typeof value !== 'boolean' : !Number.isFinite(value) || value < 0 || value > 1e9)) ||
        feedback.progress > 1 || feedback.queueDepth !== undefined && (!Number.isSafeInteger(feedback.queueDepth) || feedback.queueDepth > 1e6))
      throw new ResourceBudgetError('RESOURCE_INVALID_FEEDBACK', 'Invalid task feedback.');
    if (this.#mode === 'rust') return this.#request('report', { leaseId, feedback });
    const now = this.#clock(), previous = this.#feedback, taskId = this.#leases.get(leaseId).taskId;
    const backend = feedback.backend ?? 'legacy', phase = feedback.phase ?? 'legacy';
    const historyKey = `${taskId}|${backend}|${phase}|${feedback.unit ?? 'legacy'}`;
    const controlKey = backend === 'legacy' ? 'legacy' : `${taskId}|${['gpu', 'dml', 'cuda'].includes(backend) ? 'gpu' : backend}`;
    const control = this.#feedbackControls.get(controlKey) ?? { fraction: 1, lastAdjustmentMs: 0 };
    const history = this.#feedbackHistories.get(historyKey) ?? { throughputPerSecond: null, latencyMs: null };
    if (feedback.allocationFailure || feedback.foregroundLatencyMs > 250) {
      control.fraction = Math.max(0.125, control.fraction / 2); control.lastAdjustmentMs = now;
      previous.lastAdjustmentMs = now; previous.adjustmentReason = feedback.allocationFailure ? 'allocation-pressure' : 'foreground-latency';
    } else if (feedback.throughputPerSecond !== undefined && history.throughputPerSecond !== null &&
        feedback.throughputPerSecond > history.throughputPerSecond * 1.05 && feedback.queueDepth > 0 &&
        (feedback.latencyMs === undefined || history.latencyMs === null || feedback.latencyMs <= history.latencyMs * 1.1) &&
        ['legacy', 'hot-inference'].includes(phase) && now - control.lastAdjustmentMs >= 5000) {
      control.fraction = Math.min(1, control.fraction + 0.125); control.lastAdjustmentMs = now;
      previous.lastAdjustmentMs = now; previous.adjustmentReason = 'measured-hot-throughput-gain';
    } else previous.adjustmentReason = ['legacy', 'hot-inference'].includes(phase) ? 'no-comparable-throughput-gain' : 'non-hot-sample-held';
    if (backend === 'legacy') this.#backgroundFraction = control.fraction;
    previous.taskBackendFraction = control.fraction;
    previous.measurementContext = historyKey;
    this.#feedbackControls.delete(controlKey); this.#feedbackControls.set(controlKey, control);
    while (this.#feedbackControls.size > 128) this.#feedbackControls.delete(this.#feedbackControls.keys().next().value);
    for (const key of ['throughputPerSecond', 'latencyMs']) if (feedback[key] !== undefined)
      history[key] = history[key] === null ? feedback[key] : history[key] * 0.75 + feedback[key] * 0.25;
    previous.throughputPerSecond = history.throughputPerSecond; previous.latencyMs = history.latencyMs;
    this.#feedbackHistories.delete(historyKey); this.#feedbackHistories.set(historyKey, history);
    while (this.#feedbackHistories.size > 128) this.#feedbackHistories.delete(this.#feedbackHistories.keys().next().value);
    if (feedback.queueDepth !== undefined) previous.queueDepth = feedback.queueDepth;
    previous.reports++;
    return { status: 'reported', feedback: { ...previous, backgroundFraction: control.fraction } };
  }

  async reconcile({ restartService = false, signal } = {}) {
    this.#assertOpen(signal);
    if (typeof restartService !== 'boolean') throw new ResourceBudgetError('RESOURCE_INVALID_REQUEST', 'Invalid resource recovery option.');
    if (this.#mode === 'uninitialized') await this.#start();
    if (this.#recovering) return this.#recovering;
    this.#recovering = (async () => {
      if (restartService && this.#mode !== 'rust' && this.#executablePath) {
        // Admission pauses until every existing lease has been restored; restart never clears held debt.
        // 所有旧预约恢复前暂停新准入；监控重启不能清空未结清账目或接受调用方声称释放显存。
        if (this.#exit) {
          let retirementTimer;
          const retired = await Promise.race([this.#exit.then(() => true), new Promise(resolveTimeout => {
            retirementTimer = setTimeout(() => resolveTimeout(false), this.#timeoutMs);
          })]);
          clearTimeout(retirementTimer);
          if (!retired) return { status: 'quarantined', mode: 'fallback', retainedLeases: this.#leases.size,
            recoveryState: 'monitor-retirement-unconfirmed', reason: 'RESOURCE_SERVICE_RETIREMENT_PENDING' };
        }
        this.#mode = 'uninitialized'; this.#starting = undefined;
        await this.#start(); this.#assertOpen(signal);
        if (this.#mode === 'rust') {
          try {
            const leases = [...this.#leases.values()].map(lease => ({ leaseId: lease.leaseId, taskId: lease.taskId,
              expiresAt: lease.expiresAt, cpuThreads: lease.cpuThreads, memoryBytes: lease.memoryBytes, gpuMemoryBytes: lease.gpuMemoryBytes }));
            const restored = await this.#request('restore', { leases });
            if (restored.status !== 'restored' || restored.restoredLeases !== leases.length)
              throw new ResourceBudgetError('RESOURCE_RESTORE_FAILED', 'Held resource reservations could not be restored.');
            for (const lease of this.#leases.values()) {
              lease.mode = 'rust';
              if (lease.executor?.startTimeMs !== null && lease.executor?.startTimeMs !== undefined)
                await this.#request('recoverExecutor', { leaseId: lease.leaseId, ...lease.executor });
            }
          } catch { this.#retire('RESOURCE_RESTORE_FAILED'); }
        }
      }
      if (this.#mode === 'rust') {
        try {
          const result = await this.#request('reconcile'); this.#nativeSnapshot = result;
          return { ...result, recoveryState: 'reconciled' };
        } catch { /* A failed reconcile retains all local debt. 核对失败仍保留本地全部账目。 */ }
      }
      return { status: 'quarantined', mode: 'fallback', retainedLeases: this.#leases.size,
        recoveryState: this.#executablePath ? 'monitor-unavailable' : 'native-monitor-not-bundled',
        reason: this.#failureCode ?? 'RESOURCE_NATIVE_MONITOR_UNAVAILABLE' };
    })();
    try { return await this.#recovering; }
    finally { this.#recovering = undefined; this.#drainFallback(); }
  }

  #assertOpen(signal) {
    if (this.#closed) throw new ResourceBudgetError('RESOURCE_CLOSED', 'Resource service is closed.');
    if (signal?.aborted) throw cancellationError();
  }

  async #start() {
    if (this.#mode !== 'uninitialized') return this.#starting;
    if (!this.#executablePath) { this.#mode = 'fallback'; this.#failureCode = 'RESOURCE_SERVICE_NOT_BUNDLED'; return; }
    this.#mode = 'starting';
    this.#starting = (async () => {
      let child;
      try {
        child = spawn(this.#executablePath, [...this.#executableArgs, '--owner-pid', String(process.pid)], { windowsHide: true, stdio: ['pipe', 'pipe', 'ignore'], shell: false });
      } catch { this.#retire('RESOURCE_SERVICE_START_FAILED'); return; }
      this.#child = child;
      this.#exit = new Promise(resolveExit => child.once('close', () => {
        if (this.#child === child) this.#retire('RESOURCE_SERVICE_EXITED');
        resolveExit();
      }));
      child.once('error', () => this.#retire('RESOURCE_SERVICE_START_FAILED'));
      child.stdin.on('error', () => this.#retire('RESOURCE_SERVICE_IO_FAILED'));
      child.stdout.on('data', data => this.#onData(data));
      try {
        const health = await this.#request('health');
        if (!health || health.mode !== 'rust' || !Number.isSafeInteger(health.cpu?.logicalCores) ||
            !Number.isSafeInteger(health.memory?.availableBytes)) throw new Error('Invalid resource health result.');
        this.#nativeSnapshot = health; this.#mode = 'rust'; this.#failureCode = undefined;
      } catch { this.#retire('RESOURCE_SERVICE_START_FAILED'); }
    })();
    return this.#starting;
  }

  #request(method, params = {}, { signal, timeoutMs = this.#timeoutMs } = {}) {
    if (!this.#child || this.#pending.size >= (['cancelAcquire', 'release'].includes(method) ? MAX_PENDING_REQUESTS + 32 : MAX_PENDING_REQUESTS))
      return Promise.reject(new ResourceBudgetError('RESOURCE_QUEUE_FULL', 'Resource request queue is unavailable.'));
    const id = ++this.#requestId;
    const frame = Buffer.from(`${JSON.stringify({ id, method, params })}\n`);
    if (frame.length > MAX_FRAME_BYTES) return Promise.reject(new ResourceBudgetError('RESOURCE_FRAME_LIMIT', 'Resource request exceeds its limit.'));
    return new Promise((resolveResult, rejectResult) => {
      this.#setReferenced(true);
      const operation = { resolve: resolveResult, reject: rejectResult, cancelled: false, method, timer: undefined, cleanup: undefined };
      operation.timer = setTimeout(() => this.#retire('RESOURCE_SERVICE_TIMEOUT'), timeoutMs);
      const cancel = () => {
        if (operation.cancelled) return;
        operation.cancelled = true; rejectResult(cancellationError());
        clearTimeout(operation.timer); operation.timer = setTimeout(() => this.#retire('RESOURCE_SERVICE_TIMEOUT'), this.#timeoutMs);
        this.#request('cancelAcquire', { requestId: id }).catch(() => {});
      };
      operation.cleanup = () => { clearTimeout(operation.timer); signal?.removeEventListener('abort', cancel); };
      this.#pending.set(id, operation); signal?.addEventListener('abort', cancel, { once: true });
      this.#child.stdin.write(frame, error => { if (error) this.#retire('RESOURCE_SERVICE_IO_FAILED'); });
    });
  }

  #onData(data) {
    this.#buffer = Buffer.concat([this.#buffer, data]);
    let end;
    while ((end = this.#buffer.indexOf(10)) >= 0) {
      if (end > MAX_FRAME_BYTES) { this.#retire('RESOURCE_FRAME_LIMIT'); return; }
      let message;
      try { message = JSON.parse(this.#buffer.subarray(0, end).toString('utf8')); }
      catch { this.#retire('RESOURCE_INVALID_RESULT'); return; }
      this.#buffer = this.#buffer.subarray(end + 1);
      const pending = this.#pending.get(message.id);
      if (!pending) { this.#retire('RESOURCE_INVALID_RESULT'); return; }
      if (Array.isArray(message.result?.retiredLeaseIds)) for (const leaseId of message.result.retiredLeaseIds)
        this.#leases.delete(leaseId);
      pending.cleanup(); this.#pending.delete(message.id);
      if (pending.cancelled) {
        if (pending.method === 'acquire' && message.result?.status === 'granted') this.#request('release', { leaseId: message.result.leaseId }).catch(() => {});
      } else pending.resolve(message.result);
    }
    if (this.#buffer.length > MAX_FRAME_BYTES) this.#retire('RESOURCE_FRAME_LIMIT');
    else if (!this.#pending.size) this.#setReferenced(false);
  }

  #setReferenced(isReferenced) {
    // Idle monitoring must not keep an otherwise finished owner process alive.
    // 空闲监控不能让已完成的拥有者进程继续存活；有请求时重新引用直到返回或超时。
    const method = isReferenced ? 'ref' : 'unref';
    this.#child?.[method]();
    this.#child?.stdin?.[method]?.();
    this.#child?.stdout?.[method]?.();
  }

  #retire(code) {
    if (!this.#closed) { this.#mode = 'fallback'; this.#failureCode = code; }
    for (const pending of this.#pending.values()) {
      pending.cleanup();
      pending.reject(new ResourceBudgetError(code, 'The owned resource service is unavailable.'));
    }
    this.#pending.clear(); this.#buffer = Buffer.alloc(0);
    this.#child?.stdin.destroy(); this.#child?.kill(); this.#child = undefined;
  }

  #sample() {
    if (this.#sampler) return this.#sampler();
    const now = this.#clock();
    if (this.#lastSample && now - this.#lastSample.sampledAt < 1000) return this.#lastSample;
    const counters = cpus().reduce((sum, cpu) => {
      sum.idle += cpu.times.idle; sum.total += Object.values(cpu.times).reduce((total, time) => total + time, 0); return sum;
    }, { idle: 0, total: 0 });
    const delta = this.#lastCpuTimes && counters.total - this.#lastCpuTimes.total;
    const usagePercent = delta > 0 ? Math.max(0, Math.min(100, 100 * (1 - (counters.idle - this.#lastCpuTimes.idle) / delta))) : null;
    this.#lastCpuTimes = counters;
    this.#lastSample = { cpu: { logicalCores: availableParallelism(), usagePercent },
      memory: { totalBytes: totalmem(), availableBytes: freemem() },
      gpu: { state: 'unknown', availableMemoryBytes: null, reason: 'GPU_MONITOR_UNAVAILABLE' }, sampledAt: now };
    return this.#lastSample;
  }

  #reserved() {
    return [...this.#leases.values()].reduce((sum, lease) => ({ cpuThreads: sum.cpuThreads + lease.cpuThreads,
      memoryBytes: sum.memoryBytes + lease.memoryBytes, gpuMemoryBytes: sum.gpuMemoryBytes + lease.gpuMemoryBytes }),
    { cpuThreads: 0, memoryBytes: 0, gpuMemoryBytes: 0 });
  }

  #cpuCapacity(hardware) { return Math.max(1, Math.min(8, Math.floor(hardware.cpu.logicalCores / 2))); }
  #memoryCapacity(hardware) {
    // Without a live native monitor, materialized ownership is unknown and all unresolved debt stays fenced.
    // 没有原生实时监控时无法确认已兑现内存的归属，未结清预约继续全额隔离保留。
    return Math.max(0, Math.floor(Math.min(hardware.memory.availableBytes * 0.6 - 512 * 1024 * 1024,
      hardware.memory.totalBytes * 0.4)));
  }

  #acquireFallback(request) {
    const hardware = this.#sample(), reserved = this.#reserved();
    const cpuThreads = Math.max(0, this.#cpuCapacity(hardware) - reserved.cpuThreads);
    const memoryBytes = Math.max(0, this.#memoryCapacity(hardware) - reserved.memoryBytes);
    if (request.gpuMemoryBytes) return { status: 'denied', reason: 'RESOURCE_GPU_UNKNOWN', mode: 'fallback' };
    if (this.#leases.size >= MAX_LEASES) return { status: 'denied', reason: 'RESOURCE_LEASE_LIMIT', mode: 'fallback' };
    if (request.memoryBytes > memoryBytes || request.cpuThreads > 0 && cpuThreads === 0 ||
        request.kind === 'background' && request.cpuThreads > 0 && hardware.cpu.usagePercent > 90)
      return { status: 'denied', reason: 'RESOURCE_PRESSURE', mode: 'fallback' };
    const fraction = this.#feedbackControls.get(`${request.taskId}|cpu`)?.fraction ?? this.#backgroundFraction;
    const grantedCpu = Math.min(cpuThreads, request.cpuThreads, request.kind === 'background' ?
      Math.max(1, Math.floor(this.#cpuCapacity(hardware) * fraction)) : cpuThreads);
    return { status: 'granted', mode: 'fallback', device: 'cpu', leaseId: `fallback-${randomUUID()}`,
      expiresAt: this.#clock() + request.ttlMs, cpuThreads: grantedCpu,
      memoryBytes: request.memoryBytes, gpuMemoryBytes: 0,
      suggestions: this.#suggestions(hardware, grantedCpu, request.memoryBytes, fraction) };
  }

  #suggestions(hardware, cpuThreads, memoryBytes, fraction = this.#backgroundFraction) {
    const candidates = Math.max(16, Math.min(136, Math.round((hardware.cpu.usagePercent > 85 ? 40 :
      40 + Math.min(hardware.cpu.logicalCores, 32) * 3) * fraction)));
    const memory = Math.min(memoryBytes, Math.floor(hardware.memory.availableBytes / 3));
    return { annShardBytes: Math.min(Math.floor(memory / 2), 1024 ** 3), annCacheBytes: memory,
      annBuildConcurrency: Math.max(1, Math.min(4, cpuThreads)), candidateLimit: candidates,
      fusedCandidateLimit: Math.min(160, Math.floor(candidates * 1.2)), evidenceBudgetTokens: Math.max(2048, Math.min(16384, candidates * 128)),
      batchMultiplier: fraction, adjustmentReason: fraction < 1 ? 'held-task-backend-pressure' : 'capacity-approved',
      lastFeedbackReason: this.#feedback.adjustmentReason ?? 'not-adjusted',
      measurementContext: this.#feedback.measurementContext ?? null, source: 'resource-authority' };
  }

  #waitFallback(request, signal) {
    if (this.#fallbackWaiters.size >= 32) return Promise.resolve({ status: 'denied', reason: 'RESOURCE_QUEUE_FULL', mode: 'fallback' });
    return new Promise((resolveResult, rejectResult) => {
      const id = randomUUID(), queuedAt = this.#clock();
      const cancel = () => { this.#fallbackWaiters.delete(id); cleanup(); rejectResult(cancellationError()); };
      const timer = setTimeout(() => { this.#fallbackWaiters.delete(id); cleanup(); resolveResult({ status: 'denied', reason: 'RESOURCE_WAIT_TIMEOUT', mode: 'fallback' }); }, request.waitMs);
      const cleanup = () => { clearTimeout(timer); signal?.removeEventListener('abort', cancel);
        if (!this.#fallbackWaiters.size) { clearInterval(this.#fallbackTimer); this.#fallbackTimer = undefined; } };
      this.#fallbackWaiters.set(id, { request, queuedAt, resolve: resolveResult, reject: rejectResult, cleanup });
      signal?.addEventListener('abort', cancel, { once: true });
      this.#fallbackTimer ??= setInterval(() => this.#drainFallback(), 200);
    });
  }

  #drainFallback() {
    if (this.#closed || this.#recovering) return;
    const now = this.#clock();
    const priority = waiter => waiter.request.kind === 'background' && (now - waiter.queuedAt >= 2000 || this.#foregroundStreak >= 3) ? 0
      : waiter.request.kind === 'foreground' ? 1 : 2;
    const attempted = new Set();
    while (attempted.size < this.#fallbackWaiters.size) {
      const next = [...this.#fallbackWaiters.entries()].filter(([id]) => !attempted.has(id))
        .sort(([, first], [, second]) => priority(first) - priority(second) || first.queuedAt - second.queuedAt)[0];
      if (!next) break;
      const [id, waiter] = next; attempted.add(id);
      const result = this.#acquireFallback(waiter.request);
      if (result.status !== 'granted') continue;
      this.#leases.set(result.leaseId, { ...result, taskId: waiter.request.taskId });
      this.#foregroundStreak = waiter.request.kind === 'foreground' ? this.#foregroundStreak + 1 : 0;
      this.#fallbackWaiters.delete(id); waiter.cleanup(); waiter.resolve(result);
      attempted.clear();
    }
  }

  close() {
    if (this.#closing) return this.#closing;
    this.#closed = true;
    for (const waiter of this.#fallbackWaiters.values()) { waiter.cleanup(); waiter.reject(new ResourceBudgetError('RESOURCE_CLOSED', 'Resource service is closed.')); }
    this.#fallbackWaiters.clear(); clearInterval(this.#fallbackTimer);
    this.#closing = (async () => {
      await this.#recovering?.catch(() => {});
      await this.#starting;
      let timer;
      try {
        if (this.#exit) {
          this.#child?.stdin.end();
          this.#setReferenced(true);
          await Promise.race([this.#exit, new Promise(resolveTimeout => {
            timer = setTimeout(() => { this.#retire('RESOURCE_CLOSED'); resolveTimeout(); }, this.#timeoutMs);
          })]);
        }
      } finally { clearTimeout(timer); this.#retire('RESOURCE_CLOSED'); this.#leases.clear(); this.#mode = 'closed'; }
    })();
    return this.#closing;
  }
}

let sharedService;
export function sharedResourceBudget() {
  if (!sharedService || sharedService.status().closed) sharedService = new ResourceBudgetService();
  return sharedService;
}

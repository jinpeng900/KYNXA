import { randomUUID } from 'node:crypto';

const RECOVERABLE_CAPACITY_REASONS = new Set(['RESOURCE_PRESSURE', 'RESOURCE_WAIT_TIMEOUT', 'RESOURCE_CAPACITY_UNAVAILABLE']);

async function admissionObservation(service, signal) {
  if (!service.snapshot) return { state: 'unavailable' };
  try {
    const snapshot = await service.snapshot({ signal });
    return { state: 'observed', mode: snapshot.mode, sampledAt: snapshot.sampledAt,
      accounting: snapshot.accounting, budget: snapshot.budget, admissionPriority: snapshot.admissionPriority };
  } catch (error) { signal?.throwIfAborted(); return { state: 'unavailable', code: error.code ?? 'RESOURCE_OBSERVATION_UNAVAILABLE' }; }
}

async function acquireTaskLease(service, request, signal, onCapacityUnavailable) {
  const releasePriority = request.kind === 'foreground' ? service.beginForegroundAdmission?.({ signal }) : undefined;
  try {
    // Reclaim idle inference on a cheap transport probe before spending the bounded queue wait.
    // 轻量通信先立即探测容量，必要时回收闲置推理，再进入有界等待，避免先空等十秒才回收。
    const probeFirst = request.workload === 'model-transport' && Boolean(onCapacityUnavailable);
    let lease = await service.acquire(probeFirst ? { ...request, waitMs: 0 } : request, { signal });
    if (lease?.leaseId || !RECOVERABLE_CAPACITY_REASONS.has(lease?.reason) || !onCapacityUnavailable) return lease;
    signal?.throwIfAborted();
    const before = await admissionObservation(service, signal);
    // Only a rejected admission may retry; hold foreground priority through cleanup and the remaining bounded wait.
    // 仅被拒绝的准入可以重试；清理及剩余有界等待期间保留前台优先，尚未派发任何实际操作。
    const recovery = await onCapacityUnavailable({ signal, reason: 'memory-pressure', minimumIdleMs: 0 });
    signal?.throwIfAborted();
    // Returned bytes describe released reservations; OS samples independently describe actual availability.
    // 回收字节描述已释放预约，操作系统采样独立描述实际可用量，不能将两者相加宣称真实释放量。
    const audit = { attempted: true, initialReason: lease.reason, releasedIdleInference: recovery?.released === true,
      requested: { workload: request.workload ?? 'compute', cpuThreads: request.cpuThreads,
        memoryBytes: request.memoryBytes, gpuMemoryBytes: request.gpuMemoryBytes ?? 0, waitMs: request.waitMs },
      before, after: await admissionObservation(service, signal), releases: (recovery?.results ?? []).slice(0, 16) };
    if (recovery?.released !== true && !probeFirst) return { ...lease, admissionRecovery: { ...audit, granted: false } };
    const initialReason = lease.reason;
    lease = await service.acquire(request, { signal });
    return { ...lease, admissionRecovery: { ...audit, initialReason,
      granted: Boolean(lease?.leaseId) } };
  } finally { releasePriority?.(); }
}

/** Keep reservations until native work settles, including cancellation acknowledgement.
 * 包括取消确认在内，原生任务真正结束后才释放资源，不以调用方提前返回作为释放依据。 */
export async function runResourceTask(service, options, operation, { signal, onLease, onCapacityUnavailable } = {}) {
  signal?.throwIfAborted();
  if (!service?.acquire) return operation(null);
  const ttlMs = options.ttlMs ?? 30000;
  const lease = await acquireTaskLease(service, { taskId: randomUUID(), kind: 'foreground', cpuThreads: 1,
    memoryBytes: 16 * 1024 * 1024, waitMs: 10000, ...options, ttlMs }, signal, onCapacityUnavailable);
  if (!lease?.leaseId) throw Object.assign(new Error('Resource capacity unavailable. / 当前资源容量不足，请稍后重试。'), {
    code: lease?.reason ?? 'RESOURCE_CAPACITY_UNAVAILABLE', statusCode: 503,
    details: { reason: lease?.reason ?? 'RESOURCE_CAPACITY_UNAVAILABLE', mode: lease?.mode ?? 'unknown', actionStarted: false,
      workload: options.workload ?? 'compute',
      ...(lease?.admissionRecovery ? { admissionRecovery: lease.admissionRecovery } : {}) } });
  let renewing = false;
  const timer = setInterval(() => {
    if (renewing) return;
    renewing = true;
    Promise.resolve(service.renew(lease.leaseId, { ttlMs })).catch(() => {}).finally(() => { renewing = false; });
  }, Math.max(1000, Math.floor(ttlMs / 3)));
  timer.unref?.();
  const started = performance.now();
  try {
    signal?.throwIfAborted();
    await onLease?.(lease);
    return await operation(lease);
  } finally {
    clearInterval(timer);
    await service.report?.(lease.leaseId, { latencyMs: performance.now() - started,
      phase: 'other', unit: 'operations', backend: 'host' }).catch(() => {});
    // A cleanup transport failure cannot erase an already returned mutation receipt.
    // 清理通道失败不能抹掉已经返回的写入回执；未确认释放仍由资源服务隔离保留。
    await service.release(lease.leaseId).catch(() => {});
  }
}

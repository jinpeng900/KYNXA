import { randomUUID } from 'node:crypto';

/** Keep reservations until native work settles, including cancellation acknowledgement.
 * 包括取消确认在内，原生任务真正结束后才释放资源，不以调用方提前返回作为释放依据。 */
export async function runResourceTask(service, options, operation, { signal, onLease } = {}) {
  signal?.throwIfAborted();
  if (!service?.acquire) return operation(null);
  const ttlMs = options.ttlMs ?? 30000;
  const lease = await service.acquire({ taskId: randomUUID(), kind: 'foreground', cpuThreads: 1,
    memoryBytes: 16 * 1024 * 1024, waitMs: 10000, ...options, ttlMs }, { signal });
  if (!lease?.leaseId) throw Object.assign(new Error('Resource capacity unavailable. / 当前资源容量不足，请稍后重试。'), {
    code: lease?.reason ?? 'RESOURCE_CAPACITY_UNAVAILABLE', statusCode: 503,
    details: { reason: lease?.reason ?? 'RESOURCE_CAPACITY_UNAVAILABLE', mode: lease?.mode ?? 'unknown' } });
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

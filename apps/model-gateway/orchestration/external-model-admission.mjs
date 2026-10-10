import { createHash } from 'node:crypto';
import { planExternalModelDemand } from '../models/external-model-demand.mjs';

const TTL_MS = 30000;

function coordinationError(code, message) {
  return Object.assign(new Error(message), { code, statusCode: 503 });
}

async function waitForPreparation(promise, signal) {
  signal?.throwIfAborted();
  if (!signal) return promise;
  let cancel;
  const cancelled = new Promise((resolve, reject) => {
    cancel = () => reject(signal.reason);
    signal.addEventListener('abort', cancel, { once: true });
  });
  try { return await Promise.race([promise, cancelled]); }
  finally { signal.removeEventListener('abort', cancel); }
}

/** Forecast fences protect pending allocations, not externally owned resident memory.
 * 预测预约保护尚未发生的分配，不重复扣除外部已驻留模型，也不取得外部进程控制权。 */
export class ExternalModelAdmission {
  constructor({ resources, observer, yieldIdleGpu, onState = () => {} }) {
    this.resources = resources;
    this.observer = observer;
    this.yieldIdleGpu = yieldIdleGpu;
    this.onState = onState;
    this.entries = new Map();
    this.closed = false;
  }

  async acquire(connection, { modelId, signal } = {}) {
    if (this.closed) throw coordinationError('RESOURCE_EXTERNAL_COORDINATION_CLOSED', 'External admission is closed.');
    signal?.throwIfAborted();
    const key = createHash('sha256').update(connection.baseUrl + '\0' + modelId).digest('hex');
    await this.reconcile(key);
    let entry = this.entries.get(key);
    while (entry?.finishing || entry?.releasing) {
      await waitForPreparation(entry.finishing ?? entry.releasing, signal);
      entry = this.entries.get(key);
    }
    if (entry?.quarantined) throw coordinationError('RESOURCE_EXTERNAL_ALLOCATION_UNCONFIRMED',
      'A prior external allocation remains unconfirmed.');
    if (this.closed) throw coordinationError('RESOURCE_EXTERNAL_COORDINATION_CLOSED', 'External admission is closed.');
    if (!entry) {
      if (this.entries.size >= 32) throw coordinationError('RESOURCE_EXTERNAL_COORDINATION_CAPACITY',
        'External allocation coordination is full.');
      entry = { connection, modelId, users: 0, waiters: 0, inFlight: 0, controller: new AbortController() };
      this.entries.set(key, entry);
      entry.promise = this._prepare(entry, entry.controller.signal);
    }
    entry.waiters++;
    try {
      // Preparation belongs to all waiters; one caller cannot cancel another caller's reservation.
      // 准备属于所有等待者，一个请求取消不能撤销其他请求仍需使用的共享预约。
      await waitForPreparation(entry.promise, signal);
      signal?.throwIfAborted();
      if (this.closed || entry.quarantined) throw coordinationError('RESOURCE_EXTERNAL_ALLOCATION_UNCONFIRMED',
        'External allocation coordination is no longer ready.');
      entry.users++;
      let released = false, pending = false;
      return { snapshot: entry.snapshot, plan: entry.plan,
        dispatched: () => {
          if (released || pending || entry.quarantined || this.closed)
            throw coordinationError('RESOURCE_EXTERNAL_ALLOCATION_UNCONFIRMED', 'External dispatch is no longer admitted.');
          pending = true;
          entry.inFlight++;
        },
        settled: () => {
          if (!pending) return;
          pending = false;
          entry.inFlight--;
        },
        release: async () => {
          if (released) return;
          released = true;
          entry.users--;
          await this._finish(key, entry);
        } };
    } finally {
      entry.waiters--;
      if (!entry.users && !entry.waiters) await this._finish(key, entry);
    }
  }

  async _prepare(entry, signal) {
    entry.snapshot = await this.observer.observe(entry.connection, { modelId: entry.modelId, signal, refresh: true });
    if (entry.snapshot.backend !== 'ollama') return;
    const hardware = await this.resources.snapshot({ signal });
    // OpenAI requests do not send num_ctx; a 1M input budget is not an Ollama allocation request.
    // OpenAI 请求没有 num_ctx，连接的 1M 输入预算不能被当成 Ollama 实际分配目标。
    const contextTokens = entry.snapshot.runtimeContextTokens || entry.snapshot.configuredContextTokens;
    entry.plan = planExternalModelDemand(entry.snapshot, { contextTokens, hardware });
    // A resident foreground model still competes with owned GPU inference even when it needs no new allocation.
    // 已驻留前台模型即便没有新增分配，也会与自有 GPU 推理争用；仅协调释放 KYNXA 的空闲执行器。
    const shouldYieldOwnedIdleGpu = entry.snapshot.observedGpuMemoryBytes > 0 || entry.plan.gpuMemoryBytes > 0;
    if (shouldYieldOwnedIdleGpu) {
      const yielded = await this.yieldIdleGpu?.({ signal });
      entry.coordination = { state: 'owned-idle-gpu-yield-requested', externalModelControlled: false,
        receipt: yielded ?? null,
        globalGenerationState: entry.snapshot.globalGenerationState ?? 'unknown',
        applicationGenerationState: entry.snapshot.applicationGenerationState ?? 'idle' };
    }
    if (!entry.plan.requiresAdmission) {
      this._state(entry, { state: entry.plan.state === 'ready' ? 'no-pending-increment' : 'unconfirmed', plan: entry.plan });
      return;
    }
    const gpuMemoryBytes = entry.plan.gpuMemoryBytes;
    if (gpuMemoryBytes > 0 && hardware.gpu?.mappingStatus !== 'verified') {
      this._state(entry, { state: 'unconfirmed', reason: 'GPU_DEVICE_MAPPING_UNVERIFIED', plan: entry.plan });
      return;
    }
    // GPU staging is an uncertain host-memory upper bound; VRAM admission is strict, host staging remains advisory.
    // GPU 上传的主机暂存量属于不确定上界；显存增量严格准入，主机暂存明确标为观察建议。
    const memoryBytes = gpuMemoryBytes > 0 && entry.plan.breakdown.memoryIncludesTransientStaging ? 0 : entry.plan.memoryBytes;
    const request = { taskId: ('external:' + entry.modelId).slice(0, 128), workspaceId: 'local-external', kind: 'foreground',
      cpuThreads: 0, memoryBytes, gpuMemoryBytes, ttlMs: TTL_MS, waitMs: 5000 };
    let lease = await this.resources.acquire(request, { signal });
    if (!lease?.leaseId && gpuMemoryBytes > 0) {
      await this.yieldIdleGpu?.({ signal });
      lease = await this.resources.acquire(request, { signal });
    }
    if (!lease?.leaseId) throw coordinationError(lease?.reason ?? 'RESOURCE_EXTERNAL_MODEL_BUSY',
      'Pending external model resources cannot be admitted.');
    entry.lease = lease;
    entry.timer = setInterval(() => this._renew(entry), TTL_MS / 3);
    entry.timer.unref?.();
    this._state(entry, { state: entry.plan.unknownComponents?.includes('kv-cache') ? 'admitted-known-weights-kv-unconfirmed'
      : entry.plan.partialCoverage ? 'admitted-predicted-increment-partial' : 'admitted-predicted-increment',
      plan: entry.plan, gpuMemoryBytes, memoryBytes,
      hostMemoryAdmission: memoryBytes === 0 && entry.plan.memoryBytes > 0 ? 'advisory-transient-staging' : 'predicted-increment',
      observedResidencyReservedAgain: false });
  }

  async _renew(entry) {
    if (entry.renewing || entry.releasing) return;
    entry.renewing = true;
    try {
      const result = await this.resources.renew(entry.lease.leaseId, { ttlMs: TTL_MS });
      if (result?.status !== 'renewed' || result.mode === 'fallback') throw new Error('Reservation renewal unconfirmed.');
    } catch {
      entry.quarantined = true;
      this._state(entry, { state: 'quarantined', reason: 'resource-renewal-unconfirmed' });
    } finally { entry.renewing = false; }
  }

  _state(entry, state) {
    entry.state = state;
    // Presentation failures cannot invalidate or leak an acquired fence.
    // 展示失败不能破坏或泄漏已经取得的资源预约。
    try { this.onState(entry.connection, entry.modelId, { ...state, snapshot: entry.snapshot,
      ...(entry.coordination ? { coordination: entry.coordination } : {}) }); }
    catch { /* Reservation ownership remains authoritative. 资源所有者仍是权威。 */ }
  }

  async _finish(key, entry) {
    if (entry.users || entry.waiters) return;
    if (entry.finishing) return entry.finishing;
    // Retire before awaiting preparation so a new caller cannot join a cancelled, late-granted fence.
    // 等待准备结束之前先设退役屏障，禁止新请求加入已经取消但稍后才获准的预约。
    entry.finishing = (async () => {
      entry.controller.abort();
      await entry.promise.catch(() => {});
      if (entry.users || entry.waiters) return;
      if (entry.inFlight === 0 || !entry.lease) await this._release(key, entry, 'request-settled');
      else {
        entry.quarantined = true;
        this._state(entry, { state: 'quarantined', reason: 'external-request-outcome-unconfirmed' });
        await this.reconcile(key);
      }
    })();
    try { await entry.finishing; }
    finally { entry.finishing = undefined; }
  }

  async _release(key, entry, reason) {
    if (entry.releasing) return entry.releasing;
    entry.releasing = (async () => {
      clearInterval(entry.timer);
      if (entry.lease) {
        try { await this.resources.release(entry.lease.leaseId); }
        catch {
          entry.quarantined = true;
          this._state(entry, { state: 'quarantined', reason: 'reservation-release-unconfirmed' });
          return;
        }
      }
      if (this.entries.get(key) === entry) this.entries.delete(key);
      this._state(entry, { ...entry.state, state: 'prediction-fence-released', reason, externalResidentOwnership: false });
    })();
    try { await entry.releasing; }
    finally { entry.releasing = undefined; }
  }

  async reconcile(key) {
    for (const [entryKey, entry] of this.entries) {
      if (key && entryKey !== key || !entry.quarantined || entry.users || entry.waiters || entry.releasing) continue;
      const snapshot = await this.observer.observe(entry.connection, { modelId: entry.modelId, refresh: true });
      // A partial weight fence needs a reported loaded allocation, not its previously unknown target KV size.
      // 权重部分预约只需核实模型已分配并加载，不能等待原本未知的目标 KV；这不认证取消生成成功。
      const hasMaterializedWeights = entry.plan?.unknownComponents?.includes('kv-cache') && entry.plan.breakdown.weightIncrementBytes > 0 &&
        snapshot.runtimeContextTokens > 0;
      const hasMaterializedContext = snapshot.runtimeContextTokens >= (entry.plan?.breakdown.targetContextTokens ?? Infinity);
      if (snapshot.loaded === true && (hasMaterializedWeights || hasMaterializedContext))
        await this._release(entryKey, entry, 'pending-allocation-now-observed');
    }
  }

  async close() {
    this.closed = true;
    for (const entry of this.entries.values()) entry.controller.abort();
    for (const [key, entry] of this.entries) {
      await entry.promise.catch(() => {});
      clearInterval(entry.timer);
      if (!entry.users && !entry.waiters) await this._finish(key, entry);
    }
  }
}

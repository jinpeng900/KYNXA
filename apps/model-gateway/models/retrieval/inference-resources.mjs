import { randomUUID } from 'node:crypto';
import { availableParallelism } from 'node:os';
import { sharedResourceBudget } from '../../platform/resources/resource-client.mjs';

const MIB = 1024 * 1024;
const LEASE_TTL_MS = 30_000;
const COST_HISTORY_LIMIT = 64;
const measuredCosts = new Map();

function costIdentity(profile, backend) {
  return `${profile.modelVersion ?? profile.id}:${backend}:${profile.dtype ?? 'fp32'}`;
}

function modelShape(profile) {
  return { hiddenSize: profile.hiddenSize ?? profile.dimensions ?? 384, heads: profile.attentionHeads ?? 12 };
}

function activationBytes(profile, sequenceTokens, batchSize) {
  const shape = modelShape(profile);
  return Math.ceil(batchSize * (sequenceTokens * shape.hiddenSize * 4 * 8 +
    sequenceTokens * sequenceTokens * shape.heads * 4));
}

// Only an owned, isolated inference process can provide workload memory samples.
// 只接纳独立、受本应用拥有的推理子进程采样；根进程 RSS 不能冒充单个模型内存。
function rememberCost(profile, feedback, executorProcessId) {
  if (feedback.memoryMeasurementSource !== 'isolated-native-process-delta' ||
      feedback.processId !== executorProcessId || feedback.processId === process.pid ||
      !['cpu', 'dml'].includes(feedback.backend) ||
      !Number.isSafeInteger(feedback.workloadMemoryDeltaBytes) || feedback.workloadMemoryDeltaBytes <= 0 ||
      feedback.workloadMemoryDeltaBytes > 32 * 1024 ** 3) return;
  const identity = costIdentity(profile, feedback.backend);
  const records = measuredCosts.get(identity) ?? [];
  records.push({ bytes: feedback.workloadMemoryDeltaBytes, phase: feedback.phase,
    sequenceTokens: feedback.sequenceTokens ?? 0, batchSize: feedback.batchSize ?? 0 });
  measuredCosts.delete(identity); measuredCosts.set(identity, records.slice(-16));
  while (measuredCosts.size > COST_HISTORY_LIMIT) measuredCosts.delete(measuredCosts.keys().next().value);
}

// These are requested model costs, never a competing resource allocation policy.
// 此处只描述模型需求；实际额度由唯一资源服务批准，不另设抢占资源的调度器。
export function inferenceResourceRequest(profile, requestedCpuThreads, { backend = 'cpu', sequenceTokens = 128, batchSize = 16 } = {}) {
  const weightBytes = profile.files.filter(asset => asset.path.endsWith('.onnx')).reduce((sum, asset) => sum + asset.bytes, 0);
  const logicalCores = availableParallelism();
  const cpuThreads = Number.isInteger(requestedCpuThreads) ? Math.max(1, Math.min(32, requestedCpuThreads))
    : Math.max(1, Math.min(32, Math.floor(logicalCores / 2)));
  const tokenizerBytes = profile.files.filter(asset => asset.path.includes('tokenizer')).reduce((sum, asset) => sum + asset.bytes, 0);
  const tokenizerMemoryBytes = Math.ceil(tokenizerBytes * 4 + 64 * MIB);
  const quantized = /q[48]|int[48]/u.test(profile.dtype ?? '');
  const runtimeWeightBytes = weightBytes * (quantized ? 2 : 1.4);
  const scratchBytes = activationBytes(profile, Math.max(1, Math.min(profile.maxInputTokens ?? 512, sequenceTokens)),
    Math.max(1, Math.min(128, batchSize)));
  const initialMemoryBytes = runtimeWeightBytes + tokenizerMemoryBytes + 96 * MIB + scratchBytes;
  const records = measuredCosts.get(costIdentity(profile, backend)) ?? [];
  const observedPeakBytes = records.length ? Math.max(...records.map(record => record.bytes)) : 0;
  return { cpuThreads, memoryBytes: Math.ceil(Math.max(initialMemoryBytes, observedPeakBytes * 1.125 + 32 * MIB)),
    tokenizerMemoryBytes, gpuMemoryBytes: Math.ceil(runtimeWeightBytes + 128 * MIB +
      activationBytes(profile, Math.min(profile.maxInputTokens ?? 512, 256), 32)),
    memoryEstimate: { backend, dtype: profile.dtype ?? 'fp32', sequenceTokens, batchSize,
      runtimeWeightBytes: Math.ceil(runtimeWeightBytes),
      observedPeakBytes: observedPeakBytes || null, sampleCount: records.length,
      source: records.length ? 'owned-workload-peak-with-margin' : 'model-shape-startup-estimate' } };
}

export function inferenceBatchBudget({ cpuThreads, memoryBytes, gpuMemoryBytes = 0, device = 'cpu',
  batchMultiplier = 1 }, maxInputTokens = 512) {
  const fraction = Math.max(0.125, Math.min(1, Number.isFinite(batchMultiplier) ? batchMultiplier : 1));
  // GPU work is limited by GPU/host buffers and tokens, not by the CPU thread multiplier.
  // GPU 推理受显存、主机供数缓冲与 token 上限约束，不机械套用 CPU 线程倍数。
  const capacity = device !== 'cpu' && gpuMemoryBytes > 0
    ? Math.min(gpuMemoryBytes / (16 * MIB), memoryBytes / (8 * MIB))
    : Math.min(Math.max(1, cpuThreads) * 4, memoryBytes / (32 * MIB));
  const batchSize = Math.max(1, Math.min(128, Math.floor(capacity * fraction)));
  const tokensPerItem = device !== 'cpu' ? 128 : 384;
  return { batchSize, batchTokenBudget: Math.max(maxInputTokens, Math.min(65_536, batchSize * tokensPerItem)) };
}

const budgetError = reason => Object.assign(new Error('Local inference resources are temporarily unavailable.'), {
  code: 'INFERENCE_RESOURCE_BUSY', details: { reason: reason ?? 'resource-denied' },
});
const cancelledError = () => Object.assign(new Error('Inference cancelled.'), { name: 'AbortError', code: 'INFERENCE_CANCELLED' });

function awaitAdmission(promise, signal) {
  if (!signal) return promise;
  if (signal.aborted) return Promise.reject(cancelledError());
  return new Promise((resolve, reject) => {
    const abort = () => { signal.removeEventListener('abort', abort); reject(cancelledError()); };
    signal.addEventListener('abort', abort, { once: true });
    promise.then(value => { signal.removeEventListener('abort', abort); resolve(value); },
      error => { signal.removeEventListener('abort', abort); reject(error); });
  });
}

/** Resident memory survives idle and cancelled callers; CPU is released only after native work drains.
 * 驻留内存不随空闲或调用方取消而提前释放；CPU 仅在原生工作排空后释放。 */
export class InferenceResourceReservation {
  #service; #request; #taskId; #kind; #devicePreference; #profile;
  #resident; #residentUpgrades = []; #gpuResident; #execution; #residentPromise; #gpuPromise; #executionPromise; #releasePromise; #timer; #closed = false; #renewError;
  #diagnostic; #lastGrant; #lastGpuSnapshotAt = 0;
  #lifetime = new AbortController();
  #admissions = new Set();
  #executorProcessId; #feedback;
  #adjustments = [];
  #appliedBackend; #lastBatchMeasurement;

  constructor({ profile, cpuThreads, resourceService, kind = 'background', devicePreference = 'auto' }) {
    if (!['auto', 'cpu'].includes(devicePreference)) throw new TypeError('Unsupported inference device preference.');
    this.#service = resourceService ?? sharedResourceBudget();
    this.#profile = profile;
    this.#request = inferenceResourceRequest(profile, cpuThreads, { backend: profile.requiredDevice ?? 'cpu' });
    this.#kind = kind;
    this.#devicePreference = devicePreference;
    this.#taskId = `inference-${randomUUID()}`;
  }

  async acquire({ signal, kind = this.#kind, tokenizerOnly = false } = {}) {
    if (this.#closed || this.#renewError) throw budgetError(this.#closed ? 'reservation-closed' : 'lease-renewal-failed');
    if (signal?.aborted) throw cancelledError();
    // Cancelling one caller must not cancel the coalesced admission needed by another caller.
    // 单个调用方取消不能取消其他调用方仍需要的共享资源申请。
    const admission = this.#acquireOwned(kind, tokenizerOnly);
    this.#admissions.add(admission);
    const tracked = admission.finally(() => { this.#admissions.delete(admission); });
    return awaitAdmission(tracked, signal);
  }

  async #acquireOwned(kind, tokenizerOnly) {
    const signal = this.#lifetime.signal;
    await this.#releasePromise;
    await (this.#residentPromise ??= this.#acquireResident(signal, tokenizerOnly).finally(() => { this.#residentPromise = undefined; }));
    if (!tokenizerOnly && this.#residentBytes() < this.#request.memoryBytes)
      await (this.#residentPromise ??= this.#acquireResident(signal, false).finally(() => { this.#residentPromise = undefined; }));
    if (this.#closed) throw budgetError('reservation-closed');
    await (this.#executionPromise ??= this.#acquireExecution(kind, signal).finally(() => { this.#executionPromise = undefined; }));
    if (!tokenizerOnly)
      await (this.#gpuPromise ??= this.#acquireGpu(signal).finally(() => { this.#gpuPromise = undefined; }));
    if (this.#closed) throw budgetError('reservation-closed');
    const grant = this.#grantedBudget();
    if (this.#lastGrant && ['device', 'deviceId', 'cpuThreads', 'batchSize', 'batchTokenBudget'].some(key => grant[key] !== this.#lastGrant[key])) {
      this.#adjustments.push({ appliedAt: Date.now(), boundary: 'next-request',
        phase: 'granted', reason: grant.device !== this.#lastGrant.device ? 'backend-admission-changed'
          : grant.cpuThreads !== this.#lastGrant.cpuThreads ? 'approved-thread-budget-changed' : 'steady-state-batch-feedback',
        from: { device: this.#lastGrant.device, cpuThreads: this.#lastGrant.cpuThreads, batchSize: this.#lastGrant.batchSize },
        to: { device: grant.device, cpuThreads: grant.cpuThreads, batchSize: grant.batchSize } });
      this.#adjustments = this.#adjustments.slice(-16);
    }
    this.#lastGrant = grant;
    return this.#lastGrant;
  }

  #residentBytes() { return (this.#resident?.memoryBytes ?? 0) + this.#residentUpgrades.reduce((sum, lease) => sum + lease.memoryBytes, 0); }

  async #acquireResident(signal, tokenizerOnly) {
    const desiredMemoryBytes = tokenizerOnly ? this.#request.tokenizerMemoryBytes
      : Math.ceil(this.#request.memoryBytes / (64 * MIB)) * 64 * MIB;
    if (this.#residentBytes() >= desiredMemoryBytes) return;
    const lease = await this.#service.acquire({ taskId: `${this.#taskId}-resident`, workspaceId: 'local', kind: this.#kind,
      cpuThreads: 0, memoryBytes: desiredMemoryBytes - this.#residentBytes(), gpuMemoryBytes: 0, ttlMs: LEASE_TTL_MS,
      waitMs: 5000 }, { signal });
    if (lease.status !== 'granted') throw budgetError(lease.reason);
    if (this.#resident) this.#residentUpgrades.push(lease);
    else this.#resident = lease;
    if (this.#executorProcessId && this.#service.registerExecutor)
      await this.#service.registerExecutor(lease.leaseId, { processId: this.#executorProcessId });
    this.#startRenewal();
  }

  async #acquireGpu(signal) {
    if (this.#diagnostic?.code === 'GPU_RESOURCE_UNAVAILABLE' && Date.now() - this.#lastGpuSnapshotAt >= 1000 && this.#service.snapshot) {
      this.#lastGpuSnapshotAt = Date.now();
      const snapshot = await this.#service.snapshot({ signal });
      if (snapshot.gpu?.state === 'available' && snapshot.gpu.availableMemoryBytes >= this.#request.gpuMemoryBytes)
        this.#diagnostic = undefined;
    }
    if (this.#devicePreference === 'auto' && !this.#diagnostic) {
      if (!this.#gpuResident) {
        const gpuLease = await this.#service.acquire({ taskId: `${this.#taskId}-gpu`, workspaceId: 'local', kind: this.#kind,
          memoryBytes: 0, cpuThreads: 0, gpuMemoryBytes: this.#request.gpuMemoryBytes, ttlMs: LEASE_TTL_MS }, { signal });
        if (gpuLease.status === 'granted') {
          const isMappedDirectMl = process.platform === 'win32' && gpuLease.executionProvider === 'dml' &&
            Number.isSafeInteger(gpuLease.executionDeviceId) && gpuLease.executionDeviceId >= 0;
          const isUnmaskedCuda = process.platform === 'linux' && process.arch === 'x64' &&
            gpuLease.deviceId === 0 && ['', '0'].includes(process.env.CUDA_VISIBLE_DEVICES ?? '');
          if (isMappedDirectMl || isUnmaskedCuda) {
            this.#gpuResident = gpuLease;
            if (this.#executorProcessId && this.#service.registerExecutor)
              await this.#service.registerExecutor(gpuLease.leaseId, { processId: this.#executorProcessId });
          }
          else {
            // NVML physical GPU ordinal is not the DirectML DXGI ordinal; never reserve one and use another.
            // NVML 物理显卡序号不等于 DirectML DXGI 序号，不能预留一张显卡却实际调用另一张。
            await this.#service.release(gpuLease.leaseId);
            this.#diagnostic = { code: 'GPU_DEVICE_MAPPING_UNVERIFIED' };
          }
        }
        else {
          this.#lastGpuSnapshotAt = Date.now();
          this.#diagnostic = { code: 'GPU_RESOURCE_UNAVAILABLE', reason: gpuLease.reason ?? 'gpu-budget-unavailable' };
        }
      }
    }
  }

  async #acquireExecution(kind, signal) {
    if (this.#execution) return;
    const request = { taskId: `${this.#taskId}-execution`, workspaceId: 'local', kind,
      cpuThreads: this.#request.cpuThreads, memoryBytes: 0, gpuMemoryBytes: 0, ttlMs: LEASE_TTL_MS, waitMs: 5000 };
    const lease = await this.#service.acquire(request, { signal });
    if (lease.status !== 'granted') throw budgetError(lease.reason);
    this.#execution = lease;
  }

  #grantedBudget() {
    return { cpuThreads: this.#execution.cpuThreads, memoryBytes: this.#residentBytes(),
      gpuMemoryBytes: this.#gpuResident?.gpuMemoryBytes ?? 0,
      device: this.#gpuResident ? process.platform === 'win32' ? 'dml' : 'cuda' : 'cpu',
      ...(this.#gpuResident ? { deviceId: this.#gpuResident.executionDeviceId ?? this.#gpuResident.deviceId } : {}),
      resourceMode: this.#execution.mode, memoryEstimate: this.#request.memoryEstimate,
      cpuFallbackBatchBudget: inferenceBatchBudget({ cpuThreads: this.#execution.cpuThreads,
        memoryBytes: this.#residentBytes(), device: 'cpu' }),
      ...(this.#gpuResident ? { activationMemoryBytes: Math.max(1,
        this.#gpuResident.gpuMemoryBytes - this.#request.memoryEstimate.runtimeWeightBytes - 128 * MIB),
        hiddenSize: modelShape(this.#profile).hiddenSize, attentionHeads: modelShape(this.#profile).heads } : {}),
      ...inferenceBatchBudget({ cpuThreads: this.#execution.cpuThreads, memoryBytes: this.#residentBytes(),
        gpuMemoryBytes: this.#gpuResident?.gpuMemoryBytes ?? 0, device: this.#gpuResident ? 'dml' : 'cpu',
        batchMultiplier: this.#feedback?.backgroundFraction ?? this.#execution.suggestions?.batchMultiplier }),
      ...(this.#diagnostic ? { diagnostic: this.#diagnostic } : {}) };
  }

  #startRenewal() {
    if (this.#timer) return;
    this.#timer = setInterval(() => {
      Promise.all([this.#resident, ...this.#residentUpgrades, this.#gpuResident, this.#execution].filter(Boolean).map(lease => this.#service.renew(lease.leaseId, { ttlMs: LEASE_TTL_MS })))
        .then(results => { if (results.some(result => result?.status === 'denied')) this.#renewError = true; })
        .catch(() => { this.#renewError = true; });
    }, 10_000);
    this.#timer.unref();
  }

  idle() {
    const previousRelease = this.#releasePromise;
    const admitted = [...this.#admissions];
    this.#releasePromise = (async () => {
      await previousRelease;
      await Promise.all(admitted.map(admission => admission.catch(() => {})));
      const lease = this.#execution;
      this.#execution = undefined;
      if (lease) await this.#service.release(lease.leaseId);
    })();
    return this.#releasePromise;
  }

  async close() {
    this.#closed = true;
    this.#lifetime.abort();
    clearInterval(this.#timer);
    await this.#residentPromise?.catch(() => {});
    await this.idle();
    const leases = [this.#resident, ...this.#residentUpgrades, this.#gpuResident].filter(Boolean);
    this.#resident = undefined;
    this.#residentUpgrades = [];
    this.#gpuResident = undefined;
    await Promise.all(leases.map(lease => this.#service.release(lease.leaseId)));
  }

  async backend(status) {
    if (['cpu', 'dml', 'cuda'].includes(status?.device)) {
      // Worker-ready confirms session configuration, not the number of runnable OS threads or GPU speed.
      // worker-ready 确认的是会话配置，不冒充操作系统实际活跃线程数或 GPU 提速测量。
      const sessionConfigured = Number.isSafeInteger(status.cpuThreads) && status.cpuThreads > 0;
      this.#appliedBackend = { device: status.device,
        cpuThreads: sessionConfigured ? status.cpuThreads : null,
        source: sessionConfigured ? 'worker-ready-session-options' : 'backend-retirement',
        sessionInitialized: sessionConfigured, gpuValidated: status.gpuValidated === true,
        ...(status.deviceId !== undefined ? { deviceId: status.deviceId } : {}),
        ...(status.executionMode ? { executionMode: status.executionMode } : {}),
        cpuOperatorFallback: status.cpuOperatorFallback === true,
        ...(status.diagnostic ?? this.#diagnostic ? { diagnostic: status.diagnostic ?? this.#diagnostic } : {}) };
    }
    if (status?.device !== 'cpu' || !this.#gpuResident) return;
    // The worker reports CPU only after disposing its attempted GPU session.
    // worker 仅在释放尝试过的 GPU 会话后报告 CPU，届时才可解除显存预留。
    const lease = this.#gpuResident;
    this.#gpuResident = undefined;
    this.#diagnostic = status.diagnostic ?? { code: 'GPU_BACKEND_UNAVAILABLE' };
    if (this.#lastGrant) {
      const { deviceId, activationMemoryBytes, hiddenSize, attentionHeads, ...grant } = this.#lastGrant;
      this.#lastGrant = { ...grant, ...grant.cpuFallbackBatchBudget, device: 'cpu', gpuMemoryBytes: 0, diagnostic: this.#diagnostic };
    }
    await this.#service.release(lease.leaseId);
  }

  retryRequiredGpuAfterExit() {
    // A GPU-only space cannot recover using CPU math; one new caller may re-audit its retired backend.
    // 仅 GPU 的向量空间不能用 CPU 算术恢复；已退出后可由一个新调用方重新审计后端。
    if (!this.#gpuResident && this.#diagnostic?.code === 'GPU_WORKER_FAILED') this.#diagnostic = undefined;
  }

  async registerExecutor(processId) {
    if (!Number.isSafeInteger(processId) || processId <= 0 || !this.#service.registerExecutor) return;
    this.#executorProcessId = processId;
    await Promise.all([this.#resident, ...this.#residentUpgrades, this.#gpuResident].filter(Boolean)
      .map(lease => this.#service.registerExecutor(lease.leaseId, { processId })));
  }

  async report(feedback) {
    rememberCost(this.#profile, feedback, this.#executorProcessId);
    if (feedback.phase === 'hot-inference' && this.#executorProcessId && feedback.processId === this.#executorProcessId &&
        Number.isSafeInteger(feedback.sequenceTokens) && feedback.sequenceTokens > 0 &&
        Number.isSafeInteger(feedback.batchSize) && feedback.batchSize > 0 && feedback.batchSize <= 128 &&
        Number.isSafeInteger(feedback.inputTokens) && feedback.inputTokens > 0 &&
        Number.isSafeInteger(feedback.paddedTokens) && feedback.paddedTokens >= feedback.inputTokens) {
      this.#lastBatchMeasurement = { processId: feedback.processId, backend: feedback.backend,
        sequenceTokens: feedback.sequenceTokens, batchSize: feedback.batchSize,
        inputTokens: feedback.inputTokens, paddedTokens: feedback.paddedTokens,
        paddingTokens: feedback.paddedTokens - feedback.inputTokens,
        ...(Number.isSafeInteger(feedback.bucketMinimumTokens) ? { bucketMinimumTokens: feedback.bucketMinimumTokens } : {}),
        ...(Number.isFinite(feedback.latencyMs) ? { latencyMs: feedback.latencyMs } : {}),
        source: 'owned-worker-hot-inference' };
    }
    this.#request = inferenceResourceRequest(this.#profile, this.#request.cpuThreads,
      { backend: feedback.backend ?? 'cpu', sequenceTokens: feedback.sequenceTokens ?? 128, batchSize: feedback.batchSize ?? 16 });
    // Cold load, queueing and tokenization do not train the hot inference throughput controller.
    // 冷加载、排队与分词采样不参与热推理吞吐比较，内存观察仍可单独修正下一请求预算。
    if (feedback.phase && feedback.phase !== 'hot-inference' && !feedback.allocationFailure) return;
    const lease = this.#gpuResident ?? this.#execution;
    if (!lease || !this.#service.report) return;
    // Native protocol rejects unknown fields; model cost samples never enter the allocator contract.
    // 原生协议拒绝未知字段；模型成本采样留在模型层，不直接扩充资源调度器的公开合同。
    const resourceFeedback = Object.fromEntries(['throughputPerSecond', 'latencyMs', 'queueDepth', 'allocationFailure',
      'foregroundLatencyMs', 'progress', 'phase', 'unit', 'backend'].filter(key => feedback[key] !== undefined)
      .map(key => [key, feedback[key]]));
    const result = await this.#service.report(lease.leaseId, resourceFeedback);
    if (result?.status === 'reported') this.#feedback = result.feedback;
  }

  adjustment(value) {
    if (!value || typeof value.reason !== 'string' || value.reason.length > 96) return;
    this.#adjustments.push({ appliedAt: Date.now(), phase: 'applied', reason: value.reason, boundary: value.boundary,
      requiresSessionRebuild: value.requiresSessionRebuild === true,
      ...(value.from ? { from: value.from } : {}), ...(value.to ? { to: value.to } : {}) });
    this.#adjustments = this.#adjustments.slice(-16);
  }

  status() {
    return { cpuThreads: this.#execution?.cpuThreads ?? 0, residentMemoryBytes: this.#residentBytes(),
      gpuMemoryBytes: this.#gpuResident?.gpuMemoryBytes ?? 0, leaseRenewalFailed: Boolean(this.#renewError),
      ...(this.#feedback ? { feedback: this.#feedback } : {}),
      memoryEstimate: this.#request.memoryEstimate, adjustments: this.#adjustments,
      cpuThreadAudit: { requested: this.#request.cpuThreads, granted: this.#execution?.cpuThreads ?? 0,
        configuredInSession: this.#appliedBackend?.cpuThreads ?? null, source: this.#appliedBackend?.source ?? 'not-reported',
        physicalActiveThreadsMeasured: false },
      ...(this.#appliedBackend ? { appliedBackend: this.#appliedBackend } : {}),
      ...(this.#lastBatchMeasurement ? { lastBatchMeasurement: this.#lastBatchMeasurement } : {}),
      ...(this.#lastGrant ? { lastGrant: this.#lastGrant } : {}), ...(this.#diagnostic ? { diagnostic: this.#diagnostic } : {}) };
  }
}

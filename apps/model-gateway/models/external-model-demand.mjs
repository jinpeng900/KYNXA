const MIB = 1024 * 1024;
const MAX_PLANNED_BYTES = 8 * 1024 ** 4;

function boundedBytes(value) {
  return Number.isSafeInteger(value) && value >= 0 && value <= MAX_PLANNED_BYTES ? value : null;
}

function positiveTokens(value) {
  return Number.isSafeInteger(value) && value > 0 && value <= 16_777_216 ? value : null;
}

function unknownPlan(snapshot, reason, breakdown) {
  return { state: 'unknown', requiresAdmission: false, memoryBytes: null, gpuMemoryBytes: null,
    observationOnly: true, memoryOwnership: 'external', observedAt: snapshot?.observedAt ?? null,
    uncertainty: { state: 'unbounded', reasons: [reason], marginBytes: null }, breakdown };
}

/** Plan only a pending allocation; observed model residency is already reflected in system free memory.
 * 只规划尚未发生的分配，已观测模型驻留已反映在系统空闲内存中，禁止再次整份预约。
 * This is an estimate, not a process ownership receipt or permission to change an external runtime.
 * 这是需求估算，不是进程占用归属回执，也不授权修改用户的外部推理服务。 */
export function planExternalModelDemand(snapshot, { contextTokens, hardware } = {}) {
  if (snapshot?.backend !== 'ollama') {
    return { ...unknownPlan(snapshot, 'EXTERNAL_MODEL_RESOURCE_API_UNAVAILABLE'), state: 'not-applicable' };
  }
  const requestedContextTokens = positiveTokens(contextTokens);
  const maximumContextTokens = positiveTokens(snapshot.modelMaximumContextTokens);
  const targetContextTokens = requestedContextTokens && maximumContextTokens
    ? Math.min(requestedContextTokens, maximumContextTokens) : requestedContextTokens;
  const baselineContextTokens = positiveTokens(snapshot.runtimeContextTokens);
  const estimatedWeightBytes = boundedBytes(snapshot.estimatedWeightBytes);
  const breakdown = { baselineContextTokens, targetContextTokens, estimatedWeightBytes,
    weightIncrementBytes: null, kvIncrementBytes: null, observedResidencyReservedAgain: false };
  if (snapshot.loaded === null || snapshot.loaded === undefined) {
    return unknownPlan(snapshot, 'EXTERNAL_MODEL_LOAD_STATE_UNKNOWN', breakdown);
  }
  const hasPartialContext = snapshot.loaded === false && !targetContextTokens && estimatedWeightBytes > 0;
  if (!targetContextTokens && !hasPartialContext) {
    return unknownPlan(snapshot, 'EXTERNAL_MODEL_CONTEXT_TARGET_UNKNOWN', breakdown);
  }

  // The loaded runtime context is allocation evidence; a Modelfile setting is only a requested default.
  // 已加载运行时的 context 才是分配依据，Modelfile 配置仅是请求默认值，不能代替已分配基线。
  if (snapshot.loaded && baselineContextTokens && targetContextTokens <= baselineContextTokens) {
    return { state: 'ready', requiresAdmission: false, memoryBytes: 0, gpuMemoryBytes: 0,
      observationOnly: true, memoryOwnership: 'external', observedAt: snapshot.observedAt ?? null,
      uncertainty: { state: 'no-increment-observed', reasons: [], marginBytes: 0 },
      breakdown: { ...breakdown, weightIncrementBytes: 0, kvIncrementBytes: 0 } };
  }

  const kvBytesPerToken = boundedBytes(snapshot.kvBytesPerToken);
  if (!kvBytesPerToken && !hasPartialContext) return unknownPlan(snapshot, 'EXTERNAL_MODEL_KV_SHAPE_UNKNOWN', breakdown);
  if (snapshot.loaded && !baselineContextTokens) {
    return unknownPlan(snapshot, 'EXTERNAL_MODEL_ALLOCATED_CONTEXT_UNKNOWN', breakdown);
  }
  if (!snapshot.loaded && !estimatedWeightBytes) {
    return unknownPlan(snapshot, 'EXTERNAL_MODEL_WEIGHT_ESTIMATE_UNKNOWN', breakdown);
  }
  const growthTokens = snapshot.loaded ? targetContextTokens - baselineContextTokens : targetContextTokens;
  const kvIncrementBytes = hasPartialContext ? null : boundedBytes(growthTokens * kvBytesPerToken);
  const weightIncrementBytes = snapshot.loaded ? 0 : estimatedWeightBytes;
  if (kvIncrementBytes === null && !hasPartialContext) return unknownPlan(snapshot, 'EXTERNAL_MODEL_DEMAND_EXCEEDS_BOUND', breakdown);

  const reasons = [hasPartialContext ? 'CONTEXT_TARGET_UNKNOWN' : 'KV_DTYPE_ASSUMED_F16', 'RUNTIME_SCRATCH_ESTIMATED'];
  let placement;
  if (snapshot.loaded) {
    const observedGpuMemoryBytes = boundedBytes(snapshot.observedGpuMemoryBytes);
    if (observedGpuMemoryBytes === null) return unknownPlan(snapshot, 'EXTERNAL_MODEL_DEVICE_PLACEMENT_UNKNOWN', breakdown);
    placement = observedGpuMemoryBytes > 0 ? 'gpu-upper-bound' : 'cpu';
    if (placement === 'gpu-upper-bound') reasons.push('KV_OFFLOAD_DISTRIBUTION_UNKNOWN');
  } else {
    const gpuState = hardware?.gpu?.state;
    if (gpuState !== 'available' && gpuState !== 'unavailable') {
      return unknownPlan(snapshot, 'EXTERNAL_MODEL_GPU_CAPACITY_UNKNOWN', breakdown);
    }
    placement = gpuState === 'available' ? 'gpu-upper-bound' : 'cpu';
    reasons.push('WEIGHTS_ESTIMATED_FROM_QUANTIZATION');
    if (placement === 'gpu-upper-bound') reasons.push('INITIAL_OFFLOAD_DISTRIBUTION_UNKNOWN');
  }

  // A missing server-selected context does not erase the known pending weights, but KV remains explicitly unknown.
  // 服务自行选择的 context 未知时仍保留已知待加载权重需求；KV 必须明确未知，不能当作零或完整准入。
  const pendingBytes = boundedBytes(weightIncrementBytes + (kvIncrementBytes ?? 0));
  if (pendingBytes === null) return unknownPlan(snapshot, 'EXTERNAL_MODEL_DEMAND_EXCEEDS_BOUND', breakdown);
  const uncertaintyMarginBytes = Math.ceil(Math.max(64 * MIB, pendingBytes * (snapshot.loaded ? 0.15 : 0.2)));
  // Unloaded placement is unknown: RAM may stage all weights while GPU receives them. This is an explicit upper bound.
  // 未加载时不确定卸载比例，RAM 可能暂存全部权重再传入 GPU；该预约明确是上界，绝非精确显存归属。
  const memoryBytes = boundedBytes(pendingBytes + uncertaintyMarginBytes);
  const gpuMemoryBytes = placement === 'gpu-upper-bound' ? boundedBytes(pendingBytes + uncertaintyMarginBytes) : 0;
  if (memoryBytes === null || gpuMemoryBytes === null) return unknownPlan(snapshot, 'EXTERNAL_MODEL_DEMAND_EXCEEDS_BOUND', breakdown);
  return { state: hasPartialContext ? 'partial' : 'ready', requiresAdmission: memoryBytes > 0 || gpuMemoryBytes > 0,
    memoryBytes, gpuMemoryBytes, observationOnly: true, memoryOwnership: 'external',
    partialCoverage: hasPartialContext, unknownComponents: hasPartialContext ? ['kv-cache'] : [],
    observedAt: snapshot.observedAt ?? null,
    uncertainty: { state: 'estimated-upper-bound', reasons, marginBytes: uncertaintyMarginBytes },
    breakdown: { ...breakdown, weightIncrementBytes, kvIncrementBytes, placement,
      kvEstimateDtype: hasPartialContext ? null : 'assumed-f16', memoryIncludesTransientStaging: !snapshot.loaded,
      gpuReservationIncludesOnlyPendingAllocations: true } };
}

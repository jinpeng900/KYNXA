import { mkdtemp, readdir, readFile, rm, stat } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';

const COMPATIBILITY_TOLERANCE = 1e-4;
const COMPUTE_OPERATORS = new Set(['MatMul', 'Gemm', 'FusedMatMul', 'MatMulIntegerToFloat', 'DynamicQuantizeMatMul',
  'Attention', 'QAttention', 'MultiHeadAttention', 'Conv', 'DmlFusedGraph']);

export function inferenceSessionOptions(device, cpuThreads, deviceId = 0) {
  const sessionOptions = { executionProviders: [device === 'cpu' ? 'cpu' : { name: device, deviceId }], intraOpNumThreads: cpuThreads,
    interOpNumThreads: 1, executionMode: 'sequential', logSeverityLevel: 3 };
  if (device !== 'cpu') {
    // A successful strict graph cannot silently route unsupported model operators to CPU.
    // 禁用隐式 CPU 算子回退，不能把仅创建 GPU 会话误称为实际全图 GPU 推理。
    sessionOptions.extra = { session: { disable_cpu_ep_fallback: '1' } };
    if (device === 'dml') sessionOptions.enableMemPattern = false;
  }
  return sessionOptions;
}

export function isInferenceMemoryPressure(error) {
  return /out of memory|bad_alloc|failed to allocate|not enough memory|insufficient memory|E_OUTOFMEMORY/iu.test(String(error?.message ?? error));
}

function gpuFailureReason(error) {
  const message = String(error?.message ?? error);
  if (isInferenceMemoryPressure(error)) return 'GPU_MEMORY_INSUFFICIENT';
  if (/cpu.*fallback|fallback.*cpu|disable_cpu_ep_fallback/iu.test(message)) return 'CPU_OPERATOR_FALLBACK_REQUIRED';
  if (/not implemented|unsupported.*operator|operator.*not supported|unimplemented/iu.test(message)) return 'MODEL_OPERATOR_UNSUPPORTED';
  if (/driver|LoadLibrary|device removed|device lost|DXGI_ERROR/iu.test(message)) return 'GPU_DRIVER_UNAVAILABLE';
  return 'GPU_BACKEND_INITIALIZATION_FAILED';
}

export function compatibleInferenceOutputs(reference, actual) {
  if (!Array.isArray(reference) || !Array.isArray(actual) || reference.length !== actual.length || !reference.length) return false;
  return reference.every((value, index) => Number.isFinite(value) && Number.isFinite(actual[index]) &&
    Math.abs(value - actual[index]) <= COMPATIBILITY_TOLERANCE * Math.max(1, Math.abs(value)));
}

export function auditGpuPartition(events, device = 'dml') {
  if (!Array.isArray(events) || events.length > 100_000) throw Object.assign(new Error('Invalid native operator profile.'), { code: 'INFERENCE_GPU_AUDIT_FAILED' });
  const provider = device === 'dml' ? 'DmlExecutionProvider' : 'CUDAExecutionProvider';
  const result = { version: 'ort-node-partition-v1', gpuProvider: provider, gpuKernelCount: 0, cpuKernelCount: 0,
    gpuComputeKernelCount: 0, cpuComputeKernelCount: 0, gpuDurationUs: 0, cpuDurationUs: 0,
    gpuOperators: Object.create(null), cpuOperators: Object.create(null) };
  for (const event of events) {
    if (event?.cat !== 'Node' || !event.args?.provider || !Number.isFinite(event.dur) || event.dur < 0) continue;
    const op = String(event.args.op_name ?? 'unknown');
    if (op.length > 64 || !/^[\w.]+$/u.test(op)) continue;
    if (event.args.provider === provider) {
      result.gpuKernelCount++; result.gpuDurationUs += event.dur;
      if (COMPUTE_OPERATORS.has(op)) result.gpuComputeKernelCount++;
      result.gpuOperators[op] = (result.gpuOperators[op] ?? 0) + 1;
    } else if (event.args.provider === 'CPUExecutionProvider') {
      result.cpuKernelCount++; result.cpuDurationUs += event.dur;
      if (COMPUTE_OPERATORS.has(op)) result.cpuComputeKernelCount++;
      result.cpuOperators[op] = (result.cpuOperators[op] ?? 0) + 1;
    }
  }
  result.usefulGpuCompute = result.gpuComputeKernelCount > 0 && result.gpuComputeKernelCount > result.cpuComputeKernelCount && result.gpuDurationUs > 0;
  return result;
}

export function qualifiedGpuOutputCompatibility(reference, actual, dimensions) {
  if (!Array.isArray(reference) || !Array.isArray(actual) || reference.length !== actual.length || !dimensions ||
      reference.length < dimensions * 2 || reference.length % dimensions || [...reference, ...actual].some(value => !Number.isFinite(value))) return undefined;
  let minCosine = 1, maxDifference = 0;
  for (let start = 0; start < reference.length; start += dimensions) {
    let dot = 0, referenceNorm = 0, actualNorm = 0;
    for (let offset = 0; offset < dimensions; offset++) {
      const left = reference[start + offset], right = actual[start + offset];
      dot += left * right; referenceNorm += left * left; actualNorm += right * right;
      maxDifference = Math.max(maxDifference, Math.abs(left - right));
    }
    if (Math.abs(referenceNorm - 1) > 1e-4 || Math.abs(actualNorm - 1) > 1e-4) return undefined;
    minCosine = Math.min(minCosine, dot / Math.sqrt(referenceNorm * actualNorm));
  }
  return minCosine >= 0.995 && maxDifference <= 0.03 ? { minCosine, maxDifference, embeddingSpaceQualified: true } : undefined;
}

/** Audit a separate GPU vector space; no CPU result can be returned under its metadata.
 * 审计独立 GPU 向量空间；CPU 结果绝不能使用该空间的元数据返回。 */
export async function loadAuditedGpuInferenceBackend({ device, deviceId = 0, cpuThreads = 1, dtype, dimensions }, {
  load, probe, compareOutputs = (reference, actual) => qualifiedGpuOutputCompatibility(reference, actual, dimensions),
  checkCancelled = () => {}, listProviders,
}) {
  if (device !== 'dml') throw Object.assign(new Error('This GPU profile requires a mapped DirectML backend.'), { code: 'INFERENCE_GPU_REQUIRED' });
  const directory = await mkdtemp(join(tmpdir(), 'kynxa-inference-audit-'));
  let cpuModel, auditedModel, model;
  try {
    checkCancelled();
    if (!(await listProviders()).some(provider => provider.name === device && provider.bundled !== false))
      throw Object.assign(new Error('Required GPU backend is not bundled.'), { code: 'INFERENCE_GPU_REQUIRED' });
    cpuModel = await load({ device: 'cpu', dtype, session_options: inferenceSessionOptions('cpu', cpuThreads) });
    const reference = await probe(cpuModel); checkCancelled();
    const hybridOptions = { ...inferenceSessionOptions(device, cpuThreads, deviceId), executionProviders: [{ name: device, deviceId }, 'cpu'] };
    delete hybridOptions.extra;
    auditedModel = await load({ device, dtype, session_options: { ...hybridOptions, enableProfiling: true, profileFilePrefix: join(directory, 'partition') } });
    const actual = await probe(auditedModel); checkCancelled();
    const compatibility = compareOutputs(reference, actual);
    if (!compatibility) throw Object.assign(new Error('GPU probes are incompatible with the pinned model semantics.'), { code: 'INFERENCE_GPU_OUTPUT_INCOMPATIBLE' });
    const repeated = await probe(auditedModel); checkCancelled();
    if (!compatibleInferenceOutputs(actual, repeated))
      throw Object.assign(new Error('GPU probes are not repeatable.'), { code: 'INFERENCE_GPU_OUTPUT_INCOMPATIBLE' });
    // Node 1.21 profiling methods are no-ops; native session disposal flushes the enabled trace.
    // Node 1.21 的 profiling 方法不执行结束操作；释放原生会话才会刷新已启用的追踪文件。
    await auditedModel.dispose(); auditedModel = undefined;
    const files = (await readdir(directory)).filter(filename => /^partition.*\.json$/u.test(filename));
    if (files.length !== 1) throw Object.assign(new Error('Native operator audit did not produce exactly one trace.'), { code: 'INFERENCE_GPU_AUDIT_FAILED' });
    const path = join(directory, files[0]);
    if ((await stat(path)).size > 16 * 1024 * 1024) throw Object.assign(new Error('Native operator audit exceeds its bound.'), { code: 'INFERENCE_GPU_AUDIT_FAILED' });
    const partition = auditGpuPartition(JSON.parse(await readFile(path, 'utf8')), device);
    if (!partition.usefulGpuCompute) throw Object.assign(new Error('No useful GPU compute was proved.'), { code: 'INFERENCE_GPU_AUDIT_FAILED' });
    await cpuModel.dispose(); cpuModel = undefined;
    model = await load({ device, dtype, session_options: hybridOptions }); checkCancelled();
    const finalProbe = await probe(model);
    if (!compatibleInferenceOutputs(actual, finalProbe))
      throw Object.assign(new Error('Unprofiled GPU session changed the verified projection.'), { code: 'INFERENCE_GPU_OUTPUT_INCOMPATIBLE' });
    return { model, status: { device, deviceId, cpuThreads, dtype, gpuValidated: true, executionMode: 'hybrid',
      cpuOperatorFallback: true, operatorAudit: partition, compatibility } };
  } catch (error) {
    await model?.dispose(); await auditedModel?.dispose(); await cpuModel?.dispose(); throw error;
  } finally {
    const suffix = relative(resolve(tmpdir()), resolve(directory));
    if (suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`)) await rm(directory, { recursive: true, force: true });
  }
}

/** Probe the pinned model itself, with unchanged dtype and a real inference before advertising GPU.
 * 使用固定模型、相同 dtype 与真实推理验证后端，成功前绝不宣称 GPU 已可用。 */
export async function loadVerifiedInferenceBackend({ device = 'cpu', deviceId = 0, cpuThreads = 1, dtype }, {
  load, probe, checkCancelled = () => {}, listProviders,
}) {
  checkCancelled();
  // Start the reference session first: Transformers.js retains its first session initialization promise.
  // 先建立 CPU 参考会话：Transformers.js 会保留首次初始化 Promise，避免 GPU 失败污染 CPU 兜底。
  const cpuModel = await load({ device: 'cpu', dtype, session_options: inferenceSessionOptions('cpu', cpuThreads) });
  let gpuModel;
  try {
    checkCancelled();
    if (device === 'cpu') return { model: cpuModel, status: { device: 'cpu', cpuThreads, gpuValidated: false, dtype } };
    const providers = await listProviders();
    if (!providers.some(provider => provider.name === device && provider.bundled !== false)) {
      return { model: cpuModel, status: { device: 'cpu', cpuThreads, gpuValidated: false, dtype,
        diagnostic: { code: 'GPU_BACKEND_NOT_BUNDLED', requestedDevice: device } } };
    }
    const reference = await probe(cpuModel);
    checkCancelled();
    gpuModel = await load({ device, dtype, session_options: inferenceSessionOptions(device, cpuThreads, deviceId) });
    const actual = await probe(gpuModel);
    checkCancelled();
    if (!compatibleInferenceOutputs(reference, actual)) throw Object.assign(new Error('Backend changed pinned-model output beyond tolerance.'),
      { code: 'GPU_EMBEDDING_INCOMPATIBLE' });
    await cpuModel.dispose();
    return { model: gpuModel, status: { device, deviceId, cpuThreads, gpuValidated: true, dtype,
      cpuOperatorFallback: false, compatibilityTolerance: COMPATIBILITY_TOLERANCE } };
  } catch (error) {
    await gpuModel?.dispose();
    if (error?.code?.endsWith('_CANCELLED') || error?.name === 'AbortError') {
      await cpuModel.dispose();
      throw error;
    }
    try { checkCancelled(); }
    catch (cancelled) { await cpuModel.dispose(); throw cancelled; }
    return { model: cpuModel, status: { device: 'cpu', cpuThreads, gpuValidated: false, dtype,
      diagnostic: { code: error.code === 'GPU_EMBEDDING_INCOMPATIBLE' ? error.code
        : isInferenceMemoryPressure(error) ? 'GPU_MEMORY_INSUFFICIENT' : 'GPU_MODEL_PROBE_FAILED',
        reason: gpuFailureReason(error), requestedDevice: device } } };
  }
}

// Bucketing reduces padding without changing the caller's output order; retries stay within the grant.
// 按 token 长度分桶减少 padding，输出仍按原输入顺序排列；减批重试始终受已批准额度约束。
export async function executeInferenceBatches(items, { batchSize = 1, batchTokenBudget = 512, tokenLengths,
  activationMemoryBytes, hiddenSize, attentionHeads,
  infer, checkCancelled = () => {}, yieldToMessages = () => Promise.resolve(), onPressure = () => {}, onMeasurement = () => {} }) {
  if (!Array.isArray(items) || !Array.isArray(tokenLengths) || tokenLengths.length !== items.length ||
      tokenLengths.some(length => !Number.isSafeInteger(length) || length < 1) ||
      !Number.isSafeInteger(batchTokenBudget) || batchTokenBudget < 1)
    throw Object.assign(new Error('Invalid token lengths or approved batch token budget.'), { code: 'INFERENCE_INVALID_BATCH' });
  const order = items.map((_, index) => index).sort((left, right) => (tokenLengths[left] - tokenLengths[right]) || left - right);
  const results = new Array(items.length);
  let offset = 0, activeBatchSize = Math.max(1, Math.min(128, batchSize)), retries = 0;
  while (offset < order.length) {
    await yieldToMessages(); checkCancelled();
    let count = Math.min(activeBatchSize, order.length - offset);
    const activationCost = () => {
      const sequenceTokens = tokenLengths[order[offset + count - 1]];
      return count * (sequenceTokens * hiddenSize * 4 * 8 + sequenceTokens ** 2 * attentionHeads * 4);
    };
    // Padded attention tensors grow quadratically with sequence length; tokens alone are not a memory bound.
    // padding 后 attention 张量随序列长度平方增长，只有总 token 限制不能代表已批准显存边界。
    // Keep each bucket within twice its shortest sequence, then enforce the approved padded/token budgets.
    // 每个桶最长序列不超过最短序列两倍，再按已批准的 padding/token 预算减批，避免短长输入混装。
    while (count > 1 && (tokenLengths[order[offset + count - 1]] > tokenLengths[order[offset]] * 2 ||
        tokenLengths[order[offset + count - 1]] * count > batchTokenBudget ||
        activationMemoryBytes && hiddenSize && attentionHeads && activationCost() > activationMemoryBytes)) count--;
    if (tokenLengths[order[offset]] > batchTokenBudget)
      throw Object.assign(new Error('A single inference input exceeds the approved batch token budget.'), { code: 'INFERENCE_RESOURCE_BUSY' });
    if (activationMemoryBytes && hiddenSize && attentionHeads && activationCost() > activationMemoryBytes)
      throw Object.assign(new Error('A single inference input exceeds the approved activation memory budget.'), { code: 'INFERENCE_RESOURCE_BUSY' });
    const indexes = order.slice(offset, offset + count);
    try {
      const started = performance.now();
      const values = await infer(indexes.map(index => items[index]));
      const latencyMs = Math.max(0.01, performance.now() - started);
      checkCancelled();
      if (!Array.isArray(values) || values.length !== indexes.length) throw new Error('Invalid batched inference result count.');
      indexes.forEach((index, position) => { results[index] = values[position]; });
      offset += count;
      const sequenceTokens = Math.max(...indexes.map(index => tokenLengths[index]));
      const inputTokens = indexes.reduce((sum, index) => sum + tokenLengths[index], 0);
      onMeasurement({ latencyMs, throughputPerSecond: inputTokens * 1000 / latencyMs,
        phase: 'hot-inference', unit: 'tokens', sequenceTokens, inputTokens, paddedTokens: sequenceTokens * count, batchSize: count,
        bucketMinimumTokens: tokenLengths[indexes[0]], paddingTokens: sequenceTokens * count - inputTokens,
        paddingRatio: (sequenceTokens * count - inputTokens) / (sequenceTokens * count), retries,
        queueDepth: Math.max(0, order.length - offset), progress: offset / Math.max(1, order.length) });
    } catch (error) {
      checkCancelled();
      if (!isInferenceMemoryPressure(error) || count <= 1 || retries >= 5) throw error;
      activeBatchSize = Math.max(1, Math.floor(count / 2)); retries++;
      onPressure({ batchSize: activeBatchSize, retries, code: 'INFERENCE_BATCH_REDUCED' });
    }
  }
  return results;
}

import { createHash } from 'node:crypto';
import { isIP } from 'node:net';

const MIB = 1024 * 1024;
const MAX_RESPONSE_BYTES = 1024 * 1024;
const MAX_CACHE_ENTRIES = 32;
const MAX_ESTIMATED_BYTES = 8 * 1024 ** 4;

function cancelledError() {
  return Object.assign(new Error('Local model observation cancelled.'), { name: 'AbortError', code: 'LOCAL_MODEL_OBSERVATION_CANCELLED' });
}

function localEndpoint(connection) {
  let url;
  try { url = new URL(connection?.baseUrl); } catch { return undefined; }
  const host = url.hostname.replace(/^\[|\]$/gu, '').toLowerCase();
  if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password || url.search || url.hash ||
      !(host === 'localhost' || host === '::1' || isIP(host) === 4 && host.split('.')[0] === '127')) return undefined;
  return url;
}

function selectedModel(connection, modelId) {
  const value = modelId ?? connection?.modelId ?? connection?.model ?? connection?.models?.[0];
  return typeof value === 'string' && value.length > 0 && value.length <= 160 && !/[\x00-\x1f]/u.test(value) ? value : undefined;
}

function identity(url, model) {
  return createHash('sha256').update(`${url.origin}\0${model ?? ''}`).digest('hex');
}

function positiveInteger(value) {
  return Number.isSafeInteger(value) && value >= 0 ? value : null;
}

function architectureInfo(show) {
  const info = show?.model_info ?? {};
  const architecture = info['general.architecture'];
  if (typeof architecture !== 'string' || !/^[a-z0-9_]{1,48}$/iu.test(architecture)) return {};
  const number = suffix => positiveInteger(info[`${architecture}.${suffix}`]);
  const hidden = number('embedding_length'), heads = number('attention.head_count');
  return { layers: number('block_count'), kvHeads: number('attention.head_count_kv') ?? heads,
    keyLength: number('attention.key_length') ?? (hidden && heads && hidden % heads === 0 ? hidden / heads : null),
    valueLength: number('attention.value_length') ?? (hidden && heads && hidden % heads === 0 ? hidden / heads : null),
    maximumContextTokens: number('context_length') };
}

function matchingModel(models, model) {
  return models.find(item => item?.model === model || item?.name === model ||
    !model.includes(':') && (item?.model === `${model}:latest` || item?.name === `${model}:latest`));
}

function allocationMetadata(show, running, shape, modelFileBytes) {
  const parameterCount = positiveInteger(show?.model_info?.['general.parameter_count']);
  const quantization = show?.details?.quantization_level ?? running?.details?.quantization_level;
  const quantizationLevel = typeof quantization === 'string' && /^[a-z0-9_]{1,32}$/iu.test(quantization)
    ? quantization.toUpperCase() : null;
  // The effective bits include bounded quantization scales; file names and model templates remain private.
  // 有效位宽包括有界量化比例开销，不导出模型路径、文件名或提示模板。
  const bits = { Q2_K: 3, Q3_K_S: 4, Q3_K_M: 4.5, Q3_K_L: 5, Q4_0: 5, Q4_1: 5,
    Q4_K_S: 5, Q4_K_M: 5.5, Q5_0: 6, Q5_1: 6, Q5_K_S: 6, Q5_K_M: 6.5,
    Q6_K: 7, Q8_0: 9, Q8_1: 9, F16: 16, BF16: 16, F32: 32 }[quantizationLevel];
  const weightBytes = modelFileBytes || (parameterCount && bits ? Math.ceil(parameterCount * bits / 8) : null);
  const hasWeightEstimate = Number.isSafeInteger(weightBytes) && weightBytes > 0;
  const kvBytesPerToken = shape.layers && shape.kvHeads && shape.keyLength && shape.valueLength
    ? shape.layers * shape.kvHeads * (shape.keyLength + shape.valueLength) * 2 : null;
  return { parameterCount: parameterCount || null, quantizationLevel,
    estimatedWeightBytes: hasWeightEstimate ? weightBytes : null,
    modelFileBytes: modelFileBytes ?? null,
    weightEstimate: hasWeightEstimate ? { exact: false, effectiveBitsPerParameter: modelFileBytes ? null : bits,
      source: modelFileBytes ? 'serialized-size' : 'parameter-count-and-quantization', includesRuntimeScratch: false,
      ...(modelFileBytes ? { includesQuantizationMetadata: true } : {}) } : null,
    kvBytesPerToken: Number.isSafeInteger(kvBytesPerToken) && kvBytesPerToken > 0 ? kvBytesPerToken : null };
}

function allocationBackend(running, configuredGpuLayers) {
  const sizeBytes = positiveInteger(running?.size), gpuBytes = positiveInteger(running?.size_vram);
  const state = running && gpuBytes !== null && sizeBytes > 0 && gpuBytes <= sizeBytes
    ? gpuBytes === 0 ? 'cpu' : gpuBytes < sizeBytes ? 'mixed' : 'gpu' : 'unknown';
  return { state, source: state === 'unknown' ? 'unreported' : 'ollama-api-ps-residency',
    configuredGpuLayers, executionBackend: 'unknown', gpuDeviceIdentity: 'unknown', exact: false };
}

function kvCacheEstimate(shape, contextTokens, backend) {
  if (!contextTokens || !shape.layers || !shape.kvHeads || !shape.keyLength || !shape.valueLength ||
      shape.layers > 512 || shape.kvHeads > 256 || shape.keyLength > 4096 || shape.valueLength > 4096) return null;
  const f16PerToken = shape.layers * shape.kvHeads * (shape.keyLength + shape.valueLength) * 2;
  // The public API does not report OLLAMA_KV_CACHE_TYPE. Weight quantization and gateway env are not KV evidence.
  // 公开 API 不报告 OLLAMA_KV_CACHE_TYPE；权重量化和网关环境变量都不能冒充外部服务 KV dtype。
  const quantizedPerToken = blockBytes => shape.layers * shape.kvHeads *
    (Math.ceil(shape.keyLength / 32) + Math.ceil(shape.valueLength / 32)) * blockBytes;
  const maximumBytes = contextTokens * f16PerToken;
  if (!Number.isSafeInteger(maximumBytes) || maximumBytes > MAX_ESTIMATED_BYTES) return null;
  return { dtype: 'unknown', dtypeSource: 'ollama-api-not-reported', reservationDtype: 'f16-upper-bound',
    supportedDtypeEstimates: { f16BytesPerToken: f16PerToken, q8_0BytesPerToken: quantizedPerToken(34),
      q4_0BytesPerToken: quantizedPerToken(18) },
    minimumBytes: contextTokens * Math.min(f16PerToken, quantizedPerToken(18)), maximumBytes,
    contextTokens, backend: backend.state, parallelSlots: 'unknown', accountsForSlidingWindow: false,
    includesParallelSlotMultiplication: false, exact: false };
}

/** Read-only coordination for a user-owned local inference server. Never load/unload or stop its models.
 * 用户拥有的本机推理服务只做观察与协调，绝不加载、卸载模型或停止其进程。 */
export class LocalModelResourceObserver {
  #fetch; #timeoutMs; #cacheTtlMs; #cache = new Map(); #active = new Map();

  constructor({ fetchImpl = globalThis.fetch, timeoutMs = 1200, cacheTtlMs = 2000 } = {}) {
    if (typeof fetchImpl !== 'function') throw new TypeError('Local model observation requires a fetch implementation.');
    this.#fetch = fetchImpl;
    this.#timeoutMs = Math.max(50, Math.min(5000, timeoutMs));
    this.#cacheTtlMs = Math.max(0, Math.min(10_000, cacheTtlMs));
  }

  beginGeneration(connection, { modelId } = {}) {
    const url = localEndpoint(connection);
    if (!url) return () => {};
    const key = identity(url, selectedModel(connection, modelId));
    if (!this.#active.has(key) && this.#active.size >= 128) return () => {};
    this.#active.set(key, (this.#active.get(key) ?? 0) + 1);
    let released = false;
    return () => {
      if (released) return;
      released = true;
      const remaining = (this.#active.get(key) ?? 1) - 1;
      if (remaining > 0) this.#active.set(key, remaining); else this.#active.delete(key);
    };
  }

  async #read(url, connection, { signal, method = 'GET', body } = {}) {
    if (signal?.aborted) throw cancelledError();
    const controller = new AbortController();
    const abort = () => controller.abort();
    signal?.addEventListener('abort', abort, { once: true });
    const timer = setTimeout(() => controller.abort(), this.#timeoutMs);
    let reader;
    try {
      const headers = { Accept: 'application/json', ...(body ? { 'Content-Type': 'application/json' } : {}) };
      if (connection.apiKey) headers.Authorization = `Bearer ${connection.apiKey}`;
      // Redirects must not send model credentials to a different endpoint.
      // 禁止重定向，防止模型连接凭据被带到另一个地址；响应错误文本也不会进入诊断。
      const response = await this.#fetch(url, { method, headers, body, signal: controller.signal, redirect: 'error' });
      if (!response.ok) throw Object.assign(new Error('Local model observation endpoint is unavailable.'), { code: 'LOCAL_MODEL_OBSERVATION_HTTP', status: response.status });
      if (Number(response.headers?.get('content-length')) > MAX_RESPONSE_BYTES) throw new Error('Observation response exceeds its bound.');
      reader = response.body?.getReader();
      if (!reader) throw new Error('Observation response body is unavailable.');
      const chunks = []; let length = 0;
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        length += value.byteLength;
        if (length > MAX_RESPONSE_BYTES) throw new Error('Observation response exceeds its bound.');
        chunks.push(Buffer.from(value));
      }
      return JSON.parse(Buffer.concat(chunks, length).toString('utf8'));
    } catch (error) {
      if (signal?.aborted) throw cancelledError();
      throw Object.assign(new Error('Local model observation did not return a bounded valid response.'), {
        code: controller.signal.aborted ? 'LOCAL_MODEL_OBSERVATION_TIMEOUT' : error.code ?? 'LOCAL_MODEL_OBSERVATION_UNAVAILABLE' });
    } finally {
      clearTimeout(timer); signal?.removeEventListener('abort', abort);
      await reader?.cancel().catch(() => {});
    }
  }

  async observe(connection, { signal, contextTokens, modelId, refresh = false } = {}) {
    if (signal?.aborted) throw cancelledError();
    const url = localEndpoint(connection), model = selectedModel(connection, modelId);
    const unknown = { backend: 'external-openai', loaded: null, contextTokens: null,
      observedMemoryBytes: null, observedGpuMemoryBytes: null, estimatedKvCacheBytes: null,
      uncertaintyMarginBytes: 256 * MIB, observationOnly: true, memoryOwnership: 'external',
      generationState: 'unknown', applicationGenerationState: 'idle', globalGenerationState: 'unknown' };
    if (!url) return { ...unknown, diagnostic: { code: 'LOCAL_MODEL_OBSERVATION_NOT_LOCAL' } };
    const key = identity(url, model);
    const activity = () => ({ generationState: this.#active.has(key) ? 'generating' : 'unknown',
      applicationGenerationState: this.#active.has(key) ? 'generating' : 'idle', globalGenerationState: 'unknown' });
    const isOllama = /ollama/iu.test(connection.providerId ?? '') || url.port === '11434';
    if (!isOllama || !model) return { ...unknown, ...activity(), diagnostic: {
      code: !model ? 'LOCAL_MODEL_OBSERVATION_MODEL_UNSPECIFIED' : 'LOCAL_MODEL_RESOURCE_API_UNAVAILABLE' } };
    const requestedContext = positiveInteger(contextTokens);
    const cacheKey = `${key}:${requestedContext ?? 'unknown'}`;
    const cached = this.#cache.get(cacheKey);
    if (!refresh && cached && Date.now() - cached.observedAt <= this.#cacheTtlMs) return { ...cached, ...activity(), cached: true };
    const [runningResult, detailsResult] = await Promise.allSettled([
      this.#read(new URL('/api/ps', url), connection, { signal }),
      this.#read(new URL('/api/show', url), connection, { signal, method: 'POST', body: JSON.stringify({ model, verbose: false }) }),
    ]);
    if (signal?.aborted) throw cancelledError();
    const hasRunningList = runningResult.status === 'fulfilled' && Array.isArray(runningResult.value?.models);
    const hasCompleteRunningList = hasRunningList && runningResult.value.models.length <= 128;
    const running = hasRunningList ? matchingModel(runningResult.value.models.slice(0, 128), model) : undefined;
    const show = detailsResult.status === 'fulfilled' ? detailsResult.value : undefined;
    const shape = architectureInfo(show);
    const parameters = typeof show?.parameters === 'string' && show.parameters.length <= 16_384 ? show.parameters : '';
    const configuredContext = positiveInteger(Number(/^\s*num_ctx\s+(\d+)\s*$/mu.exec(parameters)?.[1]));
    const configuredGpuLayers = positiveInteger(Number(/^\s*num_gpu\s+(\d+)\s*$/mu.exec(parameters)?.[1]));
    const runtimeContext = positiveInteger(running?.context_length);
    const effectiveContext = runtimeContext || configuredContext || null;
    const estimationContext = requestedContext || effectiveContext;
    const backendSelection = allocationBackend(running, configuredGpuLayers);
    const kvEstimate = kvCacheEstimate(shape, estimationContext, backendSelection);
    const estimatedKvCacheBytes = kvEstimate?.maximumBytes ?? null;
    const observedMemoryBytes = positiveInteger(running?.size), observedGpuMemoryBytes = positiveInteger(running?.size_vram);
    let modelFileBytes = null;
    if (!running && hasCompleteRunningList) {
      // Tags report serialized weight size, whereas ps.size may include allocated KV. Use tags only for cold loading.
      // tags 返回序列化模型大小，ps.size 则可能含已分配 KV；冷加载才读取 tags，防止驻留量冒充权重。
      try {
        const tags = await this.#read(new URL('/api/tags', url), connection, { signal });
        if (Array.isArray(tags?.models) && tags.models.length <= 128) {
          const bytes = positiveInteger(matchingModel(tags.models, model)?.size);
          if (bytes > 0) modelFileBytes = bytes;
        }
      } catch {
        // Optional listing failures retain the parameter estimate and never expose remote error bodies.
        // 可选列表失败保留参数量估算，不导出远端错误正文；明确取消仍传播给调用方。
        if (signal?.aborted) throw cancelledError();
      }
    }
    if (signal?.aborted) throw cancelledError();
    const allocation = allocationMetadata(show, running, shape, modelFileBytes);
    const snapshot = { ...unknown, backend: 'ollama', loaded: running ? true : hasCompleteRunningList ? false : null,
      contextTokens: effectiveContext, runtimeContextTokens: runtimeContext, configuredContextTokens: configuredContext,
      modelMaximumContextTokens: shape.maximumContextTokens ?? null,
      observedMemoryBytes, observedGpuMemoryBytes, estimatedKvCacheBytes, ...allocation,
      backendSelection,
      kvBytesPerToken: kvEstimate?.supportedDtypeEstimates.f16BytesPerToken ?? null,
      observedMemoryScope: 'server-reported-model-size-not-host-rss',
      kvCacheEstimate: kvEstimate,
      uncertaintyComponents: ['kv-dtype-not-reported', 'parallel-slots-not-reported', 'runtime-scratch-not-reported',
        ...(backendSelection.state === 'unknown' ? ['execution-device-not-reported'] : [])],
      // The server may already include KV buffers in size/size_vram. Never add this estimate to measured residency.
      // 服务返回的 size/size_vram 可能已含 KV 缓冲，估算只描述不确定性，不能再次叠加扣除已观测驻留量。
      uncertaintyMarginBytes: Math.ceil(Math.max(256 * MIB, (observedMemoryBytes ?? estimatedKvCacheBytes ?? 0) * 0.2)),
      observedAt: Date.now(), source: hasRunningList ? 'ollama-api-ps' : 'unknown',
      activityScope: 'this-application', ...activity(),
      ...(!hasCompleteRunningList || detailsResult.status === 'rejected' ? { diagnostic: {
        code: runningResult.status === 'rejected' ? runningResult.reason.code
          : !hasRunningList ? 'LOCAL_MODEL_OBSERVATION_INVALID_PAYLOAD'
            : !hasCompleteRunningList ? 'LOCAL_MODEL_OBSERVATION_LIST_LIMIT' : detailsResult.reason.code,
        metadataPartial: true } } : {}) };
    this.#cache.delete(cacheKey); this.#cache.set(cacheKey, snapshot);
    while (this.#cache.size > MAX_CACHE_ENTRIES) this.#cache.delete(this.#cache.keys().next().value);
    return { ...snapshot, cached: false };
  }

  clear() { this.#cache.clear(); }
}

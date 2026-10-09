import { openNativeInferencePort } from './native-inference-port.mjs';
import { BUILTIN_RERANKER_PROFILE } from './reranker-profile.mjs';
import { verifyEmbeddingAsset } from './embedding-assets.mjs';
import { resolveRetrievalModelProfile } from './model-registry.mjs';
import { executeInferenceBatches, isInferenceMemoryPressure, loadAuditedGpuInferenceBackend, loadVerifiedInferenceBackend } from './inference-backend.mjs';
import { InferenceAdmission } from './inference-admission.mjs';

const { parentPort, workerData } = await openNativeInferencePort();
const profile = resolveRetrievalModelProfile('reranker', workerData.profileId ?? BUILTIN_RERANKER_PROFILE.id);

let tokenizer, model, loadPromise, shutdownPromise;
let queue = Promise.resolve(), closing = false;
let lastReceivedId = 0;
const cancelled = new Set(), active = new Set();
const admission = new InferenceAdmission(workerData.requestLimits);
const isCancelled = id => closing || cancelled.has(id);
const phase = value => { if (!closing) parentPort.postMessage({ type: 'phase', phase: value }); };
let activeBudget = { device: 'cpu', cpuThreads: workerData.cpuThreads ?? 1, batchSize: 1, batchTokenBudget: 512 };
let loadedBudget;
let lastMeasurementAt = 0;
const processBaselineBytes = process.memoryUsage().rss;
let workloadPeakBytes = 0;

function memoryMeasurement(backend) {
  // An owned native process measures its whole workload delta, not a fabricated model-weight RSS.
  // 独立受管原生进程只测整体工作负载增量，不把它伪装成单个模型权重的精确 RSS。
  workloadPeakBytes = Math.max(workloadPeakBytes, Math.max(0, process.memoryUsage().rss - processBaselineBytes));
  return { backend, processId: process.pid, workloadMemoryDeltaBytes: workloadPeakBytes,
    memoryMeasurementSource: 'isolated-native-process-delta' };
}

function checkCancelled(id) {
  if (isCancelled(id)) throw Object.assign(new Error('Reranking cancelled.'), { code: 'RERANK_CANCELLED' });
}

async function loadModel(id) {
  checkCancelled(id);
  phase('verifying-assets');
  for (const asset of profile.files) {
    checkCancelled(id);
    if (!await verifyEmbeddingAsset(workerData.modelRoot, asset))
      throw Object.assign(new Error('Reranker asset integrity verification failed.'), { code: 'RERANK_ASSET_INVALID' });
    checkCancelled(id);
  }
  const { env, AutoTokenizer, AutoModelForSequenceClassification } = await import('@huggingface/transformers');
  checkCancelled(id);
  env.allowRemoteModels = false; env.allowLocalModels = true;
  env.useFSCache = false; env.useBrowserCache = false; env.useCustomCache = false;
  globalThis.fetch = async () => { throw Object.assign(new Error('Reranker networking is disabled.'), { code: 'RERANK_NETWORK_DISABLED' }); };
  phase('loading-model');
  tokenizer = await AutoTokenizer.from_pretrained(workerData.modelRoot, { local_files_only: true });
  checkCancelled(id);
  const backendLoader = profile.requiredDevice ? loadAuditedGpuInferenceBackend : loadVerifiedInferenceBackend;
  const backend = await backendLoader({ ...activeBudget, dtype: profile.dtype }, {
    load: options => AutoModelForSequenceClassification.from_pretrained(workerData.modelRoot, { local_files_only: true, ...options }),
    probe: async candidate => {
      const inputs = profile.requiredDevice ? tokenizer(['password recovery', 'password recovery', '如何重置密码'],
        { text_pair: ['Reset your password in account settings.', 'Tomorrow will be sunny.', 'Reset your password in account settings.'],
          padding: true, truncation: true, max_length: profile.maxInputTokens })
        : tokenizer('password recovery', { text_pair: 'Reset your password in account settings.', padding: true, truncation: true,
        max_length: profile.maxInputTokens });
      let output;
      try { output = await candidate(inputs); return Array.from(output.logits.data, Number); }
      finally {
        for (const tensor of Object.values(output ?? {})) tensor?.dispose?.();
        for (const tensor of Object.values(inputs)) tensor?.dispose?.();
      }
    },
    checkCancelled: () => checkCancelled(id),
    listProviders: async () => (await import('onnxruntime-node')).listSupportedBackends(),
    compareOutputs: (reference, actual) => {
      if (reference.length !== 3 || actual.length !== 3 || [...reference, ...actual].some(value => !Number.isFinite(value))) return undefined;
      const sigmoid = value => value >= 0 ? 1 / (1 + Math.exp(-value)) : Math.exp(value) / (1 + Math.exp(value));
      const differences = reference.map((value, index) => Math.abs(sigmoid(value) - sigmoid(actual[index])));
      if (Math.max(...differences) > 0.05 || reference[0] <= reference[1] || actual[0] <= actual[1] ||
          reference[2] <= reference[1] || actual[2] <= actual[1]) return undefined;
      return { maxProbabilityDifference: Math.max(...differences), probeRankAgreement: true, separateProfile: true };
    },
  });
  model = backend.model;
  if (backend.status.device === 'cpu') {
    const { activationMemoryBytes, hiddenSize, attentionHeads, deviceId, ...remainingBudget } = activeBudget;
    activeBudget = { ...remainingBudget, ...activeBudget.cpuFallbackBatchBudget, device: 'cpu', gpuMemoryBytes: 0 };
  }
  loadedBudget = { ...activeBudget, device: backend.status.device };
  activeBudget = loadedBudget;
  if (backend.status.device === 'cpu' && activeBudget.diagnostic) backend.status.diagnostic ??= activeBudget.diagnostic;
  if (!closing) parentPort.postMessage({ type: 'ready', inferenceBackend: backend.status });
  if (!closing) parentPort.postMessage({ type: 'measurement', feedback: {
    ...memoryMeasurement(backend.status.device), phase: 'cold-load', unit: 'tokens' } });
}

async function rank(message, canRecoverGpuMemory = true) {
  if (isCancelled(message.id)) return;
  const requestedBudget = message.resourceBudget ?? activeBudget;
  if (workerData.devicePreference === 'cpu' && requestedBudget.device !== 'cpu')
    throw Object.assign(new Error('CPU preference cannot dispatch GPU inference. / CPU 偏好不能派发 GPU 推理。'),
      { code: 'RERANK_DEVICE_PREFERENCE_CHANGED' });
  if (model && (requestedBudget.cpuThreads !== loadedBudget.cpuThreads || requestedBudget.device !== loadedBudget.device ||
      requestedBudget.deviceId !== loadedBudget.deviceId)) {
    parentPort.postMessage({ type: 'adjustment', adjustment: { boundary: 'completed-request', requiresSessionRebuild: true,
      reason: requestedBudget.device !== loadedBudget.device || requestedBudget.deviceId !== loadedBudget.deviceId
        ? 'approved-backend-changed' : 'approved-thread-count-changed',
      from: { device: loadedBudget.device, cpuThreads: loadedBudget.cpuThreads },
      to: { device: requestedBudget.device, cpuThreads: requestedBudget.cpuThreads } } });
    await model.dispose(); model = undefined; loadPromise = undefined;
  }
  activeBudget = requestedBudget;
  await (loadPromise ??= loadModel(message.id).catch(error => {
    loadPromise = undefined;
    if (error.code === 'RERANK_CANCELLED') throw error;
    const code = error.code === 'ENOENT' ? 'RERANK_ASSET_MISSING' : error.code ?? 'RERANK_RUNTIME_UNAVAILABLE';
    parentPort.postMessage({ type: 'fatal', code, message: 'The pinned local reranker could not be loaded.' });
    shutdown();
    throw error;
  }));
  if (isCancelled(message.id)) return;
  const tokenLengths = [];
  let truncatedInputsCount = 0;
  for (const text of message.texts) {
    await new Promise(resolve => setImmediate(resolve));
    if (isCancelled(message.id)) return;
    const raw = tokenizer(message.query, { text_pair: text, padding: false, truncation: false, return_tensor: false });
    if (raw.input_ids.length > profile.maxInputTokens) truncatedInputsCount++;
    tokenLengths.push(Math.min(profile.maxInputTokens, raw.input_ids.length));
  }
  if (tokenLengths.reduce((sum, count) => sum + count, 0) > admission.limits.maxBatchInputTokens)
    throw Object.assign(new Error('Reranker batch exceeds its configured token budget.'), { code: 'RERANK_INPUT_TOO_LONG' });
  let scores;
  let hasReportedFirstBatch = false;
  try { scores = await executeInferenceBatches(message.texts, { ...activeBudget, tokenLengths,
    checkCancelled: () => checkCancelled(message.id), yieldToMessages: () => new Promise(resolve => setImmediate(resolve)),
    onPressure: diagnostic => parentPort.postMessage({ type: 'resource-pressure', diagnostic: { ...diagnostic,
      backend: activeBudget.device, boundary: 'completed-native-batch', requiresSessionRebuild: false } }),
    onMeasurement: feedback => {
      // Keep a queued hot sample even when the whole warm request takes less than the report interval.
      // 整个热请求短于采样间隔时，也保留仍有排队工作的热采样。
      if (!hasReportedFirstBatch || feedback.progress === 1 || performance.now() - lastMeasurementAt >= 250) {
        hasReportedFirstBatch = true;
        lastMeasurementAt = performance.now(); parentPort.postMessage({ type: 'measurement',
          feedback: { ...feedback, ...memoryMeasurement(activeBudget.device) } });
      }
    },
    infer: async texts => {
    // Only the ranking preview is bounded; the original passage and its source reference stay intact.
    // 只限制重排预览长度；正式片段与回源引用保持完整，并显式返回发生截短的数量。
    const inputs = tokenizer(texts.map(() => message.query), { text_pair: texts, padding: true, truncation: true,
      max_length: profile.maxInputTokens });
    let output;
    try {
      phase('inference');
      output = await model(inputs);
      const logits = Array.from(output.logits?.data ?? [], Number);
      if (logits.length !== texts.length || logits.some(logit => !Number.isFinite(logit)))
        throw Object.assign(new Error('Invalid reranker logit.'), { code: 'RERANK_INVALID_RESULT' });
      return logits.map(logit => logit >= 0 ? 1 / (1 + Math.exp(-logit)) : Math.exp(logit) / (1 + Math.exp(logit)));
    } finally {
      for (const tensor of Object.values(output ?? {})) tensor?.dispose?.();
      for (const tensor of Object.values(inputs)) tensor?.dispose?.();
    }
    },
  }); } catch (error) {
    checkCancelled(message.id);
    if (profile.requiredDevice || !canRecoverGpuMemory || activeBudget.device === 'cpu' || !isInferenceMemoryPressure(error)) throw error;
    await model.dispose(); model = undefined; loadPromise = undefined;
    const { activationMemoryBytes, hiddenSize, attentionHeads, deviceId, ...remainingBudget } = activeBudget;
    const cpuBudget = { ...remainingBudget, ...activeBudget.cpuFallbackBatchBudget,
      device: 'cpu', gpuMemoryBytes: 0, diagnostic: { code: 'GPU_MEMORY_INSUFFICIENT' } };
    return rank({ ...message, resourceBudget: cpuBudget }, false);
  }
  if (!isCancelled(message.id)) parentPort.postMessage({ type: 'result', id: message.id, scores, truncatedInputsCount });
}

function shutdown() {
  if (shutdownPromise) return shutdownPromise;
  closing = true;
  for (const id of active) cancelled.add(id);
  // Normally retire the native session after the queued call settles; the owner bounds shutdown.
  // 正常关闭时等待队列中的原生调用结束后再退役会话，所有者负责限制关闭时长。
  shutdownPromise = queue.then(async () => {
    await model?.dispose(); model = undefined; tokenizer = undefined;
    parentPort.postMessage({ type: 'closed', disposed: true });
    parentPort.off('message', receiveMessage); parentPort.close();
  }).catch(() => parentPort.postMessage({ type: 'shutdown-error' }));
  return shutdownPromise;
}

function receiveMessage(message) {
  if (message.type === 'close') { shutdown(); return; }
  if (message.type === 'cancel') { if (active.has(message.id)) cancelled.add(message.id); return; }
  if (message.type !== 'rerank') return;
  lastReceivedId = Math.max(lastReceivedId, message.id);
  if (closing) { parentPort.postMessage({ type: 'error', id: message.id, code: 'RERANK_CLOSED', message: 'Reranker is closing.' }); return; }
  // Caller cancellation releases its promise, not the native queue slot; bound admitted work here as well.
  // 调用方取消只释放其 Promise，不代表原生队列已排空；worker 同样限制真正已接纳的工作数量。
  let ticket;
  try {
    admission.setResourceBudget(message.resourceBudget?.memoryBytes);
    ticket = admission.reserve(message.texts, message.query);
  } catch { parentPort.postMessage({ type: 'error', id: message.id, code: 'RERANK_BUSY', message: 'Reranker input budget is full.' }); return; }
  active.add(message.id);
  queue = queue.then(() => rank(message)).catch(error => {
    if (!isCancelled(message.id)) parentPort.postMessage({ type: 'error', id: message.id,
      code: error.code ?? 'RERANK_FAILED', message: 'The local reranker could not score this request.' });
  }).finally(() => {
    active.delete(message.id); cancelled.delete(message.id);
    admission.release(ticket); parentPort.postMessage({ type: 'settled', id: message.id });
    if (!active.size && !closing) parentPort.postMessage({ type: 'idle', throughId: lastReceivedId });
  });
}
parentPort.on('message', receiveMessage);

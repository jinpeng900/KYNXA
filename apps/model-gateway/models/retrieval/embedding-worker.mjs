import { openNativeInferencePort } from './native-inference-port.mjs';
import { BUILTIN_EMBEDDING_PROFILE } from './embedding-profile.mjs';
import { verifyEmbeddingBundle } from './embedding-assets.mjs';
import { resolveRetrievalModelProfile } from './model-registry.mjs';
import { fitEmbeddingDocuments } from './embedding-document-fit.mjs';
import { auditEmbeddingBatchCompatibility, executeInferenceBatches, isInferenceMemoryPressure, loadAuditedGpuInferenceBackend, loadVerifiedInferenceBackend } from './inference-backend.mjs';
import { InferenceAdmission } from './inference-admission.mjs';

const { parentPort, workerData } = await openNativeInferencePort();
const profile = resolveRetrievalModelProfile('embedding', workerData.profileId ?? BUILTIN_EMBEDDING_PROFILE.id);

const cancelledRequests = new Set();
const activeRequestIds = new Set();
const admission = new InferenceAdmission(workerData.requestLimits);
let extractor;
let loadPromise;
let tokenizer;
let tokenizerPromise;
let queue = Promise.resolve();
let closing = false;
let shutdownPromise;
let latestRequestId = 0;
let activeBudget = { device: 'cpu', cpuThreads: workerData.cpuThreads ?? 1, batchSize: 1, batchTokenBudget: 512 };
let loadedBudget;
let lastMeasurementAt = 0;
const processBaselineBytes = process.memoryUsage().rss;
let workloadPeakBytes = 0;
let canonicalBatchReference;
let batchAuditKey;
let qualifiedBatchSize = 1;
let backendStatus;
let auditedBatchSize = 1;

function memoryMeasurement(backend) {
  // This process owns one inference workload; the delta includes its native runtime and tokenizer.
  // 此进程只拥有一项推理工作负载；增量包含原生运行时与 tokenizer，不冒充精确权重内存。
  workloadPeakBytes = Math.max(workloadPeakBytes, Math.max(0, process.memoryUsage().rss - processBaselineBytes));
  return { backend, processId: process.pid, workloadMemoryDeltaBytes: workloadPeakBytes,
    memoryMeasurementSource: 'isolated-native-process-delta' };
}

const isCancelled = id => closing || cancelledRequests.has(id);
const yieldToMessages = () => new Promise(resolve => setImmediate(resolve));
const phase = value => { if (!closing) parentPort.postMessage({ type: 'phase', phase: value }); };

function checkCancelled(id) {
  if (isCancelled(id)) throw Object.assign(new Error('Embedding request was cancelled.'), { code: 'EMBEDDING_CANCELLED' });
}

function shutdown() {
  if (shutdownPromise) return shutdownPromise;
  closing = true;
  for (const id of activeRequestIds) cancelledRequests.add(id);
  // The queue includes the current native call. Dispose its session only after that call settles.
  // 队列包含当前原生调用；先等待调用结束，再释放对应会话，正常关闭保持有序释放。
  shutdownPromise = queue.then(async () => {
    await extractor?.dispose();
    extractor = undefined;
    loadPromise = undefined;
    tokenizer = undefined;
    tokenizerPromise = undefined;
    parentPort.postMessage({ type: 'closed', disposed: true });
    parentPort.off('message', receiveMessage);
    parentPort.close();
  }).catch(() => {
    // Failed disposal cannot claim a safe exit; the owner reaps this isolated process.
    // 释放失败不能宣称安全退出；报告失败后由所有者回收此隔离进程。
    parentPort.postMessage({ type: 'shutdown-error' });
  });
  return shutdownPromise;
}

async function loadTokenizer(id) {
  checkCancelled(id);
  phase('verifying-assets');
  await verifyEmbeddingBundle(workerData.modelRoot, { profile, checkCancelled: () => checkCancelled(id) });
  checkCancelled(id);
  phase('loading-runtime');
  const { env, AutoTokenizer } = await import('@huggingface/transformers');
  checkCancelled(id);
  env.allowRemoteModels = false;
  env.allowLocalModels = true;
  env.useFSCache = false;
  env.useBrowserCache = false;
  env.useCustomCache = false;
  // Native inference needs no CDN/WASM, remote model, or writable model cache.
  // 原生推理不需要 CDN/WASM、远端模型或可写模型缓存；所有 fetch 都明确拒绝。
  globalThis.fetch = async () => { throw Object.assign(new Error('Embedding runtime networking is disabled.'), { code: 'EMBEDDING_NETWORK_DISABLED' }); };
  phase('loading-tokenizer');
  const activeTokenizer = await AutoTokenizer.from_pretrained(workerData.modelRoot, { local_files_only: true });
  checkCancelled(id);
  tokenizer = activeTokenizer;
  if (!closing) parentPort.postMessage({ type: 'tokenizer-ready' });
  return tokenizer;
}

function reportLoadFailure(error) {
  if (closing || error.code === 'EMBEDDING_CANCELLED') return;
  const code = error.code === 'ENOENT' ? 'EMBEDDING_ASSET_MISSING' : error.code ?? 'EMBEDDING_RUNTIME_UNAVAILABLE';
  parentPort.postMessage({ type: 'fatal', code, message: 'The bundled local embedding model could not be loaded.' });
  shutdown();
}

async function getTokenizer(id) {
  checkCancelled(id);
  return tokenizer ?? await (tokenizerPromise ??= loadTokenizer(id).catch(error => {
    tokenizerPromise = undefined;
    reportLoadFailure(error);
    throw error;
  }));
}

async function loadExtractor(id) {
  const activeTokenizer = await getTokenizer(id);
  checkCancelled(id);
  const { AutoModel, FeatureExtractionPipeline } = await import('@huggingface/transformers');
  checkCancelled(id);
  phase('loading-model');
  const backendLoader = profile.requiredDevice ? loadAuditedGpuInferenceBackend : loadVerifiedInferenceBackend;
  const { model, status } = await backendLoader({ ...activeBudget, dtype: profile.dtype, dimensions: profile.dimensions }, {
    load: options => AutoModel.from_pretrained(workerData.modelRoot, { local_files_only: true, ...options }),
    probe: async candidate => {
      const pipeline = new FeatureExtractionPipeline({ task: 'feature-extraction', model: candidate, tokenizer: activeTokenizer });
      const texts = profile.requiredDevice ? [
        `${profile.queryPrefix}How do I reset an account password?`,
        `${profile.queryPrefix}如何重置账户密码？`,
        `${profile.documentPrefix}function resolveEvidence(sourceId) { return sources.get(sourceId); }`,
      ] : [`${profile.queryPrefix}local inference compatibility probe`];
      const output = await pipeline(texts, { pooling: 'mean', normalize: true });
      try { return output.tolist().flat(); } finally { output.dispose(); }
    },
    checkCancelled: () => checkCancelled(id),
    listProviders: async () => (await import('onnxruntime-node')).listSupportedBackends(),
  });
  // The tokenizer that sized document slices also owns the unchanged feature-extraction pipeline.
  // 生成分块预算的 tokenizer 同时供原有特征提取管线使用，保证测量与实际推理输入一致。
  extractor = new FeatureExtractionPipeline({ task: 'feature-extraction', model, tokenizer: activeTokenizer });
  if (status.device === 'cpu') {
    const { activationMemoryBytes, hiddenSize, attentionHeads, deviceId, ...remainingBudget } = activeBudget;
    activeBudget = { ...remainingBudget, ...activeBudget.cpuFallbackBatchBudget, device: 'cpu', gpuMemoryBytes: 0 };
  }
  loadedBudget = { ...activeBudget, device: status.device };
  activeBudget = loadedBudget;
  backendStatus = status;
  batchAuditKey = undefined;
  if (status.device === 'cpu' && activeBudget.diagnostic) status.diagnostic ??= activeBudget.diagnostic;
  if (!closing) parentPort.postMessage({ type: 'ready', inferenceBackend: status });
  if (!closing) parentPort.postMessage({ type: 'measurement', feedback: {
    ...memoryMeasurement(status.device), phase: 'cold-load', unit: 'tokens' } });
  return extractor;
}

async function auditBatchBudget(id, activeExtractor) {
  const auditKey = `${activeBudget.device}:${activeBudget.cpuThreads}:${activeBudget.batchSize}`;
  if (batchAuditKey === auditKey) { activeBudget = { ...activeBudget, batchSize: Math.min(activeBudget.batchSize, auditedBatchSize) }; return; }
  const texts = [`${profile.documentPrefix}How do I reset an account password?`,
    `${profile.documentPrefix}如何重置账户密码？`,
    `${profile.documentPrefix}function resolveEvidence(sourceId) { return sources.get(sourceId); }`];
  const tokenLengths = texts.map(text => activeExtractor.tokenizer(text,
    { padding: false, truncation: false, return_tensor: false }).input_ids.length);
  const infer = async batch => {
    const output = await activeExtractor(batch, { pooling: 'mean', normalize: true });
    try { return output.tolist(); } finally { output.dispose(); }
  };
  const audit = await auditEmbeddingBatchCompatibility({ texts, tokenLengths, dimensions: profile.dimensions,
    budget: activeBudget, reference: canonicalBatchReference, infer, checkCancelled: () => checkCancelled(id), yieldToMessages });
  canonicalBatchReference ??= audit.reference;
  if (audit.status.qualified) qualifiedBatchSize = activeBudget.batchSize;
  else {
    // Keep the existing space identity and fall back to a qualified execution shape.
    // 保留现有空间身份，回退到已通过的执行形状；不通过改 ID 或清缓存掩盖漂移。
    activeBudget = { ...activeBudget, batchSize: qualifiedBatchSize <= activeBudget.batchSize ? qualifiedBatchSize : 1 };
    let fallback = await auditEmbeddingBatchCompatibility({ texts, tokenLengths, dimensions: profile.dimensions,
      budget: activeBudget, reference: canonicalBatchReference, infer, checkCancelled: () => checkCancelled(id), yieldToMessages });
    if (!fallback.status.qualified && activeBudget.batchSize > 1) {
      activeBudget = { ...activeBudget, batchSize: 1 };
      fallback = await auditEmbeddingBatchCompatibility({ texts, tokenLengths, dimensions: profile.dimensions,
        budget: activeBudget, reference: canonicalBatchReference, infer, checkCancelled: () => checkCancelled(id), yieldToMessages });
    }
    if (!fallback.status.qualified) throw Object.assign(new Error('No compatible execution shape is available for the existing embedding space.'),
      { code: 'EMBEDDING_SPACE_INCOMPATIBLE' });
    audit.status.fallbackQualified = true;
    parentPort.postMessage({ type: 'adjustment', adjustment: { reason: 'embedding-batch-compatibility-held',
      boundary: 'completed-request', requiresSessionRebuild: false,
      to: { batchSize: activeBudget.batchSize } } });
  }
  batchAuditKey = auditKey;
  auditedBatchSize = activeBudget.batchSize;
  backendStatus = { ...backendStatus, batchCompatibility: { ...audit.status,
    appliedBatchSize: activeBudget.batchSize, embeddingSpaceId: profile.embeddingSpaceId } };
  parentPort.postMessage({ type: 'ready', inferenceBackend: backendStatus });
}

async function embed(message, canRecoverGpuMemory = true) {
  if (isCancelled(message.id)) return;
  const requestedBudget = message.resourceBudget ?? activeBudget;
  if (extractor && (requestedBudget.cpuThreads !== loadedBudget.cpuThreads || requestedBudget.device !== loadedBudget.device ||
      requestedBudget.deviceId !== loadedBudget.deviceId)) {
    // Thread counts are native session settings, so change them only between completed requests.
    // 线程数属于原生会话设置，只在上一请求完整结束后重建，不能中途更换正在运行的会话。
    const reason = requestedBudget.device !== loadedBudget.device || requestedBudget.deviceId !== loadedBudget.deviceId
      ? 'approved-backend-changed' : 'approved-thread-count-changed';
    parentPort.postMessage({ type: 'adjustment', adjustment: { reason, boundary: 'completed-request',
      requiresSessionRebuild: true, from: { device: loadedBudget.device, cpuThreads: loadedBudget.cpuThreads },
      to: { device: requestedBudget.device, cpuThreads: requestedBudget.cpuThreads } } });
    await extractor.dispose(); extractor = undefined; loadPromise = undefined;
  }
  activeBudget = requestedBudget;
  const activeExtractor = extractor ?? await (loadPromise ??= loadExtractor(message.id).catch(error => {
    loadPromise = undefined;
    reportLoadFailure(error);
    throw error;
  }));
  if (isCancelled(message.id)) return;
  await auditBatchBudget(message.id, activeExtractor);
  const prefix = message.kind === 'query' ? profile.queryPrefix : profile.documentPrefix;
  const inputs = message.texts.map(text => `${prefix}${text}`);
  // Count the same prefixed input before the pipeline's default truncation can act.
  // 先对实际带前缀输入计数；超过模型位置上限明确报错，不让管线默认截断悄悄丢掉正文。
  phase('tokenizing');
  const tokenLengths = [];
  for (let index = 0; index < inputs.length; index += 1) {
    if (index % 8 === 0) await yieldToMessages();
    if (isCancelled(message.id)) return;
    const tokens = activeExtractor.tokenizer(inputs[index], { padding: false, truncation: false, return_tensor: false });
    const tokenCount = tokens.input_ids.length;
    tokenLengths.push(tokenCount);
    if (tokenCount > profile.maxInputTokens) {
      throw Object.assign(new Error('Embedding input exceeds 512 tokens. Split the source first.'), {
        code: 'EMBEDDING_INPUT_TOO_LONG', details: { index, tokenCount, maxInputTokens: profile.maxInputTokens },
      });
    }
  }
  const totalInputTokens = tokenLengths.reduce((sum, count) => sum + count, 0);
  if (totalInputTokens > admission.limits.maxBatchInputTokens)
    throw Object.assign(new Error('Embedding batch exceeds its configured token budget.'), {
      code: 'EMBEDDING_INPUT_TOO_LONG', details: { totalInputTokens, maxBatchInputTokens: admission.limits.maxBatchInputTokens },
    });
  let vectors;
  let hasReportedFirstBatch = false;
  try { vectors = await executeInferenceBatches(inputs, { ...activeBudget, tokenLengths, yieldToMessages,
    checkCancelled: () => checkCancelled(message.id),
    onPressure: diagnostic => parentPort.postMessage({ type: 'resource-pressure', diagnostic: { ...diagnostic,
      backend: activeBudget.device, boundary: 'completed-native-batch', requiresSessionRebuild: false } }),
    onMeasurement: feedback => {
      // Short warm requests still need a queued hot sample; final-only reports hide all expansion opportunities.
      // 短热请求也需要仍有排队工作的采样；只报告最后一批会掩盖全部上探机会。
      if (!hasReportedFirstBatch || feedback.progress === 1 || performance.now() - lastMeasurementAt >= 250) {
        hasReportedFirstBatch = true;
        lastMeasurementAt = performance.now(); parentPort.postMessage({ type: 'measurement',
          feedback: { ...feedback, ...memoryMeasurement(activeBudget.device) } });
      }
    },
    infer: async batch => {
      phase('inference');
      const output = await activeExtractor(batch, { pooling: 'mean', normalize: true });
      try {
        const batchVectors = output.tolist();
        if (batchVectors.some(vector => vector.length !== profile.dimensions || !vector.every(Number.isFinite)))
          throw Object.assign(new Error('The embedding model returned invalid vectors.'), { code: 'EMBEDDING_INVALID_VECTOR' });
        return batchVectors;
      } finally { output.dispose(); }
    },
  }); } catch (error) {
    checkCancelled(message.id);
    if (profile.requiredDevice || !canRecoverGpuMemory || activeBudget.device === 'cpu' || !isInferenceMemoryPressure(error)) throw error;
    // Read-only vectors can be recomputed once on the reserved CPU after the GPU session is disposed.
    // GPU 减批仍不足时，先释放其会话，再用已预留的 CPU 重算一次只读向量；取消绝不自动续跑。
    await extractor.dispose(); extractor = undefined; loadPromise = undefined;
    const { activationMemoryBytes, hiddenSize, attentionHeads, deviceId, ...remainingBudget } = activeBudget;
    const cpuBudget = { ...remainingBudget, ...activeBudget.cpuFallbackBatchBudget,
      device: 'cpu', gpuMemoryBytes: 0, diagnostic: { code: 'GPU_MEMORY_INSUFFICIENT' } };
    return embed({ ...message, resourceBudget: cpuBudget }, false);
  }
  if (!isCancelled(message.id)) parentPort.postMessage({ type: 'result', id: message.id, vectors });
}

async function fitDocuments(message) {
  if (isCancelled(message.id)) return;
  const activeTokenizer = await getTokenizer(message.id);
  phase('tokenizing');
  const documents = await fitEmbeddingDocuments(message.documents, {
    countTokens: text => activeTokenizer(text, { padding: false, truncation: false, return_tensor: false }).input_ids.length,
    documentPrefix: profile.documentPrefix, maxInputTokens: profile.maxInputTokens,
    checkCancelled: () => checkCancelled(message.id), yieldToMessages,
  });
  if (!isCancelled(message.id)) parentPort.postMessage({ type: 'result', id: message.id, documents });
}

function receiveMessage(message) {
  if (message.type === 'close') {
    shutdown();
    return;
  }
  if (message.type === 'cancel') {
    if (activeRequestIds.has(message.id)) cancelledRequests.add(message.id);
    return;
  }
  if (!['embed', 'fit-documents'].includes(message.type)) return;
  if (closing) {
    parentPort.postMessage({ type: 'error', id: message.id, code: 'EMBEDDING_CLOSED', message: 'Embedding worker is closing.' });
    return;
  }
  latestRequestId = message.id;
  let ticket;
  try {
    admission.setResourceBudget(message.resourceBudget?.memoryBytes);
    ticket = admission.reserve(message.type === 'fit-documents' ? message.documents : message.texts);
  } catch {
    parentPort.postMessage({ type: 'error', id: message.id, code: 'EMBEDDING_BUSY', message: 'Embedding worker queue is full.' });
    return;
  }
  activeRequestIds.add(message.id);
  queue = queue.then(() => message.type === 'fit-documents' ? fitDocuments(message) : embed(message)).catch(error => {
    if (!isCancelled(message.id) && !error.inferencePressureReported && isInferenceMemoryPressure(error)) parentPort.postMessage({ type: 'resource-pressure',
      diagnostic: { code: 'INFERENCE_ALLOCATION_FAILED', backend: activeBudget.device, terminal: true } });
    if (!isCancelled(message.id)) {
      parentPort.postMessage({ type: 'error', id: message.id, message: error.message,
        code: error.code ?? 'EMBEDDING_FAILED', ...(error.details ? { details: error.details } : {}) });
    }
  }).finally(() => {
    cancelledRequests.delete(message.id);
    activeRequestIds.delete(message.id);
    admission.release(ticket);
    parentPort.postMessage({ type: 'settled', id: message.id });
    if (!closing && !activeRequestIds.size) parentPort.postMessage({ type: 'idle', throughId: latestRequestId });
  });
}

parentPort.on('message', receiveMessage);

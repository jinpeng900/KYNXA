import { parentPort, workerData } from 'node:worker_threads';
import { BUILTIN_EMBEDDING_PROFILE } from './embedding-profile.mjs';
import { verifyEmbeddingBundle } from './embedding-assets.mjs';

const BATCH_SIZE = 4;
const cancelledRequests = new Set();
const activeRequestIds = new Set();
let extractor;
let loadPromise;
let queue = Promise.resolve();
let closing = false;
let shutdownPromise;
let latestRequestId = 0;

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
  // 队列包含当前原生调用；先等待调用结束，再释放对应会话，避免强杀线程导致 ONNX 原生崩溃。
  shutdownPromise = queue.then(async () => {
    await extractor?.dispose();
    extractor = undefined;
    loadPromise = undefined;
    parentPort.postMessage({ type: 'closed', disposed: true });
    parentPort.off('message', receiveMessage);
    parentPort.close();
  }).catch(() => {
    // Failed disposal cannot claim a safe exit or permit storage migration.
    // 释放失败不能宣称安全退出或允许存储迁移；保持通道以报告失败，不强杀原生线程。
    parentPort.postMessage({ type: 'shutdown-error' });
  });
  return shutdownPromise;
}

async function loadExtractor(id) {
  checkCancelled(id);
  phase('verifying-assets');
  await verifyEmbeddingBundle(workerData.modelRoot);
  checkCancelled(id);
  phase('loading-runtime');
  const { env, pipeline } = await import('@huggingface/transformers');
  checkCancelled(id);
  env.allowRemoteModels = false;
  env.allowLocalModels = true;
  env.useFSCache = false;
  env.useBrowserCache = false;
  env.useCustomCache = false;
  // Native CPU inference needs no CDN/WASM, remote model, or writable model cache.
  // 原生 CPU 推理不需要 CDN/WASM、远端模型或可写模型缓存；所有 fetch 都明确拒绝。
  globalThis.fetch = async () => { throw Object.assign(new Error('Embedding runtime networking is disabled.'), { code: 'EMBEDDING_NETWORK_DISABLED' }); };
  phase('loading-model');
  extractor = await pipeline('feature-extraction', workerData.modelRoot, {
    local_files_only: true,
    device: 'cpu',
    dtype: BUILTIN_EMBEDDING_PROFILE.dtype,
    session_options: { intraOpNumThreads: workerData.cpuThreads, interOpNumThreads: 1 },
  });
  if (!closing) parentPort.postMessage({ type: 'ready' });
  return extractor;
}

async function embed(message) {
  if (isCancelled(message.id)) return;
  const activeExtractor = extractor ?? await (loadPromise ??= loadExtractor(message.id).catch(error => {
    loadPromise = undefined;
    if (error.code === 'EMBEDDING_CANCELLED') throw error;
    const code = error.code === 'ENOENT' ? 'EMBEDDING_ASSET_MISSING' : error.code ?? 'EMBEDDING_RUNTIME_UNAVAILABLE';
    parentPort.postMessage({ type: 'fatal', code, message: 'The bundled local embedding model could not be loaded.' });
    shutdown();
    throw error;
  }));
  if (isCancelled(message.id)) return;
  const prefix = message.kind === 'query' ? BUILTIN_EMBEDDING_PROFILE.queryPrefix : BUILTIN_EMBEDDING_PROFILE.documentPrefix;
  const inputs = message.texts.map(text => `${prefix}${text}`);
  // Count the same prefixed input before the pipeline's default truncation can act.
  // 先对实际带前缀输入计数；超过模型位置上限明确报错，不让管线默认截断悄悄丢掉正文。
  phase('tokenizing');
  for (let index = 0; index < inputs.length; index += 1) {
    if (index % 8 === 0) await yieldToMessages();
    if (isCancelled(message.id)) return;
    const tokens = activeExtractor.tokenizer(inputs[index], { padding: false, truncation: false, return_tensor: false });
    const tokenCount = tokens.input_ids.length;
    if (tokenCount > BUILTIN_EMBEDDING_PROFILE.maxInputTokens) {
      throw Object.assign(new Error('Embedding input exceeds 512 tokens. Split the source first.'), {
        code: 'EMBEDDING_INPUT_TOO_LONG', details: { index, tokenCount, maxInputTokens: BUILTIN_EMBEDDING_PROFILE.maxInputTokens },
      });
    }
  }
  const vectors = [];
  for (let offset = 0; offset < inputs.length; offset += BATCH_SIZE) {
    await yieldToMessages();
    if (isCancelled(message.id)) return;
    const batch = inputs.slice(offset, offset + BATCH_SIZE);
    phase('inference');
    const output = await activeExtractor(batch, { pooling: 'mean', normalize: true });
    try {
      if (isCancelled(message.id)) return;
      const batchVectors = output.tolist();
      if (batchVectors.some(vector => vector.length !== BUILTIN_EMBEDDING_PROFILE.dimensions || !vector.every(Number.isFinite))) {
        throw Object.assign(new Error('The embedding model returned invalid vectors.'), { code: 'EMBEDDING_INVALID_VECTOR' });
      }
      vectors.push(...batchVectors);
    } finally {
      output.dispose();
    }
  }
  if (!isCancelled(message.id)) parentPort.postMessage({ type: 'result', id: message.id, vectors });
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
  if (message.type !== 'embed') return;
  if (closing) {
    parentPort.postMessage({ type: 'error', id: message.id, code: 'EMBEDDING_CLOSED', message: 'Embedding worker is closing.' });
    return;
  }
  latestRequestId = message.id;
  if (activeRequestIds.size >= 32) {
    parentPort.postMessage({ type: 'error', id: message.id, code: 'EMBEDDING_BUSY', message: 'Embedding worker queue is full.' });
    return;
  }
  activeRequestIds.add(message.id);
  queue = queue.then(() => embed(message)).catch(error => {
    if (!isCancelled(message.id)) {
      parentPort.postMessage({ type: 'error', id: message.id, message: error.message,
        code: error.code ?? 'EMBEDDING_FAILED', ...(error.details ? { details: error.details } : {}) });
    }
  }).finally(() => {
    cancelledRequests.delete(message.id);
    activeRequestIds.delete(message.id);
    if (!closing && !activeRequestIds.size) parentPort.postMessage({ type: 'idle', throughId: latestRequestId });
  });
}

parentPort.on('message', receiveMessage);

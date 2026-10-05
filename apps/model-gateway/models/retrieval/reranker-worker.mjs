import { parentPort, workerData } from 'node:worker_threads';
import { BUILTIN_RERANKER_PROFILE } from './reranker-profile.mjs';
import { verifyEmbeddingAsset } from './embedding-assets.mjs';

let tokenizer, model, loadPromise, shutdownPromise;
let queue = Promise.resolve(), closing = false;
let lastReceivedId = 0;
const cancelled = new Set(), active = new Set();
const isCancelled = id => closing || cancelled.has(id);
const phase = value => parentPort.postMessage({ type: 'phase', phase: value });

async function loadModel() {
  phase('verifying-assets');
  for (const asset of BUILTIN_RERANKER_PROFILE.files) {
    if (!await verifyEmbeddingAsset(workerData.modelRoot, asset))
      throw Object.assign(new Error('Reranker asset integrity verification failed.'), { code: 'RERANK_ASSET_INVALID' });
  }
  const { env, AutoTokenizer, AutoModelForSequenceClassification } = await import('@huggingface/transformers');
  env.allowRemoteModels = false; env.allowLocalModels = true;
  env.useFSCache = false; env.useBrowserCache = false; env.useCustomCache = false;
  globalThis.fetch = async () => { throw Object.assign(new Error('Reranker networking is disabled.'), { code: 'RERANK_NETWORK_DISABLED' }); };
  phase('loading-model');
  tokenizer = await AutoTokenizer.from_pretrained(workerData.modelRoot, { local_files_only: true });
  model = await AutoModelForSequenceClassification.from_pretrained(workerData.modelRoot, {
    local_files_only: true, dtype: BUILTIN_RERANKER_PROFILE.dtype, device: 'cpu',
    session_options: { intraOpNumThreads: workerData.cpuThreads, interOpNumThreads: 1 },
  });
  parentPort.postMessage({ type: 'ready' });
}

async function rank(message) {
  if (isCancelled(message.id)) return;
  await (loadPromise ??= loadModel());
  if (isCancelled(message.id)) return;
  const scores = [];
  let truncatedInputsCount = 0;
  for (const text of message.texts) {
    await new Promise(resolve => setImmediate(resolve));
    if (isCancelled(message.id)) return;
    const raw = tokenizer(message.query, { text_pair: text, padding: false, truncation: false, return_tensor: false });
    if (raw.input_ids.length > BUILTIN_RERANKER_PROFILE.maxInputTokens) truncatedInputsCount++;
    // Only the ranking preview is bounded; the original passage and its source reference stay intact.
    // 只限制重排预览长度；正式片段与回源引用保持完整，并显式返回发生截短的数量。
    const inputs = tokenizer(message.query, { text_pair: text, padding: true, truncation: true,
      max_length: BUILTIN_RERANKER_PROFILE.maxInputTokens });
    let output;
    try {
      phase('inference');
      output = await model(inputs);
      const logit = Number(output.logits?.data?.[0]);
      if (!Number.isFinite(logit) || output.logits.data.length !== 1)
        throw Object.assign(new Error('Invalid reranker logit.'), { code: 'RERANK_INVALID_RESULT' });
      scores.push(logit >= 0 ? 1 / (1 + Math.exp(-logit)) : Math.exp(logit) / (1 + Math.exp(logit)));
    } finally {
      for (const tensor of Object.values(output ?? {})) tensor?.dispose?.();
      for (const tensor of Object.values(inputs)) tensor?.dispose?.();
    }
  }
  if (!isCancelled(message.id)) parentPort.postMessage({ type: 'result', id: message.id, scores, truncatedInputsCount });
}

function shutdown() {
  if (shutdownPromise) return shutdownPromise;
  closing = true;
  // Retire the native session after the queued call settles; never terminate an ONNX worker.
  // 等待队列中的原生调用结束后再退役会话，不强行终止 ONNX worker。
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
  if (active.size >= 8) { parentPort.postMessage({ type: 'error', id: message.id, code: 'RERANK_BUSY', message: 'Reranker queue is full.' }); return; }
  active.add(message.id);
  queue = queue.then(() => rank(message)).catch(error => {
    if (!isCancelled(message.id)) parentPort.postMessage({ type: 'error', id: message.id,
      code: error.code ?? 'RERANK_FAILED', message: 'The local reranker could not score this request.' });
  }).finally(() => {
    active.delete(message.id); cancelled.delete(message.id);
    if (!active.size && !closing) parentPort.postMessage({ type: 'idle', throughId: lastReceivedId });
  });
}
parentPort.on('message', receiveMessage);

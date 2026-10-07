import { existsSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { Worker } from 'node:worker_threads';
import { BUILTIN_EMBEDDING_PROFILE, defaultEmbeddingModelRoot } from './embedding-profile.mjs';
import { resolveRetrievalModelProfile, retrievalModelMetadata, unavailableProfileStatus } from './model-registry.mjs';

const MAX_PENDING_REQUESTS = 32;
const MAX_BATCH_DOCUMENTS = 64;
const MAX_INPUT_CHARACTERS = 16_384;
const DEFAULT_CLOSE_TIMEOUT_MS = 30_000;

export class EmbeddingError extends Error {
  constructor(message, code, details) {
    super(message);
    this.name = 'EmbeddingError';
    this.code = code;
    if (details) this.details = details;
  }
}

function abortedError() {
  const error = new EmbeddingError('Embedding request was cancelled.', 'EMBEDDING_CANCELLED');
  error.name = 'AbortError';
  return error;
}

function validateTexts(texts) {
  if (!Array.isArray(texts) || texts.length > MAX_BATCH_DOCUMENTS) {
    throw new EmbeddingError('Embedding batches must contain at most 64 documents.', 'EMBEDDING_INVALID_INPUT');
  }
  for (const text of texts) {
    if (typeof text !== 'string' || !text.trim()) {
      throw new EmbeddingError('Embedding input must be non-empty text.', 'EMBEDDING_INVALID_INPUT');
    }
    if (text.length > MAX_INPUT_CHARACTERS) {
      throw new EmbeddingError('Embedding input exceeds the character safety limit. Split the source first.', 'EMBEDDING_INPUT_TOO_LONG');
    }
  }
}

// One lazily loaded worker owns the CPU model. The gateway remains responsive.
// CPU 模型由按需加载的独立 worker 持有；哈希校验、分词和推理不阻塞网关与流式展示。
export class EmbeddingService {
  #modelRoot;
  #cpuThreads;
  #timeoutMs;
  #closeTimeoutMs;
  #worker;
  #workerExit;
  #workerPhase = 'stopped';
  #closePromise;
  #pending = new Map();
  #nextRequestId = 0;
  #state;
  #loaded = false;
  #closed = false;
  #errorCode;
  #profile;
  #workerFactory;
  #assetVerification = 'pending';

  constructor({ modelRoot = defaultEmbeddingModelRoot(), cpuThreads = 2, timeoutMs = 120_000,
    closeTimeoutMs = DEFAULT_CLOSE_TIMEOUT_MS, profileId = BUILTIN_EMBEDDING_PROFILE.id,
    workerFactory = (url, options) => new Worker(url, options) } = {}) {
    this.#profile = resolveRetrievalModelProfile('embedding', profileId);
    this.#workerFactory = workerFactory;
    this.#modelRoot = resolve(modelRoot);
    this.#cpuThreads = Math.max(1, Math.min(2, Number.isInteger(cpuThreads) ? cpuThreads : 2));
    this.#timeoutMs = Math.max(1000, Math.min(300_000, Number.isFinite(timeoutMs) ? timeoutMs : 120_000));
    this.#closeTimeoutMs = Math.max(1, Math.min(300_000,
      Number.isFinite(closeTimeoutMs) ? closeTimeoutMs : DEFAULT_CLOSE_TIMEOUT_MS));
    this.#state = this.#profile.files.every(asset => existsSync(join(this.#modelRoot, asset.path))) ? 'ready' : 'unavailable';
    if (this.#state === 'unavailable') this.#errorCode = 'EMBEDDING_ASSET_MISSING';
  }

  status(profileId = this.#profile.id) {
    if (profileId !== this.#profile.id) return unavailableProfileStatus('embedding', profileId);
    return { ...retrievalModelMetadata(this.#profile), state: this.#state, loaded: this.#loaded, supported: true,
      local: true, network: false, assetVerification: this.#assetVerification,
      maxInputTokens: this.#profile.maxInputTokens, cpuThreads: this.#cpuThreads,
      pendingRequests: this.#pending.size, workerPhase: this.#workerPhase,
      ...(this.#errorCode ? { errorCode: this.#errorCode } : {}) };
  }

  async embedQuery(text, { signal, profileId = this.#profile.id } = {}) {
    resolveRetrievalModelProfile('embedding', profileId);
    validateTexts([text]);
    const vectors = await this.#request('query', [text], signal);
    return { ...retrievalModelMetadata(this.#profile), vector: vectors[0] };
  }

  async embedDocuments(texts, { signal, profileId = this.#profile.id } = {}) {
    resolveRetrievalModelProfile('embedding', profileId);
    validateTexts(texts);
    if (!texts.length) {
      if (this.#closed) throw new EmbeddingError('Embedding service is closed.', 'EMBEDDING_CLOSED');
      if (signal?.aborted) throw abortedError();
      return { ...retrievalModelMetadata(this.#profile), vectors: [] };
    }
    return { ...retrievalModelMetadata(this.#profile), vectors: await this.#request('document', texts, signal) };
  }

  #startWorker() {
    if (this.#worker) return;
    this.#state = 'loading';
    let worker;
    try {
      worker = this.#workerFactory(new URL('./embedding-worker.mjs', import.meta.url), {
        workerData: { modelRoot: this.#modelRoot, cpuThreads: this.#cpuThreads, profileId: this.#profile.id },
        // Do not inherit debugger or test runner flags into the inference worker.
        // 推理 worker 不继承调试器或测试运行器参数，避免额外进程行为。
        execArgv: [],
      });
    } catch {
      const error = new EmbeddingError('Local embedding worker could not start.', 'EMBEDDING_WORKER_FAILED');
      this.#failWorker(error);
      throw error;
    }
    this.#worker = worker;
    this.#workerPhase = 'starting';
    let resolveExit, rejectExit, shutdownAcknowledged = false;
    this.#workerExit = new Promise((resolve, reject) => { resolveExit = resolve; rejectExit = reject; });
    // A load failure can retire the worker before any caller asks to close it.
    // 模型加载失败可能在调用方要求关闭前启动退出，先观察拒绝，避免产生未处理的 Promise。
    this.#workerExit.catch(() => {});
    worker.on('message', message => {
      if (this.#worker !== worker || !message || typeof message !== 'object') return;
      if (message.type === 'phase') {
        if (!this.#closed) this.#workerPhase = message.phase;
        return;
      }
      if (message.type === 'idle') {
        if (!this.#closed && message.throughId === this.#nextRequestId && !this.#pending.size) {
          this.#workerPhase = 'idle';
          if (!this.#loaded && this.#state === 'loading') this.#state = 'ready';
          worker.unref();
        }
        return;
      }
      if (message.type === 'closed') {
        shutdownAcknowledged = message.disposed === true;
        return;
      }
      if (message.type === 'shutdown-error') {
        const error = new EmbeddingError('Local embedding resources could not be released.', 'EMBEDDING_CLOSE_FAILED');
        rejectExit(error);
        this.#failWorker(error);
        return;
      }
      if (message.type === 'ready') {
        if (this.#closed || this.#state === 'error' || this.#state === 'unavailable') return;
        this.#state = 'ready';
        this.#loaded = true;
        this.#assetVerification = 'verified';
        this.#errorCode = undefined;
        return;
      }
      if (message.type === 'fatal') {
        this.#failWorker(new EmbeddingError(message.message, message.code));
        return;
      }
      const request = this.#pending.get(message.id);
      if (!request) return;
      this.#pending.delete(message.id);
      request.cleanup();
      if (message.type === 'result') {
        if (!Array.isArray(message.vectors) || message.vectors.length !== request.expectedCount ||
            message.vectors.some(vector => !Array.isArray(vector) || vector.length !== this.#profile.dimensions || !vector.every(Number.isFinite)))
          request.reject(new EmbeddingError('The embedding worker returned invalid vectors.', 'EMBEDDING_INVALID_VECTOR'));
        else request.resolve(message.vectors);
      }
      else request.reject(new EmbeddingError(message.message, message.code, message.details));
    });
    worker.on('error', error => this.#failWorker(new EmbeddingError(error.message, 'EMBEDDING_WORKER_FAILED')));
    worker.on('exit', exitCode => {
      resolveExit({ exitCode, shutdownAcknowledged });
      if (this.#worker !== worker) return;
      this.#worker = undefined;
      this.#workerPhase = 'stopped';
      if (!this.#closed && this.#state !== 'error' && this.#state !== 'unavailable')
        this.#failWorker(new EmbeddingError(`Embedding worker exited (${exitCode}).`, 'EMBEDDING_WORKER_FAILED'));
    });
  }

  #failWorker(error) {
    if (!this.#closed) {
      this.#state = error.code === 'EMBEDDING_ASSET_MISSING' ? 'unavailable' : 'error';
      this.#errorCode = error.code;
      this.#loaded = false;
      if (error.code === 'EMBEDDING_ASSET_INVALID' || error.code === 'EMBEDDING_ASSET_MISSING') this.#assetVerification = 'failed';
    }
    for (const request of this.#pending.values()) {
      request.cleanup();
      request.reject(error);
    }
    this.#pending.clear();
  }

  async #request(kind, texts, signal) {
    if (this.#closed) throw new EmbeddingError('Embedding service is closed.', 'EMBEDDING_CLOSED');
    if (signal?.aborted) throw abortedError();
    if (this.#state === 'unavailable') throw new EmbeddingError('Bundled embedding assets are missing. Keyword search remains available.', this.#errorCode);
    if (this.#state === 'error') throw new EmbeddingError('Local embedding runtime is unavailable. Keyword search remains available.', this.#errorCode);
    if (this.#pending.size >= MAX_PENDING_REQUESTS) throw new EmbeddingError('Embedding queue is full.', 'EMBEDDING_BUSY');
    this.#startWorker();
    const worker = this.#worker;
    worker.ref();
    this.#workerPhase = 'queued';
    const id = ++this.#nextRequestId;
    return new Promise((resolveRequest, rejectRequest) => {
      let timer;
      const cancel = error => {
        if (!this.#pending.delete(id)) return;
        cleanup();
        try { worker.postMessage({ type: 'cancel', id }); } catch { /* The worker exit owns cleanup. / worker 退出负责最终清理。 */ }
        rejectRequest(error);
      };
      const abort = () => cancel(abortedError());
      const cleanup = () => {
        clearTimeout(timer);
        signal?.removeEventListener('abort', abort);
      };
      this.#pending.set(id, { resolve: resolveRequest, reject: rejectRequest, cleanup, expectedCount: texts.length });
      signal?.addEventListener('abort', abort, { once: true });
      if (signal?.aborted) { abort(); return; }
      timer = setTimeout(() => cancel(new EmbeddingError('Local embedding request timed out.', 'EMBEDDING_TIMEOUT')), this.#timeoutMs);
      try { worker.postMessage({ type: 'embed', id, kind, texts }); }
      catch { cancel(new EmbeddingError('Local embedding request could not be dispatched.', 'EMBEDDING_WORKER_FAILED')); }
    });
  }

  close() {
    if (this.#closePromise) return this.#closePromise;
    this.#closed = true;
    this.#loaded = false;
    this.#state = 'closing';
    this.#workerPhase = this.#worker ? 'closing' : 'stopped';
    this.#errorCode = 'EMBEDDING_CLOSED';
    this.#failWorker(new EmbeddingError('Embedding service is closed.', 'EMBEDDING_CLOSED'));
    const worker = this.#worker;
    this.#closePromise = this.#drainWorker(worker);
    return this.#closePromise;
  }

  async #drainWorker(worker) {
    if (!worker) {
      this.#state = 'unavailable';
      return;
    }
    worker.ref();
    let timer;
    try {
      worker.postMessage({ type: 'close' });
      const result = await Promise.race([this.#workerExit, new Promise((resolve, reject) => {
        timer = setTimeout(() => reject(new EmbeddingError(
          'Local embedding shutdown timed out; storage remains unavailable until safe retirement.',
          'EMBEDDING_CLOSE_TIMEOUT')), this.#closeTimeoutMs);
      })]);
      if (result.exitCode !== 0 || !result.shutdownAcknowledged)
        throw new EmbeddingError('Local embedding worker exited before confirming resource cleanup.', 'EMBEDDING_CLOSE_FAILED');
      this.#state = 'unavailable';
    } catch (error) {
      this.#state = 'error';
      this.#errorCode = error.code ?? 'EMBEDDING_CLOSE_FAILED';
      throw error;
    } finally {
      clearTimeout(timer);
      // Never terminate a thread running ONNX. A timeout rejects retirement while natural drain continues.
      // 不强制终止正在运行 ONNX 的线程；超时使退役失败，worker 仍继续自然排空和释放资源。
    }
  }
}

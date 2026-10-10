import { existsSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { createNativeInferenceProcess } from './native-inference-process.mjs';
import { BUILTIN_EMBEDDING_PROFILE, defaultEmbeddingModelRoot } from './embedding-profile.mjs';
import { resolveRetrievalModelProfile, retrievalModelMetadata, unavailableProfileStatus, RetrievalModelProfileError } from './model-registry.mjs';
import { EMBEDDING_DOCUMENT_FITTING_VERSION, MAX_EMBEDDING_INPUT_CHARACTERS } from './embedding-document-fit.mjs';
import { InferenceResourceReservation } from './inference-resources.mjs';
import { InferenceAdmission } from './inference-admission.mjs';

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

function validateTexts(texts, maxBatchDocuments) {
  if (!Array.isArray(texts) || texts.length > maxBatchDocuments) {
    throw new EmbeddingError('Embedding batch exceeds its configured document budget.', 'EMBEDDING_INVALID_INPUT');
  }
  for (const text of texts) {
    if (typeof text !== 'string' || !text.trim()) {
      throw new EmbeddingError('Embedding input must be non-empty text.', 'EMBEDDING_INVALID_INPUT');
    }
    if (text.length > MAX_EMBEDDING_INPUT_CHARACTERS) {
      throw new EmbeddingError('Embedding input exceeds the character safety limit. Split the source first.', 'EMBEDDING_INPUT_TOO_LONG');
    }
  }
}

function normalizeFittingDocuments(inputs, maxBatchDocuments) {
  if (!Array.isArray(inputs) || inputs.length > maxBatchDocuments)
    throw new EmbeddingError('Document fitting batch exceeds its configured document budget.', 'EMBEDDING_INVALID_INPUT');
  const documents = inputs.map(input => typeof input === 'string' ? { text: input, context: '' }
    : { text: input?.text, context: input?.context ?? '' });
  validateTexts(documents.map(document => document.text), maxBatchDocuments);
  if (documents.some(document => typeof document.context !== 'string' || document.context.length > MAX_EMBEDDING_INPUT_CHARACTERS))
    throw new EmbeddingError('Invalid document fitting context.', 'EMBEDDING_INVALID_INPUT');
  return documents;
}

function validFittingResult(documents, inputs, maxInputTokens) {
  if (!Array.isArray(documents) || documents.length !== inputs.length) return false;
  return documents.every((document, index) => {
    if (!Number.isSafeInteger(document?.tokenCount) || document.tokenCount < 1 ||
        !Array.isArray(document.segments) || !document.segments.length || document.segments.length > inputs[index].text.length) return false;
    const { text, context } = inputs[index];
    let cursor = 0;
    for (const segment of document.segments) {
      if (!Number.isSafeInteger(segment?.start) || !Number.isSafeInteger(segment.end) || segment.start !== cursor ||
          segment.end <= cursor || segment.end > text.length || !Number.isSafeInteger(segment.tokenCount) ||
          segment.tokenCount < 1 || segment.tokenCount > maxInputTokens ||
          context.length + segment.end - segment.start > MAX_EMBEDDING_INPUT_CHARACTERS ||
          segment.end < text.length && /[\uD800-\uDBFF]/u.test(text[segment.end - 1]) && /[\uDC00-\uDFFF]/u.test(text[segment.end])) return false;
      cursor = segment.end;
    }
    return cursor === text.length;
  });
}

// One lazily loaded process owns the pinned model and its process-global native runtime.
// 固定模型及进程级原生运行时由按需加载的独立进程持有；校验、分词和推理不阻塞网关。
export class EmbeddingService {
  #modelRoot;
  #cpuThreads;
  #timeoutMs;
  #closeTimeoutMs;
  #worker;
  #workerExit;
  #workerExitConfirmed = false;
  #workerPhase = 'stopped';
  #idleSince = 0;
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
  #resources;
  #preparingRequests = 0;
  #inferenceBackend;
  #resourceDiagnostic;
  #workerRestarts = 0;
  #admission; #nativeTickets = new Map();
  #devicePreference; #retiring = false; #retirement; #retirementCompletion;

  constructor({ modelRoot = defaultEmbeddingModelRoot(), cpuThreads, resourceService, devicePreference = 'auto', requestLimits, timeoutMs = 120_000,
    closeTimeoutMs = DEFAULT_CLOSE_TIMEOUT_MS, profileId = BUILTIN_EMBEDDING_PROFILE.id,
    workerFactory = createNativeInferenceProcess } = {}) {
    this.#profile = resolveRetrievalModelProfile('embedding', profileId);
    this.#devicePreference = devicePreference;
    this.#admission = new InferenceAdmission(requestLimits);
    if (this.#profile.requiredDevice && devicePreference === 'cpu')
      throw new EmbeddingError('The selected GPU vector space cannot return CPU projections.', 'EMBEDDING_GPU_REQUIRED');
    this.#workerFactory = workerFactory;
    this.#modelRoot = resolve(modelRoot);
    this.#cpuThreads = 0;
    this.#resources = new InferenceResourceReservation({ profile: this.#profile, cpuThreads, resourceService, devicePreference });
    this.#timeoutMs = Math.max(1000, Math.min(300_000, Number.isFinite(timeoutMs) ? timeoutMs : 120_000));
    this.#closeTimeoutMs = Math.max(1, Math.min(300_000,
      Number.isFinite(closeTimeoutMs) ? closeTimeoutMs : DEFAULT_CLOSE_TIMEOUT_MS));
    this.#state = this.#profile.files.every(asset => existsSync(join(this.#modelRoot, asset.path))) ? 'ready' : 'unavailable';
    if (this.#state === 'unavailable') this.#errorCode = 'EMBEDDING_ASSET_MISSING';
  }

  status(profileId = this.#profile.id) {
    if (profileId !== this.#profile.id) return unavailableProfileStatus('embedding', profileId);
    const resourceReservation = this.#resources.status();
    return { ...retrievalModelMetadata(this.#profile), state: this.#state, loaded: this.#loaded, supported: true,
      local: true, network: false, assetVerification: this.#assetVerification,
      devicePreference: this.#devicePreference, retiring: this.#retiring,
      fittingVersion: EMBEDDING_DOCUMENT_FITTING_VERSION,
      maxInputTokens: this.#profile.maxInputTokens, cpuThreads: this.#cpuThreads,
      pendingRequests: this.#pending.size, workerPhase: this.#workerPhase, idleSince: this.#idleSince || null,
      resourceReservation, batchSuggestions: resourceReservation.batchSuggestions,
      ...(resourceReservation.lastGrant ? { batchSize: resourceReservation.lastGrant.batchSize,
        batchTokenBudget: resourceReservation.lastGrant.batchTokenBudget } : {}),
      ...(this.#inferenceBackend ? { inferenceBackend: this.#inferenceBackend } : {}),
      inputAdmission: this.#admission.status(), requestLimits: this.#admission.limits,
      ...(this.#resourceDiagnostic ? { resourceDiagnostic: this.#resourceDiagnostic } : {}),
      ...(this.#errorCode ? { errorCode: this.#errorCode } : {}) };
  }

  async embedQuery(text, { signal, profileId = this.#profile.id, devicePreference = this.#devicePreference } = {}) {
    this.#assertDevicePreference(devicePreference);
    resolveRetrievalModelProfile('embedding', profileId);
    if (profileId !== this.#profile.id) throw new RetrievalModelProfileError('embedding', profileId);
    validateTexts([text], this.#admission.limits.maxBatchDocuments);
    const vectors = await this.#request('query', [text], signal);
    return { ...retrievalModelMetadata(this.#profile), vector: vectors[0] };
  }

  async embedDocuments(texts, { signal, profileId = this.#profile.id, devicePreference = this.#devicePreference } = {}) {
    this.#assertDevicePreference(devicePreference);
    resolveRetrievalModelProfile('embedding', profileId);
    if (profileId !== this.#profile.id) throw new RetrievalModelProfileError('embedding', profileId);
    validateTexts(texts, this.#admission.limits.maxBatchDocuments);
    if (!texts.length) {
      if (this.#closed) throw new EmbeddingError('Embedding service is closed.', 'EMBEDDING_CLOSED');
      if (signal?.aborted) throw abortedError();
      return { ...retrievalModelMetadata(this.#profile), vectors: [] };
    }
    return { ...retrievalModelMetadata(this.#profile), vectors: await this.#request('document', texts, signal) };
  }

  async fitDocuments(inputs, { signal, profileId = this.#profile.id, devicePreference = this.#devicePreference } = {}) {
    this.#assertDevicePreference(devicePreference);
    resolveRetrievalModelProfile('embedding', profileId);
    if (profileId !== this.#profile.id) throw new RetrievalModelProfileError('embedding', profileId);
    const documents = normalizeFittingDocuments(inputs, this.#admission.limits.maxBatchDocuments);
    if (!documents.length) {
      if (this.#closed) throw new EmbeddingError('Embedding service is closed.', 'EMBEDDING_CLOSED');
      if (signal?.aborted) throw abortedError();
    }
    const fittedDocuments = documents.length ? await this.#request('fit-documents', documents, signal) : [];
    return { ...retrievalModelMetadata(this.#profile), fittingVersion: EMBEDDING_DOCUMENT_FITTING_VERSION,
      maxInputTokens: this.#profile.maxInputTokens, documents: fittedDocuments };
  }

  #startWorker(resourceBudget) {
    if (this.#worker) return;
    this.#state = 'loading';
    let worker;
    try {
      worker = this.#workerFactory(new URL('./embedding-worker.mjs', import.meta.url), {
        workerData: { modelRoot: this.#modelRoot, cpuThreads: resourceBudget.cpuThreads, profileId: this.#profile.id,
          requestLimits: this.#admission.limits, devicePreference: this.#devicePreference },
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
    this.#workerExitConfirmed = false;
    this.#resources.registerExecutor(worker.pid).catch(() => { this.#resourceDiagnostic = { code: 'INFERENCE_EXECUTOR_REGISTRATION_FAILED' }; });
    this.#workerPhase = 'starting';
    let resolveExit, rejectExit, shutdownAcknowledged = false;
    this.#workerExit = new Promise((resolve, reject) => { resolveExit = resolve; rejectExit = reject; });
    // A load failure can retire the worker before any caller asks to close it.
    // 模型加载失败可能在调用方要求关闭前启动退出，先观察拒绝，避免产生未处理的 Promise。
    this.#workerExit.catch(() => {});
    worker.on('message', message => {
      if (this.#worker !== worker || !message || typeof message !== 'object') return;
      if (message.type === 'phase') {
        if (!this.#closed) {
          this.#workerPhase = message.phase;
          if (message.phase === 'loading-model' && !this.#loaded && ['ready', 'loading'].includes(this.#state)) this.#state = 'loading';
        }
        return;
      }
      if (message.type === 'settled') {
        this.#admission.release(this.#nativeTickets.get(message.id)); this.#nativeTickets.delete(message.id);
        this.#retireWhenIdle(); return;
      }
      if (message.type === 'idle') {
        for (const [requestId, ticket] of this.#nativeTickets)
          if (requestId <= message.throughId) { this.#admission.release(ticket); this.#nativeTickets.delete(requestId); }
        if (!this.#closed && message.throughId === this.#nextRequestId && !this.#pending.size && !this.#preparingRequests) {
          this.#workerPhase = 'idle';
          this.#idleSince ||= Date.now();
          if (!this.#loaded && this.#state === 'loading') this.#state = 'ready';
          worker.unref();
          this.#resources.idle().catch(() => { this.#resourceDiagnostic = { code: 'INFERENCE_RESOURCE_RELEASE_FAILED' }; });
          this.#retireWhenIdle();
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
        if (message.inferenceBackend) {
          this.#inferenceBackend = message.inferenceBackend;
          this.#resources.backend(message.inferenceBackend).catch(() => { this.#resourceDiagnostic = { code: 'INFERENCE_RESOURCE_RELEASE_FAILED' }; });
        }
        return;
      }
      if (message.type === 'resource-pressure') {
        this.#resourceDiagnostic = message.diagnostic;
        this.#resources.report({ allocationFailure: true, backend: message.diagnostic?.backend ?? 'cpu',
          phase: 'hot-inference', unit: 'tokens' }).catch(() => {}); return;
      }
      if (message.type === 'adjustment') { this.#resources.adjustment(message.adjustment); return; }
      if (message.type === 'measurement') { this.#resources.report(message.feedback).catch(() => {}); return; }
      if (message.type === 'tokenizer-ready') {
        if (this.#closed || this.#state === 'error' || this.#state === 'unavailable') return;
        this.#state = 'ready';
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
        if (request.kind === 'fit-documents') {
          if (validFittingResult(message.documents, request.inputs, this.#profile.maxInputTokens)) request.resolve(message.documents);
          else request.reject(new EmbeddingError('The embedding process returned invalid document ranges.', 'EMBEDDING_INVALID_FIT_RESULT'));
        }
        else if (!Array.isArray(message.vectors) || message.vectors.length !== request.expectedCount ||
            message.vectors.some(vector => !Array.isArray(vector) || vector.length !== this.#profile.dimensions || !vector.every(Number.isFinite)))
          request.reject(new EmbeddingError('The embedding worker returned invalid vectors.', 'EMBEDDING_INVALID_VECTOR'));
        else request.resolve(message.vectors);
      }
      else request.reject(new EmbeddingError(message.message, message.code, message.details));
    });
    worker.on('error', error => this.#failWorker(new EmbeddingError(error.message, 'EMBEDDING_WORKER_FAILED')));
    worker.on('exit', (exitCode, signal) => {
      resolveExit({ exitCode, shutdownAcknowledged });
      if (this.#worker !== worker) return;
      this.#workerExitConfirmed = true;
      this.#worker = undefined;
      this.#admission.clear(); this.#nativeTickets.clear();
      this.#workerPhase = 'stopped';
      this.#idleSince = 0;
      if (!this.#closed && !shutdownAcknowledged && this.#resources.status().gpuMemoryBytes > 0)
        this.#resources.backend({ device: 'cpu', diagnostic: { code: 'GPU_WORKER_FAILED' } })
          .catch(() => { this.#resourceDiagnostic = { code: 'INFERENCE_RESOURCE_RELEASE_FAILED' }; });
      this.#resources.idle().catch(() => { this.#resourceDiagnostic = { code: 'INFERENCE_RESOURCE_RELEASE_FAILED' }; });
      if (!this.#closed && (this.#pending.size || (this.#state !== 'error' && this.#state !== 'unavailable')))
        this.#failWorker(new EmbeddingError(`Embedding process exited (${signal ?? exitCode}).`, 'EMBEDDING_WORKER_FAILED'));
      this.#retireWhenIdle();
    });
  }

  #failWorker(error) {
    if (!this.#closed) {
      this.#state = error.code === 'EMBEDDING_ASSET_MISSING' ? 'unavailable' : 'error';
      this.#errorCode = error.code;
      this.#loaded = false;
      this.#inferenceBackend = undefined;
      if (error.code === 'EMBEDDING_ASSET_INVALID' || error.code === 'EMBEDDING_ASSET_MISSING') this.#assetVerification = 'failed';
    }
    for (const request of this.#pending.values()) {
      request.cleanup();
      request.reject(error);
    }
    this.#pending.clear();
  }

  async #request(kind, texts, signal) {
    if (this.#closed || this.#retiring) throw new EmbeddingError('Embedding service is closed or retiring.', 'EMBEDDING_CLOSED');
    if (signal?.aborted) throw abortedError();
    if (this.#state === 'unavailable') throw new EmbeddingError('Bundled embedding assets are missing. Keyword search remains available.', this.#errorCode);
    if (this.#state === 'error' && !this.#worker && this.#errorCode === 'EMBEDDING_WORKER_FAILED' && this.#workerRestarts < 1) {
      // A new caller may restart one crashed read-only worker; never replay a cancelled request.
      // 新调用方可重启一次崩溃的只读 worker；绝不自动重放已取消的请求。
      this.#workerRestarts++; this.#state = 'ready'; this.#errorCode = undefined;
      if (this.#profile.requiredDevice) this.#resources.retryRequiredGpuAfterExit();
    }
    if (this.#state === 'error') throw new EmbeddingError('Local embedding runtime is unavailable. Keyword search remains available.', this.#errorCode);
    let ticket;
    try { ticket = this.#admission.reserve(texts); }
    catch { throw new EmbeddingError('Embedding input budget is full.', 'EMBEDDING_BUSY'); }
    if (!this.#worker) this.#state = 'loading';
    this.#preparingRequests++;
    let resourceBudget;
    try { resourceBudget = await this.#resources.acquire({ signal, kind: kind === 'query' ? 'foreground' : 'background',
      tokenizerOnly: kind === 'fit-documents' }); }
    catch (error) {
      this.#admission.release(ticket);
      this.#resourceDiagnostic = { code: 'INFERENCE_RESOURCE_BUSY', reason: error.details?.reason ?? error.code ?? 'RESOURCE_UNAVAILABLE' };
      if (!this.#worker && this.#state === 'loading') this.#state = 'ready';
      if (!this.#pending.size && this.#preparingRequests === 1)
        this.#resources.idle().catch(() => { this.#resourceDiagnostic = { code: 'INFERENCE_RESOURCE_RELEASE_FAILED' }; });
      if (this.#closed) throw new EmbeddingError('Embedding service is closed.', 'EMBEDDING_CLOSED');
      if (signal?.aborted || error.name === 'AbortError') throw abortedError();
      throw new EmbeddingError('Local embedding resources are temporarily unavailable.', 'EMBEDDING_BUSY');
    } finally {
      this.#preparingRequests--;
      // Transfer accepted preparation into its native ticket before deciding that retirement is idle.
      // 先把已接纳的准备请求转为原生票据，再判断退役是否已排空，不能在派发前提前关闭。
      queueMicrotask(() => this.#retireWhenIdle());
    }
    if (this.#closed) { this.#admission.release(ticket); throw new EmbeddingError('Embedding service is closed.', 'EMBEDDING_CLOSED'); }
    if (signal?.aborted) {
      if (!this.#pending.size && !this.#preparingRequests) this.#resources.idle().catch(() => {});
      this.#admission.release(ticket);
      throw abortedError();
    }
    this.#cpuThreads = resourceBudget.cpuThreads;
    this.#admission.setResourceBudget(resourceBudget.memoryBytes);
    if (kind !== 'fit-documents' && this.#profile.requiredDevice && resourceBudget.device !== this.#profile.requiredDevice) {
      if (!this.#pending.size && !this.#preparingRequests) this.#resources.idle().catch(() => {});
      this.#admission.release(ticket);
      if (!this.#worker) this.#state = 'ready';
      throw new EmbeddingError('The selected GPU vector space requires an available mapped backend.', 'EMBEDDING_GPU_REQUIRED');
    }
    try { this.#startWorker(resourceBudget); }
    catch (error) { this.#admission.release(ticket); throw error; }
    const worker = this.#worker;
    worker.ref();
    this.#workerPhase = 'queued';
    this.#idleSince = 0;
    const id = ++this.#nextRequestId;
    this.#nativeTickets.set(id, ticket);
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
      this.#pending.set(id, { resolve: resolveRequest, reject: rejectRequest, cleanup,
        expectedCount: texts.length, kind, ...(kind === 'fit-documents' ? { inputs: texts } : {}) });
      signal?.addEventListener('abort', abort, { once: true });
      if (signal?.aborted) { this.#nativeTickets.delete(id); this.#admission.release(ticket); abort(); return; }
      timer = setTimeout(() => cancel(new EmbeddingError('Local embedding request timed out.', 'EMBEDDING_TIMEOUT')), this.#timeoutMs);
      try { worker.postMessage(kind === 'fit-documents' ? { type: kind, id, documents: texts, resourceBudget }
        : { type: 'embed', id, kind, texts, resourceBudget }); }
      catch {
        this.#nativeTickets.delete(id); this.#admission.release(ticket);
        cancel(new EmbeddingError('Local embedding request could not be dispatched.', 'EMBEDDING_WORKER_FAILED'));
      }
    });
  }

  // Claim retirement synchronously; the router removes this instance before another request can enter it.
  // 同步认领退役；路由在下一个请求进入前移除旧实例，原生取消回执未到时不能认领。
  tryRetireIdleGpu({ signal } = {}) {
    return this.#tryRetireIdle({ signal, gpuOnly: true });
  }

  tryRetireIdleResources({ signal, minimumIdleMs = 0 } = {}) {
    return this.#tryRetireIdle({ signal, minimumIdleMs });
  }

  #tryRetireIdle({ signal, gpuOnly = false, minimumIdleMs = 0 }) {
    if (signal?.aborted) throw abortedError();
    if (this.#closed) return { retiring: false, reason: 'closed' };
    if (this.#pending.size || this.#preparingRequests || this.#nativeTickets.size || this.#admission.status().activeRequests)
      return { retiring: false, reason: 'native-work-outstanding' };
    const { gpuMemoryBytes, residentMemoryBytes } = this.#resources.status();
    if (gpuOnly && !gpuMemoryBytes) return { retiring: false, reason: 'no-gpu-residency' };
    // A recorded native exit may leave restart reservations; pressure may reclaim those without waiting for an impossible idle event.
    // 已确认原生退出后仍可能保留重启预约；资源压力下可回收这类预约，不等待已经退出的进程再发送空闲事件。
    if (!this.#worker && this.#workerExitConfirmed && this.#workerPhase === 'stopped' && minimumIdleMs === 0 &&
        (gpuMemoryBytes > 0 || residentMemoryBytes > 0))
      return { retiring: true, gpuMemoryBytes, residentMemoryBytes, completion: this.close() };
    if (!this.#worker || this.#workerPhase !== 'idle') return { retiring: false, reason: 'worker-not-idle' };
    if (minimumIdleMs > 0 && (!this.#idleSince || Date.now() - this.#idleSince < minimumIdleMs))
      return { retiring: false, reason: 'idle-grace-period' };
    // CPU weights and tokenizer residency have the same ownership boundary as GPU memory.
    // CPU 权重与分词器驻留采用和显存相同的所有权边界：确认原生进程退出后才归还租约。
    return { retiring: true, gpuMemoryBytes, residentMemoryBytes, completion: this.close() };
  }

  #assertDevicePreference(devicePreference) {
    if (this.#closed || this.#retiring)
      throw new EmbeddingError('Embedding service is closed or retiring.', 'EMBEDDING_CLOSED');
    if (devicePreference !== this.#devicePreference)
      throw new EmbeddingError('Device preference requires a separately configured session. / 设备偏好变化需要重新配置会话。',
        'EMBEDDING_DEVICE_PREFERENCE_CHANGED');
  }

  // Policy changes stop new admission while accepted and cancelled native work keeps its ownership until idle.
  // 策略变化阻止新增接纳；已接纳及已取消的原生工作继续保留所有权，排空后才关闭和释放驻留。
  retire() {
    if (this.#retirement) return this.#retirement;
    if (this.#closed) return this.close();
    this.#retiring = true;
    this.#retirement = new Promise((resolveRetirement, rejectRetirement) => {
      this.#retirementCompletion = { resolve: resolveRetirement, reject: rejectRetirement };
    });
    this.#retireWhenIdle();
    return this.#retirement;
  }

  #retireWhenIdle() {
    if (!this.#retiring || this.#closed || this.#pending.size || this.#preparingRequests || this.#nativeTickets.size ||
        this.#admission.status().activeRequests || this.#worker && !['idle', 'stopped'].includes(this.#workerPhase)) return;
    this.close();
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
    this.#closePromise = this.#drainWorker(worker).finally(async () => {
      if (!this.#worker) await this.#resources.close();
    });
    if (this.#retirementCompletion) {
      const completion = this.#retirementCompletion;
      this.#retirementCompletion = undefined;
      this.#closePromise.then(completion.resolve, completion.reject);
    }
    return this.#closePromise;
  }

  async #drainWorker(worker) {
    if (!worker) {
      this.#admission.clear(); this.#nativeTickets.clear();
      this.#state = 'unavailable';
      return;
    }
    worker.ref();
    let timer;
    try {
      // A fatal load may have already disconnected IPC; still observe its acknowledged exit.
      // 加载失败可能已断开 IPC；仍观察其释放回执和退出结果，避免把正常排空误报为失败。
      try { worker.postMessage({ type: 'close' }); } catch {}
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
      try { await worker.terminate?.(); }
      catch {
        this.#errorCode = 'EMBEDDING_CLOSE_FAILED';
        throw new EmbeddingError('Local embedding process could not be safely reaped.', this.#errorCode);
      }
      throw error;
    } finally {
      clearTimeout(timer);
      // A timed-out owned process is reaped before retirement reports failure.
      // 关闭超时时先回收本服务拥有的进程，再报告退役失败，避免残留模型污染后续存储操作。
    }
  }
}

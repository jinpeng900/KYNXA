import { existsSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { createNativeInferenceProcess } from './native-inference-process.mjs';
import { BUILTIN_RERANKER_PROFILE, defaultRerankerModelRoot } from './reranker-profile.mjs';
import { resolveRetrievalModelProfile, retrievalModelMetadata, unavailableProfileStatus, RetrievalModelProfileError } from './model-registry.mjs';
import { InferenceResourceReservation } from './inference-resources.mjs';
import { InferenceAdmission } from './inference-admission.mjs';

const failure = (message, code) => Object.assign(new Error(message), { code });
const aborted = () => Object.assign(failure('Reranking cancelled.', 'RERANK_CANCELLED'), { name: 'AbortError' });

/** A lazy, offline native process; optional reranking never changes canonical evidence or its permissions.
 * 按需加载的离线原生进程；可选重排不改变正式证据、来源身份或权限。 */
export class RerankerService {
  #worker; #exit; #closing; #sequence = 0; #pending = new Map();
  #closed = false; #loaded = false; #phase = 'stopped'; #state; #errorCode;
  #profile; #workerFactory; #assetVerification = 'pending';
  #resources; #preparingRequests = 0; #inferenceBackend; #resourceDiagnostic; #workerRestarts = 0;
  #admission; #nativeTickets = new Map();
  #resourceOptions; #yielding; #permanentClose = false;
  #devicePreference; #retiring = false; #retirement; #retirementCompletion;
  constructor({ modelRoot = defaultRerankerModelRoot(), cpuThreads, resourceService, devicePreference = 'auto', requestLimits, timeoutMs = 60000, closeTimeoutMs = 30000,
    profileId = BUILTIN_RERANKER_PROFILE.id, workerFactory = createNativeInferenceProcess } = {}) {
    this.#profile = resolveRetrievalModelProfile('reranker', profileId);
    this.#devicePreference = devicePreference;
    this.#admission = new InferenceAdmission(requestLimits);
    if (this.#profile.requiredDevice && devicePreference === 'cpu') throw failure('Selected GPU reranker requires DirectML.', 'RERANK_GPU_REQUIRED');
    this.#workerFactory = workerFactory;
    this.modelRoot = resolve(modelRoot);
    this.cpuThreads = 0;
    this.#resourceOptions = { profile: this.#profile, cpuThreads, resourceService, devicePreference, kind: 'foreground' };
    this.#resources = new InferenceResourceReservation(this.#resourceOptions);
    this.timeoutMs = Math.max(1000, Math.min(300000, Number.isFinite(timeoutMs) ? timeoutMs : 60000));
    this.closeTimeoutMs = Math.max(1, Math.min(300000, Number.isFinite(closeTimeoutMs) ? closeTimeoutMs : 30000));
    this.#state = this.#profile.files.every(asset => existsSync(join(this.modelRoot, asset.path))) ? 'ready' : 'unavailable';
    if (this.#state === 'unavailable') this.#errorCode = 'RERANK_ASSET_MISSING';
  }
  status(profileId = this.#profile.id) {
    if (profileId !== this.#profile.id) return unavailableProfileStatus('reranker', profileId);
    const resourceReservation = this.#resources.status();
    return { ...this.#metadata(), state: this.#state, loaded: this.#loaded, supported: true,
      local: true, network: false, workerPhase: this.#phase, assetVerification: this.#assetVerification,
      devicePreference: this.#devicePreference, retiring: this.#retiring,
      maxInputTokens: this.#profile.maxInputTokens, pendingRequests: this.#pending.size,
      cpuThreads: this.cpuThreads, resourceReservation, batchSuggestions: resourceReservation.batchSuggestions,
      ...(resourceReservation.lastGrant ? { batchSize: resourceReservation.lastGrant.batchSize,
        batchTokenBudget: resourceReservation.lastGrant.batchTokenBudget } : {}),
      inputAdmission: this.#admission.status(), requestLimits: this.#admission.limits,
      ...(this.#inferenceBackend ? { inferenceBackend: this.#inferenceBackend } : {}),
      ...(this.#resourceDiagnostic ? { resourceDiagnostic: this.#resourceDiagnostic } : {}),
      ...(this.#errorCode ? { errorCode: this.#errorCode } : {}) };
  }
  async rerank({ query, candidates, signal, limit = 20, profileId = this.#profile.id, devicePreference = this.#devicePreference }) {
    if (devicePreference !== this.#devicePreference)
      throw failure('Device preference requires a separately configured session. / 设备偏好变化需要重新配置会话。', 'RERANK_DEVICE_PREFERENCE_CHANGED');
    resolveRetrievalModelProfile('reranker', profileId);
    if (profileId !== this.#profile.id) throw new RetrievalModelProfileError('reranker', profileId);
    if (this.#yielding) await this.#waitForYield(signal);
    if (this.#closed || this.#retiring) throw failure('Reranker is closed or retiring.', 'RERANK_CLOSED');
    if (signal?.aborted) throw aborted();
    if (typeof query !== 'string' || !query.trim() || query.length > 2048 || !Array.isArray(candidates) ||
        candidates.length > this.#admission.limits.maxBatchDocuments || !Number.isInteger(limit) || limit < 1 || limit > this.#profile.maxCandidates)
      throw failure('Invalid reranker request.', 'RERANK_INVALID_INPUT');
    const items = candidates.slice(0, limit);
    const texts = items.map(item => item.excerpt ?? item.text);
    if (texts.some(text => typeof text !== 'string' || !text.trim() || text.length > 16384))
      throw failure('Invalid reranker passage.', 'RERANK_INVALID_INPUT');
    if (!items.length) return { items: [], ...this.#metadata(), truncatedInputsCount: 0 };
    if (this.#state === 'unavailable') throw failure('Bundled reranker assets are missing.', 'RERANK_ASSET_MISSING');
    if (this.#state === 'error' && !this.#worker && this.#errorCode === 'RERANK_WORKER_FAILED' && this.#workerRestarts < 1) {
      this.#workerRestarts++; this.#state = 'ready'; this.#errorCode = undefined;
      if (this.#profile.requiredDevice) this.#resources.retryRequiredGpuAfterExit();
    }
    if (this.#state === 'error') throw failure('The local reranker is unavailable.', this.#errorCode ?? 'RERANK_WORKER_FAILED');
    let ticket;
    try { ticket = this.#admission.reserve(texts, query); }
    catch { throw failure('Reranker input budget is full.', 'RERANK_BUSY'); }
    this.#preparingRequests++;
    let resourceBudget;
    try { resourceBudget = await this.#resources.acquire({ signal, kind: 'foreground' }); }
    catch (error) {
      this.#admission.release(ticket);
      this.#resourceDiagnostic = { code: 'INFERENCE_RESOURCE_BUSY', reason: error.details?.reason ?? error.code ?? 'RESOURCE_UNAVAILABLE' };
      if (!this.#pending.size && this.#preparingRequests === 1)
        this.#resources.idle().catch(() => { this.#resourceDiagnostic = { code: 'INFERENCE_RESOURCE_RELEASE_FAILED' }; });
      if (this.#closed) throw failure('Reranker is closed.', 'RERANK_CLOSED');
      if (signal?.aborted || error.name === 'AbortError') throw aborted();
      throw failure('Local reranker resources are temporarily unavailable.', 'RERANK_BUSY');
    } finally {
      this.#preparingRequests--;
      // Accepted preparation must become a native ticket before retirement can claim the idle boundary.
      // 已接纳的准备请求须先转为原生票据，退役才能认领排空边界。
      queueMicrotask(() => this.#retireWhenIdle());
    }
    if (this.#closed) { this.#admission.release(ticket); throw failure('Reranker is closed.', 'RERANK_CLOSED'); }
    if (signal?.aborted) {
      if (!this.#pending.size && !this.#preparingRequests) this.#resources.idle().catch(() => {});
      this.#admission.release(ticket);
      throw aborted();
    }
    this.cpuThreads = resourceBudget.cpuThreads;
    this.#admission.setResourceBudget(resourceBudget.memoryBytes);
    if (this.#profile.requiredDevice && resourceBudget.device !== this.#profile.requiredDevice) {
      if (!this.#pending.size && !this.#preparingRequests) this.#resources.idle().catch(() => {});
      this.#admission.release(ticket);
      throw failure('Selected GPU reranker requires an available mapped backend.', 'RERANK_GPU_REQUIRED');
    }
    try { this.#start(resourceBudget); } catch (error) { this.#admission.release(ticket); throw error; }
    const id = ++this.#sequence, worker = this.#worker;
    this.#nativeTickets.set(id, ticket);
    worker.ref();
    const result = await new Promise((resolveResult, rejectResult) => {
      const cancel = error => {
        if (!this.#pending.delete(id)) return;
        cleanup();
        try { worker.postMessage({ type: 'cancel', id }); } catch {}
        rejectResult(error);
      };
      const abort = () => cancel(aborted());
      const timer = setTimeout(() => cancel(failure('Reranking timed out.', 'RERANK_TIMEOUT')), this.timeoutMs);
      const cleanup = () => { clearTimeout(timer); signal?.removeEventListener('abort', abort); };
      this.#pending.set(id, { resolveResult, rejectResult, cleanup });
      signal?.addEventListener('abort', abort, { once: true });
      if (signal?.aborted) { this.#nativeTickets.delete(id); this.#admission.release(ticket); abort(); return; }
      try { worker.postMessage({ type: 'rerank', id, query, texts, resourceBudget }); }
      catch (error) { this.#nativeTickets.delete(id); this.#admission.release(ticket); cancel(failure(error.message, 'RERANK_WORKER_FAILED')); }
    });
    if (!Array.isArray(result.scores) || result.scores.length !== items.length ||
        result.scores.some(score => !Number.isFinite(score) || score < 0 || score > 1) ||
        !Number.isInteger(result.truncatedInputsCount) || result.truncatedInputsCount < 0 || result.truncatedInputsCount > items.length)
      throw failure('Invalid reranker response.', 'RERANK_INVALID_RESULT');
    const ranked = items.map((item, index) => ({ ...item, rerankScore: result.scores[index], originalRank: index }))
      .sort((left, right) => right.rerankScore - left.rerankScore || left.originalRank - right.originalRank)
      .map(({ originalRank, ...item }, index) => ({ ...item, rerankRank: index + 1 }));
    return { items: [...ranked, ...candidates.slice(limit)], ...this.#metadata(), truncatedInputsCount: result.truncatedInputsCount };
  }
  #metadata() { return retrievalModelMetadata(this.#profile); }
  #fail(error) {
    if (!this.#closed) {
      this.#state = error.code === 'RERANK_ASSET_MISSING' ? 'unavailable' : 'error';
      this.#errorCode = error.code ?? 'RERANK_WORKER_FAILED'; this.#loaded = false;
      this.#inferenceBackend = undefined;
      if (error.code === 'RERANK_ASSET_INVALID' || error.code === 'RERANK_ASSET_MISSING') this.#assetVerification = 'failed';
    }
    for (const pending of this.#pending.values()) { pending.cleanup(); pending.rejectResult(error); }
    this.#pending.clear();
  }
  #start(resourceBudget) {
    if (this.#worker) return;
    this.#state = 'loading'; this.#phase = 'starting';
    let worker;
    try {
      worker = this.#workerFactory(new URL('./reranker-worker.mjs', import.meta.url), { execArgv: [],
        workerData: { modelRoot: this.modelRoot, cpuThreads: resourceBudget.cpuThreads, profileId: this.#profile.id,
          requestLimits: this.#admission.limits, devicePreference: this.#devicePreference } });
    } catch {
      const error = failure('The local reranker worker could not start.', 'RERANK_WORKER_FAILED');
      this.#fail(error); throw error;
    }
    this.#worker = worker;
    this.#resources.registerExecutor(worker.pid).catch(() => { this.#resourceDiagnostic = { code: 'INFERENCE_EXECUTOR_REGISTRATION_FAILED' }; });
    let resolveExit, rejectExit, acknowledged = false;
    this.#exit = new Promise((resolveResult, rejectResult) => { resolveExit = resolveResult; rejectExit = rejectResult; });
    this.#exit.catch(() => {});
    worker.on('message', message => {
      if (this.#worker !== worker || !message || typeof message !== 'object') return;
      if (message.type === 'phase') { if (!this.#closed) this.#phase = message.phase; return; }
      if (message.type === 'settled') {
        this.#admission.release(this.#nativeTickets.get(message.id)); this.#nativeTickets.delete(message.id);
        this.#retireWhenIdle(); return;
      }
      if (message.type === 'ready') {
        if (!this.#closed && this.#state !== 'error' && this.#state !== 'unavailable') {
          this.#state = 'ready'; this.#loaded = true; this.#errorCode = undefined; this.#assetVerification = 'verified';
          if (message.inferenceBackend) {
            this.#inferenceBackend = message.inferenceBackend;
            this.#resources.backend(message.inferenceBackend).catch(() => { this.#resourceDiagnostic = { code: 'INFERENCE_RESOURCE_RELEASE_FAILED' }; });
          }
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
      if (message.type === 'fatal') { this.#fail(failure(message.message, message.code)); return; }
      if (message.type === 'closed') { acknowledged = message.disposed === true; return; }
      if (message.type === 'shutdown-error') {
        const error = failure('Reranker shutdown failed.', 'RERANK_CLOSE_FAILED');
        rejectExit(error); this.#fail(error); return;
      }
      if (message.type === 'idle') {
        for (const [requestId, ticket] of this.#nativeTickets)
          if (requestId <= message.throughId) { this.#admission.release(ticket); this.#nativeTickets.delete(requestId); }
        if (!this.#pending.size && !this.#preparingRequests && !this.#closed && message.throughId >= this.#sequence) {
          this.#phase = 'idle';
          if (!this.#loaded && this.#state === 'loading') this.#state = 'ready';
          worker.unref();
          this.#resources.idle().catch(() => { this.#resourceDiagnostic = { code: 'INFERENCE_RESOURCE_RELEASE_FAILED' }; });
          this.#retireWhenIdle();
        }
        return;
      }
      const pending = this.#pending.get(message.id);
      if (!pending) return;
      this.#pending.delete(message.id); pending.cleanup();
      if (message.type === 'result') pending.resolveResult(message);
      else {
        if (message.code !== 'RERANK_BUSY') { this.#state = 'error'; this.#errorCode = message.code ?? 'RERANK_FAILED'; }
        pending.rejectResult(failure(message.message, message.code));
      }
    });
    worker.on('error', error => this.#fail(failure(error.message, 'RERANK_WORKER_FAILED')));
    worker.on('exit', (exitCode, signal) => {
      resolveExit({ exitCode, acknowledged });
      if (this.#worker !== worker) return;
      this.#worker = undefined; this.#phase = 'stopped';
      this.#admission.clear(); this.#nativeTickets.clear();
      if (!this.#closed && !acknowledged && this.#resources.status().gpuMemoryBytes > 0)
        this.#resources.backend({ device: 'cpu', diagnostic: { code: 'GPU_WORKER_FAILED' } })
          .catch(() => { this.#resourceDiagnostic = { code: 'INFERENCE_RESOURCE_RELEASE_FAILED' }; });
      this.#resources.idle().catch(() => { this.#resourceDiagnostic = { code: 'INFERENCE_RESOURCE_RELEASE_FAILED' }; });
      if (!this.#closed && (this.#pending.size || (this.#state !== 'error' && this.#state !== 'unavailable')))
        this.#fail(failure(`Reranker process exited (${signal ?? exitCode}).`, 'RERANK_WORKER_FAILED'));
      this.#retireWhenIdle();
    });
  }
  #waitForYield(signal) {
    if (signal?.aborted) return Promise.reject(aborted());
    if (!signal) return this.#yielding;
    return new Promise((resolve, reject) => {
      const abort = () => { signal.removeEventListener('abort', abort); reject(aborted()); };
      signal.addEventListener('abort', abort, { once: true });
      this.#yielding.then(result => { signal.removeEventListener('abort', abort); resolve(result); },
        error => { signal.removeEventListener('abort', abort); reject(error); });
    });
  }

  // Lock before the first await; callers cancelled before native settlement still prevent retirement.
  // 首次 await 前锁定退役；调用方取消但原生工作尚未确认完成时，仍禁止释放驻留。
  releaseIdleGpu({ signal } = {}) {
    if (signal?.aborted) return Promise.reject(aborted());
    if (this.#yielding) return this.#yielding;
    if (this.#closed) return Promise.resolve({ released: false, reason: 'closed' });
    if (this.#pending.size || this.#preparingRequests || this.#nativeTickets.size || this.#admission.status().activeRequests)
      return Promise.resolve({ released: false, reason: 'native-work-outstanding' });
    const gpuMemoryBytes = this.#resources.status().gpuMemoryBytes;
    if (!gpuMemoryBytes) return Promise.resolve({ released: false, reason: 'no-gpu-residency' });
    if (!this.#worker || this.#phase !== 'idle') return Promise.resolve({ released: false, reason: 'worker-not-idle' });
    const retirement = this.#closeOwned();
    const yielding = retirement.then(() => {
      if (!this.#permanentClose) {
        this.#resources = new InferenceResourceReservation(this.#resourceOptions);
        this.#closed = false; this.#closing = undefined;
        this.#loaded = false; this.#phase = 'stopped'; this.#errorCode = undefined;
        this.#inferenceBackend = undefined; this.#resourceDiagnostic = undefined; this.#workerRestarts = 0;
        this.#assetVerification = 'pending'; this.cpuThreads = 0;
        this.#state = this.#profile.files.every(asset => existsSync(join(this.modelRoot, asset.path))) ? 'ready' : 'unavailable';
        if (this.#state === 'unavailable') this.#errorCode = 'RERANK_ASSET_MISSING';
      }
      return { released: true, gpuMemoryBytes };
    }).finally(() => { if (this.#yielding === yielding) this.#yielding = undefined; });
    this.#yielding = yielding;
    return yielding;
  }

  close() {
    this.#permanentClose = true;
    return this.#closeOwned();
  }

  // A preference switch waits for accepted requests and native settlement instead of cancelling another caller.
  // 设备偏好切换等待已接纳请求及原生排空，不能为了新偏好取消其他调用方的工作。
  retire() {
    if (this.#retirement) return this.#retirement;
    if (this.#permanentClose) return this.close();
    this.#retiring = true;
    this.#retirement = new Promise((resolveRetirement, rejectRetirement) => {
      this.#retirementCompletion = { resolve: resolveRetirement, reject: rejectRetirement };
    });
    this.#yielding?.then(() => this.#retireWhenIdle(), error => this.#retirementCompletion?.reject(error));
    this.#retireWhenIdle();
    return this.#retirement;
  }

  #retireWhenIdle() {
    if (!this.#retiring || this.#yielding || this.#closed || this.#pending.size || this.#preparingRequests || this.#nativeTickets.size ||
        this.#admission.status().activeRequests || this.#worker && !['idle', 'stopped'].includes(this.#phase)) return;
    this.close();
  }

  #closeOwned() {
    if (this.#closing) {
      if (this.#retirementCompletion) {
        const completion = this.#retirementCompletion;
        this.#retirementCompletion = undefined;
        this.#closing.then(completion.resolve, completion.reject);
      }
      return this.#closing;
    }
    this.#closed = true; this.#loaded = false;
    this.#errorCode = 'RERANK_CLOSED';
    for (const pending of this.#pending.values()) { pending.cleanup(); pending.rejectResult(failure('Reranker closing.', 'RERANK_CLOSED')); }
    this.#pending.clear();
    this.#closing = this.#drain().finally(async () => { if (!this.#worker) await this.#resources.close(); });
    if (this.#retirementCompletion) {
      const completion = this.#retirementCompletion;
      this.#retirementCompletion = undefined;
      this.#closing.then(completion.resolve, completion.reject);
    }
    return this.#closing;
  }
  async #drain() {
    const worker = this.#worker;
    if (!worker) { this.#admission.clear(); this.#nativeTickets.clear(); this.#state = 'unavailable'; return; }
    this.#state = 'closing'; this.#phase = 'closing'; worker.ref();
    let timer;
    try {
      // A fatal load may already be draining after IPC disconnect; the exit receipt decides retirement.
      // 加载失败可能已在 IPC 断开后排空；退役结果由释放回执和退出状态决定。
      try { worker.postMessage({ type: 'close' }); } catch {}
      const result = await Promise.race([this.#exit, new Promise((resolveResult, rejectResult) => {
        timer = setTimeout(() => rejectResult(failure('Reranker shutdown timed out.', 'RERANK_CLOSE_TIMEOUT')), this.closeTimeoutMs);
      })]);
      if (result.exitCode !== 0 || !result.acknowledged) throw failure('Reranker was not safely retired.', 'RERANK_CLOSE_FAILED');
      this.#state = 'unavailable';
    } catch (error) {
      this.#state = 'error'; this.#errorCode = error.code ?? 'RERANK_CLOSE_FAILED';
      this.#fail(error);
      try { await worker.terminate?.(); }
      catch {
        const terminationError = failure('Reranker process could not be safely reaped.', 'RERANK_CLOSE_FAILED');
        this.#errorCode = terminationError.code;
        this.#fail(terminationError); throw terminationError;
      }
      throw error;
    }
    finally { clearTimeout(timer); }
  }
}

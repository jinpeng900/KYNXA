import { existsSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { Worker } from 'node:worker_threads';
import { BUILTIN_RERANKER_PROFILE, defaultRerankerModelRoot } from './reranker-profile.mjs';

const failure = (message, code) => Object.assign(new Error(message), { code });
const aborted = () => Object.assign(failure('Reranking cancelled.', 'RERANK_CANCELLED'), { name: 'AbortError' });

/** A lazy, offline native worker; optional reranking never changes canonical evidence or its permissions.
 * 按需加载的离线原生 worker；可选重排不改变正式证据、来源身份或权限。 */
export class RerankerService {
  #worker; #exit; #closing; #sequence = 0; #pending = new Map();
  #closed = false; #loaded = false; #phase = 'stopped'; #state; #errorCode;
  constructor({ modelRoot = defaultRerankerModelRoot(), cpuThreads = 2, timeoutMs = 60000, closeTimeoutMs = 30000 } = {}) {
    this.modelRoot = resolve(modelRoot);
    this.cpuThreads = Math.max(1, Math.min(2, Number.isInteger(cpuThreads) ? cpuThreads : 2));
    this.timeoutMs = Math.max(1000, Math.min(300000, Number.isFinite(timeoutMs) ? timeoutMs : 60000));
    this.closeTimeoutMs = Math.max(1, Math.min(300000, Number.isFinite(closeTimeoutMs) ? closeTimeoutMs : 30000));
    this.#state = BUILTIN_RERANKER_PROFILE.files.every(asset => existsSync(join(this.modelRoot, asset.path))) ? 'ready' : 'unavailable';
    if (this.#state === 'unavailable') this.#errorCode = 'RERANK_ASSET_MISSING';
  }
  status() {
    return { profileId: BUILTIN_RERANKER_PROFILE.id, modelVersion: BUILTIN_RERANKER_PROFILE.modelVersion,
      state: this.#state, loaded: this.#loaded, local: true, network: false, workerPhase: this.#phase,
      maxInputTokens: BUILTIN_RERANKER_PROFILE.maxInputTokens, pendingRequests: this.#pending.size,
      ...(this.#errorCode ? { errorCode: this.#errorCode } : {}) };
  }
  async rerank({ query, candidates, signal, limit = 20 }) {
    if (this.#closed) throw failure('Reranker is closed.', 'RERANK_CLOSED');
    if (signal?.aborted) throw aborted();
    if (typeof query !== 'string' || !query.trim() || query.length > 2048 || !Array.isArray(candidates) ||
        candidates.length > 60 || !Number.isInteger(limit) || limit < 1 || limit > BUILTIN_RERANKER_PROFILE.maxCandidates)
      throw failure('Invalid reranker request.', 'RERANK_INVALID_INPUT');
    const items = candidates.slice(0, limit);
    const texts = items.map(item => item.excerpt ?? item.text);
    if (texts.some(text => typeof text !== 'string' || !text.trim() || text.length > 16384))
      throw failure('Invalid reranker passage.', 'RERANK_INVALID_INPUT');
    if (!items.length) return { items: [], ...this.#metadata(), truncatedInputsCount: 0 };
    if (this.#state === 'unavailable') throw failure('Bundled reranker assets are missing.', 'RERANK_ASSET_MISSING');
    if (this.#state === 'error') throw failure('The local reranker is unavailable.', this.#errorCode ?? 'RERANK_WORKER_FAILED');
    if (this.#pending.size >= 8) throw failure('Reranker queue is full.', 'RERANK_BUSY');
    this.#start();
    const id = ++this.#sequence, worker = this.#worker;
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
      try { worker.postMessage({ type: 'rerank', id, query, texts }); }
      catch (error) { cancel(failure(error.message, 'RERANK_WORKER_FAILED')); }
    });
    if (!Array.isArray(result.scores) || result.scores.length !== items.length || result.scores.some(score => !Number.isFinite(score) || score < 0 || score > 1))
      throw failure('Invalid reranker response.', 'RERANK_INVALID_RESULT');
    const ranked = items.map((item, index) => ({ ...item, rerankScore: result.scores[index], originalRank: index }))
      .sort((left, right) => right.rerankScore - left.rerankScore || left.originalRank - right.originalRank)
      .map(({ originalRank, ...item }, index) => ({ ...item, rerankRank: index + 1 }));
    return { items: [...ranked, ...candidates.slice(limit)], ...this.#metadata(), truncatedInputsCount: result.truncatedInputsCount };
  }
  #metadata() { return { profileId: BUILTIN_RERANKER_PROFILE.id, modelVersion: BUILTIN_RERANKER_PROFILE.modelVersion }; }
  #fail(error) {
    this.#state = 'error'; this.#errorCode = error.code ?? 'RERANK_WORKER_FAILED';
    for (const pending of this.#pending.values()) { pending.cleanup(); pending.rejectResult(error); }
    this.#pending.clear();
  }
  #start() {
    if (this.#worker) return;
    this.#state = 'loading'; this.#phase = 'starting';
    const worker = new Worker(new URL('./reranker-worker.mjs', import.meta.url), { execArgv: [],
      workerData: { modelRoot: this.modelRoot, cpuThreads: this.cpuThreads } });
    this.#worker = worker;
    let resolveExit, acknowledged = false;
    this.#exit = new Promise(resolveResult => { resolveExit = resolveResult; });
    worker.on('message', message => {
      if (message.type === 'phase') { this.#phase = message.phase; return; }
      if (message.type === 'ready') { if (!this.#closed) { this.#state = 'ready'; this.#loaded = true; } return; }
      if (message.type === 'closed') { acknowledged = message.disposed === true; return; }
      if (message.type === 'shutdown-error') { this.#fail(failure('Reranker shutdown failed.', 'RERANK_CLOSE_FAILED')); return; }
      if (message.type === 'idle') {
        if (!this.#pending.size && !this.#closed && message.throughId >= this.#sequence) { this.#phase = 'idle'; worker.unref(); }
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
    worker.on('exit', exitCode => {
      resolveExit({ exitCode, acknowledged }); this.#worker = undefined; this.#phase = 'stopped';
      if (!this.#closed) this.#fail(failure('Reranker worker exited.', 'RERANK_WORKER_FAILED'));
    });
  }
  close() {
    if (this.#closing) return this.#closing;
    this.#closed = true; this.#loaded = false;
    for (const pending of this.#pending.values()) { pending.cleanup(); pending.rejectResult(failure('Reranker closing.', 'RERANK_CLOSED')); }
    this.#pending.clear();
    this.#closing = this.#drain();
    return this.#closing;
  }
  async #drain() {
    const worker = this.#worker;
    if (!worker) { this.#state = 'unavailable'; return; }
    this.#state = 'closing'; worker.ref();
    let timer;
    try {
      worker.postMessage({ type: 'close' });
      const result = await Promise.race([this.#exit, new Promise((resolveResult, rejectResult) => {
        timer = setTimeout(() => rejectResult(failure('Reranker shutdown timed out.', 'RERANK_CLOSE_TIMEOUT')), this.closeTimeoutMs);
      })]);
      if (result.exitCode !== 0 || !result.acknowledged) throw failure('Reranker was not safely retired.', 'RERANK_CLOSE_FAILED');
      this.#state = 'unavailable';
    } catch (error) { this.#fail(error); throw error; }
    finally { clearTimeout(timer); }
  }
}

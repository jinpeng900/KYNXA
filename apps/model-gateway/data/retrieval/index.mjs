import { resolve } from 'node:path';
import { Worker } from 'node:worker_threads';
import { attachResourceWorkerBridge } from '../../platform/resources/resource-worker-client.mjs';
import { MAX_QUERY_CHARACTERS, retrievalFailure, retrievalScopeKeys, retrievalSourceId, validateRetrievalIntent } from './retrieval-contracts.mjs';
import { validateSourceWindowOptions } from './source-window.mjs';
import { validatedEvidenceReference } from './evidence-references.mjs';
import { validateAnnOptions } from './ann-store.mjs';

export { chunkSource } from './retrieval-text.mjs';

function boundedInteger(value, fallback, minimum, maximum) {
  value ??= fallback;
  if (!Number.isSafeInteger(value) || value < minimum || value > maximum)
    throw retrievalFailure('Invalid retrieval page size. / 检索分页大小无效。');
  return value;
}

/** SQLite and text work stays in one owned worker, away from SSE and the desktop UI.
 * SQLite 与文本计算由网关拥有的单个 worker 执行，不阻塞 SSE 或桌面界面。 */
export class RetrievalIndex {
  constructor({ root, vectorEnabled = true, ann, resourceService }) {
    if (typeof root !== 'string' || !root.trim()) throw retrievalFailure('A managed data root is required. / 必须提供统一数据根目录。');
    this.root = resolve(root);
    this.vectorEnabled = vectorEnabled;
    this.ann = validateAnnOptions(ann);
    this.resources = resourceService;
    this.worker = null;
    this.pending = new Map();
    this.sequence = 0;
    this.closed = false;
    this.closing = null;
    this.failed = null;
    this.restartTimes = [];
    this.workerRestarts = 0;
  }

  _start() {
    if (this.worker) return;
    this.worker = new Worker(new URL('./index-worker.mjs', import.meta.url), {
      // Test/debug launcher flags can be process-only; this worker needs no custom VM flags.
      // 测试或调试启动参数可能只允许主进程使用，本 worker 无需继承它们。
      execArgv: [],
      workerData: { root: this.root, vectorEnabled: this.vectorEnabled, ann: this.ann, resourceBridge: Boolean(this.resources) } });
    const worker = this.worker;
    if (this.resources) attachResourceWorkerBridge(this.worker, this.resources, {
      context: { taskIdPrefix: 'retrieval-index', kind: 'background' } });
    this.worker.on('message', message => {
      const pending = this.pending.get(message.id);
      if (!pending) return;
      this.pending.delete(message.id);
      pending.signal?.removeEventListener('abort', pending.abort);
      if (message.error) pending.reject(Object.assign(new Error(message.error.message), message.error));
      else pending.resolve(message.result);
      if (!this.pending.size) this.worker?.unref();
    });
    const fail = error => {
      if (this.worker !== worker) return;
      this.failed = error;
      for (const pending of this.pending.values()) {
        pending.signal?.removeEventListener('abort', pending.abort);
        pending.reject(error);
      }
      this.pending.clear();
      this.retiring ??= worker.terminate().then(() => { if (this.worker === worker) this.worker = null; })
        .catch(() => { this.retirementFailed = true; }).finally(() => { this.retiring = null; });
    };
    this.worker.on('error', fail);
    this.worker.on('exit', code => {
      // An unexpected exit must settle callers even while shutdown is waiting for its receipt.
      // 意外退出即使发生在关闭期间，也必须结算等待回执的调用，不能留下悬挂请求。
      if (this.pending.size || !this.closed && !this.closing)
        fail(retrievalFailure(`Retrieval worker exited (${code}). / 检索 worker 已退出。`, 'RETRIEVAL_WORKER_EXITED', 500));
    });
    this.worker.unref();
  }

  async _recover(signal) {
    if (!this.failed) return;
    if (!this.recovery) this.recovery = (async () => {
      await this.retiring;
      signal?.throwIfAborted();
      this.restartTimes = this.restartTimes.filter(time => time > Date.now() - 60000);
      if (this.closed || this.closing || this.retirementFailed || this.restartTimes.length >= 2) throw this.failed;
      this.restartTimes.push(Date.now()); this.workerRestarts++;
      this.failed = null;
    })().finally(() => { this.recovery = null; });
    await this.recovery;
  }

  async _request(method, input = {}, signal) {
    if (this.closed || (this.closing && method !== 'close')) return Promise.reject(retrievalFailure('Retrieval index is closed. / 检索索引已关闭。', 'RETRIEVAL_INDEX_CLOSED', 409));
    // Never replay an unknown commit. Only a new caller can reopen the authoritative SQLite log.
    // 不重放提交结果未知的旧请求；只允许新调用重新打开权威 SQLite，保持原始记录不变。
    if (this.failed && method !== 'close') await this._recover(signal);
    if (this.failed && method === 'close') {
      await this.retiring; this.closed = true;
      return { closed: true, workerFailed: true };
    }
    if (signal?.aborted) return Promise.reject(Object.assign(new Error('Retrieval cancelled. / 检索已取消。'), { name: 'AbortError', code: 'ABORT_ERR' }));
    if (this.pending.size >= 256) return Promise.reject(retrievalFailure('Retrieval queue is full. / 检索队列已满。', 'RETRIEVAL_QUEUE_FULL', 503));
    this._start();
    const id = ++this.sequence;
    const cancelBuffer = new SharedArrayBuffer(Int32Array.BYTES_PER_ELEMENT);
    const cancelFlag = new Int32Array(cancelBuffer);
    // Cancellation stops an uncommitted batch. Returned committed receipts remain observable.
    // 取消只阻止未提交批次；已经提交并返回的回执仍交付调用方。
    const abort = () => { Atomics.store(cancelFlag, 0, 1); };
    signal?.addEventListener('abort', abort, { once: true });
    if (signal?.aborted) abort();
    return new Promise((resolveResult, reject) => {
      this.pending.set(id, { resolve: resolveResult, reject, signal, abort, cancelFlag });
      this.worker.ref();
      try { this.worker.postMessage({ id, method, input, cancelBuffer }); }
      catch (error) {
        this.pending.delete(id);
        signal?.removeEventListener('abort', abort);
        if (!this.pending.size) this.worker?.unref();
        reject(error);
      }
    });
  }

  upsertSources(sources, { signal } = {}) { return this._request('upsertSources', { sources }, signal); }

  vectorSpaceStatus({ scopeKeys, signal }) {
    return this._request('vectorSpaceStatus', { scopeKeys: retrievalScopeKeys(scopeKeys) }, signal);
  }

  search({ query, scopeKeys, queryVector, embeddingProfileId, embeddingModelVersion, embeddingSpaceId, retrievalIntent, ann, limit, channelCandidates = 40, signal }) {
    if (typeof query !== 'string' || !query.trim() || query.length > MAX_QUERY_CHARACTERS)
      return Promise.reject(retrievalFailure('Invalid retrieval query. / 检索查询为空或过长。'));
    const scopes = retrievalScopeKeys(scopeKeys);
    limit = boundedInteger(limit, 8, 1, 192);
    channelCandidates = boundedInteger(channelCandidates, 40, 1, 160);
    if (queryVector !== undefined && queryVector !== null) {
      if (!(Array.isArray(queryVector) || queryVector instanceof Float32Array) || !queryVector.length || queryVector.length > 4096)
        return Promise.reject(retrievalFailure('Invalid query embedding. / 查询向量或嵌入模型配置无效。'));
      const floatVector = Array.from(new Float32Array(queryVector));
      if (!Array.from(queryVector).every(item => Number.isFinite(item)) || !floatVector.every(item => Number.isFinite(item)) || !floatVector.some(item => item !== 0) ||
          typeof embeddingProfileId !== 'string' || !embeddingProfileId)
        return Promise.reject(retrievalFailure('Invalid query embedding. / 查询向量或嵌入模型配置无效。'));
    }
    if (embeddingModelVersion !== undefined && (typeof embeddingModelVersion !== 'string' || embeddingModelVersion.length > 512))
      return Promise.reject(retrievalFailure('Invalid embedding model version. / 嵌入模型版本无效。'));
    if (embeddingSpaceId !== undefined && (typeof embeddingSpaceId !== 'string' || !/^[a-f0-9]{64}$/.test(embeddingSpaceId)))
      return Promise.reject(retrievalFailure('Invalid embedding space. / 嵌入空间身份无效。'));
    if (ann !== undefined) validateAnnOptions(ann);
    return this._request('search', { query, scopeKeys: scopes, queryVector, embeddingProfileId, embeddingModelVersion, embeddingSpaceId,
      retrievalIntent: validateRetrievalIntent(retrievalIntent),
      ...(ann === undefined ? {} : { ann: validateAnnOptions({ ...this.ann, ...ann }) }), limit, channelCandidates }, signal);
  }

  read({ sourceId, sourceRef, scopeKeys, offset, limit, signal }) {
    if (!sourceRef) retrievalSourceId(sourceId);
    return this._request('read', { sourceId, sourceRef, scopeKeys: retrievalScopeKeys(scopeKeys),
      offset: boundedInteger(offset, 0, 0, 2 * 1024 * 1024), limit: boundedInteger(limit, 12000, 1, 65536) }, signal);
  }

  verifyReference({ sourceRef, scopeKeys, signal }) {
    validatedEvidenceReference({ sourceRef });
    return this._request('verifyReference', { sourceRef, scopeKeys: retrievalScopeKeys(scopeKeys) }, signal);
  }

  relations({ sourceRef, sourceId, scopeKeys, symbol, kind, limit = 40, signal }) {
    if (sourceRef) validatedEvidenceReference({ sourceRef });
    if (sourceId) retrievalSourceId(sourceId);
    return this._request('relations', { sourceRef, sourceId, scopeKeys: retrievalScopeKeys(scopeKeys), symbol, kind, limit }, signal);
  }

  recordCoverage({ entries, scopeKeys, signal }) {
    return this._request('recordCoverage', { entries, scopeKeys: retrievalScopeKeys(scopeKeys) }, signal);
  }

  coverage({ scopeKeys, sourceId, limit = 1000, signal }) {
    if (sourceId) retrievalSourceId(sourceId);
    return this._request('coverage', { scopeKeys: retrievalScopeKeys(scopeKeys), sourceId,
      limit: boundedInteger(limit, 1000, 1, 1000) }, signal);
  }

  readWindow({ sourceRef, scopeKeys, mode, anchorOffset, beforeCharacters, limit, signal }) {
    if (typeof sourceRef !== 'string' || !sourceRef.startsWith('rag1:'))
      throw retrievalFailure('A canonical source reference is required. / 必须提供完整资料引用。', 'INVALID_RETRIEVAL_WINDOW');
    validatedEvidenceReference({ sourceRef });
    const windowOptions = validateSourceWindowOptions({ mode: mode === 'unit' ? 'window' : mode, anchorOffset, beforeCharacters, limit });
    return this._request('readWindow', { sourceRef, scopeKeys: retrievalScopeKeys(scopeKeys),
      ...windowOptions, ...(mode === 'unit' ? { mode: 'unit' } : {}) }, signal);
  }

  removeSource(sourceId, { scopeKeys, permanent = true, signal } = {}) {
    if (typeof permanent !== 'boolean') return Promise.reject(retrievalFailure('Invalid deletion mode. / 资料删除模式无效。'));
    return this._request('removeSource', { sourceId: retrievalSourceId(sourceId), scopeKeys: retrievalScopeKeys(scopeKeys), permanent }, signal);
  }

  listSources({ scopeKeys, sourceType, sourceId, signal }) {
    if (sourceType !== undefined && (typeof sourceType !== 'string' || !sourceType || sourceType.length > 100))
      return Promise.reject(retrievalFailure('Invalid source type. / 资料类型无效。'));
    return this._request('listSources', { scopeKeys: retrievalScopeKeys(scopeKeys), sourceType,
      ...(sourceId === undefined ? {} : { sourceId: retrievalSourceId(sourceId) }) }, signal);
  }

  invalidateScope(scopeKey, { signal } = {}) {
    return this._request('invalidateScope', { scopeKey: retrievalScopeKeys([scopeKey])[0] }, signal);
  }

  scopeVersion({ scopeKeys, signal }) {
    return this._request('scopeVersion', { scopeKeys: retrievalScopeKeys(scopeKeys) }, signal);
  }

  prepareVectors({ scopeKeys, ann, signal }) {
    return this._request('prepareVectors', { scopeKeys: retrievalScopeKeys(scopeKeys),
      ...(ann === undefined ? {} : { ann: validateAnnOptions({ ...this.ann, ...ann }) }) }, signal);
  }

  async status() { return { ...await this._request('status'), workerRestarts: this.workerRestarts }; }

  close() {
    if (this.closed) return Promise.resolve({ closed: true });
    if (this.closing) return this.closing;
    if (!this.worker) { this.closed = true; return Promise.resolve(this.retiring).then(() => ({ closed: true })); }
    // Closing revokes queued work before the close message; already committed receipts still settle normally.
    // 发送关闭消息前撤销排队工作，已经提交的批次仍正常交付真实回执。
    for (const pending of this.pending.values()) Atomics.store(pending.cancelFlag, 0, 1);
    this.closing = this._request('close').then(async result => {
      this.closed = true;
      await this.worker?.terminate();
      this.worker = null;
      return result;
    }).catch(async error => {
      if (this.failed) {
        this.closed = true;
        await this.worker?.terminate();
        this.worker = null;
      }
      this.closing = null;
      throw error;
    });
    return this.closing;
  }
}

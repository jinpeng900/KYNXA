import { Worker } from 'node:worker_threads';
import { randomUUID } from 'node:crypto';
import { hashText, retrievalFailure, validateSource } from './retrieval-contracts.mjs';
import { CODE_PARSER_VERSION } from './code-structure.mjs';
import { MARKDOWN_PARSER_VERSION, TEXT_PARSER_VERSION } from './document-structure.mjs';
import { STRUCTURED_CHUNKER_VERSION, STRUCTURED_EMBEDDING_TEXT_VERSION } from './retrieval-text.mjs';

const MAX_PENDING_REQUESTS = 32;
const MAX_CACHE_ENTRIES = 64;
const MAX_CACHE_BYTES = 8 * 1024 * 1024;
const REQUEST_TIMEOUT_MS = 30000;
const MAX_WORKER_RESTARTS = 2;
const WORKER_RESTART_WINDOW_MS = 60000;

function abortedError() {
  return Object.assign(new Error('Source parsing cancelled. / 来源解析已取消。'), { name: 'AbortError', code: 'ABORT_ERR' });
}

/** One lazy worker owns parsers and bounded derived-text caching; no file or database access is granted.
 * 按需启动的单个 worker 拥有解析器与有界派生缓存，不获得文件或数据库访问权限。 */
export class RetrievalStructureService {
  // This identity invalidates lightweight preparation receipts without retaining full syntax trees.
  // 此身份使轻量准备回执随解析规则失效，无需缓存完整语法树或正文。
  get derivationVersion() {
    return ['worker-dispatch-v1', CODE_PARSER_VERSION, MARKDOWN_PARSER_VERSION, TEXT_PARSER_VERSION,
      STRUCTURED_CHUNKER_VERSION, STRUCTURED_EMBEDDING_TEXT_VERSION].join('|');
  }

  constructor({ workerFactory = (url, options) => new Worker(url, options), timeoutMs = REQUEST_TIMEOUT_MS, resourceService } = {}) {
    if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > REQUEST_TIMEOUT_MS)
      throw retrievalFailure('Invalid parser timeout. / 解析期限无效。');
    this.workerFactory = workerFactory;
    this.resources = resourceService;
    this.resourceTickets = new Map();
    this.queuedBytes = 0;
    this.timeoutMs = timeoutMs;
    this.worker = null;
    this.pending = new Map();
    this.inFlight = new Set();
    this.cache = new Map();
    this.cacheBytes = 0;
    this.sequence = 0;
    this.cacheHits = 0;
    this.closed = false;
    this.failed = null;
    this.retiring = null;
    this.recovery = null;
    this.restartTimes = [];
    this.workerRestarts = 0;
    this.retirementFailed = false;
  }

  status() {
    return { state: this.closed ? 'closed' : this.failed ? 'unavailable' : this.inFlight.size ? 'busy' : 'idle', workerStarted: Boolean(this.worker),
      local: true, network: false, languages: ['csharp', 'javascript', 'typescript', 'tsx', 'python', 'go', 'rust', 'markdown', 'text'],
      pendingRequests: this.pending.size, inFlightRequests: this.inFlight.size,
      cacheEntries: this.cache.size, cacheBytes: this.cacheBytes, cacheHits: this.cacheHits, queuedBytes: this.queuedBytes,
      workerRestarts: this.workerRestarts, recovering: Boolean(this.recovery),
      ...(this.failed ? { errorCode: this.failed.code } : {}) };
  }

  _finish(id, error, result) {
    const request = this.pending.get(id);
    if (!request) return;
    this.pending.delete(id);
    clearTimeout(request.timer);
    request.signal?.removeEventListener('abort', request.abort);
    if (error) request.reject(error); else request.resolve(result);
    if (!this.inFlight.size) this.worker?.unref();
  }

  _fail(error, worker = this.worker) {
    if (worker && this.worker !== worker) return;
    this.failed ??= error;
    for (const [id, request] of this.pending) {
      Atomics.store(request.cancelFlag, 0, 1);
      this._finish(id, error);
    }
    this.cache.clear(); this.cacheBytes = 0;
    this.inFlight.clear(); worker?.unref();
    this.worker = null;
    if (worker) {
      // Finish retirement before admitting replacement work; an old exit cannot poison a new worker.
      // 旧 worker 退出完成后才接纳替代任务，旧退出事件不能污染新 worker。
      this.retiring = Promise.resolve().then(() => worker.terminate()).catch(() => { this.retirementFailed = true; })
        .then(() => this.retirementFailed ? undefined : this._releaseResources())
        .finally(() => { this.retiring = null; });
    }
  }

  async _releaseTicket(id) {
    const ticket = this.resourceTickets.get(id);
    if (!ticket) return;
    this.resourceTickets.delete(id); this.queuedBytes -= ticket.bytes;
    if (!this.inFlight.size && this.executionLease) {
      const lease = this.executionLease; this.executionLease = null;
      await this.resources.release(lease.leaseId);
    }
  }

  async _releaseResources() {
    clearInterval(this.resourceRenewal);
    await Promise.all([...this.resourceTickets.keys()].map(id => this._releaseTicket(id)));
    const lease = this.residentLease; this.residentLease = null;
    if (lease) await this.resources.release(lease.leaseId);
    const execution = this.executionLease; this.executionLease = null;
    if (execution) await this.resources.release(execution.leaseId);
  }

  async _reserve(signal) {
    if (!this.resources || this.residentLease) return;
    if (!this.resourceLoading) this.resourceLoading = (async () => {
      const lease = await this.resources.acquire({ taskId: `parser:${randomUUID()}`, kind: 'background', cpuThreads: 0,
        memoryBytes: 192 * 1024 * 1024, waitMs: 10000, ttlMs: 30000 }, { signal });
      if (lease.status !== 'granted') throw retrievalFailure('Parser is waiting for resources. / 解析器等待可用资源。', 'STRUCTURE_RESOURCE_BUSY', 503);
      this.residentLease = lease;
      await this.resources.registerExecutor?.(lease.leaseId, { processId: process.pid });
      this.resourceRenewal = setInterval(() => Promise.allSettled([this.residentLease, this.executionLease].filter(Boolean)
        .map(lease => this.resources.renew(lease.leaseId, { ttlMs: 30000 }))), 10000);
      this.resourceRenewal.unref?.();
    })().finally(() => { this.resourceLoading = null; });
    await this.resourceLoading;
  }

  async _reserveCompute(signal) {
    if (!this.resources || this.executionLease) return;
    if (!this.executionLoading) this.executionLoading = (async () => {
      const lease = await this.resources.acquire({ taskId: `parse:${randomUUID()}`, kind: 'background', cpuThreads: 1,
        memoryBytes: 0, waitMs: 10000, ttlMs: 30000 }, { signal });
      if (lease.status !== 'granted') throw retrievalFailure('Parser compute is busy. / 解析计算资源忙。', 'STRUCTURE_RESOURCE_BUSY', 503);
      this.executionLease = lease;
    })().finally(() => { this.executionLoading = null; });
    await this.executionLoading;
  }

  async _releaseCompute() {
    const lease = this.executionLease; this.executionLease = null;
    if (lease) await this.resources.release(lease.leaseId);
  }

  async _recover() {
    if (!this.failed) return;
    if (this.recovery) return this.recovery;
    this.recovery = (async () => {
      this.restartTimes = this.restartTimes.filter(time => time > Date.now() - WORKER_RESTART_WINDOW_MS);
      if (this.restartTimes.length >= MAX_WORKER_RESTARTS) throw this.failed;
      await this.retiring;
      if (this.closed) throw abortedError();
      if (this.retirementFailed) throw this.failed;
      this.restartTimes.push(Date.now()); this.workerRestarts++;
      this.failed = null;
    })().finally(() => { this.recovery = null; });
    return this.recovery;
  }

  _start() {
    if (this.worker) return;
    let worker;
    try { worker = this.workerFactory(new URL('./structure-worker.mjs', import.meta.url), { execArgv: [],
      resourceLimits: { maxOldGenerationSizeMb: 128 } }); }
    catch {
      const error = retrievalFailure('Source parser worker could not start. / 来源解析 worker 无法启动。', 'STRUCTURE_WORKER_FAILED', 503);
      this._fail(error); throw error;
    }
    this.worker = worker;
    worker.on('message', message => {
      if (this.worker !== worker || this.closed || !message || typeof message !== 'object') return;
      if (!this.inFlight.delete(message.id)) return;
      this._releaseTicket(message.id).catch(() => {});
      if (message.error) this._finish(message.id, Object.assign(new Error(message.error.message), message.error));
      else this._finish(message.id, null, message.result);
      if (!this.inFlight.size) worker.unref();
    });
    worker.on('error', () => this._fail(retrievalFailure('Source parser worker failed. / 来源解析 worker 失败。', 'STRUCTURE_WORKER_FAILED', 503), worker));
    worker.on('exit', code => {
      if (!this.closed && this.worker === worker) this._fail(retrievalFailure(`Source parser worker exited (${code}). / 来源解析 worker 已退出。`, 'STRUCTURE_WORKER_EXITED', 503), worker);
    });
    worker.unref();
  }

  async parse(input, { signal, maxChars = 384 } = {}) {
    signal?.throwIfAborted();
    if (this.closed) throw retrievalFailure('Source parser is closed. / 来源解析服务已关闭。', 'STRUCTURE_SERVICE_CLOSED', 409);
    // Failed pure parsing is not replayed; only a fresh caller may request bounded recovery.
    // 失败的纯解析请求不自动重放，只由后续新调用触发有界恢复，用户取消不会重新派发原任务。
    if (this.failed) await this._recover();
    signal?.throwIfAborted();
    if (this.closed) throw abortedError();
    if (this.failed) throw this.failed;
    const source = validateSource(input);
    if (!Number.isSafeInteger(maxChars) || maxChars < 64 || maxChars > 8000) throw retrievalFailure('Invalid structure chunk size. / 结构分块大小无效。');
    const key = hashText(JSON.stringify([source.sourceId, source.contentHash, source.title, source.locator, source.sourceType, maxChars]));
    const cached = this.cache.get(key);
    if (cached) {
      this.cache.delete(key); this.cache.set(key, cached); this.cacheHits++;
      return structuredClone(cached.result);
    }
    // Cancelled callers remain admitted until the worker acknowledges them; cancellation cannot bypass capacity.
    // 调用方取消后仍占用已派发容量，直到 worker 确认结束，避免取消绕过队列上限。
    const bytesQueued = Buffer.byteLength(source.text) * 2;
    const queueBytes = this.residentLease?.memoryBytes ?? 64 * 1024 * 1024;
    if (this.inFlight.size >= (this.resources ? 128 : MAX_PENDING_REQUESTS) || this.queuedBytes + bytesQueued > queueBytes / 2)
      throw retrievalFailure('Source parser queue is full. / 来源解析队列已满。', 'STRUCTURE_QUEUE_FULL', 503);
    if (this.resources) await this._reserve(signal);
    if (this.closed) throw abortedError();
    if (this.resources) await this._reserveCompute(signal);
    if (signal?.aborted || this.closed) { if (!this.inFlight.size) await this._releaseCompute(); throw abortedError(); }
    // Coalesced resource admission yields; recheck capacity before dispatching concurrent callers.
    // 合并资源预约期间会让出执行，派发前再次检查容量，避免并发调用绕过队列预算。
    const admittedBytes = this.residentLease?.memoryBytes ?? 64 * 1024 * 1024;
    if (this.inFlight.size >= (this.resources ? 128 : MAX_PENDING_REQUESTS) || this.queuedBytes + bytesQueued > admittedBytes / 2)
      throw retrievalFailure('Source parser queue is full. / 来源解析队列已满。', 'STRUCTURE_QUEUE_FULL', 503);
    try { this._start(); }
    catch (error) { await this._releaseResources(); throw error; }
    const id = ++this.sequence;
    this.resourceTickets.set(id, { bytes: bytesQueued }); this.queuedBytes += bytesQueued;
    const cancelBuffer = new SharedArrayBuffer(Int32Array.BYTES_PER_ELEMENT), cancelFlag = new Int32Array(cancelBuffer);
    const result = await new Promise((resolve, reject) => {
      const abort = () => { Atomics.store(cancelFlag, 0, 1); this._finish(id, abortedError()); };
      const timer = setTimeout(() => {
        Atomics.store(cancelFlag, 0, 1);
        this._fail(retrievalFailure('Source parsing timed out. / 来源解析超时。', 'STRUCTURE_PARSE_TIMED_OUT', 503));
      }, this.timeoutMs);
      this.pending.set(id, { resolve, reject, signal, abort, cancelFlag, timer });
      signal?.addEventListener('abort', abort, { once: true });
      if (signal?.aborted) { abort(); this._releaseTicket(id).catch(() => {}); return; }
      this.worker.ref();
      this.inFlight.add(id);
      try { this.worker.postMessage({ id, source, maxChars, cancelBuffer }); }
      catch (error) { this.inFlight.delete(id); this._releaseTicket(id).catch(() => {}); this._finish(id, error); }
    });
    signal?.throwIfAborted();
    if (this.closed) throw abortedError();
    const bytes = Buffer.byteLength(JSON.stringify(result));
    const cacheBudget = this.residentLease ? Math.min(64 * 1024 * 1024, Math.floor(this.residentLease.memoryBytes / 8)) : MAX_CACHE_BYTES;
    if (bytes <= cacheBudget && result.structure.parseStatus !== 'unavailable') {
      const replaced = this.cache.get(key);
      if (replaced) this.cacheBytes -= replaced.bytes;
      this.cache.set(key, { result: structuredClone(result), bytes }); this.cacheBytes += bytes;
      while (this.cache.size > (this.resources ? 256 : MAX_CACHE_ENTRIES) || this.cacheBytes > cacheBudget) {
        const oldest = this.cache.keys().next().value;
        this.cacheBytes -= this.cache.get(oldest).bytes; this.cache.delete(oldest);
      }
    }
    return result;
  }

  close() {
    if (this.closure) return this.closure;
    this.closed = true;
    for (const [id, request] of this.pending) {
      Atomics.store(request.cancelFlag, 0, 1); this._finish(id, abortedError());
    }
    this.cache.clear(); this.cacheBytes = 0;
    this.inFlight.clear();
    // Parsing is pure WASM/text work, so terminating this owned worker cannot discard a database commit or native model receipt.
    // 此 worker 仅执行纯 WASM 与文本计算，终止它不会丢失数据库提交或原生推理回执。
    const worker = this.worker; this.worker = null;
    this.closure = Promise.all([this.retiring, this.recovery?.catch(() => {}), this.resourceLoading?.catch(() => {}),
      this.executionLoading?.catch(() => {}), worker?.terminate()])
      .then(() => this.retirementFailed ? undefined : this._releaseResources());
    return this.closure;
  }
}

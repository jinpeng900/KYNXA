import { createHash, randomUUID } from 'node:crypto';
import { closeSync, existsSync, lstatSync, mkdirSync, openSync, readFileSync, readSync, readdirSync, renameSync, unlinkSync, writeFileSync } from 'node:fs';
import { basename, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { fork } from 'node:child_process';
import { retrievalFailure } from './retrieval-contracts.mjs';
import { AnnBuildQueue } from './ann-build-queue.mjs';
import { runResourceTask } from '../../platform/resources/resource-task.mjs';

const ANN_PACKAGE_VERSION = '2.26.4';
const ANN_CACHE_VERSION = 1;
const MAX_MANIFEST_BYTES = 65536;
const NATIVE_RETRY_WINDOW_MS = 60000;
const ANN_RUNTIME_BYTES = 96 * 1024 * 1024;
const DOMAIN_SQL = `CASE WHEN c.structure_domain<>'' THEN c.structure_domain
  WHEN s.structure_json<>'' THEN json_extract(s.structure_json,'$.domain')
  WHEN s.source_type='code' THEN 'code' WHEN s.source_type IN ('document','memory','conversation','web') THEN 'knowledge' ELSE '' END`;

export { DOMAIN_SQL as RETRIEVAL_DOMAIN_SQL };
export const DEFAULT_ANN_OPTIONS = Object.freeze({ mode: 'auto', adaptive: true, threshold: 50000, maxCachedShards: 4,
  maxShardBytes: 256 * 1024 * 1024, connectivity: 16, expansionAdd: 128, expansionSearch: 128, exactScanLimit: 1000000 });

export function validateAnnOptions(input = {}) {
  if (!input || typeof input !== 'object' || Array.isArray(input))
    throw retrievalFailure('Invalid ANN options. / 本地向量索引配置无效。', 'INVALID_RETRIEVAL_ANN');
  const bounds = { threshold: [1, 1000000], maxCachedShards: [1, 16], maxShardBytes: [1024 * 1024, 1024 * 1024 * 1024],
    connectivity: [4, 64], expansionAdd: [16, 2048], expansionSearch: [16, 2048], exactScanLimit: [1, 1000000] };
  for (const [key, value] of Object.entries(input)) {
    if (key === 'adaptive') {
      if (typeof value !== 'boolean') throw retrievalFailure('Invalid adaptive ANN setting. / 自适应向量预算无效。', 'INVALID_RETRIEVAL_ANN');
    } else if (key === 'mode') {
      if (!['auto', 'off', 'ann', 'exact'].includes(value))
        throw retrievalFailure('Invalid ANN mode. / 本地向量索引模式无效。', 'INVALID_RETRIEVAL_ANN');
    } else if (!bounds[key] || !Number.isSafeInteger(value) || value < bounds[key][0] || value > bounds[key][1])
      throw retrievalFailure('Invalid ANN resource budget. / 本地向量索引资源预算无效。', 'INVALID_RETRIEVAL_ANN');
  }
  return { ...DEFAULT_ANN_OPTIONS, ...input };
}

/** Split only the current authorized descriptor, including its existing key range.
 * 仅拆分当前已授权目录及其已有键区间；缩额不能扩大范围、领域或向量空间。 */
export function partitionAnnDescriptor(database, vectorTable, descriptor, options) {
  if (!options.adaptive) return [descriptor];
  const perVectorBytes = descriptor.dimensions * 4 + options.connectivity * 16 + 256;
  const vectorsPerShard = Math.max(1, Math.floor(options.maxShardBytes / perVectorBytes));
  if (descriptor.count <= vectorsPerShard) return [descriptor];
  const segments = Math.ceil(descriptor.count / vectorsPerShard);
  return database.prepare(`WITH authorized AS MATERIALIZED (
    SELECT c.id,ntile(?) OVER (ORDER BY c.id) AS segment FROM ${vectorTable} c JOIN sources s ON s.source_id=c.source_id
    WHERE s.scope_key=? AND c.embedding_profile_id=? AND c.dimensions=? AND c.embedding_model_version=?
    AND c.embedding_space_id=? AND ${DOMAIN_SQL}=? AND c.vector IS NOT NULL AND c.id>=? AND c.id<=?)
    SELECT min(id) AS minimumId,max(id) AS maximumId,count(*) AS count FROM authorized GROUP BY segment ORDER BY segment`)
    .all(segments, descriptor.scope_key, descriptor.embedding_profile_id, descriptor.dimensions,
      descriptor.embedding_model_version, descriptor.embedding_space_id, descriptor.domain,
      descriptor.minimumId ?? 0, descriptor.maximumId ?? Number.MAX_SAFE_INTEGER)
    .map(part => ({ ...descriptor, ...part, partitioned: true }));
}

function checkStorage(path, directory = false) {
  if (!existsSync(path)) return;
  const info = lstatSync(path);
  if (info.isSymbolicLink() || (directory ? !info.isDirectory() : !info.isFile() || info.nlink > 1))
    throw retrievalFailure('Unsafe ANN cache path. / 向量缓存包含链接或异常结构。', 'UNSAFE_RETRIEVAL_PATH', 409);
}

function vectorFromBlob(value) {
  const bytes = new Uint8Array(value);
  return new Float32Array(bytes.buffer, bytes.byteOffset, bytes.byteLength / Float32Array.BYTES_PER_ELEMENT);
}

function fileHash(path) {
  const hash = createHash('sha256'), descriptor = openSync(path, 'r');
  const block = Buffer.allocUnsafe(65536);
  try {
    for (;;) {
      const bytes = readSync(descriptor, block, 0, block.length, null);
      if (!bytes) break;
      hash.update(block.subarray(0, bytes));
    }
    return hash.digest('hex');
  } finally { closeSync(descriptor); }
}

/** Scope, model space and domain select a graph before any approximate Top-K operation.
 * 近似 Top-K 之前按范围、模型空间和领域选择独立图；SQLite 保持正式向量与身份的权威。 */
export class LocalAnnStore {
  constructor({ directory, epoch, database, now = Date.now, resourceService, isolatedBuilder = false,
    processFactory = fork, requestTimeoutMs = 30000, vectorTable = 'chunks' }) {
    if (!['chunks', 'retrieval_vector_rows'].includes(vectorTable)) throw retrievalFailure('Invalid vector view. / 向量视图无效。');
    this.vectorTable = vectorTable;
    if (!Number.isSafeInteger(requestTimeoutMs) || requestTimeoutMs < 1 || requestTimeoutMs > 30000)
      throw retrievalFailure('Invalid ANN timeout. / 向量计算期限无效。', 'INVALID_RETRIEVAL_ANN');
    this.rootDirectory = directory;
    this.resources = resourceService;
    this.isolatedBuilder = isolatedBuilder;
    this.resourceLease = null;
    this.resourceTaskId = randomUUID();
    this.directory = join(directory, 'ann');
    this.epoch = epoch;
    this.database = database;
    this.shards = new Map();
    this.preparedOnDisk = new Map();
    this.child = null;
    this.processFactory = processFactory;
    this.requestTimeoutMs = requestTimeoutMs;
    this.retiring = null;
    this.retiringChild = null;
    this.closingChild = null;
    this.resourceContext = null;
    this.requests = new Map();
    this.sequence = 0;
    this.builds = new AnnBuildQueue({ now });
    this.ownerQueue = Promise.resolve();
    this.now = now;
    this.closed = false;
    this.helperMemory = { rssBytes: 0, observedPeakRssBytes: 0 };
    this.nativeError = null;
    this.nativeRetry = { attempts: 0, windowStartedAt: 0, retryAt: 0 };
    this.lastError = null;
    this.counters = { built: 0, loaded: 0, updated: 0, invalidated: 0, cacheMisses: 0, recycled: 0 };
  }

  _native() {
    if (this.child) return this.child;
    if (this.retiring) throw retrievalFailure('ANN helper is retiring. / 向量进程正在退出。', 'RETRIEVAL_ANN_RETIREMENT_PENDING');
    if (this.closed) throw retrievalFailure('ANN store is closed. / 向量索引已关闭。', 'RETRIEVAL_INDEX_CLOSED', 409);
    if (this.resources && !this.resourceLease) throw retrievalFailure('ANN memory has not been reserved. / 尚未取得向量进程内存预约。',
      'RETRIEVAL_ANN_RESOURCE_LIMIT');
    if (this.nativeError && this.now() < this.nativeRetry.retryAt) throw this.nativeError;
    checkStorage(this.directory, true);
    if (!existsSync(this.directory)) mkdirSync(this.directory, { mode: 0o700 });
    const child = this.processFactory(fileURLToPath(new URL('./ann-worker.mjs', import.meta.url)), [], {
      cwd: this.directory, windowsHide: true, execArgv: [], serialization: 'advanced', stdio: ['ignore', 'ignore', 'ignore', 'ipc']
    });
    this.child = child;
    if (this.resourceLease && child.pid) this.resources.registerExecutor?.(this.resourceLease.leaseId,
      { processId: child.pid }).catch(() => {});
    child.on('message', message => {
      if (this.child !== child) return;
      if (Number.isSafeInteger(message.memory?.rssBytes) && Number.isSafeInteger(message.memory?.observedPeakRssBytes))
        this.helperMemory = { rssBytes: message.memory.rssBytes,
          observedPeakRssBytes: Math.max(this.helperMemory.observedPeakRssBytes, message.memory.observedPeakRssBytes) };
      const request = this.requests.get(message.id);
      if (!request) return;
      this.requests.delete(message.id); clearTimeout(request.timeout);
      if (message.error) request.reject(Object.assign(new Error(message.error.message), { code: message.error.code }));
      else { this.nativeError = null; request.resolve(message.result); }
      if (!this.requests.size) { child.unref(); child.channel?.unref(); }
    });
    child.on('error', error => this._failNative(child, error));
    child.on('exit', code => {
      if (this.child !== child) return;
      if (this.closingChild === child) this._retireChild(child).catch(() => {});
      else this._failNative(child, retrievalFailure(`ANN helper exited (${code}). / 向量计算进程已退出。`, 'RETRIEVAL_ANN_WORKER_EXITED'));
    });
    child.unref(); child.channel?.unref();
    return child;
  }

  _failNative(child, error) {
    if (this.child !== child) return;
    this.nativeError = Object.assign(error, { code: error.code ?? 'RETRIEVAL_ANN_UNAVAILABLE' });
    const now = this.now();
    if (!this.nativeRetry.attempts || now - this.nativeRetry.windowStartedAt >= NATIVE_RETRY_WINDOW_MS)
      this.nativeRetry = { attempts: 0, windowStartedAt: now, retryAt: 0 };
    this.nativeRetry.attempts++;
    this.nativeRetry.retryAt = this.nativeRetry.attempts >= 3 ? this.nativeRetry.windowStartedAt + NATIVE_RETRY_WINDOW_MS
      : now + 1000 * 2 ** (this.nativeRetry.attempts - 1);
    this.shards.clear();
    this._retireChild(child, this.nativeError, true).catch(() => {});
  }

  _retireChild(child, error, kill = false) {
    if (this.retiringChild === child) return this.retiring;
    const reservation = this._takeReservation();
    const requests = [...this.requests.values()];
    for (const request of requests) clearTimeout(request.timeout);
    this.requests.clear();
    this.child = null;
    this.retiringChild = child;
    if (this.closingChild === child) this.closingChild = null;
    // Capture ownership before waiting: a late exit can release only this child's reservation.
    // 等待前快照所有权，晚到的退出只能释放此旧进程的预约，不能读取替代进程的 lease。
    const exited = new Promise(resolveExit => {
      if (!child.pid || child.exitCode !== null || child.signalCode !== null) { resolveExit(); return; }
      const timeout = setTimeout(() => child.kill(), 1000);
      timeout.unref?.();
      child.once('exit', () => { clearTimeout(timeout); resolveExit(); });
      if (kill) child.kill();
    });
    const retirement = exited.then(async () => {
      this.helperMemory.rssBytes = 0;
      await this._releaseReservation(reservation);
    }).finally(() => {
      for (const request of requests) request.reject(error ?? retrievalFailure(
        'ANN helper was closed. / 向量进程已关闭。', 'RETRIEVAL_ANN_WORKER_EXITED'));
      if (this.retiring === retirement) { this.retiring = null; this.retiringChild = null; }
    });
    this.retiring = retirement;
    return retirement;
  }

  async _waitForRetirement() {
    if (!this.retiring) return;
    let timeout;
    try {
      await Promise.race([this.retiring, new Promise((_, reject) => {
        timeout = setTimeout(() => reject(retrievalFailure('ANN helper has not exited yet. / 向量旧进程尚未退出。',
          'RETRIEVAL_ANN_RETIREMENT_PENDING')), 5000);
        timeout.unref?.();
      })]);
    } finally { clearTimeout(timeout); }
  }

  _withOwner(operation) {
    // Serialize short lifecycle sections, while background builds yield between bounded batches.
    // 仅串行化短生命周期片段，后台建图在有界批次之间释放所有者，前台不会等待整个大图。
    const result = this.ownerQueue.catch(() => {}).then(operation);
    this.ownerQueue = result.catch(() => {});
    return result;
  }

  async planningOptions(options, descriptors, kind, signal) {
    if (!this.resources || !options.adaptive || !descriptors.length) return options;
    const largest = descriptors.reduce((previous, current) =>
      current.count * (current.dimensions * 4 + options.connectivity * 16 + 256) >
      previous.count * (previous.dimensions * 4 + options.connectivity * 16 + 256) ? current : previous);
    const perVectorBytes = largest.dimensions * 4 + options.connectivity * 16 + 256;
    const descriptor = { ...largest, count: Math.min(largest.count, Math.max(1, Math.floor(options.maxShardBytes / perVectorBytes))) };
    const planned = await this.resourceOptions(options, descriptor, kind, signal, { planning: true });
    const maxShardBytes = this.buildShardLimit && this.now() < this.buildShardLimit.expiresAt
      ? Math.min(planned.maxShardBytes, this.buildShardLimit.bytes) : planned.maxShardBytes;
    this.adaptiveOptions = { ...planned, maxShardBytes };
    return this.adaptiveOptions;
  }

  async resourceOptions(options, descriptor, kind, signal, { planning = false } = {}) {
    if (!this.resources) return options;
    await this._waitForRetirement();
    if (this.nativeError && this.now() < this.nativeRetry.retryAt) throw this.nativeError;
    this.resourceContext = { options, descriptor: { ...descriptor }, kind };
    const incomingBytes = descriptor.count * (descriptor.dimensions * 4 + options.connectivity * 16 + 256);
    if (!planning && this.resourceLease && incomingBytes + ANN_RUNTIME_BYTES > this.resourceLease.memoryBytes) {
      await this._withOwner(async () => {
        for (const entry of this.shards.values()) await this._persist(entry);
        this.shards.clear(); await this._releaseHelper(); await this._releaseReservation();
      });
    }
    if (!this.resourceLease) {
      const snapshot = await this.resources.snapshot({ signal });
      const available = snapshot.hardware?.memory?.availableBytes ?? snapshot.memory?.availableBytes ??
        snapshot.budget?.memoryBytes ?? 512 * 1024 * 1024;
      const estimate = descriptor.count * (descriptor.dimensions * 4 + options.connectivity * 16 + 256);
      const capacity = snapshot.accounting?.availableMemoryBytes ?? snapshot.budget?.memoryBytes ?? available;
      const target = this.isolatedBuilder ? estimate + ANN_RUNTIME_BYTES : Math.max(estimate * 2, available / 8);
      const memoryBytes = Math.floor(Math.min(2 * 1024 ** 3, available / 3, capacity / (this.isolatedBuilder ? 1 : 2),
        Math.max(128 * 1024 * 1024, target)));
      this.resourceAdmissionAudit = { requestedMemoryBytes: memoryBytes, availableCapacityBytes: capacity,
        memoryAdmission: 'full-request-or-denial', configured: { maxShardBytes: options.maxShardBytes,
          maxCachedShards: options.maxCachedShards }, reason: 'awaiting-resource-admission' };
      if (memoryBytes < ANN_RUNTIME_BYTES + 1024 * 1024)
        throw retrievalFailure('ANN runtime headroom is unavailable. / 向量运行时余量不足。', 'RETRIEVAL_ANN_RESOURCE_LIMIT');
      const lease = await this.resources.acquire({ taskId: `ann:${randomUUID()}`, workspaceId: descriptor.scope_key,
        kind, cpuThreads: 0, memoryBytes, ttlMs: 30000, waitMs: 10000 }, { signal });
      if (lease.status !== 'granted') throw retrievalFailure('ANN is waiting for resource capacity. / 向量图等待可用资源。',
        'RETRIEVAL_ANN_RESOURCE_LIMIT');
      this.resourceLease = lease;
      this.resourceRenewal = setInterval(() => this.resources.renew(lease.leaseId, { ttlMs: 30000 }).catch(() => {}), 10000);
      this.resourceRenewal.unref?.();
    }
    if (!options.adaptive) return options;
    const cacheBytes = Math.max(1024 * 1024, this.resourceLease.memoryBytes - ANN_RUNTIME_BYTES);
    const shardBytes = Math.max(1024 * 1024, Math.min(1024 ** 3, cacheBytes / (this.isolatedBuilder ? 1 : 2)));
    const maxShardBytes = options.maxShardBytes === DEFAULT_ANN_OPTIONS.maxShardBytes ? Math.floor(shardBytes) :
      Math.min(options.maxShardBytes, Math.floor(cacheBytes));
    const estimatedShardBytes = Math.max(1024 * 1024, descriptor.count *
      (descriptor.dimensions * 4 + options.connectivity * 16 + 256));
    const maxCachedShards = options.maxCachedShards === DEFAULT_ANN_OPTIONS.maxCachedShards
      ? Math.max(1, Math.min(16, Math.floor(cacheBytes / estimatedShardBytes))) :
        Math.min(options.maxCachedShards, Math.max(1, Math.floor(cacheBytes / maxShardBytes)));
    this.adaptiveOptions = { ...options, maxShardBytes, maxCachedShards };
    this.resourceAdmissionAudit = { ...this.resourceAdmissionAudit, approvedMemoryBytes: this.resourceLease.memoryBytes,
      cacheBytes, approved: { maxShardBytes, maxCachedShards }, reason: 'approved-resident-capacity' };
    return this.adaptiveOptions;
  }

  _takeReservation() {
    const reservation = { lease: this.resourceLease, renewal: this.resourceRenewal };
    this.resourceLease = null; this.resourceRenewal = null;
    return reservation;
  }

  async _releaseReservation(reservation = this._takeReservation()) {
    clearInterval(reservation.renewal);
    const lease = reservation.lease;
    if (lease) await this.resources.release(lease.leaseId);
  }

  async _request(method, input) {
    await this._waitForRetirement();
    if (this.resources && !this.child && !this.resourceLease && this.resourceContext) {
      const { options, descriptor, kind } = this.resourceContext;
      await this.resourceOptions(options, descriptor, kind);
    }
    if (this.resources && ['add', 'search', 'load', 'save'].includes(method))
      return runResourceTask(this.resources, { taskId: `ann:${this.resourceTaskId}:${method}`, kind: this.isolatedBuilder ? 'background' : 'foreground',
        cpuThreads: this.isolatedBuilder ? 32 : 4, memoryBytes: 0, waitMs: 10000 },
        async lease => {
          this.parentBuildQueue?.configure(lease?.suggestions?.annBuildConcurrency ?? lease?.cpuThreads ?? 1);
          const started = performance.now();
          const result = await this._sendRequest(method, { ...input, threads: lease?.cpuThreads ?? 1 });
          if (lease && method === 'add') await this.resources.report?.(lease.leaseId, {
            throughputPerSecond: input.keys.length * 1000 / Math.max(1, performance.now() - started), queueDepth: this.requests.size }).catch(() => {});
          return result;
        });
    return this._sendRequest(method, input);
  }

  _sendRequest(method, input) {
    const child = this._native(), id = ++this.sequence;
    if (this.requests.size >= 32) return Promise.reject(retrievalFailure('ANN helper queue is full. / 向量计算队列已满。', 'RETRIEVAL_ANN_QUEUE_FULL'));
    return new Promise((resolveResult, reject) => {
      const timeout = setTimeout(() => {
        const error = retrievalFailure('ANN computation timed out. / 向量计算超时。', 'RETRIEVAL_ANN_TIMEOUT');
        this._failNative(child, error);
      }, this.requestTimeoutMs);
      timeout.unref();
      this.requests.set(id, { resolve: resolveResult, reject, timeout });
      child.ref(); child.channel?.ref();
      child.send({ id, method, input }, error => {
        if (!error) return;
        const request = this.requests.get(id);
        if (!request) return;
        this._failNative(child, error);
      });
    });
  }

  _identity(descriptor, options) {
    return { schemaVersion: ANN_CACHE_VERSION, packageVersion: ANN_PACKAGE_VERSION, epoch: this.epoch,
      scopeKey: descriptor.scope_key, profileId: descriptor.embedding_profile_id, dimensions: descriptor.dimensions,
      modelVersion: descriptor.embedding_model_version, spaceId: descriptor.embedding_space_id, domain: descriptor.domain,
      ...(descriptor.partitioned ? { minimumId: descriptor.minimumId, maximumId: descriptor.maximumId } : {}),
      metric: 'cos', quantization: 'f32', connectivity: options.connectivity,
      expansionAdd: options.expansionAdd, expansionSearch: options.expansionSearch };
  }

  _paths(key) { return { manifest: join(this.directory, `${key}.json`), graph: join(this.directory, `${key}.usearch`) }; }

  async _persist(entry) {
    if (!entry.dirty) return;
    checkStorage(this.directory, true);
    if (!existsSync(this.directory)) mkdirSync(this.directory, { mode: 0o700 });
    const paths = this._paths(entry.key), temporary = `${paths.graph}.${randomUUID()}.tmp`;
    const temporaryManifest = `${paths.manifest}.${randomUUID()}.tmp`;
    const generation = entry.generation, count = entry.count;
    try {
      checkStorage(paths.graph); checkStorage(paths.manifest);
      await this._request('save', { key: entry.key, filename: basename(temporary) });
      if (entry.owner !== this.child || entry.generation !== generation || entry.count !== count || this.database.prepare(
        'SELECT generation FROM scope_snapshots WHERE scope_key=?').get(entry.identity.scopeKey)?.generation !== generation)
        throw retrievalFailure('ANN cache changed during save. / 向量缓存在保存期间已变化。', 'RETRIEVAL_ANN_STALE_BUILD');
      checkStorage(temporary);
      if (lstatSync(temporary).size > entry.options.maxShardBytes)
        throw retrievalFailure('ANN cache exceeds its disk budget. / 向量缓存超过单分片磁盘预算。', 'RETRIEVAL_ANN_RESOURCE_LIMIT');
      const hash = fileHash(temporary);
      const metadata = { identity: entry.identity, generation, count, graphHash: hash };
      writeFileSync(temporaryManifest, JSON.stringify(metadata), { flag: 'wx', mode: 0o600 });
      checkStorage(paths.graph); checkStorage(paths.manifest);
      renameSync(temporary, paths.graph);
      // Manifest publication follows the graph. A crash between files causes a verified rebuild.
      // 图文件先发布，再发布校验清单；两者之间崩溃会触发核验重建，不使用半份缓存。
      renameSync(temporaryManifest, paths.manifest);
      entry.dirty = false;
      this._prune(entry.options);
    } finally {
      if (existsSync(temporary)) unlinkSync(temporary);
      if (existsSync(temporaryManifest)) unlinkSync(temporaryManifest);
    }
  }

  _prune(options) {
    // Only cache pairs created in this owned directory are eligible for removal.
    // 仅清理由本模块在受管目录中创建的缓存对，不遍历或删除来源文件。
    const files = readdirSync(this.directory).filter(name => /^[a-f0-9]{64}\.usearch$/u.test(name));
    const entries = files.map(name => {
      const key = name.slice(0, -8), paths = this._paths(key);
      checkStorage(paths.graph); checkStorage(paths.manifest);
      const info = lstatSync(paths.graph);
      return { key, paths, size: info.size, modifiedAt: info.mtimeMs };
    }).sort((a, b) => b.modifiedAt - a.modifiedAt);
    let retainedBytes = 0, retainedCount = 0;
    for (const entry of entries) {
      if (entry.size <= options.maxShardBytes && (this.shards.has(entry.key) || retainedCount < options.maxCachedShards * 4 &&
          retainedBytes + entry.size <= options.maxCachedShards * options.maxShardBytes)) {
        retainedBytes += entry.size; retainedCount++; continue;
      }
      checkStorage(entry.paths.graph); checkStorage(entry.paths.manifest);
      if (existsSync(entry.paths.manifest)) unlinkSync(entry.paths.manifest);
      unlinkSync(entry.paths.graph);
    }
  }

  async enforceBudget(options) {
    if (options.adaptive && this.adaptiveOptions) options = { ...options,
      maxShardBytes: options.maxShardBytes === DEFAULT_ANN_OPTIONS.maxShardBytes ? this.adaptiveOptions.maxShardBytes :
        Math.min(options.maxShardBytes, this.adaptiveOptions.maxShardBytes),
      maxCachedShards: options.maxCachedShards === DEFAULT_ANN_OPTIONS.maxCachedShards ? this.adaptiveOptions.maxCachedShards :
        Math.min(options.maxCachedShards, this.adaptiveOptions.maxCachedShards) };
    const entries = [...this.shards.values()];
    const fits = entry => entry.count * (entry.identity.dimensions * 4 + entry.identity.connectivity * 16 + 256) <= options.maxShardBytes;
    const incompatibleBuild = [...this.builds.jobs.values()].some(job => (job.descriptor.buildLargestShardBytes ?? job.descriptor.count *
      (job.descriptor.dimensions * 4 + (job.descriptor.buildConnectivity ?? options.connectivity) * 16 + 256)) > options.maxShardBytes ||
      job.descriptor.buildMaxShardBytes > options.maxShardBytes || job.descriptor.buildMaxCachedShards > options.maxCachedShards);
    const unusedOwner = (options.mode === 'off' || options.mode === 'exact') && (entries.length || this.builds.jobs.size || this.child);
    if (!unusedOwner && !incompatibleBuild && entries.length <= options.maxCachedShards && entries.every(fits)) return;
    // Drain before acquiring the owner: the cancelled batch may itself be waiting for this lock.
    // 先等待被取消批次结束，再获取所有者锁；批次本身可能正在等待此锁，反序会死锁。
    await this.builds.drain();
    return this._withOwner(() => this._enforceBudget(options));
  }

  async _enforceBudget(options) {
    const fits = entry => entry.count * (entry.identity.dimensions * 4 + entry.identity.connectivity * 16 + 256) <= options.maxShardBytes;
    // Retain compliant disk graphs, then release the owned process; a dropped native reference alone does not bound RSS.
    // 保留合预算的磁盘图后关闭已拥有的进程；只撤销原生引用不能保证释放进程内存。
    for (const entry of [...this.shards.values()].filter(fits).reverse().slice(0, options.maxCachedShards)) {
      entry.options = options;
      try { await this._persist(entry); }
      catch (error) { this.lastError = error.code ?? 'RETRIEVAL_ANN_CACHE_WRITE_FAILED'; }
    }
    this.counters.invalidated += this.shards.size;
    this.shards.clear();
    await this._releaseHelper();
    if (existsSync(this.directory)) {
      try { this._prune(options); }
      catch (error) { this.lastError = error.code ?? 'RETRIEVAL_ANN_CACHE_PRUNE_FAILED'; }
    }
  }

  async _load(entry, count, options) {
    const paths = this._paths(entry.key);
    checkStorage(this.directory, true); checkStorage(paths.manifest); checkStorage(paths.graph);
    if (!existsSync(paths.manifest) || !existsSync(paths.graph)) return false;
    if (lstatSync(paths.manifest).size > MAX_MANIFEST_BYTES || lstatSync(paths.graph).size > options.maxShardBytes) return false;
    let attemptedLoad = false, loadedSuccessfully = false;
    try {
      const metadata = JSON.parse(readFileSync(paths.manifest, 'utf8'));
      if (JSON.stringify(metadata.identity) !== JSON.stringify(entry.identity) || metadata.generation !== entry.generation || metadata.count !== count)
        return false;
      const hash = fileHash(paths.graph);
      if (metadata.graphHash !== hash) return false;
      attemptedLoad = true;
      const loaded = await this._request('load', { key: entry.key, filename: basename(paths.graph) });
      if (loaded.count !== count || loaded.dimensions !== entry.identity.dimensions) return false;
      loadedSuccessfully = true;
      this.counters.loaded++;
      return true;
    } catch { return false; }
    finally {
      // A mismatched native graph may retain large allocations even after its JS reference is replaced.
      // 原生图的数量或维度不匹配时，即使替换 JS 引用也可能保留大量内存，须退出实际所有者。
      if (attemptedLoad && !loadedSuccessfully) await this._discardEntry(entry);
    }
  }

  async _evict(options, incoming = true) {
    if (this.shards.size + Number(incoming) <= options.maxCachedShards) return;
    // Native finalizers do not give deterministic release; retain disk graphs and retire the actual owner.
    // 原生终结器不能保证确定释放；保留磁盘图并退出实际拥有内存的进程。
    for (const entry of this.shards.values()) {
      try { await this._persist(entry); } catch (error) { this.lastError = error.code ?? 'RETRIEVAL_ANN_CACHE_WRITE_FAILED'; }
    }
    this.shards.clear();
    this._cancelOwnedBuild();
    await this._releaseHelper();
    this.counters.recycled++;
  }

  _entry(descriptor, options) {
    const identity = this._identity(descriptor, options);
    const key = createHash('sha256').update(JSON.stringify(identity)).digest('hex');
    return { key, identity, descriptor: { ...descriptor }, options, generation: descriptor.generation, count: descriptor.count, dirty: false };
  }

  _assertBuildCurrent(entry, signal) {
    signal?.throwIfAborted();
    if (entry.owner && entry.owner !== this.child && this.nativeError) throw this.nativeError;
    if (this.closed || entry.owner && entry.owner !== this.child || this.database.prepare('SELECT generation FROM scope_snapshots WHERE scope_key=?')
      .get(entry.identity.scopeKey)?.generation !== entry.generation) {
      if (signal) throw Object.assign(new Error('ANN build superseded. / 向量建图版本已被替换。'), { name: 'AbortError', code: 'ABORT_ERR' });
      throw retrievalFailure('ANN graph was replaced. / 向量图已被替换。', 'RETRIEVAL_ANN_STALE_BUILD', 409);
    }
  }

  async _prepareEntry(entry, checkCancelled, job) {
    const { descriptor, key } = entry;
    const create = () => this._request('create', { key, dimensions: descriptor.dimensions,
      connectivity: entry.options.connectivity, expansionAdd: entry.options.expansionAdd, expansionSearch: entry.options.expansionSearch });
    try {
      const loaded = await this._withOwner(async () => {
        checkCancelled(); this._assertBuildCurrent(entry, job?.controller.signal);
        const previous = this.shards.get(key);
        if (previous) await this._discardEntry(previous, true);
        await this._evict(entry.options);
        await create();
        entry.owner = this.child;
        if (job) job.owner = entry.owner;
        const loaded = await this._load(entry, descriptor.count, entry.options);
        if (!loaded) {
          await create(); entry.owner = this.child;
          if (job) job.owner = entry.owner;
        }
        checkCancelled(); this._assertBuildCurrent(entry, job?.controller.signal);
        return loaded;
      });
      if (!loaded) {
        // Keyset pages leave no SQLite cursor or transaction open across native computation.
        // 键集分页保证等待原生计算期间不持有 SQLite 游标或事务，前台读写可继续执行。
        const rows = this.database.prepare(`SELECT c.id,c.vector FROM ${this.vectorTable} c JOIN sources s ON s.source_id=c.source_id
          WHERE s.scope_key=? AND c.embedding_profile_id=? AND c.dimensions=? AND c.embedding_model_version=?
          AND c.embedding_space_id=? AND ${DOMAIN_SQL}=? AND c.vector IS NOT NULL AND c.id>? AND c.id>=? AND c.id<=?
          ORDER BY c.id LIMIT 256`);
        let lastId = 0, completedVectors = 0;
        for (;;) {
          const added = await this._withOwner(async () => {
            checkCancelled(); this._assertBuildCurrent(entry, job?.controller.signal);
            const batch = rows.all(descriptor.scope_key, descriptor.embedding_profile_id, descriptor.dimensions,
              descriptor.embedding_model_version, descriptor.embedding_space_id, descriptor.domain, lastId,
              descriptor.minimumId ?? 0, descriptor.maximumId ?? Number.MAX_SAFE_INTEGER);
            if (!batch.length) return 0;
            await this._request('add', { key, keys: BigUint64Array.from(batch, row => BigInt(row.id)),
              vectors: batch.map(row => vectorFromBlob(row.vector)) });
            await this._checkResidentBudget(entry.options);
            checkCancelled(); this._assertBuildCurrent(entry, job?.controller.signal);
            lastId = batch.at(-1).id;
            completedVectors += batch.length;
            if (job) job.completedVectors = (job.completedBeforeShard ?? 0) + completedVectors;
            return batch.length;
          });
          if (!added) break;
          // Yield between bounded batches; a full cold graph must not monopolize the index owner.
          // 在有界批次间让出执行权，冷图构建不能独占索引所有者。
          await new Promise(resolveTurn => setImmediate(resolveTurn));
        }
        this._assertBuildCurrent(entry, job?.controller.signal);
        if (completedVectors !== descriptor.count) throw retrievalFailure('ANN build changed during publication. / 向量建图发布时数量已变化。', 'RETRIEVAL_ANN_STALE_BUILD');
        entry.dirty = true;
      }
      await this._withOwner(async () => {
        checkCancelled(); this._assertBuildCurrent(entry, job?.controller.signal);
        await this._checkResidentBudget(entry.options);
        this.shards.set(key, entry);
        try { await this._persist(entry); }
        catch (error) {
          this.lastError = error.code ?? 'RETRIEVAL_ANN_CACHE_WRITE_FAILED';
          // An isolated owner is retired after the job; without its disk receipt there is no reusable graph.
          // 独立建图所有者作业后退役；缺少磁盘发布回执就没有可复用图，不能记为准备成功。
          if (this.isolatedBuilder) throw error;
        }
        checkCancelled(); this._assertBuildCurrent(entry, job?.controller.signal);
        if (!loaded) this.counters.built++;
      });
    } catch (error) {
      await this._withOwner(() => this._discardEntry(entry, !job));
      throw error;
    }
  }

  _cancelOwnedBuild() {
    for (const job of this.builds.active.values()) if (job.owner === this.child) job.controller.abort();
  }

  async _discardEntry(entry, cancelBuild = false) {
    if (this.shards.get(entry.key) === entry) this.shards.delete(entry.key);
    if (!entry.owner || this.child !== entry.owner) return;
    // Retiring the actual helper releases partial native graphs even when other shards are cached.
    // 退出实际计算进程，确保部分原生图在存在其他缓存分片时也释放，不依赖延迟 GC。
    for (const retained of this.shards.values()) {
      try { await this._persist(retained); }
      catch (error) { this.lastError = error.code ?? 'RETRIEVAL_ANN_CACHE_WRITE_FAILED'; }
    }
    this.shards.clear();
    if (cancelBuild) this._cancelOwnedBuild();
    await this._releaseHelper();
    this.counters.recycled++;
  }

  async _checkResidentBudget(options) {
    const rssBudget = this.resourceLease?.memoryBytes ?? 128 * 1024 * 1024 + options.maxCachedShards * options.maxShardBytes;
    if (this.helperMemory.rssBytes <= rssBudget) return;
    this.shards.clear();
    await this._releaseHelper();
    this.counters.recycled++;
    throw retrievalFailure('ANN helper exceeded its observed RSS budget. / 向量进程超过实际观察内存预算。', 'RETRIEVAL_ANN_RSS_LIMIT');
  }

  warm(descriptor, options) {
    if (this.closed || options.mode === 'off' || options.mode === 'exact' ||
        descriptor.count <= Math.max(1024, options.threshold)) return;
    const estimatedBytes = descriptor.count * (descriptor.dimensions * 4 + options.connectivity * 16 + 256);
    if (estimatedBytes > options.maxShardBytes && !this.resources) return;
    const entry = this._entry(descriptor, options);
    if (this.preparedOnDisk.get(entry.key) === entry.generation) return;
    if (this.shards.get(entry.key)?.generation === entry.generation) return;
    this._enqueue(entry);
  }

  _enqueue(entry) {
    if (this.nativeError && this.now() < this.nativeRetry.retryAt) throw this.nativeError;
    const coveringBuild = [...this.builds.jobs.values()].some(job => !job.controller.signal.aborted &&
      job.descriptor.buildConnectivity === entry.options.connectivity &&
      job.descriptor.buildExpansionAdd === entry.options.expansionAdd && job.descriptor.buildExpansionSearch === entry.options.expansionSearch &&
      ['scope_key', 'embedding_profile_id', 'dimensions', 'embedding_model_version', 'embedding_space_id', 'domain', 'generation']
        .every(field => job.descriptor[field] === entry.descriptor[field]) &&
      (job.descriptor.minimumId ?? 0) <= (entry.descriptor.minimumId ?? 0) &&
      (job.descriptor.maximumId ?? Number.MAX_SAFE_INTEGER) >= (entry.descriptor.maximumId ?? Number.MAX_SAFE_INTEGER));
    if (coveringBuild) return;
    const descriptor = { ...entry.descriptor, buildBudgetKey: JSON.stringify([entry.options.maxShardBytes, entry.options.maxCachedShards]),
      buildMaxShardBytes: entry.options.maxShardBytes, buildMaxCachedShards: entry.options.maxCachedShards,
      buildConnectivity: entry.options.connectivity, buildExpansionAdd: entry.options.expansionAdd,
      buildExpansionSearch: entry.options.expansionSearch };
    const accepted = this.builds.enqueue(entry.key, descriptor, async job => {
      if (!this.resources) return this._prepareEntry(entry, () => this._assertBuildCurrent(entry, job.controller.signal), job);
      // Independent native owners permit real bounded parallel builds; SQLite remains on its single thread.
      // 独立原生所有者支持真正有界并行建图；SQLite 仍由单线程拥有，不并行提交事务。
      const builder = new LocalAnnStore({ directory: this.rootDirectory, epoch: this.epoch, database: this.database,
        now: this.now, resourceService: this.resources, isolatedBuilder: true, vectorTable: this.vectorTable });
      builder.parentBuildQueue = this.builds;
      try {
        const pending = [entry.descriptor];
        let completedVectors = 0;
        while (pending.length) {
          job.controller.signal.throwIfAborted();
          const descriptor = pending.shift();
          const options = await builder.resourceOptions(entry.options, descriptor, 'background', job.controller.signal);
          const parts = partitionAnnDescriptor(this.database, this.vectorTable, descriptor, options);
          if (parts.length > 1) {
            // Feed the admitted child budget back to foreground planning, then build smaller authorized ranges.
            // 将子作业批准额度反馈给前台规划，再按较小的已授权键区间建图，不能重试原超额目标。
            this.buildShardLimit = { bytes: options.maxShardBytes, expiresAt: this.now() + 30000 };
            this.adaptiveOptions = { ...options };
            job.descriptor.buildMaxShardBytes = options.maxShardBytes;
            job.descriptor.buildMaxCachedShards = options.maxCachedShards;
            job.descriptor.buildLargestShardBytes = Math.max(...parts.map(part => part.count *
              (part.dimensions * 4 + options.connectivity * 16 + 256)));
            pending.unshift(...parts);
            continue;
          }
          const isolatedEntry = builder._entry(descriptor, options);
          job.completedBeforeShard = completedVectors;
          await builder._prepareEntry(isolatedEntry, () => builder._assertBuildCurrent(isolatedEntry, job.controller.signal), job);
          job.controller.signal.throwIfAborted();
          builder._assertBuildCurrent(isolatedEntry, job.controller.signal);
          completedVectors += descriptor.count;
          this.preparedOnDisk.set(isolatedEntry.key, isolatedEntry.generation);
          while (this.preparedOnDisk.size > 128) this.preparedOnDisk.delete(this.preparedOnDisk.keys().next().value);
        }
        this.counters.built += builder.counters.built;
        this.backgroundObservedPeakRssBytes = Math.max(this.backgroundObservedPeakRssBytes ?? 0, builder.helperMemory.observedPeakRssBytes);
      } finally { await builder.close(); }
    });
    if (!accepted) {
      const failure = this.builds.failure(entry.key, descriptor);
      throw retrievalFailure('ANN build is waiting before retry. / 向量建图失败后正在等待重试。', failure?.code ?? 'RETRIEVAL_ANN_BUILD_PENDING');
    }
  }

  async search(descriptor, query, limit, options, checkCancelled) {
    options = await this.resourceOptions(options, descriptor, 'foreground');
    await this.enforceBudget(options);
    const estimatedBytes = descriptor.count * (descriptor.dimensions * 4 + options.connectivity * 16 + 256);
    if (estimatedBytes > options.maxShardBytes)
      throw retrievalFailure('ANN shard exceeds its memory budget. / 向量分片超过内存预算。', 'RETRIEVAL_ANN_RESOURCE_LIMIT');
    const requested = this._entry(descriptor, options), key = requested.key;
    let entry = this.shards.get(key);
    if (entry && entry.generation !== descriptor.generation) {
      await this.builds.drain();
      await this._withOwner(async () => {
        this.shards.delete(key);
        await this._evict({ ...options, maxCachedShards: 0 }, false);
        if (this.child) await this._releaseHelper();
      });
      entry = null;
      this.counters.invalidated++;
    }
    if (!entry) {
      this.counters.cacheMisses++;
      if ((descriptor.count > Math.max(1024, options.threshold) || this.builds.running) &&
          this.preparedOnDisk.get(key) !== descriptor.generation) {
        this._enqueue(requested);
        throw retrievalFailure('ANN graph is being prepared in the background. / 向量图正在后台准备。', 'RETRIEVAL_ANN_BUILD_PENDING');
      }
      entry = requested;
      await this._prepareEntry(entry, checkCancelled);
    }
    return this._withOwner(async () => {
      checkCancelled(); this._assertBuildCurrent(entry);
      if (this.shards.get(key) !== entry)
        throw retrievalFailure('ANN graph was replaced. / 向量图已被替换。', 'RETRIEVAL_ANN_BUILD_PENDING');
      entry.options = options;
      this.shards.delete(key); this.shards.set(key, entry);
      await this._evict(options, false);
      const matches = await this._request('search', { key, query: new Float32Array(query), limit: Math.min(limit, descriptor.count) });
      checkCancelled(); this._assertBuildCurrent(entry);
      const result = Array.from(matches.keys, (key, index) => ({ id: Number(key), distance: matches.distances[index] }));
      // A cache failure must not change a successfully computed retrieval result.
      // 缓存写入失败不能改变已计算的检索结果，诊断保留真实失败状态。
      try { await this._persist(entry); this.lastError = null; }
      catch (error) { this.lastError = error.code ?? 'RETRIEVAL_ANN_CACHE_WRITE_FAILED'; }
      return result;
    });
  }

  async updateSources(oldRows, newRows, changedScopes, generationForScope) {
    this.builds.cancelScopes(changedScopes);
    return this._withOwner(() => this._updateSources(oldRows, newRows, changedScopes, generationForScope));
  }

  async _updateSources(oldRows, newRows, changedScopes, generationForScope) {
    const belongs = (entry, row) => row.scope_key === entry.identity.scopeKey && row.embedding_profile_id === entry.identity.profileId &&
      row.dimensions === entry.identity.dimensions && row.embedding_model_version === entry.identity.modelVersion &&
      row.embedding_space_id === entry.identity.spaceId && row.domain === entry.identity.domain;
    for (const [key, entry] of this.shards) {
      if (!changedScopes.includes(entry.identity.scopeKey)) continue;
      if (entry.descriptor.partitioned) {
        this.preparedOnDisk.delete(key);
        await this._discardEntry(entry, true);
        continue;
      }
      try {
        const removed = oldRows.filter(row => belongs(entry, row)).map(row => BigInt(row.id));
        if (removed.length) await this._request('remove', { key, keys: BigUint64Array.from(removed) });
        const added = newRows.filter(row => belongs(entry, row));
        for (let offset = 0; offset < added.length; offset += 256) {
          const batch = added.slice(offset, offset + 256);
          await this._request('add', { key, keys: BigUint64Array.from(batch.map(row => BigInt(row.id))), vectors: batch.map(row => vectorFromBlob(row.vector)) });
        }
        entry.count = entry.count - removed.length + added.length;
        entry.generation = generationForScope(entry.identity.scopeKey);
        entry.dirty = true;
        this.counters.updated++;
        await this._checkResidentBudget(entry.options);
      } catch (error) {
        await this._discardEntry(entry, true); this.counters.invalidated++;
        this.lastError = error.code ?? 'RETRIEVAL_ANN_UPDATE_FAILED';
      }
    }
  }

  async invalidateScopes(scopes) {
    this.builds.cancelScopes(scopes);
    return this._withOwner(async () => {
      const entries = [...this.shards.values()].filter(entry => scopes.includes(entry.identity.scopeKey));
      for (const entry of entries) { this.shards.delete(entry.key); this.counters.invalidated++; }
      if (entries.length) await this._discardEntry(entries[0], true);
    });
  }

  status() { return { backend: 'usearch', packageVersion: ANN_PACKAGE_VERSION, state: this.nativeError ? 'unavailable' : this.child ? 'ready' : 'idle',
    ...this.builds.status(),
    cachedShards: this.shards.size, cachedVectors: [...this.shards.values()].reduce((sum, entry) => sum + entry.count, 0),
    ...this.counters, helperPid: this.child?.pid ?? null, helperRssBytes: this.helperMemory.rssBytes,
    helperObservedPeakRssBytes: this.helperMemory.observedPeakRssBytes,
    backgroundObservedPeakRssBytes: this.backgroundObservedPeakRssBytes ?? 0,
    adaptiveOptions: this.adaptiveOptions ?? null,
    planningAudit: this.planningAudit ?? null,
    resourceAdmissionAudit: this.resourceAdmissionAudit ?? null,
    nativeRetryAt: this.nativeError ? this.nativeRetry.retryAt : null, errorCode: this.nativeError?.code ?? this.lastError }; }

  async close() {
    this.closed = true;
    await this.builds.close();
    await this._withOwner(async () => {
      for (const entry of this.shards.values()) {
        try { await this._persist(entry); } catch (error) { this.lastError = error.code ?? 'RETRIEVAL_ANN_CACHE_WRITE_FAILED'; }
      }
      this.shards.clear();
      await this._releaseHelper();
    });
    await this._waitForRetirement();
    await this._releaseReservation();
  }

  async _releaseHelper() {
    if (!this.child) { await this._waitForRetirement(); return; }
    const child = this.child;
    this.closingChild = child;
    await this._request('close', {}).catch(() => child.kill());
    if (this.child === child) await this._retireChild(child);
    else await this._waitForRetirement();
  }
}

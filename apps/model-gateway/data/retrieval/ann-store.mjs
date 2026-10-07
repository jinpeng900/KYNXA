import { createHash, randomUUID } from 'node:crypto';
import { closeSync, existsSync, lstatSync, mkdirSync, openSync, readFileSync, readSync, readdirSync, renameSync, unlinkSync, writeFileSync } from 'node:fs';
import { basename, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { fork } from 'node:child_process';
import { retrievalFailure } from './retrieval-contracts.mjs';

const ANN_PACKAGE_VERSION = '2.26.4';
const ANN_CACHE_VERSION = 1;
const MAX_MANIFEST_BYTES = 65536;
const DOMAIN_SQL = `CASE WHEN c.structure_domain<>'' THEN c.structure_domain
  WHEN s.structure_json<>'' THEN json_extract(s.structure_json,'$.domain')
  WHEN s.source_type='code' THEN 'code' WHEN s.source_type IN ('document','memory','conversation','web') THEN 'knowledge' ELSE '' END`;

export { DOMAIN_SQL as RETRIEVAL_DOMAIN_SQL };
export const DEFAULT_ANN_OPTIONS = Object.freeze({ mode: 'auto', threshold: 50000, maxCachedShards: 4,
  maxShardBytes: 256 * 1024 * 1024, connectivity: 16, expansionAdd: 128, expansionSearch: 128, exactScanLimit: 1000000 });

export function validateAnnOptions(input = {}) {
  if (!input || typeof input !== 'object' || Array.isArray(input))
    throw retrievalFailure('Invalid ANN options. / 本地向量索引配置无效。', 'INVALID_RETRIEVAL_ANN');
  const bounds = { threshold: [1, 1000000], maxCachedShards: [1, 16], maxShardBytes: [1024 * 1024, 1024 * 1024 * 1024],
    connectivity: [4, 64], expansionAdd: [16, 2048], expansionSearch: [16, 2048], exactScanLimit: [1, 1000000] };
  for (const [key, value] of Object.entries(input)) {
    if (key === 'mode') {
      if (!['auto', 'off', 'ann', 'exact'].includes(value))
        throw retrievalFailure('Invalid ANN mode. / 本地向量索引模式无效。', 'INVALID_RETRIEVAL_ANN');
    } else if (!bounds[key] || !Number.isSafeInteger(value) || value < bounds[key][0] || value > bounds[key][1])
      throw retrievalFailure('Invalid ANN resource budget. / 本地向量索引资源预算无效。', 'INVALID_RETRIEVAL_ANN');
  }
  return { ...DEFAULT_ANN_OPTIONS, ...input };
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
  constructor({ directory, epoch, database }) {
    this.directory = join(directory, 'ann');
    this.epoch = epoch;
    this.database = database;
    this.shards = new Map();
    this.child = null;
    this.requests = new Map();
    this.sequence = 0;
    this.helperMemory = { rssBytes: 0, observedPeakRssBytes: 0 };
    this.nativeError = null;
    this.lastError = null;
    this.counters = { built: 0, loaded: 0, updated: 0, invalidated: 0, cacheMisses: 0 };
  }

  _native() {
    if (this.child) return this.child;
    if (this.nativeError) throw this.nativeError;
    checkStorage(this.directory, true);
    if (!existsSync(this.directory)) mkdirSync(this.directory, { mode: 0o700 });
    const child = fork(fileURLToPath(new URL('./ann-worker.mjs', import.meta.url)), [], {
      cwd: this.directory, windowsHide: true, execArgv: [], serialization: 'advanced', stdio: ['ignore', 'ignore', 'ignore', 'ipc']
    });
    this.child = child;
    const fail = error => {
      this.nativeError = Object.assign(error, { code: error.code ?? 'RETRIEVAL_ANN_UNAVAILABLE' });
      for (const request of this.requests.values()) { clearTimeout(request.timeout); request.reject(this.nativeError); }
      this.requests.clear(); this.child = null;
      this.shards.clear();
      this.helperMemory.rssBytes = 0;
    };
    child.on('message', message => {
      if (Number.isSafeInteger(message.memory?.rssBytes) && Number.isSafeInteger(message.memory?.observedPeakRssBytes))
        this.helperMemory = { rssBytes: message.memory.rssBytes,
          observedPeakRssBytes: Math.max(this.helperMemory.observedPeakRssBytes, message.memory.observedPeakRssBytes) };
      const request = this.requests.get(message.id);
      if (!request) return;
      this.requests.delete(message.id); clearTimeout(request.timeout);
      if (message.error) request.reject(Object.assign(new Error(message.error.message), { code: message.error.code }));
      else request.resolve(message.result);
      if (!this.requests.size) { child.unref(); child.channel?.unref(); }
    });
    child.on('error', fail);
    child.on('exit', code => { if (this.child === child) fail(retrievalFailure(`ANN helper exited (${code}). / 向量计算进程已退出。`, 'RETRIEVAL_ANN_WORKER_EXITED')); });
    child.unref(); child.channel?.unref();
    return child;
  }

  _request(method, input) {
    const child = this._native(), id = ++this.sequence;
    if (this.requests.size >= 32) return Promise.reject(retrievalFailure('ANN helper queue is full. / 向量计算队列已满。', 'RETRIEVAL_ANN_QUEUE_FULL'));
    return new Promise((resolveResult, reject) => {
      const timeout = setTimeout(() => {
        const error = retrievalFailure('ANN computation timed out. / 向量计算超时。', 'RETRIEVAL_ANN_TIMEOUT');
        this.requests.delete(id); reject(error); child.kill();
      }, 30000);
      timeout.unref();
      this.requests.set(id, { resolve: resolveResult, reject, timeout });
      child.ref(); child.channel?.ref();
      child.send({ id, method, input }, error => {
        if (!error) return;
        const request = this.requests.get(id);
        if (!request) return;
        this.requests.delete(id); clearTimeout(timeout); reject(error);
      });
    });
  }

  _identity(descriptor, options) {
    return { schemaVersion: ANN_CACHE_VERSION, packageVersion: ANN_PACKAGE_VERSION, epoch: this.epoch,
      scopeKey: descriptor.scope_key, profileId: descriptor.embedding_profile_id, dimensions: descriptor.dimensions,
      modelVersion: descriptor.embedding_model_version, spaceId: descriptor.embedding_space_id, domain: descriptor.domain,
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
    try {
      checkStorage(paths.graph); checkStorage(paths.manifest);
      await this._request('save', { key: entry.key, filename: basename(temporary) });
      checkStorage(temporary);
      if (lstatSync(temporary).size > entry.options.maxShardBytes)
        throw retrievalFailure('ANN cache exceeds its disk budget. / 向量缓存超过单分片磁盘预算。', 'RETRIEVAL_ANN_RESOURCE_LIMIT');
      const hash = fileHash(temporary);
      const metadata = { identity: entry.identity, generation: entry.generation, count: entry.count, graphHash: hash };
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
    const entries = [...this.shards.values()];
    const fits = entry => entry.count * (entry.identity.dimensions * 4 + entry.identity.connectivity * 16 + 256) <= options.maxShardBytes;
    if (entries.length <= options.maxCachedShards && entries.every(fits)) return;
    // Retain compliant disk graphs, then release the owned process; a dropped native reference alone does not bound RSS.
    // 保留合预算的磁盘图后关闭已拥有的进程；只撤销原生引用不能保证释放进程内存。
    for (const entry of entries.filter(fits).reverse().slice(0, options.maxCachedShards)) {
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
    try {
      const metadata = JSON.parse(readFileSync(paths.manifest, 'utf8'));
      if (JSON.stringify(metadata.identity) !== JSON.stringify(entry.identity) || metadata.generation !== entry.generation || metadata.count !== count)
        return false;
      const hash = fileHash(paths.graph);
      if (metadata.graphHash !== hash) return false;
      const loaded = await this._request('load', { key: entry.key, filename: basename(paths.graph) });
      if (loaded.count !== count || loaded.dimensions !== entry.identity.dimensions) return false;
      this.counters.loaded++;
      return true;
    } catch { return false; }
  }

  async _evict(options, incoming = true) {
    while (this.shards.size + Number(incoming) > options.maxCachedShards) {
      const [key, entry] = this.shards.entries().next().value;
      try { await this._persist(entry); } catch (error) { this.lastError = error.code ?? 'RETRIEVAL_ANN_CACHE_WRITE_FAILED'; }
      this.shards.delete(key);
      // USearch's Node binding releases native ownership through its GC finalizer.
      // USearch 的 Node 绑定通过 GC 终结器释放原生所有权；这里撤销引用，不宣称进程 RSS 是硬上限。
      await this._request('drop', { key });
    }
  }

  async search(descriptor, query, limit, options, checkCancelled) {
    await this.enforceBudget(options);
    const estimatedBytes = descriptor.count * (descriptor.dimensions * 4 + options.connectivity * 16 + 256);
    if (estimatedBytes > options.maxShardBytes)
      throw retrievalFailure('ANN shard exceeds its memory budget. / 向量分片超过内存预算。', 'RETRIEVAL_ANN_RESOURCE_LIMIT');
    const identity = this._identity(descriptor, options);
    const key = createHash('sha256').update(JSON.stringify(identity)).digest('hex');
    let entry = this.shards.get(key);
    if (entry && entry.generation !== descriptor.generation) {
      this.shards.delete(key); await this._request('drop', { key }); entry = null; this.counters.invalidated++;
    }
    if (!entry) {
      this.counters.cacheMisses++;
      await this._evict(options);
      entry = { key, identity, descriptor, options, generation: descriptor.generation, count: descriptor.count, dirty: false };
      const create = () => this._request('create', { key, dimensions: descriptor.dimensions,
        connectivity: options.connectivity, expansionAdd: options.expansionAdd, expansionSearch: options.expansionSearch });
      try {
        await create();
        if (!await this._load(entry, descriptor.count, options)) {
          // A failed load may already have populated native state; rebuild in a fresh owner.
          // 加载失败可能已填充原生状态，重建必须使用新实例，不能混入过期键。
          await create();
          // Build in bounded batches; never copy the whole vector corpus into a JS matrix.
          // 按有界批次建图，不把整个向量语料复制为 JavaScript 大矩阵。
          const rows = this.database.prepare(`SELECT c.id,c.vector FROM chunks c JOIN sources s ON s.source_id=c.source_id
            WHERE s.scope_key=? AND c.embedding_profile_id=? AND c.dimensions=? AND c.embedding_model_version=?
            AND c.embedding_space_id=? AND ${DOMAIN_SQL}=? AND c.vector IS NOT NULL ORDER BY c.id`);
          let keys = [], vectors = [];
          const flush = async () => {
            if (!keys.length) return;
            checkCancelled();
            await this._request('add', { key, keys: BigUint64Array.from(keys), vectors });
            keys = []; vectors = [];
          };
          for (const row of rows.iterate(descriptor.scope_key, descriptor.embedding_profile_id, descriptor.dimensions,
            descriptor.embedding_model_version, descriptor.embedding_space_id, descriptor.domain)) {
            checkCancelled();
            keys.push(BigInt(row.id)); vectors.push(vectorFromBlob(row.vector));
            if (keys.length === 256) await flush();
          }
          await flush(); checkCancelled();
          entry.dirty = true;
          this.counters.built++;
        }
        this.shards.set(key, entry);
      } catch (error) {
        await this._request('drop', { key }).catch(() => {});
        throw error;
      }
    } else {
      entry.options = options;
      this.shards.delete(key); this.shards.set(key, entry);
      await this._evict(options, false);
    }
    checkCancelled();
    const matches = await this._request('search', { key, query: new Float32Array(query), limit: Math.min(limit, descriptor.count) });
    const result = Array.from(matches.keys, (key, index) => ({ id: Number(key), distance: matches.distances[index] }));
    // A cache failure must not change a successfully computed retrieval result.
    // 缓存写入失败不能改变已计算的检索结果，诊断保留真实失败状态。
    try { await this._persist(entry); this.lastError = null; }
    catch (error) { this.lastError = error.code ?? 'RETRIEVAL_ANN_CACHE_WRITE_FAILED'; }
    return result;
  }

  async updateSources(oldRows, newRows, changedScopes, generationForScope) {
    const belongs = (entry, row) => row.scope_key === entry.identity.scopeKey && row.embedding_profile_id === entry.identity.profileId &&
      row.dimensions === entry.identity.dimensions && row.embedding_model_version === entry.identity.modelVersion &&
      row.embedding_space_id === entry.identity.spaceId && row.domain === entry.identity.domain;
    for (const [key, entry] of this.shards) {
      if (!changedScopes.includes(entry.identity.scopeKey)) continue;
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
      } catch (error) {
        this.shards.delete(key); await this._request('drop', { key }).catch(() => {}); this.counters.invalidated++;
        this.lastError = error.code ?? 'RETRIEVAL_ANN_UPDATE_FAILED';
      }
    }
  }

  status() { return { backend: 'usearch', packageVersion: ANN_PACKAGE_VERSION, state: this.nativeError ? 'unavailable' : this.child ? 'ready' : 'idle',
    cachedShards: this.shards.size, cachedVectors: [...this.shards.values()].reduce((sum, entry) => sum + entry.count, 0),
    ...this.counters, helperPid: this.child?.pid ?? null, helperRssBytes: this.helperMemory.rssBytes,
    helperObservedPeakRssBytes: this.helperMemory.observedPeakRssBytes, errorCode: this.nativeError?.code ?? this.lastError }; }

  async close() {
    for (const entry of this.shards.values()) {
      try { await this._persist(entry); } catch (error) { this.lastError = error.code ?? 'RETRIEVAL_ANN_CACHE_WRITE_FAILED'; }
    }
    this.shards.clear();
    await this._releaseHelper();
  }

  async _releaseHelper() {
    if (!this.child) return;
    const child = this.child;
    await this._request('close', {}).catch(() => child.kill());
    this.child = null;
    await new Promise(resolveExit => { if (child.exitCode !== null) resolveExit(); else child.once('exit', resolveExit); });
    this.helperMemory.rssBytes = 0;
  }
}

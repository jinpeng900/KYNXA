import { DEFAULT_ANN_OPTIONS, LocalAnnStore, RETRIEVAL_DOMAIN_SQL, partitionAnnDescriptor, validateAnnOptions } from './ann-store.mjs';

const MAX_CHANNEL_CANDIDATES = 40;
const MAX_DESCRIPTOR_CACHE_ENTRIES = 64;
const MAX_DESCRIPTOR_CACHE_BYTES = 8 * 1024 * 1024;
const EXACT_TARGET_LATENCY_MS = 25;
const MIN_ADAPTIVE_THRESHOLD = 1024;
const MAX_ADAPTIVE_THRESHOLD = 200000;

/** Exact and approximate channels share the same authorized SQLite corpus and model identity.
 * 精确与近似通道共享同一已授权 SQLite 语料及模型身份，不混用向量空间或跨范围召回。 */
export class RetrievalVectorSearch {
  constructor({ database, directory, epoch, options, resourceService }) {
    this.database = database;
    this.vectorTable = database.prepare("SELECT 1 FROM sqlite_master WHERE type='view' AND name='retrieval_vector_rows'").get()
      ? 'retrieval_vector_rows' : 'chunks';
    this.options = validateAnnOptions(options);
    this.ann = new LocalAnnStore({ database, directory, epoch, resourceService, vectorTable: this.vectorTable });
    this.descriptorCache = new Map();
    this.descriptorCacheBytes = 0;
    this.descriptorCacheCounters = { hits: 0, misses: 0, invalidated: 0 };
    this.partitionCache = new Map();
    this.partitionCacheBytes = 0;
    this.exactCosts = new Map();
    this.exactLatencyMs = null;
  }

  _invalidateDescriptors(scopeKeys) {
    this.partitionCache.clear();
    this.partitionCacheBytes = 0;
    for (const [key, cost] of this.exactCosts) if (scopeKeys.includes(cost.scopeKey)) this.exactCosts.delete(key);
    for (const [key, entry] of this.descriptorCache) if (entry.scopeKeys.some(scopeKey => scopeKeys.includes(scopeKey))) {
      this.descriptorCache.delete(key);
      this.descriptorCacheBytes -= entry.bytes;
      this.descriptorCacheCounters.invalidated++;
    }
  }

  _partitionDescriptors(descriptors, options) {
    if (!options.adaptive) return descriptors;
    const result = [];
    for (const descriptor of descriptors) {
      const perVector = descriptor.dimensions * 4 + options.connectivity * 16 + 256;
      const vectorsPerShard = Math.max(1, Math.floor(options.maxShardBytes / perVector));
      if (descriptor.count <= vectorsPerShard) { result.push(descriptor); continue; }
      const key = JSON.stringify([descriptor, vectorsPerShard]);
      let parts = this.partitionCache.get(key)?.parts;
      if (!parts) {
        parts = partitionAnnDescriptor(this.database, this.vectorTable, descriptor, options);
        const bytes = Buffer.byteLength(key) + Buffer.byteLength(JSON.stringify(parts));
        if (bytes <= MAX_DESCRIPTOR_CACHE_BYTES) {
          while (this.partitionCache.size >= MAX_DESCRIPTOR_CACHE_ENTRIES || this.partitionCacheBytes + bytes > MAX_DESCRIPTOR_CACHE_BYTES) {
            const [oldestKey, oldest] = this.partitionCache.entries().next().value;
            this.partitionCache.delete(oldestKey); this.partitionCacheBytes -= oldest.bytes;
          }
          this.partitionCache.set(key, { parts, bytes }); this.partitionCacheBytes += bytes;
        }
      }
      result.push(...parts);
    }
    return result;
  }

  _costKey(descriptor) {
    return JSON.stringify([this.ann.epoch, descriptor.scope_key, descriptor.embedding_profile_id, descriptor.dimensions,
      descriptor.embedding_model_version, descriptor.embedding_space_id, descriptor.domain, descriptor.generation]);
  }

  _recordExactCost(descriptor, elapsedMs) {
    if (!descriptor.count || !Number.isFinite(elapsedMs) || elapsedMs <= 0) return;
    const key = this._costKey(descriptor), previous = this.exactCosts.get(key);
    this.exactCosts.delete(key);
    this.exactCosts.set(key, { scopeKey: descriptor.scope_key,
      msPerVector: previous ? previous.msPerVector * 0.8 + elapsedMs / descriptor.count * 0.2 : elapsedMs / descriptor.count });
    while (this.exactCosts.size > MAX_DESCRIPTOR_CACHE_ENTRIES) this.exactCosts.delete(this.exactCosts.keys().next().value);
  }

  _routingOptions(options, descriptors, { approvedCapacity = false } = {}) {
    // Tune only the default automatic switch using measured exact cost and admitted capacity; ranking stays unchanged.
    // 仅按实测精确扫描成本及批准容量调整默认自动切换阈值，明确配置和排序算法保持原合同。
    let threshold = options.threshold, reason = 'configured-threshold';
    const costs = descriptors.map(descriptor => this.exactCosts.get(this._costKey(descriptor))?.msPerVector).filter(value => value > 0);
    if (options.mode === 'auto' && options.adaptive && threshold === DEFAULT_ANN_OPTIONS.threshold) {
      if (costs.length) {
        threshold = Math.max(MIN_ADAPTIVE_THRESHOLD, Math.min(MAX_ADAPTIVE_THRESHOLD,
          Math.floor(EXACT_TARGET_LATENCY_MS / Math.max(...costs))));
        reason = 'observed-exact-cost';
      }
      if (approvedCapacity && descriptors.length) {
        const maximumVectorBytes = Math.max(...descriptors.map(descriptor => descriptor.dimensions * 4 + options.connectivity * 16 + 256));
        const capacityThreshold = Math.max(MIN_ADAPTIVE_THRESHOLD, Math.floor(options.maxShardBytes / maximumVectorBytes / 2));
        if (capacityThreshold < threshold) { threshold = capacityThreshold; reason = 'approved-shard-capacity'; }
      }
    }
    this.routingAudit = { configuredThreshold: options.threshold, effectiveThreshold: threshold, reason,
      exactTargetLatencyMs: EXACT_TARGET_LATENCY_MS, measuredCorpora: costs.length,
      thresholdBounds: { minimum: MIN_ADAPTIVE_THRESHOLD, maximum: MAX_ADAPTIVE_THRESHOLD },
      explicitThresholdPreserved: options.threshold !== DEFAULT_ANN_OPTIONS.threshold };
    return { ...options, threshold };
  }

  async _plannedDescriptors(descriptors, options, kind) {
    // Admission precedes partitioning; the shard plan cannot rely on a configured budget that was never reserved.
    // 先准入后分片，分片计划不能依赖未预约的配置额度；CPU/内存执行仍由原生所有者逐次准入。
    const routed = this._routingOptions(options, descriptors);
    if (options.mode !== 'ann' && (options.mode !== 'auto' || !descriptors.some(descriptor => descriptor.count > routed.threshold))) {
      this.ann.planningAudit = { configured: { maxShardBytes: options.maxShardBytes, threshold: options.threshold },
        approved: null, reason: 'exact-route-no-ann-admission' };
      return { options: routed, descriptors: this._partitionDescriptors(descriptors, routed) };
    }
    let admitted;
    try { admitted = await this.ann.planningOptions(options, descriptors, kind); }
    catch (error) {
      this.ann.planningAudit = { ...this.ann.resourceAdmissionAudit, approved: null,
        configured: { maxShardBytes: options.maxShardBytes, threshold: options.threshold }, reason: error.code ?? 'RETRIEVAL_ANN_RESOURCE_LIMIT' };
      throw error;
    }
    const effective = this._routingOptions(admitted, descriptors, { approvedCapacity: Boolean(this.ann.resourceLease) });
    const parts = this._partitionDescriptors(descriptors, effective);
    this.ann.planningAudit = { ...this.ann.resourceAdmissionAudit, configured: { maxShardBytes: options.maxShardBytes,
      maxCachedShards: options.maxCachedShards, threshold: options.threshold }, approved: { maxShardBytes: effective.maxShardBytes,
      maxCachedShards: effective.maxCachedShards, threshold: effective.threshold }, originalDescriptors: descriptors.length,
      plannedShards: parts.length, largestShardBytes: Math.max(0, ...parts.map(descriptor => descriptor.count *
        (descriptor.dimensions * 4 + effective.connectivity * 16 + 256))) };
    return { options: effective, descriptors: parts };
  }

  _descriptors({ scopeKeys, embeddingProfileId, dimensions, embeddingModelVersion, embeddingSpaceId, requestedDomain }) {
    const scopes = [...scopeKeys].sort(), placeholders = scopes.map(() => '?').join(',');
    const rows = this.database.prepare(`SELECT scope_key,generation FROM scope_snapshots
      WHERE scope_key IN (${placeholders})`).all(...scopes);
    const generations = new Map(rows.map(row => [row.scope_key, row.generation]));
    const epoch = this.database.prepare("SELECT value FROM retrieval_metadata WHERE key='index_epoch'").get().value;
    // Permissions and live database versions are part of every lookup, never inferred from an old cached graph.
    // 每次查询都核验请求权限范围及数据库实时版本，不能从旧图或旧缓存推断当前权限。
    const key = JSON.stringify([epoch, scopes.map(scopeKey => [scopeKey, generations.get(scopeKey) ?? 0]),
      embeddingProfileId, dimensions, embeddingModelVersion ?? null, embeddingSpaceId ?? null, requestedDomain ?? null]);
    const cached = this.descriptorCache.get(key);
    if (cached) {
      this.descriptorCache.delete(key); this.descriptorCache.set(key, cached);
      this.descriptorCacheCounters.hits++;
      return cached.descriptors;
    }
    this.descriptorCacheCounters.misses++;
    const domainFilter = requestedDomain ? ` AND (${RETRIEVAL_DOMAIN_SQL}=? OR ${RETRIEVAL_DOMAIN_SQL}='')` : '';
    const versionFilter = (embeddingModelVersion === undefined ? '' : ' AND c.embedding_model_version=?') +
      (embeddingSpaceId === undefined ? '' : ' AND c.embedding_space_id=?');
    const parameters = [...scopes, ...(requestedDomain ? [requestedDomain] : []), embeddingProfileId, dimensions,
      ...(embeddingModelVersion === undefined ? [] : [embeddingModelVersion]), ...(embeddingSpaceId === undefined ? [] : [embeddingSpaceId])];
    const descriptors = this.database.prepare(`SELECT s.scope_key,c.embedding_profile_id,c.dimensions,c.embedding_model_version,
      c.embedding_space_id,${RETRIEVAL_DOMAIN_SQL} AS domain,count(*) AS count,
      COALESCE(ss.generation,0) AS generation FROM ${this.vectorTable} c JOIN sources s ON s.source_id=c.source_id
      LEFT JOIN scope_snapshots ss ON ss.scope_key=s.scope_key
      WHERE s.scope_key IN (${placeholders})${domainFilter} AND c.embedding_profile_id=? AND c.dimensions=?
      AND c.vector IS NOT NULL${versionFilter}
      GROUP BY s.scope_key,c.embedding_profile_id,c.dimensions,c.embedding_model_version,c.embedding_space_id,domain`).all(...parameters);
    const bytes = Buffer.byteLength(key) + Buffer.byteLength(JSON.stringify(descriptors));
    // Only scalar shard descriptors are cached, with both a count and serialized-size bound.
    // 仅缓存标量分片目录，同时限制数量与序列化大小；原文和向量不进入此缓存。
    if (bytes <= MAX_DESCRIPTOR_CACHE_BYTES) {
      while (this.descriptorCache.size >= MAX_DESCRIPTOR_CACHE_ENTRIES || this.descriptorCacheBytes + bytes > MAX_DESCRIPTOR_CACHE_BYTES) {
        const [oldestKey, oldest] = this.descriptorCache.entries().next().value;
        this.descriptorCache.delete(oldestKey); this.descriptorCacheBytes -= oldest.bytes;
      }
      this.descriptorCache.set(key, { scopeKeys: scopes, descriptors, bytes });
      this.descriptorCacheBytes += bytes;
    }
    return descriptors;
  }

  sourceRows(sourceIds) {
    if (!sourceIds.length || !this.ann.shards.size) return [];
    return this.database.prepare(`SELECT c.id,c.vector,c.embedding_profile_id,c.dimensions,c.embedding_model_version,
      c.embedding_space_id,s.scope_key,${RETRIEVAL_DOMAIN_SQL} AS domain FROM ${this.vectorTable} c JOIN sources s ON s.source_id=c.source_id
      WHERE c.source_id IN (${sourceIds.map(() => '?').join(',')}) AND c.vector IS NOT NULL`).all(...sourceIds);
  }

  async updateSources(oldRows, sourceIds, changedScopes) {
    this._invalidateDescriptors(changedScopes);
    if (!changedScopes.length) return;
    try {
      await this.ann.updateSources(oldRows, this.sourceRows(sourceIds), changedScopes, scopeKey =>
        this.database.prepare('SELECT generation FROM scope_snapshots WHERE scope_key=?').get(scopeKey)?.generation ?? 0);
    } catch (error) {
      this.ann.lastError = error.code ?? 'RETRIEVAL_ANN_UPDATE_FAILED';
      await this.invalidateScopes(changedScopes);
    }
  }

  async prepareScopes(scopeKeys, ann) {
    let options = ann === undefined ? this.lastOptions ?? this.options : validateAnnOptions({ ...this.options, ...ann });
    await this.ann.enforceBudget(options);
    if (options.mode === 'off' || options.mode === 'exact') return this.ann.status();
    const descriptors = this.database.prepare(`SELECT s.scope_key,c.embedding_profile_id,c.dimensions,c.embedding_model_version,
      c.embedding_space_id,${RETRIEVAL_DOMAIN_SQL} AS domain,count(*) AS count,COALESCE(ss.generation,0) AS generation
      FROM ${this.vectorTable} c JOIN sources s ON s.source_id=c.source_id LEFT JOIN scope_snapshots ss ON ss.scope_key=s.scope_key
      WHERE s.scope_key IN (${scopeKeys.map(() => '?').join(',')}) AND c.vector IS NOT NULL
      GROUP BY s.scope_key,c.embedding_profile_id,c.dimensions,c.embedding_model_version,c.embedding_space_id,domain`)
      .all(...scopeKeys);
    let planned;
    try { planned = await this._plannedDescriptors(descriptors, options, 'background'); }
    catch (error) { this.ann.lastError = error.code ?? 'RETRIEVAL_ANN_RESOURCE_LIMIT'; return this.ann.status(); }
    options = planned.options;
    await this.ann.enforceBudget(options);
    for (const descriptor of planned.descriptors) {
      try { this.ann.warm(descriptor, options); }
      catch (error) { this.ann.lastError = error.code ?? 'RETRIEVAL_ANN_BUILD_FAILED'; }
    }
    return this.ann.status();
  }

  async invalidateScopes(scopeKeys) {
    this._invalidateDescriptors(scopeKeys);
    await this.ann.invalidateScopes(scopeKeys);
  }

  async search({ scopeKeys, queryVector, embeddingProfileId, embeddingModelVersion, embeddingSpaceId, requestedDomain, ann,
    channelCandidates = MAX_CHANNEL_CANDIDATES }, checkCancelled) {
    const candidateLimit = Math.max(1, Math.min(160, channelCandidates));
    let options = validateAnnOptions({ ...this.options, ...ann });
    this.lastOptions = options;
    await this.ann.enforceBudget(options);
    checkCancelled();
    const rawDescriptors = this._descriptors({ scopeKeys, embeddingProfileId, dimensions: queryVector.length,
      embeddingModelVersion, embeddingSpaceId, requestedDomain });
    let descriptors, planningError;
    try {
      const planned = await this._plannedDescriptors(rawDescriptors, options, 'foreground');
      options = planned.options; descriptors = planned.descriptors;
      checkCancelled(); await this.ann.enforceBudget(options); checkCancelled();
    } catch (error) {
      if (error.name === 'AbortError') throw error;
      planningError = error.code ?? 'RETRIEVAL_ANN_RESOURCE_LIMIT';
      descriptors = this._partitionDescriptors(rawDescriptors, options);
    }
    this.effectiveOptions = options;
    let degradedReason = planningError ?? null;
    let exactScannedChunks = 0;
    const matches = [], backends = new Set();
    const exactCandidates = (descriptor, fallback = false) => {
      const limit = fallback ? Math.min(options.adaptive && (this.exactLatencyMs ?? 0) < 25 ? 250000 : 50000, options.exactScanLimit)
        : options.mode === 'off' ? 50000 : options.exactScanLimit;
      if (exactScannedChunks + descriptor.count > limit) { degradedReason ??= 'RETRIEVAL_VECTOR_SCAN_LIMIT'; return; }
      exactScannedChunks += descriptor.count;
      const started = performance.now();
      // The graph domain is an exact prefilter, including a separate legacy unknown-domain shard.
      // 图所属领域必须精确预筛选；旧数据的未知领域使用独立分片，保持兼容且不污染明确领域。
      const rows = this.database.prepare(`WITH authorized AS MATERIALIZED (
        SELECT c.id,c.chunk_id,c.vector FROM ${this.vectorTable} c JOIN sources s ON s.source_id=c.source_id
        WHERE s.scope_key=? AND c.embedding_profile_id=? AND c.dimensions=? AND c.embedding_model_version=?
        AND c.embedding_space_id=? AND ${RETRIEVAL_DOMAIN_SQL}=? AND c.vector IS NOT NULL
        AND c.id>=? AND c.id<=?),
        distances AS MATERIALIZED (SELECT id,chunk_id,vec_distance_cosine(vector,?) AS distance FROM authorized),
        candidates AS MATERIALIZED (SELECT id,chunk_id,distance FROM distances WHERE distance IS NOT NULL
          AND distance>=-1.7976931348623157e308 AND distance<=1.7976931348623157e308
          ORDER BY distance,chunk_id LIMIT ${candidateLimit})
        SELECT candidates.*,(SELECT count(*) FROM distances WHERE distance IS NULL
          OR distance<-1.7976931348623157e308 OR distance>1.7976931348623157e308) AS invalid_distance_count FROM candidates`)
        .all(descriptor.scope_key, descriptor.embedding_profile_id, descriptor.dimensions, descriptor.embedding_model_version,
          descriptor.embedding_space_id, descriptor.domain, descriptor.minimumId ?? 0, descriptor.maximumId ?? Number.MAX_SAFE_INTEGER,
          new Uint8Array(new Float32Array(queryVector).buffer));
      const elapsed = performance.now() - started;
      this.exactLatencyMs = this.exactLatencyMs === null ? elapsed : this.exactLatencyMs * 0.8 + elapsed * 0.2;
      this._recordExactCost(descriptor, elapsed);
      if (rows.length ? rows[0].invalid_distance_count : descriptor.count) degradedReason ??= 'RETRIEVAL_VECTOR_DISTANCE_INVALID';
      matches.push(...rows.map(row => ({ id: row.id, distance: row.distance,
        identity: descriptor, domainRank: requestedDomain && descriptor.domain !== requestedDomain ? 1 : 0 })));
      backends.add('exact');
    };
    if (options.mode === 'off' && descriptors.reduce((sum, descriptor) => sum + descriptor.count, 0) > 50000)
      return { items: [], degradedReason: 'RETRIEVAL_VECTOR_SCAN_LIMIT', semanticBackend: null };
    for (const descriptor of descriptors) {
      checkCancelled();
      const shouldApproximate = options.mode === 'ann' || options.mode === 'auto' && descriptor.count > options.threshold;
      if (!shouldApproximate) { exactCandidates(descriptor); continue; }
      if (planningError) { exactCandidates(descriptor, true); continue; }
      try {
        const rows = await this.ann.search(descriptor, queryVector, candidateLimit, options, checkCancelled);
        matches.push(...rows.filter(row => Number.isFinite(row.distance)).map(row => ({ ...row,
          identity: descriptor, domainRank: requestedDomain && descriptor.domain !== requestedDomain ? 1 : 0 })));
        if (rows.some(row => !Number.isFinite(row.distance))) degradedReason ??= 'RETRIEVAL_VECTOR_DISTANCE_INVALID';
        backends.add('ann');
      } catch (error) {
        if (error.name === 'AbortError') throw error;
        if (error.code === 'RETRIEVAL_ANN_BUILD_PENDING') {
          // Adaptive graph selection must preserve existing vectors within the bounded exact fallback budget.
          // 自适应切换到建图时，在有界精确回退额度内继续使用已有向量，不因图尚未就绪丢失语义通道。
          const scannedBefore = exactScannedChunks;
          exactCandidates(descriptor, true);
          if (exactScannedChunks > scannedBefore) continue;
          degradedReason ??= error.code;
        } else {
          degradedReason ??= error.code ?? 'RETRIEVAL_ANN_FAILED';
          exactCandidates(descriptor, true);
        }
      }
    }
    checkCancelled();
    const ranked = matches.sort((a, b) => a.domainRank - b.domainRank || a.distance - b.distance || a.id - b.id)
      .slice(0, candidateLimit);
    const items = [];
    const getRow = this.database.prepare(`SELECT c.*,s.scope_key,s.source_type,s.title,s.locator,s.content_hash,s.source_revision,
      s.binding_revision,s.active_generation,s.derivation_signature,${RETRIEVAL_DOMAIN_SQL} AS actual_domain
      FROM ${this.vectorTable} c JOIN sources s ON s.source_id=c.source_id WHERE c.id=? AND c.embedding_space_id=? AND c.embedding_model_version=? AND c.embedding_profile_id=?`);
    for (const match of ranked) {
      const row = getRow.get(match.id, match.identity.embedding_space_id, match.identity.embedding_model_version, match.identity.embedding_profile_id);
      // SQLite authorization is rechecked as a defence against a corrupt or stale derived graph.
      // SQLite 再次核验身份，防止损坏或过期派生图返回无效键；正确图已在检索前完成范围筛选。
      if (!row || !scopeKeys.includes(row.scope_key) || row.embedding_profile_id !== embeddingProfileId || row.dimensions !== queryVector.length ||
          row.scope_key !== match.identity.scope_key || row.embedding_space_id !== match.identity.embedding_space_id ||
          row.embedding_model_version !== match.identity.embedding_model_version || row.actual_domain !== match.identity.domain ||
          embeddingModelVersion !== undefined && row.embedding_model_version !== embeddingModelVersion ||
          embeddingSpaceId !== undefined && row.embedding_space_id !== embeddingSpaceId) {
        degradedReason ??= 'RETRIEVAL_ANN_IDENTITY_MISMATCH'; continue;
      }
      items.push({ ...row, distance: match.distance });
    }
    return { items, degradedReason, semanticBackend: backends.size > 1 ? 'mixed' : [...backends][0] ?? null };
  }

  status() { return { ...this.ann.status(), options: this.lastOptions ?? this.options,
    effectiveOptions: this.effectiveOptions ?? null, routingAudit: this.routingAudit ?? null, exactLatencyMs: this.exactLatencyMs,
    partitionCache: { entries: this.partitionCache.size, payloadBytes: this.partitionCacheBytes },
    descriptorCache: { entries: this.descriptorCache.size, payloadBytes: this.descriptorCacheBytes, ...this.descriptorCacheCounters } }; }
  close() {
    this.descriptorCache.clear(); this.descriptorCacheBytes = 0;
    this.partitionCache.clear();
    this.partitionCacheBytes = 0; this.exactCosts.clear();
    return this.ann.close();
  }
}

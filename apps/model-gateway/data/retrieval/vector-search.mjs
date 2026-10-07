import { LocalAnnStore, RETRIEVAL_DOMAIN_SQL, validateAnnOptions } from './ann-store.mjs';

const MAX_CHANNEL_CANDIDATES = 40;
const MAX_DESCRIPTOR_CACHE_ENTRIES = 64;
const MAX_DESCRIPTOR_CACHE_BYTES = 8 * 1024 * 1024;

/** Exact and approximate channels share the same authorized SQLite corpus and model identity.
 * 精确与近似通道共享同一已授权 SQLite 语料及模型身份，不混用向量空间或跨范围召回。 */
export class RetrievalVectorSearch {
  constructor({ database, directory, epoch, options }) {
    this.database = database;
    this.options = validateAnnOptions(options);
    this.ann = new LocalAnnStore({ database, directory, epoch });
    this.descriptorCache = new Map();
    this.descriptorCacheBytes = 0;
    this.descriptorCacheCounters = { hits: 0, misses: 0, invalidated: 0 };
  }

  _invalidateDescriptors(scopeKeys) {
    for (const [key, entry] of this.descriptorCache) if (entry.scopeKeys.some(scopeKey => scopeKeys.includes(scopeKey))) {
      this.descriptorCache.delete(key);
      this.descriptorCacheBytes -= entry.bytes;
      this.descriptorCacheCounters.invalidated++;
    }
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
      COALESCE(ss.generation,0) AS generation FROM chunks c JOIN sources s ON s.source_id=c.source_id
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
      c.embedding_space_id,s.scope_key,${RETRIEVAL_DOMAIN_SQL} AS domain FROM chunks c JOIN sources s ON s.source_id=c.source_id
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

  prepareScopes(scopeKeys, ann) {
    const options = ann === undefined ? this.lastOptions ?? this.options : validateAnnOptions({ ...this.options, ...ann });
    if (options.mode === 'off' || options.mode === 'exact') return this.ann.status();
    const descriptors = this.database.prepare(`SELECT s.scope_key,c.embedding_profile_id,c.dimensions,c.embedding_model_version,
      c.embedding_space_id,${RETRIEVAL_DOMAIN_SQL} AS domain,count(*) AS count,COALESCE(ss.generation,0) AS generation
      FROM chunks c JOIN sources s ON s.source_id=c.source_id LEFT JOIN scope_snapshots ss ON ss.scope_key=s.scope_key
      WHERE s.scope_key IN (${scopeKeys.map(() => '?').join(',')}) AND c.vector IS NOT NULL
      GROUP BY s.scope_key,c.embedding_profile_id,c.dimensions,c.embedding_model_version,c.embedding_space_id,domain`)
      .all(...scopeKeys);
    for (const descriptor of descriptors) {
      try { this.ann.warm(descriptor, options); }
      catch (error) { this.ann.lastError = error.code ?? 'RETRIEVAL_ANN_BUILD_FAILED'; }
    }
    return this.ann.status();
  }

  async invalidateScopes(scopeKeys) {
    this._invalidateDescriptors(scopeKeys);
    this.ann.builds.cancelScopes(scopeKeys);
    for (const [key, entry] of this.ann.shards) if (scopeKeys.includes(entry.identity.scopeKey)) {
      this.ann.shards.delete(key); this.ann.counters.invalidated++;
      await this.ann._request('drop', { key }).catch(() => {});
    }
  }

  async search({ scopeKeys, queryVector, embeddingProfileId, embeddingModelVersion, embeddingSpaceId, requestedDomain, ann }, checkCancelled) {
    const options = validateAnnOptions({ ...this.options, ...ann });
    this.lastOptions = options;
    await this.ann.enforceBudget(options);
    checkCancelled();
    const descriptors = this._descriptors({ scopeKeys, embeddingProfileId, dimensions: queryVector.length,
      embeddingModelVersion, embeddingSpaceId, requestedDomain });
    let degradedReason = null;
    let exactScannedChunks = 0;
    const matches = [], backends = new Set();
    const exactCandidates = (descriptor, fallback = false) => {
      const limit = fallback ? Math.min(50000, options.threshold, options.exactScanLimit)
        : options.mode === 'off' ? 50000 : options.exactScanLimit;
      if (exactScannedChunks + descriptor.count > limit) { degradedReason ??= 'RETRIEVAL_VECTOR_SCAN_LIMIT'; return; }
      exactScannedChunks += descriptor.count;
      // The graph domain is an exact prefilter, including a separate legacy unknown-domain shard.
      // 图所属领域必须精确预筛选；旧数据的未知领域使用独立分片，保持兼容且不污染明确领域。
      const rows = this.database.prepare(`WITH authorized AS MATERIALIZED (
        SELECT c.id,c.chunk_id,c.vector FROM chunks c JOIN sources s ON s.source_id=c.source_id
        WHERE s.scope_key=? AND c.embedding_profile_id=? AND c.dimensions=? AND c.embedding_model_version=?
        AND c.embedding_space_id=? AND ${RETRIEVAL_DOMAIN_SQL}=? AND c.vector IS NOT NULL),
        distances AS MATERIALIZED (SELECT id,chunk_id,vec_distance_cosine(vector,?) AS distance FROM authorized),
        candidates AS MATERIALIZED (SELECT id,chunk_id,distance FROM distances WHERE distance IS NOT NULL
          AND distance>=-1.7976931348623157e308 AND distance<=1.7976931348623157e308
          ORDER BY distance,chunk_id LIMIT ${MAX_CHANNEL_CANDIDATES})
        SELECT candidates.*,(SELECT count(*) FROM distances WHERE distance IS NULL
          OR distance<-1.7976931348623157e308 OR distance>1.7976931348623157e308) AS invalid_distance_count FROM candidates`)
        .all(descriptor.scope_key, descriptor.embedding_profile_id, descriptor.dimensions, descriptor.embedding_model_version,
          descriptor.embedding_space_id, descriptor.domain, new Uint8Array(new Float32Array(queryVector).buffer));
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
      try {
        const rows = await this.ann.search(descriptor, queryVector, MAX_CHANNEL_CANDIDATES, options, checkCancelled);
        matches.push(...rows.filter(row => Number.isFinite(row.distance)).map(row => ({ ...row,
          identity: descriptor, domainRank: requestedDomain && descriptor.domain !== requestedDomain ? 1 : 0 })));
        if (rows.some(row => !Number.isFinite(row.distance))) degradedReason ??= 'RETRIEVAL_VECTOR_DISTANCE_INVALID';
        backends.add('ann');
      } catch (error) {
        if (error.name === 'AbortError') throw error;
        degradedReason ??= error.code ?? 'RETRIEVAL_ANN_FAILED';
        // A pending large graph cannot be replaced by an equally expensive full foreground scan.
        // 大图未就绪时不能改为同样昂贵的前台全扫描；保留词法通道和真实准备状态。
        if (descriptor.count <= options.threshold || error.code !== 'RETRIEVAL_ANN_BUILD_PENDING') exactCandidates(descriptor, true);
      }
    }
    checkCancelled();
    const ranked = matches.sort((a, b) => a.domainRank - b.domainRank || a.distance - b.distance || a.id - b.id)
      .slice(0, MAX_CHANNEL_CANDIDATES);
    const items = [];
    const getRow = this.database.prepare(`SELECT c.*,s.scope_key,s.source_type,s.title,s.locator,s.content_hash,s.source_revision,
      s.binding_revision,s.active_generation,s.derivation_signature,${RETRIEVAL_DOMAIN_SQL} AS actual_domain
      FROM chunks c JOIN sources s ON s.source_id=c.source_id WHERE c.id=?`);
    for (const match of ranked) {
      const row = getRow.get(match.id);
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
    descriptorCache: { entries: this.descriptorCache.size, payloadBytes: this.descriptorCacheBytes, ...this.descriptorCacheCounters } }; }
  close() {
    this.descriptorCache.clear(); this.descriptorCacheBytes = 0;
    return this.ann.close();
  }
}

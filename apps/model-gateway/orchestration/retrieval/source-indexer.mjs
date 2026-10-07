import { resolve } from 'node:path';
import { chunkSource } from '../../data/retrieval/index.mjs';
import { validateSource } from '../../data/retrieval/retrieval-contracts.mjs';
import { deriveSourceVersion } from '../../data/retrieval/derivation-version.mjs';
import { CHUNKER_VERSION, TOKENIZER_VERSION, EMBEDDING_TEXT_VERSION, STRUCTURED_CHUNKER_VERSION, STRUCTURED_EMBEDDING_TEXT_VERSION,
  chunkStructuredSource, embeddingTextForChunk } from '../../data/retrieval/retrieval-text.mjs';
import { readSourceFile } from '../../tools/retrieval/source-reader.mjs';
import { toolFailure } from '../../platform/tool-paths.mjs';
import { sourceIdentity } from './source-projection.mjs';

const MAX_BATCH_SOURCES = 4;
const MAX_EMBEDDING_BATCH_CHUNKS = 32;
const MAX_FINGERPRINTS = 64000;
const MAX_DIAGNOSTIC_CODES = 8;
const embeddingVersion = modelVersion => modelVersion ? `${modelVersion}|${EMBEDDING_TEXT_VERSION}` : undefined;
const EMBEDDING_CONTRACT_FIELDS = ['profileId', 'modelVersion', 'inputProjectionVersion', 'embeddingSpaceId', 'dimensions'];

function sourceMetadata(input) {
  if (input.text !== undefined) return validateSource(input);
  if (!/^[a-f0-9]{64}$/u.test(input.contentHash ?? ''))
    throw toolFailure('资料描述缺少有效内容哈希。', 'RETRIEVAL_SOURCE_CHANGED', 409);
  const source = validateSource({ ...input, text: '', contentHash: undefined });
  return { ...source, text: undefined, contentHash: input.contentHash, sourceRevision: input.sourceRevision ?? input.contentHash };
}

function fingerprintFor(source, versions, status, settings, semantic) {
  return sourceIdentity(versions.derivationSignature, versions.embeddingInputSignature, source.bindingRevision,
    settings.local.semantic, settings.local.embeddingProfileId, semantic ? embeddingVersion(status.modelVersion) : null,
    semantic ? status.embeddingSpaceId ?? null : null, semantic ? status.inputProjectionVersion ?? null : null,
    semantic ? status.dimensions ?? null : null);
}

function compatibleEmbeddingContract(expected, actual) {
  return EMBEDDING_CONTRACT_FIELDS.every(field => expected[field] === undefined || actual[field] === undefined ||
    expected[field] === actual[field]);
}

function preparationVersion(structures) {
  if (!structures) return `${CHUNKER_VERSION}|${TOKENIZER_VERSION}|${EMBEDDING_TEXT_VERSION}`;
  if (['unavailable', 'closed'].includes(structures.status?.()?.state)) return undefined;
  const version = structures.version?.() ?? structures.derivationVersion;
  return typeof version === 'string' && version.length > 0 && version.length <= 512 ? version : undefined;
}

function preparationSignature(source) {
  return sourceIdentity(source.sourceId, source.scopeKey, source.sourceType, source.contentHash, source.sourceRevision,
    source.bindingRevision, source.title, source.locator, source.parserVersion, source.chunkerVersion,
    source.tokenizerVersion, source.embeddingInputVersion, source.structure);
}

/** Bounded derivation and publication; cancellation prevents only uncommitted batches.
 * 分批派生与发布，取消阻止尚未提交的批次，已经完成的索引提交不回滚。 */
export class SourceIndexer {
  constructor({ library, index, embeddings, structures, serialize, effectiveSettings, getProject,
    excludedRoots = [], readFile = readSourceFile }) {
    this.library = library;
    this.index = index;
    this.embeddings = embeddings;
    this.structures = structures;
    this.serialize = serialize;
    this.effectiveSettings = effectiveSettings;
    this.getProject = getProject;
    this.excludedRoots = excludedRoots;
    this.readFile = readFile;
    this.fingerprints = new Map();
    this.lastEmbeddingError = undefined;
    this.preparationFailures = 0;
  }

  invalidate(sourceId) {
    this.fingerprints.delete(`${sourceId}:lexical`);
    this.fingerprints.delete(`${sourceId}:semantic`);
  }

  preparationVersion() { return preparationVersion(this.structures) ?? null; }

  async restore(sources, settings, records, signal) {
    const scopes = [...new Set(sources.map(source => source.scopeKey))];
    if (!scopes.length || !records.length) return;
    const published = new Map((await this.index.listSources({ scopeKeys: scopes, signal })).map(source => [source.sourceId, source]));
    const current = new Map(sources.map(source => [source.sourceId, sourceMetadata(source)]));
    const serviceVersion = preparationVersion(this.structures);
    const serviceIdentity = this.structures?.identity?.() ?? this.structures;
    const status = this.embeddings.status(settings.local.embeddingProfileId);
    for (const record of records) {
      signal?.throwIfAborted();
      const source = current.get(record.sourceId), row = published.get(record.sourceId);
      if (!source || !row || !serviceVersion || record.preparationVersion !== serviceVersion ||
          preparationSignature(source) !== record.inputSignature || row.contentHash !== source.contentHash ||
          row.sourceRevision !== source.sourceRevision || row.scopeKey !== source.scopeKey ||
          (row.bindingRevision ?? 0) !== (source.bindingRevision ?? 0) || row.derivationSignature !== record.derivationSignature ||
          row.embeddingInputSignature !== record.embeddingInputSignature || row.chunkCount !== record.chunkCount ||
          row.vectorChunks !== record.vectorChunks ||
          record.vectorChunks > 0 && (row.embeddingProfileId !== record.embeddingProfileId ||
            row.embeddingModelVersion !== record.embeddingModelVersion || row.embeddingSpaceId !== record.embeddingSpaceId ||
            row.vectorDimensions !== record.vectorDimensions)) continue;
      const semanticRequested = record.semantic && settings.local.semantic !== 'off' && settings.local.embeddingProfileId !== null;
      if (semanticRequested && (record.vectorChunks !== record.chunkCount ||
          record.embeddingProfileId !== settings.local.embeddingProfileId ||
          record.embeddingModelVersion !== embeddingVersion(status.modelVersion) ||
          (record.embeddingSpaceId ?? null) !== (status.embeddingSpaceId ?? null) ||
          status.dimensions !== undefined && record.vectorDimensions !== status.dimensions)) continue;
      if (fingerprintFor(source, record, status, settings, record.semantic) !== record.fingerprint) continue;
      // Restart recovery hashes the current authorized source once; a persisted manifest is not path authority.
      // 重启恢复时重新校验当前已授权来源哈希一次，持久清单不能替代当前路径授权。
      if (!await this.backgroundSourceActive(source, settings, signal)) continue;
      const cached = {
        fingerprint: record.fingerprint, preparation: { service: serviceIdentity, serviceVersion,
          inputSignature: record.inputSignature, chunkCount: record.chunkCount,
          derivationSignature: record.derivationSignature, embeddingInputSignature: record.embeddingInputSignature },
        checkpoint: record
      };
      this.fingerprints.set(`${source.sourceId}:${record.semantic ? 'semantic' : 'lexical'}`, cached);
      if (record.semantic) this.fingerprints.set(`${source.sourceId}:lexical`, { ...cached,
        fingerprint: fingerprintFor(source, record, status, settings, false) });
    }
    while (this.fingerprints.size > MAX_FINGERPRINTS) this.fingerprints.delete(this.fingerprints.keys().next().value);
  }

  async upsert(sources, settings, signal, progress, { semantic = true, loadSource, isCurrent } = {}) {
    const profileId = settings.local.embeddingProfileId;
    const semanticRequested = semantic && settings.local.semantic !== 'off' && profileId !== null;
    const diagnosticCodes = new Set();
    let totalChunks = 0, vectorChunks = 0, cachedChunks = 0, skippedSources = 0, skippedChunks = 0;
    const recordDiagnostic = (code, fallback = 'EMBEDDING_FAILED') => {
      if (diagnosticCodes.size < MAX_DIAGNOSTIC_CODES)
        diagnosticCodes.add(typeof code === 'string' && /^[A-Z][A-Z0-9_]{0,127}$/.test(code) ? code : fallback);
    };
    const report = () => ({ semantic: { requested: semanticRequested, profileId: profileId ?? null,
      state: !semanticRequested ? 'disabled' : vectorChunks === totalChunks ? 'complete' : vectorChunks ? 'partial' : 'unavailable',
      totalChunks, vectorChunks, cachedChunks, skippedSources, skippedChunks, diagnosticCodes: [...diagnosticCodes] } });
    const currentFingerprint = (source, versions, status) => fingerprintFor(source, versions, status, settings, semantic);
    let reuseSettings = new Map();
    const reuse = async (source, count, key, entry) => {
      if (!semanticRequested && !isCurrent) return 'reused';
      const projectId = settings.projectId ?? (source.scopeKey.startsWith('project:') ? source.scopeKey.slice('project:'.length) : null);
      if (isCurrent && !reuseSettings.has(projectId)) reuseSettings.set(projectId, await this.effectiveSettings(projectId));
      const currentSettings = isCurrent ? reuseSettings.get(projectId) : settings;
      const isActive = isCurrent ? currentSettings.local.semantic === settings.local.semantic &&
        currentSettings.local.embeddingProfileId === settings.local.embeddingProfileId &&
        (source.sourceType !== 'work-file' || currentSettings.projectIndexing?.mountedFolder &&
          currentSettings.projectIndexing.bindingRevision === source.bindingRevision) &&
        await isCurrent(source, signal) : await this.backgroundSourceActive(source, settings, signal);
      if (isActive) {
        if (this.fingerprints.get(key) !== entry) return false;
        if (semanticRequested) { cachedChunks += count; vectorChunks += count; }
      } else {
        skippedSources++;
        skippedChunks += count;
        recordDiagnostic('STALE_RETRIEVAL_SOURCE');
      }
      return isActive ? 'reused' : 'skipped';
    };
    const requestedBatchSize = settings.local.indexing?.batchSize ?? MAX_BATCH_SOURCES;
    if (!Number.isSafeInteger(requestedBatchSize) || requestedBatchSize < 1 || requestedBatchSize > 128)
      throw toolFailure('索引批次大小无效。', 'INVALID_RETRIEVAL_SETTINGS', 400);
    // Four maximum-sized sources stay within SQLite's 8 Mi-character atomic publication and derivation memory bounds.
    // 最多四份最大来源满足 SQLite 的八百万字符原子发布限制，并约束派生内存。
    const batchSize = Math.min(requestedBatchSize, MAX_BATCH_SOURCES);
    for (let offset = 0; offset < sources.length; offset += batchSize) {
      signal?.throwIfAborted();
      reuseSettings = new Map();
      const batch = sources.slice(offset, offset + batchSize);
      const prepared = [];
      const fingerprintUpdates = new Map();
      const checkpointSources = [];
      for (const input of batch) {
        signal?.throwIfAborted();
        let source = sourceMetadata(input), chunks;
        const fingerprintKey = `${source.sourceId}:${semantic ? 'semantic' : 'lexical'}`;
        const service = this.structures, serviceVersion = preparationVersion(service);
        const serviceIdentity = service?.identity?.() ?? service;
        const inputSignature = preparationSignature(source);
        const previous = this.fingerprints.get(fingerprintKey);
        let status = this.embeddings.status(profileId);
        // Retain only publication metadata, so large scopes can skip parsing without retaining whole chunk arrays.
        // 只保留已发布派生的轻量元信息，大范围未变来源可跳过解析，不驻留整份分块数组。
        if (serviceVersion && previous?.preparation && previous.preparation.service === serviceIdentity &&
            previous.preparation.serviceVersion === serviceVersion && previous.preparation.inputSignature === inputSignature &&
            previous.fingerprint === currentFingerprint(source, previous.preparation, status)) {
          const cached = await reuse(source, previous.preparation.chunkCount, fingerprintKey, previous);
          if (cached) {
            totalChunks += previous.preparation.chunkCount;
            if (cached === 'reused' && previous.checkpoint) checkpointSources.push(previous.checkpoint);
            continue;
          }
        }
        if (source.text === undefined) {
          if (!loadSource) throw toolFailure('资料正文加载器不可用。', 'RETRIEVAL_SOURCE_UNAVAILABLE', 409);
          try { source = validateSource(await loadSource(source, signal)); }
          catch (error) {
            if (signal?.aborted || error.code !== 'STALE_RETRIEVAL_SOURCE') throw error;
            skippedSources++;
            recordDiagnostic('STALE_RETRIEVAL_SOURCE');
            continue;
          }
          signal?.throwIfAborted();
          if (preparationSignature(source) !== inputSignature)
            throw toolFailure('资料在派生前已经变化。', 'STALE_RETRIEVAL_SOURCE', 409);
        }
        if (this.structures) {
          try {
            const parsed = await this.structures.parse(source, { signal });
            signal?.throwIfAborted();
            source = { ...source, structure: parsed.structure, parserVersion: parsed.parserVersion,
              chunkerVersion: parsed.chunkerVersion, embeddingInputVersion: parsed.embeddingInputVersion };
            chunks = parsed.chunks;
          } catch (error) {
            if (signal?.aborted) throw error;
            // A parser fault preserves real lexical text and reports unavailable structure, never invented symbols.
            // 解析故障保留真实词法正文并标记结构不可用，不生成虚构符号。
            const path = source.locator.relativePath ?? source.locator.path ?? source.title;
            const code = !['memory', 'message'].includes(source.sourceType) && /\.(?:cs|[cm]?js|jsx|[cm]?ts|tsx|py|rs|go)$/iu.test(path);
            const structure = { domain: code ? 'code' : 'knowledge',
              language: null, parserVersion: 'plain-text-v1', parseStatus: 'unavailable',
              diagnosticCodes: [/^[A-Z][A-Z0-9_]{0,127}$/.test(error.code ?? '') ? error.code : 'STRUCTURE_PARSE_FAILED'] };
            source = { ...source, structure, parserVersion: structure.parserVersion,
              chunkerVersion: STRUCTURED_CHUNKER_VERSION, embeddingInputVersion: STRUCTURED_EMBEDDING_TEXT_VERSION };
            chunks = chunkStructuredSource(source, { ...structure, units: [] },
              { checkCancelled: () => signal?.throwIfAborted() });
          }
        } else chunks = chunkSource(source);
        if (source.structure?.parseStatus === 'unavailable' &&
            !source.structure.diagnosticCodes?.some(code => ['UNSUPPORTED_CODE_LANGUAGE', 'CODE_LANGUAGE_UNSUPPORTED'].includes(code)))
          this.preparationFailures++;
        totalChunks += chunks.length;
        const versions = deriveSourceVersion(source, chunks, { checkCancelled: () => signal?.throwIfAborted() });
        status = this.embeddings.status(profileId);
        const fingerprint = currentFingerprint(source, versions, status);
        const preparation = serviceVersion && preparationVersion(this.structures) === serviceVersion &&
          this.structures === service && (this.structures?.identity?.() ?? this.structures) === serviceIdentity &&
          source.structure?.parseStatus !== 'unavailable'
          ? { service: serviceIdentity, serviceVersion, inputSignature, chunkCount: chunks.length,
            derivationSignature: versions.derivationSignature, embeddingInputSignature: versions.embeddingInputSignature } : undefined;
        const derivedPrevious = this.fingerprints.get(fingerprintKey);
        const cached = derivedPrevious?.fingerprint === fingerprint && await reuse(source, chunks.length, fingerprintKey, derivedPrevious);
        if (cached) {
          this.fingerprints.set(fingerprintKey, { fingerprint, ...(preparation ? { preparation } : {}),
            ...(derivedPrevious.checkpoint ? { checkpoint: derivedPrevious.checkpoint } : {}) });
          if (cached === 'reused' && derivedPrevious.checkpoint) checkpointSources.push(derivedPrevious.checkpoint);
          continue;
        }
        const vectors = chunks.map(() => null);
        let embeddingModelVersion;
        let embeddingSpaceId;
        let embeddedProfileId;
        let firstEmbeddingReceipt;
        let vectorDimensions;
        if (semanticRequested && ['ready', 'loading'].includes(status.state)) {
          for (let chunkOffset = 0; chunkOffset < chunks.length; chunkOffset += MAX_EMBEDDING_BATCH_CHUNKS) {
            signal?.throwIfAborted();
            const slice = chunks.slice(chunkOffset, chunkOffset + MAX_EMBEDDING_BATCH_CHUNKS);
            try {
              const embedded = await this.embeddings.embedDocuments(slice.map(item => embeddingTextForChunk(source, item)), { signal, profileId });
              signal?.throwIfAborted();
              if (!compatibleEmbeddingContract({ ...status, profileId }, embedded) ||
                  firstEmbeddingReceipt && !compatibleEmbeddingContract(firstEmbeddingReceipt, embedded) ||
                  !Array.isArray(embedded.vectors) || embedded.vectors.length !== slice.length)
                throw toolFailure('嵌入结果与所选模型或批次不匹配。', 'EMBEDDING_PROFILE_MISMATCH', 409);
              for (const vector of embedded.vectors) {
                const expectedDimensions = status.dimensions ?? embedded.dimensions ?? vectorDimensions;
                if (!(Array.isArray(vector) || vector instanceof Float32Array) || !vector.length ||
                    expectedDimensions !== undefined && vector.length !== expectedDimensions ||
                    !vector.every(Number.isFinite) || !vector.some(value => value !== 0))
                  throw toolFailure('嵌入向量与所选模型维数不匹配。', 'EMBEDDING_PROFILE_MISMATCH', 409);
                vectorDimensions ??= vector.length;
              }
              firstEmbeddingReceipt ??= {};
              for (const field of EMBEDDING_CONTRACT_FIELDS)
                if (embedded[field] !== undefined) firstEmbeddingReceipt[field] ??= embedded[field];
              embeddingModelVersion = embeddingVersion(firstEmbeddingReceipt.modelVersion);
              embeddingSpaceId = firstEmbeddingReceipt.embeddingSpaceId;
              embeddedProfileId = firstEmbeddingReceipt.profileId ?? profileId;
              embedded.vectors.forEach((vector, index) => { vectors[chunkOffset + index] = vector; });
            } catch (error) {
              if (signal?.aborted) throw error;
              this.lastEmbeddingError = error.code ?? 'EMBEDDING_FAILED';
              recordDiagnostic(error.code);
              if (error.code === 'EMBEDDING_PROFILE_MISMATCH') {
                // One source cannot publish a mixture of model spaces, even if earlier batches completed.
                // 同一来源不能发布多个模型空间的混合向量，即使之前的批次已经返回也必须全部丢弃。
                vectors.fill(null);
                embeddingModelVersion = undefined;
                embeddingSpaceId = undefined;
                embeddedProfileId = undefined;
                break;
              }
            }
          }
        } else if (semanticRequested) {
          this.lastEmbeddingError = status.errorCode ?? 'EMBEDDING_PROFILE_UNAVAILABLE';
          recordDiagnostic(status.errorCode, 'EMBEDDING_PROFILE_UNAVAILABLE');
        }
        prepared.push({ ...source, chunks, ...versions,
          ...(vectors.some(Boolean) ? { vectors, embeddingProfileId: embeddedProfileId, embeddingModelVersion, embeddingSpaceId } : {}) });
        fingerprintUpdates.set(source.sourceId, {
          cacheEligible: !semanticRequested || vectors.every(Boolean),
          key: fingerprintKey, value: { fingerprint, ...(preparation ? { preparation } : {}) },
          source, versions, chunks, vectors, embeddingModelVersion, embeddingSpaceId, embeddedProfileId, vectorDimensions });
      }
      const publish = async () => {
        signal?.throwIfAborted();
        const current = [];
        for (const source of prepared) {
          if (!semantic || await this.backgroundSourceActive(source, settings, signal)) current.push(source);
          else {
            skippedSources++;
            skippedChunks += source.chunks.length;
            recordDiagnostic('STALE_RETRIEVAL_SOURCE');
          }
        }
        signal?.throwIfAborted();
        if (current.length) {
          const receipt = await this.index.upsertSources(current, { signal });
          const acknowledged = new Set((Array.isArray(receipt?.sources) ? receipt.sources : []).map(source => source.sourceId));
          for (const source of current) {
            const update = fingerprintUpdates.get(source.sourceId);
            if (!acknowledged.has(source.sourceId) || !update?.value.preparation) continue;
            const checkpoint = { sourceId: source.sourceId, inputSignature: update.value.preparation.inputSignature,
              fingerprint: update.value.fingerprint, semantic, preparationVersion: update.value.preparation.serviceVersion,
              derivationSignature: update.versions.derivationSignature, embeddingInputSignature: update.versions.embeddingInputSignature,
              chunkCount: update.chunks.length, vectorChunks: update.vectors.filter(Boolean).length,
              embeddingProfileId: update.embeddedProfileId ?? null, embeddingModelVersion: update.embeddingModelVersion ?? null,
              embeddingSpaceId: update.embeddingSpaceId ?? null, vectorDimensions: update.vectorDimensions ?? null };
            update.value.checkpoint = checkpoint;
            checkpointSources.push(checkpoint);
          }
          if (!semantic) {
            // Only a proven unchanged publication preserves a semantic cache; fallback derivations may drop vectors.
            // 只有正式回执证实派生未变才保留语义缓存；词法降级可能已经清除原有向量。
            const unchanged = new Set((Array.isArray(receipt?.sources) ? receipt.sources : [])
              .filter(source => source?.unchanged === true).map(source => source.sourceId));
            for (const source of current) if (!unchanged.has(source.sourceId))
              this.fingerprints.delete(`${source.sourceId}:semantic`);
          }
        }
        return new Set(current.map(source => source.sourceId));
      };
      const published = prepared.length ? await (semantic ? this.serialize(publish) : publish()) : new Set();
      if (semanticRequested) for (const source of prepared)
        if (published.has(source.sourceId)) vectorChunks += source.vectors?.filter(Boolean).length ?? 0;
      for (const [sourceId, fingerprint] of fingerprintUpdates)
        if (published.has(sourceId) && fingerprint.cacheEligible) this.fingerprints.set(fingerprint.key, fingerprint.value);
      while (this.fingerprints.size > MAX_FINGERPRINTS) this.fingerprints.delete(this.fingerprints.keys().next().value);
      // Persist completed publication before observing cancellation at the next batch boundary.
      // 已完成发布先记录进度，再在下一个批次边界处理取消，避免丢掉真实完成记录。
      if (progress) await progress(Math.min(sources.length, offset + batch.length), { ...report(),
        ...(checkpointSources.length ? { checkpointSources } : {}) });
    }
    return report();
  }

  async backgroundSourceActive(source, settings, signal) {
    signal?.throwIfAborted();
    const settingsProjectId = settings.projectId ?? (source.scopeKey.startsWith('project:') ? source.scopeKey.slice('project:'.length) : null);
    const effective = await this.effectiveSettings(settingsProjectId);
    signal?.throwIfAborted();
    if (effective.local.semantic !== settings.local.semantic || effective.local.embeddingProfileId !== settings.local.embeddingProfileId)
      return false;
    if (source.sourceType === 'knowledge') {
      const current = await this.library.readSource(source.sourceId,
        { scopeKeys: [source.scopeKey], sourceRevision: source.sourceRevision, signal });
      signal?.throwIfAborted();
      return current?.contentHash === source.contentHash;
    }
    if (source.sourceType !== 'work-file') return false;
    const projectId = source.scopeKey.slice('project:'.length);
    const project = await this.getProject(projectId);
    signal?.throwIfAborted();
    if (!effective.projectIndexing?.mountedFolder || effective.projectIndexing.bindingRevision !== source.bindingRevision ||
        !project?.FolderPath || resolve(project.FolderPath) !== source.locator.root) return false;
    try {
      return (await this.readFile(source.locator.path, { root: source.locator.root, excludedRoots: this.excludedRoots, signal })).contentHash === source.contentHash;
    } catch (error) {
      if (signal?.aborted) throw error;
      return false;
    }
  }

  async prune(snapshot, signal, { sourceTypes = ['knowledge', 'work-file', 'memory', 'message'] } = {}) {
    for (const sourceType of sourceTypes) {
      signal?.throwIfAborted();
      const previous = await this.index.listSources({ scopeKeys: snapshot.scopes, sourceType, signal });
      for (const source of previous) if (!snapshot.identities.has(source.sourceId)) {
        signal?.throwIfAborted();
        await this.index.removeSource(source.sourceId, { scopeKeys: snapshot.scopes, permanent: false, signal });
        this.invalidate(source.sourceId);
      }
    }
  }
}

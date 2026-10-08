import { resolve } from 'node:path';
import { chunkSource } from '../../data/retrieval/index.mjs';
import { validateSource, sourceFileRevision } from '../../data/retrieval/retrieval-contracts.mjs';
import { deriveSourceVersion, canonicalMetadata } from '../../data/retrieval/derivation-version.mjs';
import { CHUNKER_VERSION, TOKENIZER_VERSION, EMBEDDING_TEXT_VERSION, STRUCTURED_CHUNKER_VERSION, STRUCTURED_EMBEDDING_TEXT_VERSION,
  chunkStructuredSource, embeddingProjectionForChunk } from '../../data/retrieval/retrieval-text.mjs';
import { applyTokenFit, embeddingInputForChunk } from '../../data/retrieval/token-chunks.mjs';
import { readSourceFile, readSourceFileWindow } from '../../tools/retrieval/source-reader.mjs';
import { toolFailure } from '../../platform/tool-paths.mjs';
import { sourceIdentity } from './source-projection.mjs';
import { prioritizeEvidenceSources } from './query-plan.mjs';

const MAX_BATCH_SOURCES = 4;
const MAX_PUBLICATION_SOURCES = 100;
const MAX_PUBLICATION_CHARACTERS = 8 * 1024 * 1024;
const MAX_UNMEASURED_SOURCE_CHARACTERS = 2 * 1024 * 1024;
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
    source.bindingRevision, source.title, canonicalMetadata(source.locator), source.parserVersion, source.chunkerVersion,
    source.tokenizerVersion, source.embeddingInputVersion, canonicalMetadata(source.structure));
}

/** Bounded derivation and publication; cancellation prevents only uncommitted batches.
 * 分批派生与发布，取消阻止尚未提交的批次，已经完成的索引提交不回滚。 */
export class SourceIndexer {
  constructor({ library, index, embeddings, structures, serialize, effectiveSettings, getProject,
    excludedRoots = [], readFile = readSourceFile, resourceService }) {
    this.library = library;
    this.index = index;
    this.embeddings = embeddings;
    this.structures = structures;
    this.serialize = serialize;
    this.effectiveSettings = effectiveSettings;
    this.getProject = getProject;
    this.excludedRoots = excludedRoots;
    this.readFile = readFile;
    this.resources = resourceService;
    this.fingerprints = new Map();
    this.lastEmbeddingError = undefined;
    this.preparationFailures = 0;
  }

  invalidate(sourceId) {
    this.fingerprints.delete(`${sourceId}:lexical`);
    this.fingerprints.delete(`${sourceId}:semantic`);
  }

  preparationVersion(profileId) {
    const parserVersion = preparationVersion(this.structures);
    if (!parserVersion) return null;
    const status = typeof this.embeddings.fitDocuments === 'function' ? this.embeddings.status(profileId) : null;
    return status?.fittingVersion ? `${parserVersion}|${status.fittingVersion}|${sourceIdentity(status.profileId,
      status.modelVersion, status.inputProjectionVersion, status.maxInputTokens, status.embeddingSpaceId)}` : parserVersion;
  }

  async unchangedPublication(source, settings, signal, isCurrent) {
    if (!['knowledge', 'work-file'].includes(source.sourceType) || typeof this.index.listSources !== 'function') return null;
    const [published] = await this.index.listSources({ scopeKeys: [source.scopeKey], sourceId: source.sourceId, signal });
    if (!published || published.sourceId !== source.sourceId || published.scopeKey !== source.scopeKey ||
        published.sourceType !== source.sourceType || published.contentHash !== source.contentHash ||
        published.sourceRevision !== source.sourceRevision || published.title !== source.title ||
        (published.bindingRevision ?? 0) !== (source.bindingRevision ?? 0) ||
        JSON.stringify(canonicalMetadata(published.locator)) !== JSON.stringify(canonicalMetadata(source.locator))) return null;
    const current = isCurrent ? await isCurrent(source, signal) : await this.backgroundSourceActive(source, settings, signal);
    signal?.throwIfAborted();
    return current ? published : null;
  }

  async fitSourceChunks(source, chunks, status, { profileId, signal }, recordDiagnostic) {
    if (!chunks.length || typeof this.embeddings.fitDocuments !== 'function' || !status.fittingVersion) return { source, chunks };
    const fitted = [];
    for (let offset = 0; offset < chunks.length; offset += MAX_EMBEDDING_BATCH_CHUNKS) {
      signal?.throwIfAborted();
      const batch = chunks.slice(offset, offset + MAX_EMBEDDING_BATCH_CHUNKS);
      const projections = batch.map(chunk => embeddingProjectionForChunk(source, chunk));
      let receipt;
      for (let attempt = 0; attempt <= projections.length; attempt++) {
        try { receipt = await this.embeddings.fitDocuments(projections, { signal, profileId }); break; }
        catch (error) {
          if (signal?.aborted) throw error;
          const index = error.details?.index;
          if (error.code !== 'EMBEDDING_CONTEXT_TOO_LONG' || !Number.isSafeInteger(index) || !projections[index]?.context) throw error;
          // An explicitly oversized header may be omitted; the full original body is still fitted and the omission is recorded.
          // 明确超限的元信息头可省略，完整原文仍按真实 token 拆分，且记录省略诊断。
          projections[index] = { ...projections[index], context: '' };
          recordDiagnostic('EMBEDDING_CONTEXT_OMITTED');
        }
      }
      if (!receipt || receipt.fittingVersion !== status.fittingVersion ||
          !compatibleEmbeddingContract({ ...status, profileId }, receipt) ||
          status.maxInputTokens !== undefined && receipt.maxInputTokens !== status.maxInputTokens)
        throw toolFailure('token 分块回执与所选模型不匹配。', 'EMBEDDING_PROFILE_MISMATCH', 409);
      const tokenChunks = applyTokenFit(source, batch, projections, receipt, { checkCancelled: () => signal?.throwIfAborted() });
      for (const chunk of tokenChunks) {
        const chunkIndex = fitted.length;
        fitted.push({ ...chunk, chunkIndex,
          chunkId: `${chunk.chunkId.slice(0, chunk.chunkId.lastIndexOf(':'))}:${chunkIndex}` });
      }
      if (fitted.length > 40000) throw toolFailure('token 分块超过来源预算。', 'RETRIEVAL_SOURCE_TOO_LARGE', 413);
    }
    return { source: { ...source, chunkerVersion: fitted[0]?.chunkerVersion ?? source.chunkerVersion,
      embeddingInputVersion: `${source.embeddingInputVersion ?? EMBEDDING_TEXT_VERSION}|${status.fittingVersion}` }, chunks: fitted };
  }

  async embedBoundedDocuments(texts, options, recordDiagnostic) {
    const positions = texts.map((_, index) => index), rejectedPositions = new Set();
    for (let attempt = 0; attempt <= texts.length; attempt++) {
      options.signal?.throwIfAborted();
      if (!positions.length) return { vectors: texts.map(() => null), rejectedPositions };
      try {
        const receipt = await this.embeddings.embedDocuments(positions.map(index => texts[index]), options);
        if (!Array.isArray(receipt.vectors) || receipt.vectors.length !== positions.length)
          throw toolFailure('嵌入批次长度不匹配。', 'EMBEDDING_PROFILE_MISMATCH', 409);
        const vectors = texts.map(() => null);
        receipt.vectors.forEach((vector, index) => { vectors[positions[index]] = vector; });
        return { ...receipt, vectors, rejectedPositions };
      } catch (error) {
        const index = error.details?.index;
        if (error.code !== 'EMBEDDING_INPUT_TOO_LONG' || !Number.isSafeInteger(index) || index < 0 || index >= positions.length)
          throw error;
        // Real tokenizer rejection affects only its own block, never every otherwise valid block in the batch.
        // 真实分词器的超限拒绝只影响对应块，不能连带丢弃同批其他合法块，也不截断原文或伪造向量。
        rejectedPositions.add(positions[index]);
        positions.splice(index, 1);
        this.lastEmbeddingError = error.code;
        recordDiagnostic(error.code);
      }
    }
    throw toolFailure('嵌入输入筛选未正常结束。', 'EMBEDDING_PROFILE_MISMATCH', 409);
  }

  async restore(sources, settings, records, signal) {
    const scopes = [...new Set(sources.map(source => source.scopeKey))];
    if (!scopes.length || !records.length) return;
    const published = new Map((await this.index.listSources({ scopeKeys: scopes, signal })).map(source => [source.sourceId, source]));
    const current = new Map(sources.map(source => [source.sourceId, sourceMetadata(source)]));
    const serviceVersion = this.preparationVersion(settings.local.embeddingProfileId);
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

  async upsert(sources, settings, signal, progress, { semantic = true, loadSource, isCurrent, priorities } = {}) {
    const profileId = settings.local.embeddingProfileId;
    const semanticRequested = semantic && settings.local.semantic !== 'off' && profileId !== null;
    const diagnosticCodes = new Set();
    const coverageEntries = new Map(), coverageCounts = { discovered: sources.length, lexical: 0, semantic: 0, failed: 0, skipped: 0, partial: 0 };
    const recordCoverage = (source, status, { lexical = 'unverified', semanticState = semanticRequested ? 'pending' : 'disabled', parser = 'pending', errorCode } = {}) => {
      const old = coverageEntries.get(source.sourceId);
      if (old) {
        if (old.lexical === 'ready') coverageCounts.lexical--;
        if (old.semantic === 'ready') coverageCounts.semantic--;
        if (['failed', 'skipped', 'partial'].includes(old.status)) coverageCounts[old.status]--;
      }
      if (lexical === 'ready') coverageCounts.lexical++;
      if (semanticState === 'ready') coverageCounts.semantic++;
      if (['failed', 'skipped', 'partial'].includes(status)) coverageCounts[status]++;
      coverageEntries.set(source.sourceId, { scopeKey: source.scopeKey, sourceId: source.sourceId, sourceRevision: source.sourceRevision,
        relativePath: source.locator?.relativePath ?? source.title ?? source.sourceId, status, lexical, semantic: semanticState, parser,
        ...(errorCode ? { errorCode: /^[A-Z][A-Z0-9_]{0,127}$/u.test(errorCode) ? errorCode : 'RETRIEVAL_SOURCE_FAILED' } : {}) });
      while (coverageEntries.size > 1000) coverageEntries.delete(coverageEntries.keys().next().value);
    };
    let totalChunks = 0, vectorChunks = 0, cachedChunks = 0, skippedSources = 0, skippedChunks = 0;
    const recordDiagnostic = (code, fallback = 'EMBEDDING_FAILED') => {
      if (diagnosticCodes.size < MAX_DIAGNOSTIC_CODES)
        diagnosticCodes.add(typeof code === 'string' && /^[A-Z][A-Z0-9_]{0,127}$/.test(code) ? code : fallback);
    };
    const report = () => ({ coverage: { ...coverageCounts, complete: !coverageCounts.failed && !coverageCounts.skipped && !coverageCounts.partial,
      sources: [...coverageEntries.values()] }, semantic: { requested: semanticRequested, profileId: profileId ?? null,
      state: !semanticRequested ? 'disabled' : coverageCounts.failed || coverageCounts.skipped ? vectorChunks ? 'partial' : 'unavailable'
        : vectorChunks === totalChunks ? 'complete' : vectorChunks ? 'partial' : 'unavailable',
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
        recordCoverage(source, 'ready', { lexical: 'ready', semanticState: semanticRequested ? 'ready' : 'disabled', parser: 'ready' });
      } else {
        skippedSources++;
        skippedChunks += count;
        recordDiagnostic('STALE_RETRIEVAL_SOURCE');
        recordCoverage(source, 'skipped', { errorCode: 'STALE_RETRIEVAL_SOURCE' });
      }
      return isActive ? 'reused' : 'skipped';
    };
    const requestedBatchSize = settings.local.indexing?.batchSize ?? MAX_BATCH_SOURCES;
    if (!Number.isSafeInteger(requestedBatchSize) || requestedBatchSize < 1 || requestedBatchSize > 128)
      throw toolFailure('索引批次大小无效。', 'INVALID_RETRIEVAL_SETTINGS', 400);
    // Measured small sources can share a larger commit; unknown bodies reserve their complete maximum size.
    // 已测量的小来源可合并较大提交，未知正文按完整上限预留，继续遵守 SQLite 原子发布预算。
    const batchSize = Math.min(requestedBatchSize, MAX_PUBLICATION_SOURCES);
    const orderedSources = priorities ? [...sources] : sources;
    let priorityRevision = -1;
    for (let offset = 0; offset < sources.length;) {
      signal?.throwIfAborted();
      // New foreground targets reorder only remaining work at a safe publication boundary.
      // 新前台目标只在安全发布边界重排剩余工作，不重做已完成批次，也不恢复取消的任务。
      if (priorities && priorityRevision !== priorities.revision) {
        const remaining = prioritizeEvidenceSources(orderedSources.slice(offset), priorities.paths);
        for (let index = 0; index < remaining.length; index++) orderedSources[offset + index] = remaining[index];
        priorityRevision = priorities.revision;
      }
      reuseSettings = new Map();
      const batch = [];
      let batchCharacters = 0;
      for (const source of orderedSources.slice(offset, offset + batchSize)) {
        const window = source.locator?.fileWindow;
        const measured = typeof source.text === 'string' ? source.text.length : window ? window.endOffset - window.startOffset :
          Number.isSafeInteger(source.storedBytes) && source.storedBytes >= 0 ? source.storedBytes : MAX_UNMEASURED_SOURCE_CHARACTERS;
        const characters = Number.isSafeInteger(measured) && measured >= 0 ?
          Math.min(measured, MAX_UNMEASURED_SOURCE_CHARACTERS) : MAX_UNMEASURED_SOURCE_CHARACTERS;
        if (batch.length && batchCharacters + characters > MAX_PUBLICATION_CHARACTERS) break;
        batch.push(source); batchCharacters += characters;
      }
      const prepared = [];
      const fingerprintUpdates = new Map();
      const checkpointSources = [];
      for (const input of batch) {
        signal?.throwIfAborted();
        let source, chunks;
        try { source = sourceMetadata(input); }
        catch (error) {
          if (signal?.aborted || error.name === 'AbortError') throw error;
          skippedSources++; coverageCounts.failed++; recordDiagnostic(error.code, 'RETRIEVAL_SOURCE_FAILED'); continue;
        }
        if (input.unavailable) {
          skippedSources++; recordDiagnostic(input.errorCode, 'RETRIEVAL_SOURCE_FAILED');
          recordCoverage(source, 'failed', { parser: 'failed', errorCode: input.errorCode ?? 'RETRIEVAL_SOURCE_FAILED' }); continue;
        }
        const fingerprintKey = `${source.sourceId}:${semantic ? 'semantic' : 'lexical'}`;
        const service = this.structures, serviceVersion = this.preparationVersion(profileId);
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
          if (!loadSource) {
            skippedSources++; recordDiagnostic('RETRIEVAL_SOURCE_UNAVAILABLE');
            recordCoverage(source, 'failed', { parser: 'failed', errorCode: 'RETRIEVAL_SOURCE_UNAVAILABLE' }); continue;
          }
          try { source = validateSource(await loadSource(source, signal)); }
          catch (error) {
            if (signal?.aborted || error.name === 'AbortError') throw error;
            skippedSources++;
            recordDiagnostic(error.code, 'RETRIEVAL_SOURCE_FAILED');
            recordCoverage(source, error.code === 'STALE_RETRIEVAL_SOURCE' ? 'skipped' : 'failed', { parser: 'failed', errorCode: error.code ?? 'RETRIEVAL_SOURCE_FAILED' });
            continue;
          }
          signal?.throwIfAborted();
          if (preparationSignature(source) !== inputSignature) {
            skippedSources++; recordDiagnostic('STALE_RETRIEVAL_SOURCE'); recordCoverage(source, 'skipped', { errorCode: 'STALE_RETRIEVAL_SOURCE' }); continue;
          }
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
            const code = !['memory', 'message'].includes(source.sourceType) && /\.(?:cs|[cm]?js|jsx|[cm]?ts|tsx|pyi?|rs|go)$/iu.test(path);
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
        let fittingFailed = false;
        const fittingRequired = settings.local.semantic !== 'off' && profileId !== null &&
          typeof this.embeddings.fitDocuments === 'function' && Boolean(status.fittingVersion);
        if (fittingRequired) {
          try {
            if (!['ready', 'loading'].includes(status.state))
              throw toolFailure('当前嵌入模型不可用，token 分块待恢复。', status.errorCode ?? 'EMBEDDING_PROFILE_UNAVAILABLE', 409);
            ({ source, chunks } = await this.fitSourceChunks(source, chunks, status, { profileId, signal }, recordDiagnostic));
          }
          catch (error) {
            if (signal?.aborted) throw error;
            fittingFailed = true;
            this.preparationFailures++;
            this.lastEmbeddingError = error.code ?? 'EMBEDDING_FIT_FAILED';
            recordDiagnostic(this.lastEmbeddingError);
          }
        }
        if (fittingFailed) {
          // A transient fitting fault cannot replace a still-authorized unchanged publication with incompatible raw chunks.
          // 临时拟合故障不能用不兼容原始分块替换仍获授权且未变化的正式派生；不缓存本次降级，恢复后重试。
          const published = await this.unchangedPublication(sourceMetadata(input), settings, signal, isCurrent);
          if (published) {
            totalChunks += published.chunkCount;
            if (semanticRequested && published.embeddingProfileId === profileId &&
                published.embeddingModelVersion === embeddingVersion(status.modelVersion) &&
                published.embeddingSpaceId === status.embeddingSpaceId && published.vectorDimensions === status.dimensions)
              vectorChunks += published.vectorChunks;
            recordCoverage(source, 'partial', { lexical: 'ready', semanticState: published.vectorChunks ? 'partial' : 'pending',
              parser: 'ready', errorCode: this.lastEmbeddingError });
            continue;
          }
        }
        totalChunks += chunks.length;
        const versions = deriveSourceVersion(source, chunks, { checkCancelled: () => signal?.throwIfAborted() });
        status = this.embeddings.status(profileId);
        const fingerprint = currentFingerprint(source, versions, status);
        const preparation = !fittingFailed && serviceVersion && this.preparationVersion(profileId) === serviceVersion &&
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
              const embedded = await this.embedBoundedDocuments(slice.map(item => embeddingInputForChunk(source, item)),
                { signal, profileId }, recordDiagnostic);
              signal?.throwIfAborted();
              if (!compatibleEmbeddingContract({ ...status, profileId }, embedded) ||
                  firstEmbeddingReceipt && !compatibleEmbeddingContract(firstEmbeddingReceipt, embedded) ||
                  !Array.isArray(embedded.vectors) || embedded.vectors.length !== slice.length)
                throw toolFailure('嵌入结果与所选模型或批次不匹配。', 'EMBEDDING_PROFILE_MISMATCH', 409);
              for (const [index, vector] of embedded.vectors.entries()) {
                if (vector === null && embedded.rejectedPositions.has(index)) continue;
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
            recordCoverage(source, 'skipped', { errorCode: 'STALE_RETRIEVAL_SOURCE' });
          }
        }
        signal?.throwIfAborted();
        if (current.length) {
          let receipt;
          try { receipt = await this.index.upsertSources(current, { signal }); }
          catch (error) {
            if (signal?.aborted || error.name === 'AbortError') throw error;
            const receipts = [];
            for (const source of current) {
              try { receipts.push(await this.index.upsertSources([source], { signal })); }
              catch (sourceError) {
                if (signal?.aborted || sourceError.name === 'AbortError') throw sourceError;
                recordDiagnostic(sourceError.code, 'RETRIEVAL_SOURCE_FAILED');
                recordCoverage(source, 'failed', { parser: 'failed', errorCode: sourceError.code ?? 'RETRIEVAL_SOURCE_FAILED' });
              }
            }
            receipt = { sources: receipts.flatMap(item => Array.isArray(item?.sources) ? item.sources : []) };
          }
          const acknowledged = new Set((Array.isArray(receipt?.sources) ? receipt.sources : []).map(source => source.sourceId));
          for (const source of current) {
            if (!acknowledged.has(source.sourceId)) {
              // A resolved call without its publication receipt cannot prove that derived text or vectors committed.
              // 调用已返回但缺少发布回执，不能证明正文派生或向量已经提交，也不能缓存为成功。
              if (coverageEntries.get(source.sourceId)?.status !== 'failed') {
                recordDiagnostic('RETRIEVAL_PUBLICATION_UNVERIFIED');
                recordCoverage(source, 'partial', { parser: 'ready', errorCode: 'RETRIEVAL_PUBLICATION_UNVERIFIED' });
              }
              continue;
            }
            const parser = source.structure?.parseStatus === 'unavailable' ? 'failed' : source.structure?.parseStatus === 'partial' ? 'partial' : 'ready';
            const vectorCount = source.vectors?.filter(Boolean).length ?? 0;
            const semanticState = !semanticRequested ? 'disabled' : vectorCount === source.chunks.length ? 'ready' : vectorCount ? 'partial' : 'pending';
            recordCoverage(source, parser === 'ready' && ['ready', 'disabled'].includes(semanticState) ? 'ready' : 'partial',
              { lexical: 'ready', semanticState, parser });
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
        return new Set(current.filter(source => coverageEntries.get(source.sourceId)?.lexical === 'ready').map(source => source.sourceId));
      };
      const published = prepared.length ? await (semantic ? this.serialize(publish) : publish()) : new Set();
      if (semanticRequested) for (const source of prepared)
        if (published.has(source.sourceId)) vectorChunks += source.vectors?.filter(Boolean).length ?? 0;
      for (const [sourceId, fingerprint] of fingerprintUpdates)
        if (published.has(sourceId) && fingerprint.cacheEligible) this.fingerprints.set(fingerprint.key, fingerprint.value);
      while (this.fingerprints.size > MAX_FINGERPRINTS) this.fingerprints.delete(this.fingerprints.keys().next().value);
      if (this.index.recordCoverage) {
        const entries = batch.map(source => coverageEntries.get(source.sourceId)).filter(Boolean);
        // Publication receipts must remain recorded even when cancellation arrives after the commit.
        // 提交之后才到达的取消不能抹掉已经完成的发布与覆盖回执。
        if (entries.length) await this.index.recordCoverage({ entries, scopeKeys: [...new Set(entries.map(entry => entry.scopeKey))] });
      }
      // Persist completed publication before observing cancellation at the next batch boundary.
      // 已完成发布先记录进度，再在下一个批次边界处理取消，避免丢掉真实完成记录。
      if (progress) await progress(Math.min(sources.length, offset + batch.length), { ...report(),
        ...(priorities ? { processedSourceIds: batch.map(source => source.sourceId) } : {}),
        ...(checkpointSources.length ? { checkpointSources } : {}) });
      offset += batch.length;
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
      const file = source.locator.fileWindow ? await readSourceFileWindow(source.locator.path, source.locator.fileWindow,
        { root: source.locator.root, excludedRoots: this.excludedRoots, maximumSourceBytes: effective.local.indexing?.maximumSourceBytes, signal })
        : await this.readFile(source.locator.path, { root: source.locator.root, excludedRoots: this.excludedRoots,
          maximumSourceBytes: effective.local.indexing?.maximumSourceBytes, signal, resourceService: this.resources });
      return file.contentHash === source.contentHash &&
        (source.locator.extraction === undefined && file.extraction === undefined || sourceFileRevision(file) === source.sourceRevision);
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

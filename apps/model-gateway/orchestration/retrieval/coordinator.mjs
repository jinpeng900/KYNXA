import { watch } from 'node:fs';
import { relative, resolve } from 'node:path';
import { RetrievalSettingsStore } from '../../data/retrieval/settings.mjs';
import { RetrievalIndex, chunkSource } from '../../data/retrieval/index.mjs';
import { EMBEDDING_TEXT_VERSION, embeddingTextForChunk } from '../../data/retrieval/retrieval-text.mjs';
import { MAX_QUERY_CHARACTERS, retrievalFailure } from '../../data/retrieval/retrieval-contracts.mjs';
import { SourceLibrary } from '../../data/retrieval/source-library.mjs';
import { RetrievalJobStore } from '../../data/retrieval/job-store.mjs';
import { EmbeddingService } from '../../models/retrieval/embedding-service.mjs';
import { RerankerService } from '../../models/retrieval/reranker-service.mjs';
import { readSourceTree, readSourceFile } from '../../tools/retrieval/source-reader.mjs';
import { validateId } from '../../platform/conversation-id.mjs';
import { toolFailure } from '../../platform/tool-paths.mjs';
import { estimateTokens } from '../../models/context-tokens.mjs';
import { sourceIdentity, visibleScopes, projectConversationSources, projectEvidence, retrievalPlan, EVIDENCE_NOTICE } from './source-projection.mjs';
import { RETRIEVAL_CANDIDATE_LIMIT, selectCandidates, deduplicateCandidates, assessEvidence } from './candidate-selection.mjs';
import { EvidenceAcquisition, validateEvidenceGap } from './evidence-acquisition.mjs';
import { allocateEvidenceArchiveId, evidenceSourceRef, EvidenceReferenceStore } from '../../data/retrieval/evidence-references.mjs';

const SOURCE_CACHE_TTL_MS = 2000;
const MAX_SOURCE_CACHE_ENTRIES = 32;
const embeddingVersion = modelVersion => modelVersion ? `${modelVersion}|${EMBEDDING_TEXT_VERSION}` : undefined;

/** Application coordinator composes E-owned storage, C-owned inference and D-owned file reads.
 * 应用协调层组合数据存储、嵌入推理和工具文件读取，不改变正式会话或记忆归属。 */
export class RetrievalCoordinator {
  #preparedEvidence = new WeakMap();

  constructor({ conversations, memory, tools, index, embeddings, reranker, excludedRoots = [] }) {
    this.conversations = conversations; this.memory = memory; this.tools = tools;
    this.settings = new RetrievalSettingsStore({ root: conversations.root, conversationStore: conversations });
    this.index = index ?? new RetrievalIndex({ root: conversations.root });
    this.embeddings = embeddings ?? new EmbeddingService();
    this.reranker = reranker === undefined ? new RerankerService() : reranker;
    this.library = new SourceLibrary({ root: conversations.root, conversationStore: conversations });
    this.evidenceReferences = tools.results ? new EvidenceReferenceStore({ conversationStore: conversations, resultStore: tools.results }) : null;
    this.jobs = new RetrievalJobStore(conversations.root);
    this.excludedRoots = [conversations.root, ...excludedRoots].filter(Boolean);
    this.fingerprints = new Map(); this.activeJobs = new Map(); this.watchers = new Map(); this.mountedCache = new Map();
    this.libraryCache = new Map(); this.conversationCache = new Map();
    this.acquisitions = new WeakMap();
    this.queue = Promise.resolve(); this.closed = false;
  }

  async initialize() {
    if (!this.initialization) this.initialization = this.jobs.recover();
    await this.initialization;
  }

  _serialize(operation) {
    const pending = this.queue.catch(() => {}).then(operation);
    this.queue = pending; return pending;
  }

  async effective(projectId) { return this.settings.getEffective(projectId); }

  async status() {
    await this.initialize();
    const indexed = await this.index.status(), embedding = this.embeddings.status();
    return { ...indexed, backend: 'sqlite', sourceCount: indexed.sources, chunkCount: indexed.chunks,
      embedding: { ...embedding, available: ['ready', 'loading'].includes(embedding.state) },
      reranking: this.reranker?.status() ?? { state: 'disabled', loaded: false }, jobs: await this.jobs.list() };
  }

  async _project(projectId) {
    if (!projectId) return null;
    const project = await this.conversations.describeProject(projectId);
    if (project.isArchived || project.isFolderlessWorkspace) return null;
    return { Id: project.projectId, FolderPath: project.folderPath };
  }

  _trimSourceCaches(memoryLimitBytes) {
    const caches = [this.libraryCache, this.conversationCache, this.mountedCache];
    let retainedBytes = caches.flatMap(current => [...current.values()])
      .reduce((total, entry) => total + entry.memoryBytes, 0);
    while (retainedBytes > memoryLimitBytes || caches.some(current => current.size > MAX_SOURCE_CACHE_ENTRIES)) {
      const candidates = retainedBytes > memoryLimitBytes ? caches : caches.filter(current => current.size > MAX_SOURCE_CACHE_ENTRIES);
      let oldest;
      for (const current of candidates) for (const [key, entry] of current)
        if (!oldest || entry.capturedAt < oldest.entry.capturedAt) oldest = { cache: current, key, entry };
      if (!oldest) break;
      retainedBytes -= oldest.entry.memoryBytes;
      oldest.cache.delete(oldest.key);
    }
  }

  async _librarySources(scopes, settings) {
    this._trimSourceCaches(settings.cache.memoryLimitBytes);
    const catalog = await this.library.list(settings.projectId);
    const key = JSON.stringify(scopes.filter(scope => scope === 'user' || scope.startsWith('project:'))), cached = this.libraryCache.get(key);
    if (cached?.revision === catalog.revision && performance.now() - cached.capturedAt < SOURCE_CACHE_TTL_MS)
      return cached.sources;
    const sources = await this.library.readAll(scopes);
    const memoryBytes = sources.reduce((total, source) => total + source.text.length * 2, 0);
    this.libraryCache.set(key, { revision: catalog.revision, capturedAt: performance.now(), sources, memoryBytes });
    this._trimSourceCaches(settings.cache.memoryLimitBytes);
    return sources;
  }

  _conversationSources(relationship, entries, messages, settings) {
    this._trimSourceCaches(settings.cache.memoryLimitBytes);
    const revision = sourceIdentity(visibleScopes(relationship), entries.map(entry => [entry.id, entry.revision, entry.active, entry.content]),
      messages.map(message => [message.Id, message.Role, message.Status, message.Content]));
    const key = relationship.conversationId, cached = this.conversationCache.get(key);
    if (cached?.revision === revision && performance.now() - cached.capturedAt < SOURCE_CACHE_TTL_MS) return cached.sources;
    const sources = projectConversationSources(relationship, entries, messages);
    const memoryBytes = sources.reduce((total, source) => total + source.text.length * 2, 0);
    this.conversationCache.set(key, { revision, capturedAt: performance.now(), sources, memoryBytes });
    this._trimSourceCaches(settings.cache.memoryLimitBytes);
    return sources;
  }

  async _mountedSources(projectId, settings, signal) {
    projectId = projectId?.toLowerCase() ?? null;
    const project = await this._project(projectId);
    if (!project?.FolderPath || !settings.projectIndexing?.mountedFolder) {
      this.watchers.get(projectId)?.close(); this.watchers.delete(projectId); this.mountedCache.delete(projectId); return [];
    }
    const root = resolve(project.FolderPath), bindingRevision = settings.projectIndexing.bindingRevision;
    this._watch(projectId, root);
    const cached = this.mountedCache.get(projectId);
    if (cached?.root === root && cached.bindingRevision === bindingRevision && performance.now() - cached.capturedAt < 2000)
      return cached.sources;
    const files = await readSourceTree(root, { root, excludedRoots: this.excludedRoots, signal });
    const sources = files.map(file => ({ sourceId: sourceIdentity('work-file', projectId, root, relative(root, file.path)),
      scopeKey: `project:${projectId.toLowerCase()}`, sourceType: 'work-file', title: file.title,
      locator: { path: file.path, relativePath: relative(root, file.path), root }, text: file.text,
      contentHash: file.contentHash, sourceRevision: file.contentHash, bindingRevision }));
    const memoryBytes = sources.reduce((total, source) => total + source.text.length * 2, 0);
    const memoryLimit = settings.cache.memoryLimitBytes;
    let cachedBytes = [...this.mountedCache.values()].reduce((total, entry) => total + entry.memoryBytes, 0);
    for (const [id, entry] of this.mountedCache) {
      if (cachedBytes + memoryBytes <= memoryLimit) break;
      this.mountedCache.delete(id); cachedBytes -= entry.memoryBytes;
    }
    if (memoryBytes <= memoryLimit) this.mountedCache.set(projectId, { root, bindingRevision, capturedAt: performance.now(), sources, memoryBytes });
    this._trimSourceCaches(memoryLimit);
    return sources;
  }

  _watch(projectId, root) {
    const current = this.watchers.get(projectId);
    if (current?.root === root) return;
    current?.close();
    let timer;
    try {
      const watcher = watch(root, { recursive: true, persistent: false }, () => {
        this.mountedCache.delete(projectId);
        clearTimeout(timer);
        timer = setTimeout(() => { if (!this.closed) this.rebuild({ projectId }).catch(() => {}); }, 800);
        timer.unref();
      });
      watcher.on('error', () => { watcher.close(); this.watchers.delete(projectId); });
      this.watchers.set(projectId, { root, close: () => { clearTimeout(timer); watcher.close(); } });
    } catch { /* Freshness is still checked at every query. / 监听不可用时仍逐次校验来源。 */ }
  }

  async _upsert(sources, settings, signal, progress, { semantic = true } = {}) {
    // Four maximal sources fit the data-layer's eight-MiB publication budget.
    // 四份上限资料符合数据层每批八 MiB 的发布预算。
    for (let offset = 0; offset < sources.length; offset += 4) {
      signal?.throwIfAborted();
      const batch = sources.slice(offset, offset + 4), prepared = [], fingerprintUpdates = new Map();
      for (const source of batch) {
        const fingerprint = sourceIdentity(source.contentHash, source.sourceRevision, source.bindingRevision,
          source.locator, settings.local.semantic, settings.local.embeddingProfileId, EMBEDDING_TEXT_VERSION,
          semantic ? embeddingVersion(this.embeddings.status().modelVersion) : null);
        const fingerprintKey = `${source.sourceId}:${semantic ? 'semantic' : 'lexical'}`;
        if (this.fingerprints.get(fingerprintKey) === fingerprint) continue;
        const chunks = chunkSource(source);
        const vectors = chunks.map(() => null);
        let embeddingModelVersion;
        if (semantic && settings.local.semantic !== 'off' && settings.local.embeddingProfileId === 'builtin-multilingual' &&
            ['ready', 'loading'].includes(this.embeddings.status().state)) {
          // Split inference into bounded batches; a failed semantic item retains its lexical index.
          // 分批推理限制资源；语义失败时保留词法索引，不伪造向量或静默截断原文。
          for (let chunkOffset = 0; chunkOffset < chunks.length; chunkOffset += 32) {
            signal?.throwIfAborted();
            const slice = chunks.slice(chunkOffset, chunkOffset + 32);
            try {
              const embedded = await this.embeddings.embedDocuments(slice.map(item => embeddingTextForChunk(source, item)), { signal });
              embeddingModelVersion = embeddingVersion(embedded.modelVersion);
              embedded.vectors.forEach((vector, i) => { vectors[chunkOffset + i] = vector; });
            } catch (error) { if (signal?.aborted) throw error; this.lastEmbeddingError = error.code ?? 'EMBEDDING_FAILED'; }
          }
        }
        prepared.push({ ...source, chunks, ...(vectors.some(Boolean) ? { vectors, embeddingProfileId: 'builtin-multilingual', embeddingModelVersion } : {}) });
        if (!semantic || settings.local.semantic === 'off' || vectors.every(Boolean))
          fingerprintUpdates.set(source.sourceId, { key: fingerprintKey, value: fingerprint });
      }
      const publish = async () => {
        signal?.throwIfAborted();
        const current = [];
        for (const source of prepared) {
          if (!semantic || await this._backgroundSourceActive(source, settings, signal)) current.push(source);
        }
        if (current.length) await this.index.upsertSources(current, { signal });
        return new Set(current.map(source => source.sourceId));
      };
      const published = prepared.length ? await (semantic ? this._serialize(publish) : publish()) : new Set();
      for (const [sourceId, fingerprint] of fingerprintUpdates) if (published.has(sourceId))
        this.fingerprints.set(fingerprint.key, fingerprint.value);
      while (this.fingerprints.size > 8192) this.fingerprints.delete(this.fingerprints.keys().next().value);
      if (progress) await progress(Math.min(sources.length, offset + batch.length));
    }
  }

  async _backgroundSourceActive(source, settings, signal) {
    if (source.sourceType === 'knowledge') {
      const current = await this.library.readSource(source.sourceId,
        { scopeKeys: [source.scopeKey], sourceRevision: source.sourceRevision });
      return current?.contentHash === source.contentHash;
    }
    if (source.sourceType !== 'work-file') return false;
    const projectId = source.scopeKey.slice('project:'.length), effective = await this.effective(projectId);
    const project = await this._project(projectId);
    if (!effective.projectIndexing?.mountedFolder || effective.projectIndexing.bindingRevision !== source.bindingRevision ||
        !project?.FolderPath || resolve(project.FolderPath) !== source.locator.root || effective.local.semantic !== settings.local.semantic) return false;
    try { return (await readSourceFile(source.locator.path, { root: source.locator.root, excludedRoots: this.excludedRoots, signal })).contentHash === source.contentHash; }
    catch { return false; }
  }

  async _scopeSnapshot(context, signal) {
    signal?.throwIfAborted();
    const relationship = await this.conversations.describeConversation(validateId(context.conversationId));
    if (relationship.isArchived || relationship.projectArchived) throw toolFailure('聊天已归档。', 'RETRIEVAL_SCOPE_UNAVAILABLE', 409);
    const expectedProjectId = context.projectId?.toLowerCase() ?? null;
    const actualProjectId = relationship.projectId?.toLowerCase() ?? null;
    const projectMatches = expectedProjectId === actualProjectId || relationship.isFolderlessWorkspace && expectedProjectId === null;
    if (Object.hasOwn(context, 'projectId') && !projectMatches)
      throw toolFailure('工作关联已变化，请开始新请求。', 'RETRIEVAL_SCOPE_CHANGED', 409);
    const settings = await this.effective(relationship.isFolderlessWorkspace ? null : relationship.projectId);
    const scopes = visibleScopes(relationship);
    return { relationship, settings, scopes, sources: [], identities: new Map() };
  }

  _beforeCurrentMessage(messages, context) {
    const currentAssistant = messages.find(item => item.Id === context.requestId && item.Role === 'assistant');
    const currentUserIndex = messages.findIndex(item => item.Id === (currentAssistant?.ReplyTo ?? context.currentMessageId));
    return currentUserIndex >= 0 ? messages.slice(0, currentUserIndex) : messages;
  }

  async _snapshot(context, signal) {
    const { relationship, settings, scopes } = await this._scopeSnapshot(context, signal);
    if (!settings.local.enabled) return { relationship, settings, scopes, sources: [], identities: new Map() };
    const memory = await this.memory.contextFor(relationship.conversationId);
    const messages = this._beforeCurrentMessage(await this.conversations.readMessages(relationship.conversationId), context);
    const sources = [...await this._librarySources(scopes, settings), ...this._conversationSources(relationship, memory.entries, messages, settings),
      ...await this._mountedSources(relationship.isFolderlessWorkspace ? null : relationship.projectId, settings, signal)];
    return { relationship, settings, scopes, sources, identities: new Map(sources.map(source => [source.sourceId, source])) };
  }

  async _prune(snapshot, signal) {
    for (const sourceType of ['knowledge', 'work-file', 'memory', 'message']) {
      const previous = await this.index.listSources({ scopeKeys: snapshot.scopes, sourceType, signal });
      for (const source of previous) if (!snapshot.identities.has(source.sourceId)) {
        await this.index.removeSource(source.sourceId, { scopeKeys: snapshot.scopes, permanent: false, signal });
        this.fingerprints.delete(`${source.sourceId}:lexical`); this.fingerprints.delete(`${source.sourceId}:semantic`);
      }
    }
  }

  async _fresh(item, snapshot, signal, checks = new Map(), context = {}) {
    const key = `${item.sourceId}:${item.sourceRevision}:${item.contentHash}`;
    if (checks.has(key)) return checks.get(key);
    const operation = this._freshSource(item, snapshot, signal, checks, context);
    checks.set(key, operation);
    return operation;
  }

  async _freshSource(item, snapshot, signal, checks, context) {
    signal?.throwIfAborted();
    if (!snapshot.scopes.includes(item.scopeKey)) return false;
    let source = snapshot.identities.get(item.sourceId) ?? item;
    if (source.sourceType === 'knowledge') {
      source = await this.library.readSource(item.sourceId, { scopeKeys: snapshot.scopes, sourceRevision: item.sourceRevision });
    } else if (source.sourceType === 'memory' || source.sourceType === 'message') {
      const type = source.sourceType;
      if (!checks.has(type)) checks.set(type, (async () => {
        const current = type === 'memory' ? await this.memory.contextFor(snapshot.relationship.conversationId)
          : await this.conversations.readMessages(snapshot.relationship.conversationId);
        const entries = type === 'memory' ? current.entries : [];
        const messages = type === 'message' ? this._beforeCurrentMessage(current, context) : [];
        return new Map(projectConversationSources(snapshot.relationship, entries, messages).map(currentSource => [currentSource.sourceId, currentSource]));
      })());
      source = (await checks.get(type)).get(item.sourceId);
    }
    if (!source || source.contentHash !== item.contentHash || source.sourceRevision !== item.sourceRevision ||
        (source.bindingRevision ?? 0) !== (item.bindingRevision ?? 0)) return false;
    if (source.sourceType === 'work-file') {
      const projectId = source.scopeKey.slice('project:'.length), projectKey = `project:${projectId}`;
      if (!checks.has(projectKey)) checks.set(projectKey, this._project(projectId));
      const project = await checks.get(projectKey);
      if (!snapshot.settings.projectIndexing?.mountedFolder ||
          snapshot.settings.projectIndexing.bindingRevision !== source.bindingRevision ||
          !project?.FolderPath || resolve(project.FolderPath) !== source.locator.root) return false;
      try {
        const fresh = (await readSourceFile(source.locator.path, { root: source.locator.root, excludedRoots: this.excludedRoots, signal })).contentHash === source.contentHash;
        if (!fresh) this.mountedCache.delete(source.scopeKey.slice('project:'.length));
        return fresh;
      }
      catch (error) { if (signal?.aborted) throw error; this.mountedCache.delete(source.scopeKey.slice('project:'.length)); return false; }
    }
    return true;
  }

  async _assertCurrent(context, snapshot) {
    const current = await this.conversations.describeConversation(context.conversationId);
    const effective = await this.effective(current.isFolderlessWorkspace ? null : current.projectId);
    if (current.isArchived || current.projectArchived || !effective.local.enabled ||
        JSON.stringify(visibleScopes(current)) !== JSON.stringify(snapshot.scopes) ||
        effective.revision !== snapshot.settings.revision || effective.projectRevision !== snapshot.settings.projectRevision ||
        JSON.stringify(effective.local) !== JSON.stringify(snapshot.settings.local) ||
        JSON.stringify(effective.projectIndexing) !== JSON.stringify(snapshot.settings.projectIndexing))
      throw toolFailure('检索范围或配置已变化，请重新检索。', 'RETRIEVAL_SCOPE_CHANGED', 409);
    const mounted = snapshot.sources.find(source => source.sourceType === 'work-file');
    if (mounted) {
      const project = await this._project(current.projectId);
      if (!project?.FolderPath || resolve(project.FolderPath) !== mounted.locator.root)
        throw toolFailure('挂载文件夹已变化，请重新检索。', 'RETRIEVAL_SCOPE_CHANGED', 409);
    }
  }

  async _rerank(context, query, candidates, snapshot, taskType, signal) {
    if (!['complex', 'research'].includes(taskType) || !snapshot.settings.local.rerankProfileId || !this.reranker || candidates.length < 2)
      return { items: candidates };
    const status = this.reranker.status();
    if (status.profileId !== snapshot.settings.local.rerankProfileId || !['ready', 'loading'].includes(status.state))
      return { items: candidates, diagnostic: 'RERANK_UNAVAILABLE' };
    try {
      const result = await this.reranker.rerank({ context, query, candidates, settings: snapshot.settings, signal, limit: 20 });
      const originals = new Map(candidates.map(item => [item.sourceRef, item]));
      const ranked = [], seen = new Set();
      let scoredCount = 0;
      for (const item of result.items ?? []) {
        const original = originals.get(item.sourceRef);
        if (!original || seen.has(item.sourceRef) || item.rerankScore !== undefined && !Number.isFinite(item.rerankScore)) continue;
        seen.add(item.sourceRef);
        if (item.rerankScore !== undefined) {
          scoredCount++; ranked.push({ ...original, rerankScore: item.rerankScore, rerankRank: scoredCount });
        } else ranked.push(original);
      }
      return scoredCount ? { items: ranked, rerank: { profileId: result.profileId, modelVersion: result.modelVersion,
        truncatedInputsCount: result.truncatedInputsCount } } : { items: candidates, diagnostic: 'RERANK_EMPTY' };
    } catch (error) {
      if (signal?.aborted) throw error;
      return { items: candidates, diagnostic: error.code ?? 'RERANK_FAILED' };
    }
  }

  search(context, { query, gap, limit = 6, taskType = 'lookup', maximumTokens = 8192, existingContext = [], requiresSourceRead = false },
    { signal, modelReferences = false, archiveId = modelReferences ? allocateEvidenceArchiveId() : undefined } = {}) {
    return this._serialize(async () => {
      if (this.closed) throw toolFailure('检索服务已关闭。', 'RETRIEVAL_CLOSED', 409);
      if (typeof query !== 'string' || !query.trim() || query.length > MAX_QUERY_CHARACTERS ||
          !Number.isSafeInteger(limit) || limit < 1 || limit > 60 ||
          !Number.isSafeInteger(maximumTokens) || maximumTokens < 0 || maximumTokens > 16384)
        throw retrievalFailure('Invalid query or evidence budget. / 检索查询、数量或证据预算无效。');
      validateEvidenceGap(gap);
      const withModelReferences = items => modelReferences
        ? items.map((item, index) => ({ ...item, modelSourceRef: evidenceSourceRef(archiveId, index + 1) })) : items;
      await this.initialize();
      const snapshot = await this._snapshot(context, signal);
      if (!snapshot.settings.local.enabled) return { items: [], strategy: 'disabled', vectorAvailable: false,
        evidenceAssessment: assessEvidence([], query) };
      let acquisition = this.acquisitions.get(context);
      if (!acquisition) {
        acquisition = new EvidenceAcquisition({ research: taskType === 'research' ||
          retrievalPlan(context.message ?? query).taskType === 'research' });
        this.acquisitions.set(context, acquisition);
      }
      const ticket = acquisition.prepare({ query, gap,
        snapshotKey: sourceIdentity(snapshot.scopes, snapshot.settings, snapshot.sources.map(source =>
          [source.sourceId, source.sourceRevision, source.contentHash, source.bindingRevision, source.locator])),
        cacheKey: [query.normalize('NFKC').trim().toLowerCase(), limit, taskType, maximumTokens, requiresSourceRead, modelReferences, sourceIdentity(existingContext)] });
      // Optional inference health is checked live; a previous success must not hide a failed reranker.
      // 可选推理服务的健康状态需要实时检查，不能以历史成功掩盖重排服务失败。
      if (ticket.cached && snapshot.settings.local.rerankProfileId && ['complex', 'research'].includes(taskType))
        acquisition.invalidate(ticket);
      if (ticket.cached) {
        const checks = new Map();
        const verified = await Promise.allSettled(ticket.cached.items.map(item => this._fresh(item, snapshot, signal, checks, context)));
        signal?.throwIfAborted();
        const failed = verified.find(item => item.status === 'rejected');
        if (failed) throw failed.reason;
        if (verified.every(item => item.value)) {
          await this._assertCurrent(context, snapshot);
          signal?.throwIfAborted();
          const cached = structuredClone(ticket.cached);
          return { ...cached, items: withModelReferences(cached.items),
            ...(modelReferences ? { evidenceArchiveId: archiveId } : {}),
            acquisition: acquisition.observe(ticket, cached, { reused: true }) };
        }
        acquisition.invalidate(ticket);
      }
      if (ticket.blocked || acquisition.searches >= acquisition.maximumSearches || ticket.entry.searches >= acquisition.maximumSearchesPerGap) {
        await this._assertCurrent(context, snapshot);
        signal?.throwIfAborted();
        return { items: [], strategy: 'gap-budget-exhausted', vectorAvailable: false,
          evidenceAssessment: { ...assessEvidence([], query), reason: 'search-budget-exhausted' },
          acquisition: acquisition.status(ticket, { blocked: true }) };
      }
      // Foreground requests update lexical text only; imported/project embeddings are built in background jobs.
      // 前台请求只同步词法原文；导入资料和工作向量由后台任务生成，不等整段聊天嵌入完成。
      await this._upsert(snapshot.sources, snapshot.settings, signal, undefined, { semantic: false });
      await this._prune(snapshot, signal);
      let embedded;
      if (snapshot.sources.length && snapshot.settings.local.semantic !== 'off' && snapshot.settings.local.embeddingProfileId === 'builtin-multilingual') {
        try { embedded = await this.embeddings.embedQuery(query, { signal }); }
        catch (error) { if (signal?.aborted) throw error; this.lastEmbeddingError = error.code ?? 'EMBEDDING_FAILED'; }
      }
      const result = await this.index.search({ query, scopeKeys: snapshot.scopes, limit: RETRIEVAL_CANDIDATE_LIMIT,
        queryVector: embedded?.vector, embeddingProfileId: embedded?.profileId, embeddingModelVersion: embeddingVersion(embedded?.modelVersion), signal });
      result.items = withModelReferences(result.items);
      // Targeted current reads replace the second whole-library/history/tree snapshot.
      // 只回读命中来源并复核其版本，避免第二次全量资料、聊天和目录扫描。
      const candidateCount = result.items.length, freshChecks = new Map();
      const unique = deduplicateCandidates(result.items, { existingContext });
      const reranked = await this._rerank(context, query, unique.items, snapshot, taskType, signal);
      let current = reranked.items, selected;
      const invalidSourceIds = new Set();
      do {
        selected = selectCandidates(current, { query, limit, maximumTokens, requiresSourceRead });
        const pending = selected.items.map(item => this._fresh(item, snapshot, signal, freshChecks, context));
        const checked = await Promise.allSettled(pending);
        signal?.throwIfAborted();
        const failed = checked.find(item => item.status === 'rejected');
        if (failed) throw failed.reason;
        const stale = selected.items.filter((_, index) => !checked[index].value);
        if (!stale.length) break;
        for (const item of stale) invalidSourceIds.add(item.sourceId);
        const available = deduplicateCandidates(result.items.filter(item => !invalidSourceIds.has(item.sourceId)), { existingContext }).items;
        const rankedReferences = new Set(current.map(item => item.sourceRef));
        // A stale preferred copy must not hide an independently valid duplicate source.
        // 优先副本失效后，仍允许原候选池中其他有效来源的同文副本补位。
        current = [...current.filter(item => !invalidSourceIds.has(item.sourceId)),
          ...available.filter(item => !rankedReferences.has(item.sourceRef))];
      } while (current.length);
      if (!current.length) selected = selectCandidates([], { query, limit, maximumTokens, requiresSourceRead });
      result.items = withModelReferences(selected.items);
      if (modelReferences) result.evidenceArchiveId = archiveId;
      result.selection = { ...selected.selection,
        candidateCount, currentCandidates: current.length, staleSourceCount: invalidSourceIds.size,
        duplicateCount: unique.duplicateCount, alreadyPresentCount: unique.alreadyPresentCount };
      result.evidenceAssessment = selected.evidenceAssessment;
      if (!result.items.length && unique.alreadyPresentCount)
        result.evidenceAssessment = assessEvidence([], query, { alreadyPresentCount: unique.alreadyPresentCount });
      if (reranked.rerank) result.rerank = reranked.rerank;
      if (reranked.diagnostic) result.rerankDiagnostic = reranked.diagnostic;
      // Re-check the relationship and settings after worker/IO awaits.
      // worker 与文件 IO 等待后复核工作关联和开关，旧请求不能复活已禁用资料。
      await this._assertCurrent(context, snapshot);
      signal?.throwIfAborted();
      result.acquisition = acquisition.observe(ticket, result);
      return result;
    });
  }

  read(context, { sourceRef, offset = 0, limit = 8000, mode = 'page', anchorOffset, beforeCharacters = 384, gap }, { signal } = {}) {
    return this._serialize(async () => {
      validateEvidenceGap(gap);
      if (!['page', 'window', 'section'].includes(mode)) throw retrievalFailure('Invalid source read mode. / 资料读取模式无效。');
      const snapshot = await this._scopeSnapshot(context, signal);
      if (!snapshot.settings.local.enabled) throw toolFailure('本地检索已关闭。', 'RETRIEVAL_DISABLED', 409);
      let canonicalSourceRef = sourceRef;
      if (typeof sourceRef === 'string' && sourceRef.startsWith('ev1:')) {
        if (!this.evidenceReferences) throw toolFailure('证据引用解析不可用。', 'EVIDENCE_REFERENCE_UNAVAILABLE', 409);
        canonicalSourceRef = (await this.evidenceReferences.resolve(context, sourceRef, { scopeKeys: snapshot.scopes, signal })).canonicalSourceRef;
      }
      const item = mode === 'page'
        ? await this.index.read({ sourceRef: canonicalSourceRef, scopeKeys: snapshot.scopes, offset, limit, signal })
        : await this.index.readWindow({ sourceRef: canonicalSourceRef, scopeKeys: snapshot.scopes,
          mode, anchorOffset, beforeCharacters, limit, signal });
      if (!await this._fresh(item, snapshot, signal, new Map(), context))
        throw toolFailure('资料已更改或撤销，请重新检索。', 'STALE_RETRIEVAL_SOURCE', 409);
      await this._assertCurrent(context, snapshot);
      signal?.throwIfAborted();
      return { ...item, ...(canonicalSourceRef !== sourceRef ? { sourceRef } : {}),
        evidenceDecision: { sufficiency: 'not-evaluated', missingInformation: gap ?? null,
          next: 'answer-if-this-context-supports-the-requested-facts-otherwise-name-the-remaining-gap' } };
    });
  }

  async evidence(context, query, { signal, maximumCharacters = 10000, maximumTokens, existingContext = [], history = [], plan,
    deferArchive = false } = {}) {
    const route = plan ?? retrievalPlan(query, { history, maximumTokens });
    if (!route.shouldRetrieve || route.evidenceTokens <= 0 || maximumCharacters <= 0)
      return { prompt: '', references: [], evidenceAssessment: assessEvidence([], query), plan: route };
    const promptTokens = Math.max(0, Math.min(route.evidenceTokens, maximumTokens ?? route.evidenceTokens));
    const reservedTokens = estimateTokens(EVIDENCE_NOTICE) + 120;
    const result = await this.search(context, { query: route.query, limit: 6, taskType: route.taskType,
      maximumTokens: Math.max(0, promptTokens - reservedTokens), existingContext, requiresSourceRead: route.requiresSourceRead },
    { signal, modelReferences: Boolean(this.tools.results) });
    const projection = projectEvidence(result.items, maximumCharacters, { maximumTokens: promptTokens, assessment: result.evidenceAssessment });
    result.items = projection.items;
    result.evidenceAssessment = assessEvidence(result.items, query, { requiresSourceRead: route.requiresSourceRead,
      alreadyPresentCount: result.selection?.alreadyPresentCount });
    result.plan = route; result.selection = { ...result.selection, promptTokens: projection.usedTokens };
    if (!result.items.length) return { prompt: '', references: [], evidenceAssessment: result.evidenceAssessment, plan: route };
    // The opaque handle keeps validated excerpts internal until the final model projection exists.
    // 以不透明句柄保存已验证片段，最终请求视图确定后才去重并归档，不把临时数据暴露给模型或事件。
    const prepared = Object.freeze({});
    this.#preparedEvidence.set(prepared, { result, query, route, maximumTokens: promptTokens, maximumCharacters,
      projectedTokens: projection.usedTokens, projectedCharacters: projection.prompt.length,
      contextKey: this._evidenceContextKey(context), finalizing: false });
    const draft = { prompt: projection.prompt, prepared, evidenceAssessment: result.evidenceAssessment, plan: route,
      references: result.items.map(({ sourceRef, modelSourceRef, sourceId,
      scopeKey, sourceRevision, contentHash, title, locator }) => ({ sourceRef, ...(modelSourceRef ? { modelSourceRef } : {}), sourceId, scopeKey, sourceRevision, contentHash, title, locator })) };
    return deferArchive ? draft : this.finalizeEvidence(context, draft, { signal });
  }

  _evidenceContextKey(context) {
    return JSON.stringify([context.conversationId?.toLowerCase(), context.requestId?.toLowerCase() ?? null,
      context.projectId?.toLowerCase() ?? null]);
  }

  /** Reproject already verified evidence against the final retained context, without another retrieval.
   * 仅在已验证片段中，按最终保留上下文重新去重与投影，不进行第二次检索。 */
  async finalizeEvidence(context, preparedEvidence, { existingContext = [], maximumTokens, maximumCharacters, signal } = {}) {
    if (this.closed) throw toolFailure('检索服务已关闭。', 'RETRIEVAL_CLOSED', 409);
    signal?.throwIfAborted();
    const state = this.#preparedEvidence.get(preparedEvidence?.prepared);
    if (!state || state.finalizing || state.contextKey !== this._evidenceContextKey(context))
      throw toolFailure('证据请求视图无效或不属于当前请求。', 'RETRIEVAL_PREPARATION_INVALID', 409);
    const tokenBudget = maximumTokens ?? state.maximumTokens, characterBudget = maximumCharacters ?? state.maximumCharacters;
    if (!Number.isSafeInteger(tokenBudget) || tokenBudget < 0 || !Number.isSafeInteger(characterBudget) || characterBudget < 0)
      throw retrievalFailure('Invalid final evidence budget. / 最终证据预算无效。');
    // Admit only one finalizer before any await; source removal shares the same publication queue.
    // 首次等待前只允许一个最终投影；来源撤销共用同一发布队列，不能在回读与归档之间插入旧来源。
    state.finalizing = true;
    return this._serialize(async () => {
      try {
        if (this.closed) throw toolFailure('检索服务已关闭。', 'RETRIEVAL_CLOSED', 409);
        const snapshot = await this._scopeSnapshot(context, signal);
        if (!snapshot.settings.local.enabled) throw toolFailure('本地检索已关闭。', 'RETRIEVAL_DISABLED', 409);
        snapshot.sources = state.result.items;
        const freshness = new Map();
        const checked = await Promise.allSettled(state.result.items.map(item => this._fresh(item, snapshot, signal, freshness, context)));
        signal?.throwIfAborted();
        const failed = checked.find(item => item.status === 'rejected');
        if (failed) throw failed.reason;
        const currentItems = state.result.items.filter((_, index) => checked[index].value);
        await this._assertCurrent(context, snapshot);
        signal?.throwIfAborted();
        const unique = deduplicateCandidates(currentItems, { existingContext });
        const alreadyPresentCount = (state.result.selection?.alreadyPresentCount ?? 0) + unique.alreadyPresentCount;
        const withFinalReferences = items => state.result.evidenceArchiveId
          ? items.map((item, index) => ({ ...item, modelSourceRef: evidenceSourceRef(state.result.evidenceArchiveId, index + 1) })) : items;
        let items = withFinalReferences(unique.items), assessment = assessEvidence(items, state.query,
          { requiresSourceRead: state.route.requiresSourceRead, alreadyPresentCount }), projection;
        // A smaller final budget can change support; only shrink the projection until its notice agrees.
        // 最终预算缩小时可能改变证据支持状态；只缩减片段，直到提示与实际呈现片段一致。
        for (;;) {
          projection = projectEvidence(items, Math.min(characterBudget, state.maximumCharacters, state.projectedCharacters),
            { maximumTokens: Math.min(tokenBudget, state.maximumTokens, state.projectedTokens), assessment });
          const projectedAssessment = assessEvidence(projection.items, state.query,
            { requiresSourceRead: state.route.requiresSourceRead, alreadyPresentCount });
          if (assessment.state === projectedAssessment.state && assessment.requiresSourceRead === projectedAssessment.requiresSourceRead) {
            assessment = projectedAssessment; break;
          }
          items = withFinalReferences(projection.items); assessment = projectedAssessment;
        }
        const result = { ...state.result, items: projection.items, evidenceAssessment: assessment,
          selection: { ...state.result.selection, alreadyPresentCount, promptTokens: projection.usedTokens,
            finalStaleSourceCount: new Set(state.result.items.filter((_, index) => !checked[index].value).map(item => item.sourceId)).size } };
        let resultRef;
        if (result.items.length && context.requestId && this.tools.results) resultRef = await this.tools.results.save(context,
          { id: `retrieval:${context.requestId}`, name: 'knowledge.search' }, { content: [], structuredContent: result, isError: false },
          { id: state.result.evidenceArchiveId });
        signal?.throwIfAborted();
        this.#preparedEvidence.delete(preparedEvidence.prepared);
        return { prompt: projection.prompt, resultRef, evidenceAssessment: assessment, plan: state.route,
          references: result.items.map(({ sourceRef, modelSourceRef, sourceId, scopeKey, sourceRevision, contentHash, title, locator }) =>
            ({ sourceRef, ...(modelSourceRef ? { modelSourceRef } : {}), sourceId, scopeKey, sourceRevision, contentHash, title, locator })) };
      } catch (error) { state.finalizing = false; throw error; }
    });
  }

  async importSource(input) {
    const files = await readSourceTree(input.path, { excludedRoots: this.excludedRoots, maximumFiles: 512 });
    const result = await this.library.add(files, input);
    this.libraryCache.clear();
    const job = await this.rebuild({ projectId: input.scope === 'project' ? input.projectId : null });
    const first = result.sources[0];
    return { id: first.id, title: first.title, scope: first.scope, projectId: first.projectId, path: first.originalPath,
      revision: first.revision, status: 'ready', importedCount: result.sources.length, jobId: job.jobId };
  }

  async removeSource(id, input) {
    return this._serialize(async () => {
      const removed = await this.library.remove(id, input);
      this.libraryCache.clear();
      this.fingerprints.delete(`${removed.id}:lexical`); this.fingerprints.delete(`${removed.id}:semantic`);
      await this.index.removeSource(removed.id, { scopeKeys: [removed.scopeKey] });
      return { deleted: true, id: removed.id };
    });
  }

  async rebuild({ projectId = null } = {}) {
    if (this.closed) throw toolFailure('检索服务已关闭。', 'RETRIEVAL_CLOSED', 409);
    await this.initialize();
    if (projectId) { projectId = validateId(projectId).toLowerCase(); await this.conversations.describeProject(projectId); }
    const running = [...this.activeJobs.values()].find(item => item.projectId === projectId && !item.controller.signal.aborted);
    if (running) return this.jobs.get(running.jobId);
    this.mountedCache.delete(projectId);
    const job = await this.jobs.create(projectId), controller = new AbortController();
    const active = { jobId: job.jobId, projectId, controller };
    this.activeJobs.set(job.jobId, active);
    active.promise = (async () => {
      const signal = controller.signal;
      try {
        await this.jobs.update(job.jobId, { status: 'running', startedAt: new Date().toISOString() });
        const settings = await this.effective(projectId), scopes = ['user', ...(projectId ? [`project:${projectId}`] : [])];
        const sources = [...await this.library.readAll(scopes), ...await this._mountedSources(projectId, settings, signal)];
        await this.jobs.update(job.jobId, { totalSources: sources.length });
        await this._upsert(sources, settings, signal, completedSources => this.jobs.update(job.jobId, { completedSources }));
        signal.throwIfAborted();
        await this.jobs.update(job.jobId, { status: 'completed', finishedAt: new Date().toISOString() });
      } catch (error) {
        await this.jobs.update(job.jobId, { status: signal.aborted ? 'cancelled' : 'failed',
          error: signal.aborted ? 'INDEX_CANCELLED' : typeof error.code === 'string' ? error.code : 'INDEX_FAILED', finishedAt: new Date().toISOString() });
      } finally { this.activeJobs.delete(job.jobId); }
    })();
    active.promise.catch(() => {});
    return job;
  }

  async cancelJob(id) {
    const job = await this.jobs.get(validateId(id));
    this.activeJobs.get(job.jobId)?.controller.abort();
    return job;
  }

  close() {
    if (!this.closure) this.closure = this._close();
    return this.closure;
  }

  async _close() {
    this.closed = true;
    for (const watcher of this.watchers.values()) watcher.close();
    this.watchers.clear();
    this.mountedCache.clear();
    this.libraryCache.clear(); this.conversationCache.clear();
    for (const job of this.activeJobs.values()) job.controller.abort();
    const inferenceClosures = await Promise.allSettled([this.embeddings.close(), this.reranker?.close()]);
    await Promise.allSettled([...this.activeJobs.values()].map(job => job.promise));
    await this.queue.catch(() => {});
    await this.index.close();
    const inferenceFailure = inferenceClosures.find(item => item.status === 'rejected');
    if (inferenceFailure) throw inferenceFailure.reason;
  }
}

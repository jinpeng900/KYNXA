import { resolve } from 'node:path';
import { RetrievalSettingsStore } from '../../data/retrieval/settings.mjs';
import { RetrievalIndex } from '../../data/retrieval/index.mjs';
import { RetrievalStructureService } from '../../data/retrieval/structure-service.mjs';
import { EMBEDDING_TEXT_VERSION } from '../../data/retrieval/retrieval-text.mjs';
import { MAX_QUERY_CHARACTERS, retrievalFailure, validateRetrievalIntent } from '../../data/retrieval/retrieval-contracts.mjs';
import { SourceLibrary } from '../../data/retrieval/source-library.mjs';
import { RetrievalJobStore } from '../../data/retrieval/job-store.mjs';
import { EmbeddingRouter } from '../../models/retrieval/embedding-router.mjs';
import { RerankerService } from '../../models/retrieval/reranker-service.mjs';
import { readSourceFile, readSourceFileWindow } from '../../tools/retrieval/source-reader.mjs';
import { sourceFileRevision } from '../../data/retrieval/retrieval-contracts.mjs';
import { validateId } from '../../platform/conversation-id.mjs';
import { toolFailure } from '../../platform/tool-paths.mjs';
import { sourceIdentity, visibleScopes, projectConversationSources } from './source-projection.mjs';
import { selectCandidates, deduplicateCandidates, assessEvidence } from './candidate-selection.mjs';
import { EvidenceAcquisition, validateEvidenceGap } from './evidence-acquisition.mjs';
import { SourceIndexService, sourceScanSummary } from './source-manager.mjs';
import { RetrievalEvidenceService } from './evidence-service.mjs';
import { retrievalPlan, buildRetrievalIntent, planEvidenceAcquisition, hasExactRetrievalTarget } from './query-plan.mjs';
import { listRetrievalModelProfiles, resolveRetrievalModelProfile } from '../../models/retrieval/model-registry.mjs';
import { allocateEvidenceArchiveId, evidenceSourceRef, EvidenceReferenceStore } from '../../data/retrieval/evidence-references.mjs';
import { ResourceBudgetService } from '../../platform/resources/resource-client.mjs';
import { runResourceTask } from '../../platform/resources/resource-task.mjs';
import { retrievalBudget } from './retrieval-budget.mjs';
import { TaskExperienceStore } from '../../data/retrieval/task-experience-store.mjs';
import { EmbeddingSpacePolicy } from './embedding-space-policy.mjs';
import { retrievalOutcome } from '../request-interpretation.mjs';

const embeddingVersion = modelVersion => modelVersion ? `${modelVersion}|${EMBEDDING_TEXT_VERSION}` : undefined;

function hasModelMetadataMismatch(expected, result) {
  return ['modelVersion', 'inputProjectionVersion'].some(field => expected[field] !== undefined &&
    result[field] !== undefined && expected[field] !== result[field]);
}

/** Application coordinator composes E-owned storage, C-owned inference and D-owned file reads.
 * 应用协调层组合数据存储、嵌入推理和工具文件读取，不改变正式会话或记忆归属。 */
export class RetrievalCoordinator {
  constructor({ conversations, memory, tools, index, embeddings, reranker, structures, resources, evaluationPolicy, excludedRoots = [] }) {
    this.conversations = conversations; this.memory = memory; this.tools = tools;
    this.settings = new RetrievalSettingsStore({ root: conversations.root, conversationStore: conversations });
    this.resources = resources ?? new ResourceBudgetService();
    this.ownsResources = !resources;
    // Only the offline evaluator injects this policy; public settings and model arguments cannot change it.
    // 仅离线评测构造器注入消融策略，公开设置及模型参数不能修改此策略。
    this.evaluationPolicy = evaluationPolicy ? Object.freeze({ ...evaluationPolicy }) : null;
    this.index = index ?? new RetrievalIndex({ root: conversations.root, resourceService: this.resources });
    this.structures = structures ?? new RetrievalStructureService({ resourceService: this.resources });
    this.embeddings = embeddings ?? new EmbeddingRouter({ resourceService: this.resources });
    this.spacePolicy = new EmbeddingSpacePolicy({ resources: this.resources,
      embeddings: { status: profileId => this.embeddings.status(profileId) }, index: this.index });
    this.reranker = reranker === undefined ? new RerankerService({ resourceService: this.resources }) : reranker;
    this.library = new SourceLibrary({ root: conversations.root, conversationStore: conversations });
    this.evidenceReferences = tools.results ? new EvidenceReferenceStore({ conversationStore: conversations, resultStore: tools.results }) : null;
    this.jobs = new RetrievalJobStore(conversations.root);
    this.experiences = new TaskExperienceStore(conversations.root);
    this.excludedRoots = [conversations.root, ...excludedRoots].filter(Boolean);
    this.acquisitions = new WeakMap();
    this.queue = Promise.resolve(); this.closed = false;
    this.shutdown = new AbortController();
    this.sourceService = new SourceIndexService({ library: this.library, index: this.index, jobs: this.jobs,
      resourceService: this.resources,
      structures: { parse: (...args) => this.structures.parse(...args), version: () => this.structures.derivationVersion,
        status: () => this.structures.status?.(), identity: () => this.structures },
      embeddings: { status: (...args) => this.embeddings.status(...args),
        ...(typeof this.embeddings.fitDocuments === 'function' ? { fitDocuments: (...args) => this.embeddings.fitDocuments(...args) } : {}),
        embedDocuments: (...args) => this.embeddings.embedDocuments(...args) }, getProject: id => this._project(id),
      validateProject: id => this.conversations.describeProject(id), effectiveSettings: async id =>
        this.spacePolicy.indexingSettings(await this.effective(id), this.shutdown.signal),
      serialize: operation => this._serialize(operation), excludedRoots: this.excludedRoots });
    // Compatibility views reference the sole owner; they never create a second job or cache state.
    // 兼容视图引用唯一所有者，不建立第二份作业或缓存状态。
    this.activeJobs = this.sourceService.lifecycle.active;
    this.evidenceService = new RetrievalEvidenceService({ search: (...args) => this.search(...args),
      scopeSnapshot: (...args) => this._scopeSnapshot(...args), isFresh: (...args) => this._fresh(...args),
      assertCurrent: (...args) => this._assertCurrent(...args), serialize: operation => this._serialize(operation),
      getResultStore: () => this.tools.results, signalFor: signal => this._signal(signal), isClosed: () => this.closed,
      index: this.index, getAcquisition: context => this.acquisitions.get(context), getReferenceStore: () => this.evidenceReferences,
      experiences: this.experiences, getTaskVerification: context => this.tools.taskVerificationFor?.(context), evaluationPolicy: this.evaluationPolicy });
  }

  async initialize() {
    if (!this.initialization) this.initialization = this.sourceService.initialize();
    await this.initialization;
  }

  _serialize(operation) {
    const pending = this.queue.catch(() => {}).then(operation);
    this.queue = pending; return pending;
  }

  async effective(projectId) { return this.settings.getEffective(projectId); }

  _planEvidence(input) {
    if (this.evaluationPolicy?.gaps !== false) return planEvidenceAcquisition(input);
    return { channels: ['lexical', ...(input.semanticEnabled && input.embeddingStatus?.loaded ? ['vector'] : [])],
      shouldEmbed: Boolean(input.semanticEnabled && input.embeddingStatus?.loaded),
      shouldRerank: ['complex', 'research'].includes(input.taskType) && input.items?.length > 1 && input.rerankerStatus?.state === 'ready',
      next: 'baseline-retrieval', sufficiency: 'not-evaluated', gapPlanningDisabled: true };
  }

  async status() {
    await this.initialize();
    const settings = await this.effective();
    const indexed = await this.index.status(), embedding = this.embeddings.status(settings.local.embeddingProfileId);
    const jobs = await this.jobs.list();
    return { ...indexed, backend: 'sqlite', sourceCount: indexed.sources, chunkCount: indexed.chunks,
      embedding: { ...embedding, available: ['ready', 'loading'].includes(embedding.state) },
      configuredEmbeddingProfileId: settings.local.embeddingProfileId,
      embeddingProfiles: this.modelProfiles().filter(profile => profile.kind === 'embedding').map(profile => this.embeddings.status(profile.id)),
      reranking: settings.local.rerankProfileId ? this.reranker?.status(settings.local.rerankProfileId) ?? { state: 'unavailable', loaded: false }
        : { state: 'disabled', loaded: false },
      modelProfiles: this.modelProfiles(), parsing: this.structures.status?.(), jobs,
      resources: await this.resources.snapshot(), vectorSpacePolicy: this.spacePolicy.status(jobs),
      externalModels: this.externalModelStatus?.() ?? [],
      deployment: { platform: 'windows', gpu: 'single-nvidia-dml-verified', otherPlatformsSupported: false } };
  }

  async _project(projectId) {
    if (!projectId) return null;
    const project = await this.conversations.describeProject(projectId);
    if (project.isArchived || project.isFolderlessWorkspace) return null;
    return { Id: project.projectId, FolderPath: project.folderPath };
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
    const [library, mounted] = await Promise.all([
      this.sourceService.librarySnapshot(scopes, settings, signal),
      this.sourceService.foregroundMountedSnapshot(relationship.isFolderlessWorkspace ? null : relationship.projectId, settings, signal)
    ]);
    const sources = [...library.sources, ...this.sourceService.conversationSources(relationship, memory.entries, messages, settings), ...mounted.sources];
    // Keep source identities in the request; corpus bodies are hydrated only for changed derivations or current reads.
    // 请求只保留来源身份；正文仅在派生变化或当前回读时按需加载，避免将整个语料驻留聊天请求。
    const loadSource = (source, ownedSignal) => source.sourceType === 'knowledge' ? library.loadSource(source, ownedSignal)
      : source.sourceType === 'work-file' ? mounted.loadSource(source, ownedSignal) : Promise.resolve(source);
    const isCurrent = (source, ownedSignal) => source.sourceType === 'knowledge' ? library.isCurrent(source, ownedSignal)
      : source.sourceType === 'work-file' ? mounted.isCurrent(source, ownedSignal) : Promise.resolve(true);
    const priorUser = messages.filter(message => message.Role === 'user' && (!message.Status || message.Status === 'completed')).at(-1);
    return { relationship, settings, scopes, sources, loadSource, isCurrent,
      taskContext: context.message ?? priorUser?.Content,
      identities: new Map(sources.map(source => [source.sourceId, source])),
      ...(library.scan || mounted.scan ? { sourceScan: sourceScanSummary(library, mounted) } : {}) };
  }

  async _fresh(item, snapshot, signal, checks = new Map(), context = {}) {
    const key = `${item.sourceId}:${item.sourceRevision}:${item.contentHash}:${item.derivationSignature ?? ''}`;
    if (checks.has(key)) return checks.get(key);
    const operation = this._freshSource(item, snapshot, signal, checks, context);
    checks.set(key, operation);
    return operation;
  }

  async _freshSource(item, snapshot, signal, checks, context) {
    signal?.throwIfAborted();
    if (!snapshot.scopes.includes(item.scopeKey)) return false;
    if (item.derivationSignature && !(await this.index.verifyReference({
      sourceRef: item.sourceRef, scopeKeys: snapshot.scopes, signal })).current) return false;
    let source = snapshot.identities.get(item.sourceId) ?? item;
    if (snapshot.sourceScan?.coverage?.complete === false && !snapshot.identities.has(item.sourceId) &&
      ['knowledge', 'work-file'].includes(item.sourceType)) return false;
    if (source.unavailable) return false;
    if (source.sourceType === 'knowledge') {
      source = await this.library.readSource(item.sourceId, { scopeKeys: snapshot.scopes, sourceRevision: item.sourceRevision, signal });
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
        const options = { root: source.locator.root, excludedRoots: this.excludedRoots,
          maximumSourceBytes: snapshot.settings.local.indexing?.maximumSourceBytes, resourceService: this.resources, signal };
        const file = source.locator.fileWindow
          ? await readSourceFileWindow(source.locator.path, source.locator.fileWindow, options)
          : await readSourceFile(source.locator.path, options);
        const fresh = file.contentHash === source.contentHash &&
          (!source.locator.fileWindow && source.locator.extraction === undefined && file.extraction === undefined || sourceFileRevision(file) === source.sourceRevision);
        if (!fresh) this.sourceService.invalidateMounted(source.scopeKey.slice('project:'.length));
        return fresh;
      }
      catch (error) { if (signal?.aborted) throw error; this.sourceService.invalidateMounted(source.scopeKey.slice('project:'.length)); return false; }
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

  async _rerank(context, query, candidates, snapshot, taskType, signal, decision) {
    decision ??= this._planEvidence({ query, items: candidates, taskType,
      rerankerStatus: snapshot.settings.local.rerankProfileId ? this.reranker?.status(snapshot.settings.local.rerankProfileId) : null });
    if (['complex', 'research'].includes(taskType) && snapshot.settings.local.rerankProfileId && this.reranker &&
        decision?.rerankReason === 'optional-model-not-ready')
      return { items: candidates, diagnostic: this.reranker.status(snapshot.settings.local.rerankProfileId).errorCode ?? 'RERANK_UNAVAILABLE' };
    if (!decision?.shouldRerank || !snapshot.settings.local.rerankProfileId || !this.reranker || candidates.length < 2)
      return { items: candidates };
    try { resolveRetrievalModelProfile('reranker', snapshot.settings.local.rerankProfileId); }
    catch (error) { return { items: candidates, diagnostic: error.code }; }
    const status = this.reranker.status(snapshot.settings.local.rerankProfileId);
    if (status.profileId !== snapshot.settings.local.rerankProfileId || !['ready', 'loading'].includes(status.state))
      return { items: candidates, diagnostic: status.errorCode ?? 'RERANK_UNAVAILABLE' };
    try {
      const result = await this.reranker.rerank({ profileId: snapshot.settings.local.rerankProfileId,
        context, query, candidates, settings: snapshot.settings, signal, limit: 20 });
      if (result.profileId !== undefined && result.profileId !== snapshot.settings.local.rerankProfileId ||
          hasModelMetadataMismatch(status, result))
        throw retrievalFailure('Reranking result belongs to another model. / 重排结果不属于所选模型配置。', 'RERANK_PROFILE_MISMATCH', 409);
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

  search(context, { query, gap, domain, preferredDomain, symbol, path, limit, taskType = 'lookup', maximumTokens, existingContext = [], requiresSourceRead = false },
    { signal, modelReferences = false, archiveId = modelReferences ? allocateEvidenceArchiveId() : undefined,
      allowColdInference = true, retrievalIntent: preparedIntent } = {}) {
    signal = this._signal(signal);
    return this._serialize(() => runResourceTask(this.resources, { taskId: `retrieval:${context.requestId ?? context.conversationId}`,
      workspaceId: context.projectId ?? context.conversationId, memoryBytes: 8 * 1024 * 1024 }, async lease => {
      const budget = { ...retrievalBudget({ taskType, suggestions: lease?.suggestions, limit, maximumTokens }),
        ...this.evaluationPolicy?.fixedBudget };
      budget.maximumTokens = Math.min(budget.maximumTokens, maximumTokens ?? 32768);
      limit = budget.limit; maximumTokens = budget.maximumTokens;
      if (this.closed) throw toolFailure('检索服务已关闭。', 'RETRIEVAL_CLOSED', 409);
      if (typeof query !== 'string' || !query.trim() || query.length > MAX_QUERY_CHARACTERS ||
          !Number.isSafeInteger(limit) || limit < 1 || limit > 60 ||
          !Number.isSafeInteger(maximumTokens) || maximumTokens < 0 || maximumTokens > 32768)
        throw retrievalFailure('Invalid query or evidence budget. / 检索查询、数量或证据预算无效。');
      validateEvidenceGap(gap);
      if (maximumTokens === 0) return { items: [], strategy: 'context-budget-exhausted', vectorAvailable: false,
        outcome: retrievalOutcome({ strategy: 'context-budget-exhausted' }),
        budget, evidenceAssessment: { ...assessEvidence([], query), reason: 'no-remaining-model-context' },
        acquisition: { shouldContinue: false, next: 'answer-with-current-evidence-or-state-the-context-limit' } };
      const withModelReferences = items => modelReferences
        ? items.map((item, index) => ({ ...item, modelSourceRef: evidenceSourceRef(archiveId, index + 1) })) : items;
      await this.initialize();
      const snapshot = await this._snapshot(context, signal);
      // Automatic evidence uses intent derived from the current utterance, not historical query additions.
      // 自动证据使用从本轮原话提取的约束，历史补充文本不能重新变成硬路径或领域限制。
      const retrievalIntent = preparedIntent === undefined
        ? buildRetrievalIntent(query, { domain, preferredDomain, symbol, path, taskContext: snapshot.taskContext })
        : validateRetrievalIntent(preparedIntent);
      if (retrievalIntent.path && this.evaluationPolicy?.gaps !== false) this.sourceService.lifecycle.prioritize?.(
        snapshot.relationship.isFolderlessWorkspace ? null : snapshot.relationship.projectId, retrievalIntent.path);
      if (!snapshot.settings.local.enabled) return { items: [], strategy: 'disabled', vectorAvailable: false,
        outcome: retrievalOutcome({ strategy: 'disabled' }),
        evidenceAssessment: assessEvidence([], query) };
      let acquisition = this.acquisitions.get(context);
      if (!acquisition) {
        acquisition = new EvidenceAcquisition({ research: taskType === 'research' ||
          retrievalPlan(context.message ?? query).taskType === 'research' });
        this.acquisitions.set(context, acquisition);
      }
      const ticket = this.evaluationPolicy?.gaps === false ? { gap: query, explicitGap: false, entry: { searches: 0 }, cached: undefined } : acquisition.prepare({ query, gap,
        snapshotKey: sourceIdentity(snapshot.scopes, snapshot.settings, snapshot.sources.map(source =>
          [source.sourceId, source.sourceRevision, source.contentHash, source.bindingRevision, source.locator])),
        cacheKey: [query.normalize('NFKC').trim().toLowerCase(), limit, taskType, maximumTokens, requiresSourceRead, modelReferences,
          retrievalIntent, allowColdInference, sourceIdentity(existingContext)] });
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
      await this.sourceService.syncFormalSources(snapshot, signal);
      let embedded, embeddingDiagnostic;
      const space = await this.spacePolicy.select(snapshot.settings, snapshot.scopes, signal);
      let profileId = space.profileId;
      if (space.canMigrate && this.evaluationPolicy?.resources !== false) this.spacePolicy.schedule(
        snapshot.relationship.isFolderlessWorkspace ? null : snapshot.relationship.projectId, space.targetProfileId,
        () => this.sourceService.rebuild({ projectId: snapshot.relationship.isFolderlessWorkspace ? null : snapshot.relationship.projectId,
          signal: this.shutdown.signal }));
      const status = this.embeddings.status(profileId);
      const semanticEnabled = snapshot.settings.local.semantic !== 'off' && profileId !== null;
      // Exact paths/symbols start with current lexical/structural evidence, without waiting for model loading.
      // 精确路径或符号先使用当前词法及结构证据，不等待模型加载；可选冷模型不阻塞自动上下文准备。
      let lexical;
      if (retrievalIntent.path || retrievalIntent.symbol || !allowColdInference && status.loaded === false)
        lexical = await this.index.search({ query, scopeKeys: snapshot.scopes, limit: budget.fusedCandidates, channelCandidates: budget.channelCandidates,
          retrievalIntent, ann: snapshot.settings.local.ann, signal });
      if (lexical && (retrievalIntent.path || retrievalIntent.symbol)) {
        const targets = lexical.items.filter(item => hasExactRetrievalTarget(item, retrievalIntent));
        const checks = new Map(), staleSources = new Set();
        for (const item of targets) {
          signal.throwIfAborted();
          if (!await this._fresh(item, snapshot, signal, checks, context)) staleSources.add(item.sourceId);
        }
        lexical.items = lexical.items.filter(item => !staleSources.has(item.sourceId));
      }
      const initialDecision = this._planEvidence({ query, gap, intent: retrievalIntent, items: lexical?.items,
        taskType, semanticEnabled, embeddingStatus: { ...status,
          ...(allowColdInference && status.state === 'ready' ? { loaded: true } : {}) } });
      if (snapshot.sources.length && initialDecision.shouldEmbed) {
        for (const candidateProfile of [...new Set([profileId, space.fallbackProfileId].filter(Boolean))]) {
          profileId = candidateProfile;
          try {
            const profile = resolveRetrievalModelProfile('embedding', profileId);
            embedded = await this.embeddings.embedQuery(query, { signal, profileId });
            const status = this.embeddings.status(profileId);
            // Registered model/space identity determines dimensions; legacy seams may omit optional metadata.
            // 注册模型或空间身份确定维数；旧接口可省略可选元信息，但声明的维数必须与实际向量一致。
            const registeredModel = status.embeddingSpaceId === profile.embeddingSpaceId || embedded.embeddingSpaceId === profile.embeddingSpaceId ||
              status.modelVersion === profile.modelVersion || embedded.modelVersion === profile.modelVersion;
            const expectedDimensions = registeredModel ? profile.dimensions : status.dimensions;
            if (embedded.profileId !== profileId || status.embeddingSpaceId && embedded.embeddingSpaceId !== status.embeddingSpaceId ||
                hasModelMetadataMismatch(status, embedded) ||
                !(Array.isArray(embedded.vector) || embedded.vector instanceof Float32Array) ||
                expectedDimensions !== undefined && status.dimensions !== undefined && status.dimensions !== expectedDimensions ||
                expectedDimensions !== undefined && (embedded.vector.length !== expectedDimensions ||
                  embedded.dimensions !== undefined && embedded.dimensions !== expectedDimensions) ||
                embedded.dimensions !== undefined && embedded.vector.length !== embedded.dimensions)
              throw retrievalFailure('Embedding result belongs to another model space. / 嵌入结果不属于所选模型空间。', 'RETRIEVAL_MODEL_SPACE_MISMATCH', 409);
            break;
          } catch (error) {
            if (signal.aborted) throw error;
            embedded = undefined;
            embeddingDiagnostic = error.code ?? 'EMBEDDING_FAILED';
          }
        }
      }
      const result = lexical && !embedded ? lexical : await this.index.search({ query, scopeKeys: snapshot.scopes, limit: budget.fusedCandidates, channelCandidates: budget.channelCandidates,
        retrievalIntent, ann: snapshot.settings.local.ann,
        queryVector: embedded?.vector, embeddingProfileId: embedded?.profileId,
        embeddingModelVersion: embeddingVersion(embedded?.modelVersion), embeddingSpaceId: embedded?.embeddingSpaceId, signal });
      if (snapshot.indexingPending) result.indexingPending = true;
      if (snapshot.indexingPartial || snapshot.sourceScan?.coverage?.complete === false) {
        result.indexingPartial = true; result.sourceCoverage = snapshot.sourceScan?.coverage;
      }
      result.vectorSpacePolicy = { profileId: embedded?.profileId ?? null, targetProfileId: space.targetProfileId,
        state: space.state, compatibleFallback: embedded && embedded.profileId !== space.targetProfileId,
        ...(embeddingDiagnostic ? { diagnostic: embeddingDiagnostic } : {}) };
      if (semanticEnabled && !initialDecision.shouldEmbed && !lexical?.items.some(item => hasExactRetrievalTarget(item, retrievalIntent)))
        result.embeddingDiagnostic = status.errorCode ?? 'EMBEDDING_PREPARATION_PENDING';
      if (embeddingDiagnostic) result.embeddingDiagnostic = embeddingDiagnostic;
      // Search is bounded evidence acquisition, never an exhaustive symbol-reference enumeration.
      // 搜索只提供有界候选证据，不能冒充对符号引用的穷举。
      result.coverage = { ...result.coverage, operation: 'search', complete: false, candidateLimit: budget.fusedCandidates };
      result.budget = budget;
      result.items = withModelReferences(result.items);
      // Targeted current reads replace the second whole-library/history/tree snapshot.
      // 只回读命中来源并复核其版本，避免第二次全量资料、聊天和目录扫描。
      const candidateCount = result.items.length, freshChecks = new Map();
      const unique = deduplicateCandidates(result.items, { existingContext, retrievalIntent });
      const candidateAssessment = assessEvidence(unique.items, query, { requiresSourceRead, retrievalIntent });
      const rerankDecision = this._planEvidence({ query, gap, intent: retrievalIntent, taskType,
        items: unique.items, assessment: candidateAssessment,
        rerankerStatus: snapshot.settings.local.rerankProfileId ? this.reranker?.status(snapshot.settings.local.rerankProfileId) : null });
      const reranked = await this._rerank(context, query, unique.items, snapshot, taskType, signal, rerankDecision);
      result.rerankDecision = { executed: Boolean(reranked.rerank), reason: rerankDecision.rerankReason };
      let current = reranked.items, selected;
      const invalidSourceIds = new Set();
      do {
        selected = selectCandidates(current, { query, retrievalIntent, taskContext: snapshot.taskContext,
          limit, maximumTokens, requiresSourceRead });
        const pending = selected.items.map(item => this._fresh(item, snapshot, signal, freshChecks, context));
        const checked = await Promise.allSettled(pending);
        signal?.throwIfAborted();
        const failed = checked.find(item => item.status === 'rejected');
        if (failed) throw failed.reason;
        const stale = selected.items.filter((_, index) => !checked[index].value);
        if (!stale.length) break;
        for (const item of stale) invalidSourceIds.add(item.sourceId);
        const available = deduplicateCandidates(result.items.filter(item => !invalidSourceIds.has(item.sourceId)), { existingContext, retrievalIntent }).items;
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
      result.evidenceState = { authorization: 'checked', freshness: 'current', conclusion: 'not-verified' };
      result.outcome = retrievalOutcome(result);
      result.acquisition = this.evaluationPolicy?.gaps === false ? { state: 'disabled', remainingSearches: 0,
        remainingGapSearches: 0 } : acquisition.observe(ticket, result);
      const nextDecision = this._planEvidence({ query, gap, intent: retrievalIntent, taskType,
        items: result.items, assessment: result.evidenceAssessment, indexingPending: result.indexingPending,
        remainingSearches: Math.min(result.acquisition.remainingSearches, result.acquisition.remainingGapSearches),
        newEvidenceCount: result.acquisition.newEvidenceCount });
      result.acquisition.decision = this.evaluationPolicy?.gaps === false ? nextDecision :
        acquisition.applyProgressDecision(nextDecision, result.acquisition);
      result.acquisition.next = result.acquisition.decision.next;
      return result;
    }, { signal }));
  }

  read(context, { sourceRef, offset = 0, limit = 8000, mode = 'page', anchorOffset, beforeCharacters = 384, gap }, { signal } = {}) {
    signal = this._signal(signal);
    return this._serialize(async () => {
      validateEvidenceGap(gap);
      if (!['page', 'window', 'section', 'unit'].includes(mode)) throw retrievalFailure('Invalid source read mode. / 资料读取模式无效。');
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
      const acquisition = this.acquisitions.get(context);
      return { ...item, ...(canonicalSourceRef !== sourceRef ? { sourceRef } : {}),
        evidenceDecision: acquisition?.observeRead(item, { gap, mode, sourceRef: canonicalSourceRef }) ?? { sufficiency: 'not-evaluated',
          missingInformation: gap ?? null, sourceVersionChecked: true, contentRead: true,
          next: 'answer-if-this-context-supports-the-requested-facts-otherwise-name-the-remaining-gap' } };
    });
  }

  _signal(signal) {
    return signal ? AbortSignal.any([signal, this.shutdown.signal]) : this.shutdown.signal;
  }

  relations(...args) { return this.evidenceService.relations(...args); }
  assess(...args) { return this.evidenceService.assess(...args); }
  experience(...args) { return this.evidenceService.experience(...args); }

  modelProfiles() { return listRetrievalModelProfiles(); }
  evidence(...args) { return this.evidenceService.prepare(...args); }
  finalizeEvidence(...args) { return this.evidenceService.finalize(...args); }
  validateFinal(...args) { return this.evidenceService.validateFinal(...args); }
  importSource(input, options) { return this.sourceService.import(input, options); }
  removeSource(id, input, options) { return this.sourceService.remove(id, input, options); }
  rebuild(options) { return this.sourceService.rebuild(options); }
  cancelJob(id) { return this.sourceService.cancelJob(id); }

  close() {
    if (!this.closure) this.closure = this._close();
    return this.closure;
  }

  async _close() {
    this.closed = true;
    this.shutdown.abort();
    await this.spacePolicy.close();
    const closures = await Promise.allSettled([this.sourceService.close({
      releaseInference: async () => {
        const released = await Promise.allSettled([this.embeddings.close(), this.reranker?.close(), this.structures.close?.()]);
        const failure = released.find(item => item.status === 'rejected');
        if (failure) throw failure.reason;
      }
    }), this.queue.catch(() => {})]);
    const indexClosure = await Promise.allSettled([this.index.close()]);
    const resourceClosure = await Promise.allSettled([this.ownsResources ? this.resources.close() : Promise.resolve()]);
    const failure = [...closures, ...indexClosure, ...resourceClosure].find(item => item.status === 'rejected' && item.reason?.name !== 'AbortError');
    if (failure) throw failure.reason;
  }
}

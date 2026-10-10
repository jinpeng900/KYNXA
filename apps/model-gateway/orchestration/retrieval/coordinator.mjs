import { resolve } from 'node:path';
import { RetrievalSettingsStore } from '../../data/retrieval/settings.mjs';
import { RetrievalIndex } from '../../data/retrieval/index.mjs';
import { RetrievalStructureService } from '../../data/retrieval/structure-service.mjs';
import { EMBEDDING_TEXT_VERSION } from '../../data/retrieval/retrieval-text.mjs';
import { MAX_QUERY_CHARACTERS, retrievalFailure, validateRetrievalIntent } from '../../data/retrieval/retrieval-contracts.mjs';
import { SourceLibrary } from '../../data/retrieval/source-library.mjs';
import { RetrievalJobStore } from '../../data/retrieval/job-store.mjs';
import { EmbeddingRouter } from '../../models/retrieval/embedding-router.mjs';
import { RerankerRouter } from '../../models/retrieval/reranker-router.mjs';
import { readSourceFile, readSourceFileWindow } from '../../tools/retrieval/source-reader.mjs';
import { sourceFileRevision } from '../../data/retrieval/retrieval-contracts.mjs';
import { validateId } from '../../platform/conversation-id.mjs';
import { toolFailure } from '../../platform/tool-paths.mjs';
import { sourceHash, sourceIdentity, visibleScopes, projectConversationSources } from './source-projection.mjs';
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

// GPU policy selects the registered strict DML contract; CPU policy also constrains the execution session.
// GPU 策略选择已注册的严格 DML 合同；CPU 策略同时约束实际执行会话。
function rerankerExecution(local) {
  let profileId = local.rerankProfileId;
  const devicePreference = local.rerankDevicePolicy === 'cpu' ? 'cpu' : 'auto';
  if (['builtin-multilingual-reranker', 'builtin-multilingual-reranker-dml-q8'].includes(profileId)) {
    if (local.rerankDevicePolicy === 'cpu') profileId = 'builtin-multilingual-reranker';
    else if (local.rerankDevicePolicy === 'gpu') profileId = 'builtin-multilingual-reranker-dml-q8';
  }
  return { profileId, devicePreference };
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
      embeddings: { status: (...args) => this.embeddings.status(...args) }, index: this.index });
    this.reranker = reranker === undefined ? new RerankerRouter({ resourceService: this.resources }) : reranker;
    this.library = new SourceLibrary({ root: conversations.root, conversationStore: conversations });
    this.evidenceReferences = tools.results ? new EvidenceReferenceStore({ conversationStore: conversations, resultStore: tools.results }) : null;
    this.jobs = new RetrievalJobStore(conversations.root);
    this.experiences = new TaskExperienceStore(conversations.root);
    this.excludedRoots = [conversations.root, ...excludedRoots].filter(Boolean);
    this.acquisitions = new WeakMap();
    this.queue = Promise.resolve(); this.closed = false;
    this.memoryIndexPending = new Map();
    this.memoryIndexState = { publishedUpdates: 0, partialUpdates: 0, skippedUpdates: 0, deferred: 0, errorCode: null };
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

  /** Apply only retrieval model policy; draining old sessions never cancels accepted work or touches generation/MCP.
   * 仅应用检索模型策略；旧会话排空不取消已接纳请求，也不修改生成模型或 MCP。
   */
  async configureInferenceSettings(projectId = null, { previous, signal = this.shutdown.signal } = {}) {
    const settings = await this.effective(projectId), local = settings.local;
    const changed = fields => !previous || fields.some(field => previous.local[field] !== local[field]);
    if (changed(['enabled', 'semantic', 'embeddingProfileId', 'embeddingDevicePolicy'])) {
      if (!local.enabled || local.semantic === 'off' || local.embeddingProfileId === null) await this.embeddings.retire?.({ signal });
      else await this.embeddings.configure?.({ profileId: await this.spacePolicy.target(settings, signal),
        devicePreference: local.embeddingDevicePolicy === 'cpu' ? 'cpu' : 'auto', retireOtherProfiles: true, signal });
    }
    if (changed(['enabled', 'rerankProfileId', 'rerankDevicePolicy'])) {
      if (!local.enabled || local.rerankProfileId === null) await this.reranker?.retire?.({ signal });
      else await this.reranker?.configure?.({ ...rerankerExecution(local), retireOtherProfiles: true, signal });
    }
    return settings;
  }

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
    const observedProfileId = await this.spacePolicy.target(settings, this.shutdown.signal);
    const indexed = await this.index.status(), embedding = this.embeddings.status(observedProfileId,
      { devicePreference: settings.local.embeddingDevicePolicy === 'cpu' ? 'cpu' : 'auto' });
    const reranking = rerankerExecution(settings.local);
    const jobs = await this.jobs.list();
    return { ...indexed, backend: 'sqlite', sourceCount: indexed.sources, chunkCount: indexed.chunks,
      embedding: { ...embedding, available: ['ready', 'loading'].includes(embedding.state) },
      configuredEmbeddingProfileId: settings.local.embeddingProfileId,
      embeddingProfiles: this.modelProfiles().filter(profile => profile.kind === 'embedding').map(profile => this.embeddings.status(profile.id)),
      reranking: reranking.profileId ? this.reranker?.status(reranking.profileId, { devicePreference: reranking.devicePreference }) ?? { state: 'unavailable', loaded: false }
        : { state: 'disabled', loaded: false },
      modelProfiles: this.modelProfiles(), parsing: this.structures.status?.(), jobs,
      resources: await this.resources.snapshot(), vectorSpacePolicy: this.spacePolicy.status(jobs),
      externalModels: this.externalModelStatus?.() ?? [],
      memoryIndexing: { ...this.memoryIndexState, pending: this.memoryIndexPending?.size ?? 0 },
      deployment: { platform: 'windows', gpu: 'single-nvidia-dml-verified', otherPlatformsSupported: false } };
  }

  /** Persisted memory changes revoke old evidence first; only confirmed live entries enter the selective index.
   * 正式记忆变更先使旧证据失效；仅已确认且来源有效的条目进入选择性索引，草稿不成为模型事实。
   */
  async onMemoryChanged(event) {
    if (this.closed || event.previousStatus !== 'confirmed' && event.status !== 'confirmed') return;
    const expectedScope = event.scope === 'user' ? 'user' : `${event.scope}:${event.scopeId?.toLowerCase()}`;
    if (expectedScope !== event.scopeKey || event.sourceId !== sourceIdentity('memory', expectedScope, event.memoryId))
      throw toolFailure('记忆变更身份无效。', 'INVALID_MEMORY_CHANGE', 409);
    await this._serialize(async () => {
      if (this.closed) return;
      await this.index.removeSource(event.sourceId, { scopeKeys: [event.scopeKey], permanent: false, signal: this.shutdown.signal });
      this.sourceService.indexer.invalidate(event.sourceId);
      this.sourceService.corpusSyncCache.clear();
      this.sourceService.sync.conversationCache.clear();
    });
    if (this.closed || event.status !== 'confirmed') { this.memoryIndexPending.delete(event.sourceId); return; }
    this.memoryIndexPending.set(event.sourceId, event);
    // Queue limits bound background demand; deferred entries remain discoverable through authoritative scope sync.
    // 队列限制后台需求；被延后的条目仍由正式作用域同步发现，不能扩大范围或把未索引当作不存在。
    while (this.memoryIndexPending.size > 256) {
      this.memoryIndexPending.delete(this.memoryIndexPending.keys().next().value);
      this.memoryIndexState.deferred++;
    }
    this._startMemoryIndex();
  }

  _startMemoryIndex() {
    if (this.closed || this.memoryIndexWork || !this.memoryIndexPending.size) return;
    const work = Promise.resolve().then(() => this._indexChangedMemories()).catch(error => {
      if (!this.closed) this.memoryIndexState.errorCode = /^[A-Z][A-Z0-9_]{0,127}$/u.test(error.code ?? '')
        ? error.code : 'MEMORY_INDEX_FAILED';
    }).finally(() => {
      if (this.memoryIndexWork === work) this.memoryIndexWork = null;
      // A change arriving between loop completion and this microtask still needs its own wakeup.
      // 循环结束和清理微任务之间到达的变更仍须唤醒；关闭后不能重启后台工作。
      this._startMemoryIndex();
    });
    this.memoryIndexWork = work;
  }

  async _currentMemorySource(event) {
    if (event.scope === 'project') {
      const project = await this.conversations.describeProject(event.scopeId);
      if (project.isArchived || project.isFolderlessWorkspace) return null;
    } else if (event.scope === 'chat') {
      const relationship = await this.conversations.describeConversation(event.scopeId);
      if (relationship.isArchived || relationship.projectArchived) return null;
    }
    const document = event.scope === 'chat'
      ? (await this.memory.listFor(event.scopeId)).scopes.find(value => value.scope === 'chat')
      : await this.memory.listScope(event.scope, event.scopeId);
    const entry = document.entries.find(value => value.id === event.memoryId);
    if (!entry || entry.status !== 'confirmed' || entry.active === false || entry.revision !== event.entryRevision) return null;
    return { sourceId: event.sourceId, scopeKey: event.scopeKey, sourceType: 'memory', title: entry.kind || 'Memory',
      locator: { memoryId: entry.id }, text: entry.content, contentHash: sourceHash(entry.content), sourceRevision: entry.revision };
  }

  async _indexChangedMemories() {
    while (this.memoryIndexPending.size && !this.closed) {
      const [id, event] = this.memoryIndexPending.entries().next().value;
      this.memoryIndexPending.delete(id);
      try {
        const source = await this._currentMemorySource(event);
        if (!source || this.closed) continue;
        const projectId = event.scope === 'project' ? event.scopeId : event.scope === 'chat'
          ? (await this.conversations.describeConversation(event.scopeId)).projectId : null;
        const settings = await this.spacePolicy.indexingSettings(await this.effective(projectId), this.shutdown.signal);
        if (!settings.local.enabled) continue;
        const isCurrent = async current => {
          const live = await this._currentMemorySource(event);
          const effective = await this.spacePolicy.indexingSettings(await this.effective(projectId), this.shutdown.signal);
          return Boolean(live && live.contentHash === current.contentHash && effective.local.enabled &&
            effective.local.semantic === settings.local.semantic && effective.local.embeddingProfileId === settings.local.embeddingProfileId &&
            effective.local.embeddingDevicePolicy === settings.local.embeddingDevicePolicy);
        };
        // Lexical availability precedes optional embedding; a missing GPU/model cannot delay confirmed memory use.
        // 词法可用先于可选嵌入；显卡或模型缺失不能阻碍已确认记忆使用。
        const lexical = await this._serialize(() => this.sourceService.upsert([source], settings, this.shutdown.signal, undefined,
          { semantic: false, isCurrent }));
        const lexicalEntry = lexical.coverage?.sources.find(value => value.sourceId === id);
        if (lexicalEntry?.lexical !== 'ready') {
          this.memoryIndexState.skippedUpdates++;
          this.memoryIndexState.errorCode = lexicalEntry?.errorCode ?? lexical.semantic?.diagnosticCodes[0] ?? 'MEMORY_PUBLICATION_UNVERIFIED';
          continue;
        }
        let semantic;
        if (settings.local.semantic !== 'off' && settings.local.embeddingProfileId !== null)
          semantic = await this.sourceService.upsert([source], settings, this.shutdown.signal, undefined, { semantic: true, isCurrent });
        if (!await this._currentMemorySource(event)) { this.memoryIndexState.skippedUpdates++; continue; }
        this.memoryIndexState.publishedUpdates++;
        const semanticEntry = semantic?.coverage?.sources.find(value => value.sourceId === id);
        if (semantic && semanticEntry?.semantic !== 'ready') {
          this.memoryIndexState.partialUpdates++;
          this.memoryIndexState.errorCode = semanticEntry?.errorCode ?? semantic.semantic?.diagnosticCodes[0] ?? 'MEMORY_SEMANTIC_PENDING';
        } else this.memoryIndexState.errorCode = null;
      } catch (error) {
        if (this.closed || this.shutdown.signal.aborted) break;
        this.memoryIndexState.errorCode = /^[A-Z][A-Z0-9_]{0,127}$/u.test(error.code ?? '') ? error.code : 'MEMORY_INDEX_FAILED';
      }
    }
  }

  async flushMemoryIndex() {
    this._startMemoryIndex();
    while (this.memoryIndexWork) await this.memoryIndexWork;
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
      source = await this.library.readSource(item.sourceId, { scopeKeys: snapshot.scopes, sourceRevision: item.sourceRevision,
        limits: snapshot.settings.local.indexing, signal });
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
          maximumSourceBytes: snapshot.settings.local.indexing?.maximumSourceBytes,
          maximumDocumentInputBytes: snapshot.settings.local.indexing?.maximumDocumentInputBytes,
          maximumDocumentOutputBytes: snapshot.settings.local.indexing?.maximumDocumentOutputBytes,
          maximumPdfPages: snapshot.settings.local.indexing?.maximumPdfPages, resourceService: this.resources, signal,
          ...(source.locator.extraction?.pageWindow ? { pdfPageWindow: { startPage: source.locator.extraction.pageWindow.startPage,
            endPage: source.locator.extraction.pageWindow.endPage,
            rawContentHash: source.locator.extraction.rawContentHash } } : {}) };
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

  async _rerank(context, query, candidates, snapshot, taskType, signal, decision, rerankCandidates = 20) {
    const execution = rerankerExecution(snapshot.settings.local);
    decision ??= this._planEvidence({ query, items: candidates, taskType,
      rerankerStatus: execution.profileId ? this.reranker?.status(execution.profileId, { devicePreference: execution.devicePreference }) : null });
    if (['complex', 'research'].includes(taskType) && snapshot.settings.local.rerankProfileId && this.reranker &&
        decision?.rerankReason === 'optional-model-not-ready')
      return { items: candidates, diagnostic: this.reranker.status(execution.profileId, { devicePreference: execution.devicePreference }).errorCode ?? 'RERANK_UNAVAILABLE' };
    if (!decision?.shouldRerank || !snapshot.settings.local.rerankProfileId || !this.reranker || candidates.length < 2)
      return { items: candidates };
    try { resolveRetrievalModelProfile('reranker', execution.profileId); }
    catch (error) { return { items: candidates, diagnostic: error.code }; }
    const status = this.reranker.status(execution.profileId, { devicePreference: execution.devicePreference });
    if (status.profileId !== execution.profileId || !['ready', 'loading'].includes(status.state))
      return { items: candidates, diagnostic: status.errorCode ?? 'RERANK_UNAVAILABLE' };
    try {
      const result = await this.reranker.rerank({ ...execution,
        context, query, candidates, settings: snapshot.settings, signal, limit: rerankCandidates });
      if (result.profileId !== undefined && result.profileId !== execution.profileId ||
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
      // A partial reranker response only changes its scored prefix; unscored candidates remain selectable.
      // 部分重排回执只调整已评分前缀，其余候选仍可入选，不能在下游静默丢掉召回池。
      for (const item of candidates) if (!seen.has(item.sourceRef)) ranked.push(item);
      return scoredCount ? { items: ranked, rerank: { profileId: result.profileId, modelVersion: result.modelVersion,
        candidateLimit: rerankCandidates, scoredCandidates: scoredCount,
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
      budget.audit = { ...budget.audit, approved: { channelCandidates: budget.channelCandidates,
        fusedCandidates: budget.fusedCandidates, limit: budget.limit, maximumTokens: budget.maximumTokens,
        rerankCandidates: budget.rerankCandidates } };
      limit = budget.limit; maximumTokens = budget.maximumTokens;
      if (this.closed) throw toolFailure('检索服务已关闭。', 'RETRIEVAL_CLOSED', 409);
      if (typeof query !== 'string' || !query.trim() || query.length > MAX_QUERY_CHARACTERS ||
          !Number.isSafeInteger(limit) || limit < 1 || limit > 60 ||
          !Number.isSafeInteger(maximumTokens) || maximumTokens < 0 || maximumTokens > 32768)
        throw retrievalFailure('Invalid query or evidence budget. / 检索查询、数量或证据预算无效。');
      validateEvidenceGap(gap);
      if (maximumTokens === 0) return { items: [], strategy: 'context-budget-exhausted', vectorAvailable: false,
        outcome: retrievalOutcome({ strategy: 'context-budget-exhausted' }),
        budget: { ...budget, audit: { ...budget.audit, actual: { candidates: 0, selectedCount: 0, usedTokens: 0 },
          earlyCutReasons: [{ reason: 'model-context-exhausted', count: 0 }] } },
        evidenceAssessment: { ...assessEvidence([], query), reason: 'no-remaining-model-context' },
        acquisition: { shouldContinue: false, next: 'answer-with-current-evidence-or-state-the-context-limit' } };
      const withModelReferences = items => modelReferences
        ? items.map((item, index) => ({ ...item, modelSourceRef: evidenceSourceRef(archiveId, index + 1) })) : items;
      await this.initialize();
      const snapshot = await this._snapshot(context, signal);
      const configuredRerankCandidates = snapshot.settings.local.rerankCandidates;
      const requestedRerankCandidates = configuredRerankCandidates ?? budget.rerankCandidates;
      const resourceRerankLimit = [20, 40, 60].includes(lease?.suggestions?.rerankCandidateLimit)
        ? lease.suggestions.rerankCandidateLimit : 60;
      budget.rerankCandidates = Math.min(requestedRerankCandidates, resourceRerankLimit);
      budget.audit = { ...budget.audit,
        configured: { ...budget.audit.configured, localRerankCandidates: configuredRerankCandidates ?? null },
        approved: { ...budget.audit.approved, rerankCandidates: budget.rerankCandidates },
        rerankRequestedCandidates: requestedRerankCandidates, resourceRerankLimit,
        rerankBudgetSource: configuredRerankCandidates != null ? 'local-setting' :
          lease?.suggestions?.rerankCandidates !== undefined ? 'resource-suggestion' : 'task-policy' };
      if (budget.rerankCandidates < requestedRerankCandidates)
        budget.audit.earlyCutReasons.push({ reason: 'resource-grant', field: 'rerankCandidates',
          proposed: requestedRerankCandidates, approved: budget.rerankCandidates, unit: 'candidates' });
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
          const cached = { ...structuredClone(ticket.cached), indexingPending: Boolean(snapshot.indexingPending),
            indexingPartial: Boolean(snapshot.indexingPartial || snapshot.sourceScan?.coverage?.complete === false),
            indexingPaused: Boolean(snapshot.indexingPaused), indexingDiagnostic: snapshot.indexingDiagnostic };
          const items = withModelReferences(cached.items);
          acquisition.observeProjection(items);
          return { ...cached, items,
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
          signal: this.shutdown.signal, automatic: true }));
      const initialDevicePreference = profileId !== space.targetProfileId ? 'cpu' : space.devicePreference;
      const status = this.embeddings.status(profileId, { devicePreference: initialDevicePreference });
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
          const devicePreference = profileId !== space.targetProfileId ? 'cpu' : space.devicePreference;
          try {
            const profile = resolveRetrievalModelProfile('embedding', profileId);
            embedded = await this.embeddings.embedQuery(query, { signal, profileId, devicePreference });
            const status = this.embeddings.status(profileId, { devicePreference });
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
      let result = lexical && !embedded ? lexical : await this.index.search({ query, scopeKeys: snapshot.scopes, limit: budget.fusedCandidates, channelCandidates: budget.channelCandidates,
        retrievalIntent, ann: snapshot.settings.local.ann,
        queryVector: embedded?.vector, embeddingProfileId: embedded?.profileId,
        embeddingModelVersion: embeddingVersion(embedded?.modelVersion), embeddingSpaceId: embedded?.embeddingSpaceId, signal });
      const originalQuery = context.message?.trim();
      // One empty automatic derived query may fall back to the original utterance in exactly the same authorized scope.
      // 自动派生查询为空时最多回退一次用户原话，授权范围不变；显式领域、路径、符号限制不放宽。
      if (!result.items.length && preparedIntent !== undefined && retrievalIntent.domain === 'mixed' &&
          !retrievalIntent.path && !retrievalIntent.symbol && originalQuery && originalQuery.length <= MAX_QUERY_CHARACTERS &&
          originalQuery.normalize('NFKC') !== query.normalize('NFKC')) {
        const fallback = await this.index.search({ query: originalQuery, scopeKeys: snapshot.scopes,
          limit: budget.fusedCandidates, channelCandidates: budget.channelCandidates,
          retrievalIntent, ann: snapshot.settings.local.ann, signal });
        result = { ...fallback, fallback: { attempted: true, kind: 'original-query', attempts: 1,
          authorizationExpanded: false, explicitConstraintsPreserved: true, recoveredCandidates: fallback.items.length } };
      }
      if (snapshot.indexingPending) result.indexingPending = true;
      if (snapshot.indexingPaused) {
        result.indexingPaused = true; result.indexingDiagnostic = snapshot.indexingDiagnostic ?? 'INDEX_CANCELLED';
      }
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
      // Candidate pools may exceed 60; a fixed-length placeholder preserves token accounting before selection.
      // 候选池可超过 60；选择前使用固定长度占位引用计算 token，唯一的真实引用只分配给最终入选证据。
      if (modelReferences) {
        const provisionalModelSourceRef = evidenceSourceRef(archiveId, 1);
        result.items = result.items.map(item => ({ ...item, modelSourceRef: provisionalModelSourceRef }));
      }
      // Targeted current reads replace the second whole-library/history/tree snapshot.
      // 只回读命中来源并复核其版本，避免第二次全量资料、聊天和目录扫描。
      const candidateCount = result.items.length, freshChecks = new Map();
      const unique = deduplicateCandidates(result.items, { existingContext, retrievalIntent });
      const candidateAssessment = assessEvidence(unique.items, query, { requiresSourceRead, retrievalIntent });
      const reranking = rerankerExecution(snapshot.settings.local);
      const rerankDecision = this._planEvidence({ query, gap, intent: retrievalIntent, taskType,
        items: unique.items, assessment: candidateAssessment,
        rerankerStatus: reranking.profileId ? this.reranker?.status(reranking.profileId,
          { devicePreference: reranking.devicePreference }) : null });
      const reranked = await this._rerank(context, query, unique.items, snapshot, taskType, signal, rerankDecision, budget.rerankCandidates);
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
      const earlyCutReasons = [...budget.audit.earlyCutReasons, ...selected.selection.earlyCutReasons];
      if (unique.duplicateCount) earlyCutReasons.push({ reason: 'duplicate-or-overlapping-evidence', count: unique.duplicateCount });
      if (unique.alreadyPresentCount) earlyCutReasons.push({ reason: 'already-in-model-context', count: unique.alreadyPresentCount });
      if (invalidSourceIds.size) earlyCutReasons.push({ reason: 'stale-or-revoked-source', count: invalidSourceIds.size, unit: 'sources' });
      budget.audit = { ...budget.audit, actual: { candidates: candidateCount, uniqueCandidates: unique.items.length,
        currentCandidates: current.length, selectedCount: result.items.length, usedTokens: selected.selection.usedTokens,
        rerankScoredCandidates: reranked.rerank?.scoredCandidates ?? 0 }, earlyCutReasons,
        ann: { configured: snapshot.settings.local.ann, executedBackend: result.semanticBackend ?? 'none',
          actualPerShardParameters: 'not-reported-by-search' } };
      result.evidenceAssessment = selected.evidenceAssessment;
      if (!result.items.length && unique.alreadyPresentCount)
        result.evidenceAssessment = assessEvidence([], query, { alreadyPresentCount: unique.alreadyPresentCount });
      if (reranked.rerank) result.rerank = reranked.rerank;
      if (reranked.diagnostic) result.rerankDiagnostic = reranked.diagnostic;
      // Re-check the relationship and settings after worker/IO awaits.
      // worker 与文件 IO 等待后复核工作关联和开关，旧请求不能复活已禁用资料。
      await this._assertCurrent(context, snapshot);
      signal?.throwIfAborted();
      // Only returned evidence creates final version dependencies; selection does not certify a full source read.
      // 仅实际返回的证据登记最终版本依赖；选入摘录不能冒充已回读完整来源。
      acquisition.observeProjection(result.items);
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
    }, { signal, onCapacityUnavailable: options => this.embeddings.releaseIdleResources?.(options) }));
  }

  observeProvidedOriginals(context, items) {
    return this.acquisitions.get(context)?.observeProvidedOriginals(items);
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
      // A valid reference may come from an earlier turn; direct reads still own this turn's final version dependencies.
      // 有效引用可来自上轮；本轮直接回读仍须登记最终答复依赖的来源版本，不能依赖先执行搜索。
      let acquisition = this.acquisitions.get(context);
      if (!acquisition) {
        acquisition = new EvidenceAcquisition({ research: retrievalPlan(context.message ?? gap ?? '').taskType === 'research' });
        this.acquisitions.set(context, acquisition);
      }
      // A completed heading/section can still precede unread source text; give an exact forward page, not a guessed anchor.
      // 标题或章节读完并不等于整份来源读完；提供准确的正文续读位置，避免模型反复猜测锚点。
      const continuation = Number.isSafeInteger(item.nextOffset) && item.nextOffset < item.totalCharacters
        ? { tool: 'knowledge.read', arguments: { sourceRef, mode: 'page', offset: item.nextOffset, limit },
          offsetUnit: 'utf16-code-units', remainingRange: { startOffset: item.nextOffset, endOffset: item.totalCharacters } }
        : null;
      return { ...item, ...(canonicalSourceRef !== sourceRef ? { sourceRef } : {}), continuation,
        evidenceDecision: acquisition.observeRead(item, { gap, mode, sourceRef: canonicalSourceRef }) };
    });
  }

  _signal(signal) {
    return signal ? AbortSignal.any([signal, this.shutdown.signal]) : this.shutdown.signal;
  }

  plan(...args) { return this.evidenceService.plan(...args); }
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
    this.memoryIndexPending.clear();
    this.shutdown.abort();
    await this.spacePolicy.close();
    const closures = await Promise.allSettled([this.sourceService.close({
      releaseInference: async () => {
        const released = await Promise.allSettled([this.embeddings.close(), this.reranker?.close(), this.structures.close?.()]);
        const failure = released.find(item => item.status === 'rejected');
        if (failure) throw failure.reason;
      }
    }), this.queue.catch(() => {}), this.memoryIndexWork]);
    const indexClosure = await Promise.allSettled([this.index.close()]);
    const resourceClosure = await Promise.allSettled([this.ownsResources ? this.resources.close() : Promise.resolve()]);
    const failure = [...closures, ...indexClosure, ...resourceClosure].find(item => item.status === 'rejected' && item.reason?.name !== 'AbortError');
    if (failure) throw failure.reason;
  }
}

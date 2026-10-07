import { readSourceTree } from '../../tools/retrieval/source-reader.mjs';
import { resolve } from 'node:path';
import { toolFailure } from '../../platform/tool-paths.mjs';
import { SourceSyncService } from './source-sync.mjs';
import { SourceIndexer } from './source-indexer.mjs';
import { IndexJobService } from './index-job-service.mjs';
import { sourceIdentity } from './source-projection.mjs';

function corpusVersion(value) {
  return JSON.stringify({ indexEpoch: value.indexEpoch,
    scopes: value.scopes.map(scope => [scope.scopeKey, scope.corpusGeneration ?? scope.generation]) });
}

/** Source mutation transactions and index lifecycle; retrieval planning stays in its coordinator.
 * 来源变更事务与索引生命周期，检索规划继续由协调器负责。 */
export class SourceIndexService {
  constructor({ library, index, jobs, embeddings, structures, getProject, validateProject = getProject,
    effectiveSettings, serialize, excludedRoots = [], authorizeImport, readTree = readSourceTree, readFile }) {
    this.library = library;
    this.index = index;
    this.jobs = jobs;
    this.serialize = serialize;
    this.authorizeImport = authorizeImport;
    this.effectiveSettings = effectiveSettings;
    this.getProject = getProject;
    this.readTree = readTree;
    this.excludedRoots = excludedRoots;
    this.operations = new Map();
    this.restoredCheckpoints = new Set();
    this.corpusSyncCache = new Map();
    this.closed = false;
    this.sync = new SourceSyncService({ library, getProject, excludedRoots, readTree,
      onFolderChanged: projectId => this.rebuild({ projectId, dirty: true }) });
    this.indexer = new SourceIndexer({ library, index, embeddings, structures, serialize, effectiveSettings, getProject, excludedRoots, readFile });
    this.lifecycle = new IndexJobService({ jobs, validateProject,
      prepareSources: async (projectId, signal) => {
        const settings = await effectiveSettings(projectId);
        signal.throwIfAborted();
        const scopes = ['user', ...(projectId ? [`project:${projectId}`] : [])];
        const registered = await this.sync.librarySnapshot(scopes, settings, signal);
        const mounted = await this.sync.mountedSnapshot(projectId, settings, signal);
        const sources = [...registered.sources, ...mounted.sources];
        const checkpoint = await this.checkpointFor(projectId, settings, signal);
        signal.throwIfAborted();
        if (settings.local.enabled === false)
          throw toolFailure('本地检索已停用，不能恢复索引任务。', 'RETRIEVAL_DISABLED', 409);
        return { settings, sources, checkpoint, scopes, preparationFailures: this.indexer.preparationFailures,
          loadSource: (source, ownedSignal) => (source.sourceType === 'work-file' ? mounted : registered).loadSource(source, ownedSignal),
          isCurrent: (source, ownedSignal) => (source.sourceType === 'work-file' ? mounted : registered).isCurrent?.(source, ownedSignal) };
      },
      publishSources: (...args) => this.indexer.upsert(...args),
      restoreSources: (...args) => this.indexer.restore(...args),
      reuseSources: async (snapshot, projectId, signal) => {
        const checkpointId = snapshot.checkpoint?.checkpointId;
        if (!checkpointId || this.restoredCheckpoints.has(checkpointId)) return;
        this.restoredCheckpoints.add(checkpointId);
        while (this.restoredCheckpoints.size > 32) this.restoredCheckpoints.delete(this.restoredCheckpoints.values().next().value);
        const candidates = (await this.jobs.list()).filter(job => job.status === 'completed' && job.projectId === projectId &&
          job.checkpoint?.checkpointId === checkpointId).sort((left, right) => right.createdAt.localeCompare(left.createdAt));
        if (!candidates.length) return;
        // A completed job supplies only a reuse hint, never an instruction to execute its old work again.
        // 已完成任务仅提供复用线索，不会因此重新执行其历史工作。
        try {
          const records = await this.jobs.checkpointSources(candidates[0].jobId, candidates[0].checkpoint);
          await this.indexer.restore(snapshot.sources, snapshot.settings, records, signal);
          this.lastCheckpointError = undefined;
        } catch (error) {
          if (signal?.aborted) throw error;
          this.lastCheckpointError = error.code ?? 'INVALID_RETRIEVAL_CHECKPOINT';
        }
      },
      refreshSources: projectId => { this.sync.mountedCache.delete(projectId); this.sync.markChanged(projectId, null); },
      finalizeSources: async (snapshot, signal) => {
        await this.indexer.prune({ scopes: snapshot.scopes,
          identities: new Set(snapshot.sources.map(source => source.sourceId)) }, signal, { sourceTypes: ['knowledge', 'work-file'] });
        if (snapshot.preparationFailures === this.indexer.preparationFailures)
          await this.cacheCorpus(snapshot.sources, snapshot.scopes, snapshot.settings, signal);
        // Warm stable generations only after derivation ends, not after every embedding batch.
        // 仅在派生结束后预热稳定代次，不能每个嵌入批次都取消并重建大型图。
        await this.index.prepareVectors?.({ scopeKeys: snapshot.scopes, ann: snapshot.settings.local.ann, signal });
      } });
  }

  initialize() { return this.lifecycle.initialize(); }
  rebuild(options) { return this.lifecycle.rebuild(options); }
  cancelJob(id) { return this.lifecycle.cancelJob(id); }
  librarySources(...args) { return this.sync.librarySources(...args); }
  librarySnapshot(...args) { return this.sync.librarySnapshot(...args); }
  conversationSources(...args) { return this.sync.conversationSources(...args); }
  mountedSources(...args) { return this.sync.mountedSources(...args); }
  mountedSnapshot(...args) { return this.sync.mountedSnapshot(...args); }
  foregroundMountedSnapshot(...args) { return this.sync.foregroundMountedSnapshot(...args); }
  upsert(...args) { return this.indexer.upsert(...args); }
  invalidateMounted(projectId) { this.sync.mountedCache.delete(projectId?.toLowerCase() ?? null); }

  clearSourceCaches() {
    this.sync.libraryCache.clear();
    this.sync.conversationCache.clear();
    this.sync.mountedCache.clear();
    this.corpusSyncCache.clear();
  }

  async checkpointFor(projectId, settings, signal) {
    const project = projectId ? await this.getProject(projectId) : null;
    signal?.throwIfAborted();
    const root = settings.projectIndexing?.mountedFolder && project?.FolderPath ? resolve(project.FolderPath) : null;
    const bindingRevision = settings.projectIndexing?.bindingRevision ?? 0;
    const preparationVersion = this.indexer.preparationVersion();
    const settingsSignature = sourceIdentity(settings.local.enabled, settings.local.semantic,
      settings.local.embeddingProfileId, settings.local.vectorBackend, settings.local.indexing,
      settings.projectIndexing?.mountedFolder ?? false, bindingRevision);
    return { version: 1, kind: 'source-index',
      checkpointId: sourceIdentity(projectId, settingsSignature, root, bindingRevision, preparationVersion),
      settingsSignature, root, bindingRevision, preparationVersion, updatedAt: new Date().toISOString() };
  }

  async syncFormalSources(snapshot, signal) {
    signal?.throwIfAborted();
    const options = { semantic: false, loadSource: snapshot.loadSource, isCurrent: snapshot.isCurrent };
    if (!this.index.scopeVersion) {
      await this.indexer.upsert(snapshot.sources, snapshot.settings, signal, undefined, options);
      await this.indexer.prune(snapshot, signal);
      return;
    }
    const corpusScopes = snapshot.scopes.filter(scope => scope === 'user' || scope.startsWith('project:'));
    const corpusSources = snapshot.sources.filter(source => ['knowledge', 'work-file'].includes(source.sourceType));
    const otherSources = snapshot.sources.filter(source => !['knowledge', 'work-file'].includes(source.sourceType));
    const key = sourceIdentity(corpusScopes);
    const signature = this.corpusSignature(corpusSources, snapshot.settings);
    const version = corpusVersion(await this.index.scopeVersion({ scopeKeys: corpusScopes, signal }));
    const previous = this.corpusSyncCache.get(key);
    const cached = previous?.signature === signature && previous.version === version;
    let canCacheCorpus = cached;
    if (!cached && (snapshot.sourceScan?.backgroundPending || corpusSources.length > 512)) {
      // Large cold corpora are derived by durable jobs. Incomplete discovery must never prune unseen sources.
      // 大型冷语料由持久作业派生；目录发现未完成时绝不能裁掉尚未发现的来源。
      const projectId = snapshot.relationship?.isFolderlessWorkspace ? null
        : snapshot.relationship?.projectId ?? snapshot.settings.projectId ?? null;
      await this.rebuild({ projectId });
      canCacheCorpus = false;
      snapshot.indexingPending = true;
    } else if (!cached) {
      // Only complete corpus metadata plus the actual database epoch can skip synchronization; chat messages remain separate.
      // 只有完整语料元信息和真实数据库代次共同一致才跳过同步，聊天消息仍独立处理。
      if (previous && previous.version !== version)
        for (const source of corpusSources) this.indexer.invalidate(source.sourceId);
      const projectId = snapshot.relationship?.isFolderlessWorkspace ? null
        : snapshot.relationship?.projectId ?? snapshot.settings.projectId ?? null;
      const checkpoint = await this.checkpointFor(projectId, snapshot.settings, signal);
      await this.lifecycle.reuseSources?.({ ...snapshot, sources: corpusSources, checkpoint }, projectId, signal);
      const preparationFailures = this.indexer.preparationFailures;
      const report = await this.indexer.upsert(corpusSources, snapshot.settings, signal, undefined, options);
      canCacheCorpus = !report.semantic.skippedSources && preparationFailures === this.indexer.preparationFailures;
      await this.indexer.prune({ ...snapshot, scopes: corpusScopes }, signal, { sourceTypes: ['knowledge', 'work-file'] });
    }
    await this.indexer.upsert(otherSources, snapshot.settings, signal, undefined, options);
    await this.indexer.prune(snapshot, signal, { sourceTypes: ['memory', 'message'] });
    signal?.throwIfAborted();
    this.corpusSyncCache.delete(key);
    if (canCacheCorpus) {
      const currentVersion = corpusVersion(await this.index.scopeVersion({ scopeKeys: corpusScopes, signal }));
      this.corpusSyncCache.set(key, { signature, version: currentVersion });
    }
    while (this.corpusSyncCache.size > 32) this.corpusSyncCache.delete(this.corpusSyncCache.keys().next().value);
  }

  corpusSignature(sources, settings) {
    return sourceIdentity(this.indexer.preparationVersion(), settings.local, settings.projectIndexing,
      [...sources].sort((left, right) => left.sourceId.localeCompare(right.sourceId)).map(source => [source.sourceId,
        source.contentHash, source.sourceRevision, source.bindingRevision, source.scopeKey, source.sourceType,
        source.title, source.locator, source.parserVersion, source.chunkerVersion, source.tokenizerVersion,
        source.embeddingInputVersion, source.structure]));
  }

  async cacheCorpus(sources, scopes, settings, signal) {
    if (!this.index.scopeVersion) return;
    const version = corpusVersion(await this.index.scopeVersion({ scopeKeys: scopes, signal }));
    const key = sourceIdentity(scopes);
    this.corpusSyncCache.delete(key);
    this.corpusSyncCache.set(key, { signature: this.corpusSignature(sources, settings), version });
    while (this.corpusSyncCache.size > 32) this.corpusSyncCache.delete(this.corpusSyncCache.keys().next().value);
  }

  runMutation(operation, signal) {
    if (this.closed) return Promise.reject(toolFailure('检索服务已关闭。', 'RETRIEVAL_CLOSED', 409));
    const controller = new AbortController();
    const abort = () => controller.abort(signal?.reason);
    signal?.addEventListener('abort', abort, { once: true });
    if (signal?.aborted) abort();
    const pending = Promise.resolve().then(() => {
      controller.signal.throwIfAborted();
      return operation(controller.signal);
    }).finally(() => { signal?.removeEventListener('abort', abort); this.operations.delete(controller); });
    this.operations.set(controller, pending);
    return pending;
  }

  import(input, { signal, permissionMode } = {}) {
    return this.runMutation(async ownedSignal => {
      await this.authorizeImport?.(input, { signal: ownedSignal, permissionMode });
      ownedSignal.throwIfAborted();
      const settings = await this.effectiveSettings(input.scope === 'project' ? input.projectId : null);
      const limits = settings.local.indexing;
      const files = await this.readTree(input.path, { excludedRoots: this.excludedRoots,
        maximumFiles: Math.min(512, limits?.maximumFiles ?? 512), maximumSourceBytes: limits?.maximumSourceBytes,
        maximumBytes: limits?.maximumTotalBytes, maximumEntries: limits?.maximumEntries, signal: ownedSignal });
      ownedSignal.throwIfAborted();
      const result = await this.library.add(files, { ...input, limits, signal: ownedSignal });
      this.sync.libraryCache.clear();
      // A committed import remains registered if cancellation arrives before indexing admission.
      // 来源登记已经提交时，随后取消不删除正式资料；索引仍可在后续重建。
      let job = null;
      if (!ownedSignal.aborted && !this.closed) {
        try { job = await this.rebuild({ projectId: input.scope === 'project' ? input.projectId : null, dirty: true, signal: ownedSignal }); }
        catch (error) {
          if (!(error.code === 'RETRIEVAL_CLOSED' && this.closed || error.name === 'AbortError' && ownedSignal.aborted)) throw error;
        }
      }
      const first = result.sources[0];
      return { id: first.id, title: first.title, scope: first.scope, projectId: first.projectId, path: first.originalPath,
        revision: first.revision, status: 'ready', importedCount: result.sources.length,
        ...(job ? { jobId: job.jobId } : { indexingStatus: 'pending' }) };
    }, signal);
  }

  remove(id, input, { signal } = {}) {
    return this.runMutation(ownedSignal => this.serialize(async () => {
      ownedSignal.throwIfAborted();
      const removed = await this.library.remove(id, { ...input, signal: ownedSignal });
      this.sync.libraryCache.clear();
      this.indexer.invalidate(removed.id);
      // Once revocation commits, clean the rebuildable index even if this HTTP request is cancelled.
      // 正式撤销提交后，即使 HTTP 请求取消仍清理可重建索引，来源授权不会因此复活。
      await this.index.removeSource(removed.id, { scopeKeys: [removed.scopeKey] });
      return { deleted: true, id: removed.id };
    }), signal);
  }

  beginClose() {
    this.closed = true;
    this.sync.close();
    this.corpusSyncCache.clear();
    for (const controller of this.operations.keys()) controller.abort();
    this.lifecycle.beginClose();
  }

  close({ releaseInference } = {}) {
    if (!this.closure) this.closure = (async () => {
      this.beginClose();
      const releasing = Promise.resolve().then(() => releaseInference?.());
      const draining = Promise.allSettled([...this.operations.values(), ...this.sync.mountedOperations.values(),
        ...this.sync.foregroundContinuations.values(), this.lifecycle.drain()]);
      const [release, drained] = await Promise.allSettled([releasing, draining]);
      if (release.status === 'rejected') throw release.reason;
      if (drained.status === 'rejected') throw drained.reason;
      const failure = drained.value.find(result => result.status === 'rejected' && result.reason?.name !== 'AbortError');
      if (failure) throw failure.reason;
    })();
    return this.closure;
  }
}

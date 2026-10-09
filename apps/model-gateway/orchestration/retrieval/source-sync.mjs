import { watch } from 'node:fs';
import { relative, resolve } from 'node:path';
import { readSourceFile, readSourceTree, sameSourceMetadata, scanSourcePaths, scanSourceTree,
  sourceExtractionVersion, readSourceFileWindow } from '../../tools/retrieval/source-reader.mjs';
import { sourceFileRevision } from '../../data/retrieval/retrieval-contracts.mjs';
import { SourceManifestStore } from '../../data/retrieval/source-manifest.mjs';
import { validateIndexingLimits } from '../../data/retrieval/settings.mjs';
import { inspectLocalPath, toolFailure, within } from '../../platform/tool-paths.mjs';
import { sourceIdentity, visibleScopes, projectConversationSources } from './source-projection.mjs';

const SOURCE_CACHE_TTL_MS = 2000;
const MAX_SOURCE_CACHE_ENTRIES = 32;
const MAX_DIRTY_PATHS = 2048;
const RECONCILIATION_INTERVAL_MS = 30000;

function sourceLimits(settings) {
  const indexing = validateIndexingLimits(settings.local.indexing ?? {});
  return { maximumFiles: indexing.maximumFiles, maximumSourceBytes: indexing.maximumSourceBytes,
    maximumBytes: indexing.maximumTotalBytes, maximumEntries: indexing.maximumEntries };
}

function mountedDescriptors(files, projectId, root, bindingRevision, failures = []) {
  const failed = new Map(failures.map(item => [item.relativePath, item.errorCode]));
  const failedDirectories = failures.filter(item => item.directory);
  return files.flatMap(file => {
    const relativePath = file.relativePath ?? relative(root, file.path);
    const errorCode = failed.get(relativePath) ?? failedDirectories.find(item =>
      item.relativePath === '' || relativePath.startsWith(`${item.relativePath}/`) || relativePath.startsWith(`${item.relativePath}\\`))?.errorCode;
    const common = { scopeKey: `project:${projectId}`, sourceType: 'work-file', title: file.title ?? relativePath,
      bindingRevision, ...(errorCode ? { unavailable: true, errorCode } : {}) };
    const locator = { path: resolve(root, relativePath), relativePath, root,
      ...(file.extraction === undefined ? {} : { extraction: file.extraction }) };
    if (file.windows) return file.windows.map(window => ({ ...common,
      sourceId: sourceIdentity('work-file', projectId, root, relativePath, window.startOffset),
      locator: { ...locator, fileWindow: window }, storedBytes: window.textBytes, contentHash: window.contentHash,
      sourceRevision: sourceFileRevision({ contentHash: window.contentHash, fileWindow: window }) }));
    return [{ ...common, sourceId: sourceIdentity('work-file', projectId, root, relativePath), locator,
      ...(file.text === undefined ? {} : { text: file.text }), storedBytes: file.textBytes,
      contentHash: file.contentHash, sourceRevision: sourceFileRevision(file) }];
  });
}

/** Owns source snapshots and folder notifications, never the authoritative source registry.
 * 管理来源快照与文件夹通知，不拥有或替代正式来源登记。 */
export class SourceSyncService {
  constructor({ library, getProject, excludedRoots = [], readTree = readSourceTree, onFolderChanged,
    manifest = library.root ? new SourceManifestStore(library.root) : null, watchFactory = watch, resourceService }) {
    this.library = library;
    this.getProject = getProject;
    this.excludedRoots = excludedRoots;
    this.readTree = readTree;
    this.onFolderChanged = onFolderChanged;
    this.manifest = manifest;
    this.watchFactory = watchFactory;
    this.resources = resourceService;
    this.libraryCache = new Map();
    this.conversationCache = new Map();
    this.mountedCache = new Map();
    this.watchers = new Map();
    this.mountedStates = new Map();
    this.mountedOperations = new Map();
    this.foregroundContinuations = new Map();
    this.shutdown = new AbortController();
    this.closed = false;
  }

  trimCaches(memoryLimitBytes) {
    const caches = [this.libraryCache, this.conversationCache, this.mountedCache, this.mountedStates];
    let retainedBytes = caches.flatMap(cache => [...cache.values()])
      .reduce((total, entry) => total + entry.memoryBytes, 0);
    while (retainedBytes > memoryLimitBytes || caches.some(cache => cache.size > MAX_SOURCE_CACHE_ENTRIES)) {
      const candidates = retainedBytes > memoryLimitBytes ? caches : caches.filter(cache => cache.size > MAX_SOURCE_CACHE_ENTRIES);
      let oldest;
      for (const cache of candidates) for (const [key, entry] of cache)
        if (!oldest || entry.capturedAt < oldest.entry.capturedAt) oldest = { cache, key, entry };
      if (!oldest) break;
      retainedBytes -= oldest.entry.memoryBytes;
      oldest.cache.delete(oldest.key);
      if (oldest.cache === this.mountedStates) {
        retainedBytes -= this.mountedCache.get(oldest.key)?.memoryBytes ?? 0;
        this.mountedCache.delete(oldest.key);
      }
    }
  }

  async librarySources(scopes, settings, signal) {
    signal?.throwIfAborted();
    this.trimCaches(settings.cache.memoryLimitBytes);
    const catalog = await this.library.list(settings.projectId, { signal });
    signal?.throwIfAborted();
    const key = JSON.stringify(scopes.filter(scope => scope === 'user' || scope.startsWith('project:')));
    const cached = this.libraryCache.get(key);
    if (cached?.revision === catalog.revision && performance.now() - cached.capturedAt < SOURCE_CACHE_TTL_MS) return cached.sources;
    const sources = await this.library.readAll(scopes, { signal });
    signal?.throwIfAborted();
    const memoryBytes = sources.reduce((total, source) => total + source.text.length * 2 +
      JSON.stringify(source.locator ?? {}).length * 2 + 512, 0);
    if (!this.closed) this.libraryCache.set(key, { revision: catalog.revision, capturedAt: performance.now(), sources, memoryBytes });
    this.trimCaches(settings.cache.memoryLimitBytes);
    return sources;
  }

  async librarySnapshot(scopes, settings, signal) {
    signal?.throwIfAborted();
    if (!this.library.describeSources) {
      const sources = await this.librarySources(scopes, settings, signal);
      // Legacy seams already supplied complete formal bodies; metadata production paths use real validators below.
      // 旧接缝已经提供完整正式正文；生产元信息路径仍使用下面的真实来源校验。
      return { sources, loadSource: async source => source, isCurrent: async () => true };
    }
    const described = await this.library.describeSources(scopes, { signal, limits: settings.local.indexing, allowPartial: true });
    const sources = Array.isArray(described) ? described : described.sources;
    const scan = { loadedFiles: 0, ...(described.coverage ? { coverage: described.coverage } : {}) };
    return { sources, scan, isCurrent: async (source, ownedSignal) => {
      ownedSignal?.throwIfAborted();
      return this.library.isActive ? this.library.isActive(source.sourceId, source.sourceRevision)
        : (await this.library.readSource(source.sourceId, { scopeKeys: scopes, sourceRevision: source.sourceRevision,
          signal: ownedSignal }))?.contentHash === source.contentHash;
    }, loadSource: async (source, ownedSignal) => {
      ownedSignal?.throwIfAborted();
      const current = await this.library.readSource(source.sourceId,
        { scopeKeys: scopes, sourceRevision: source.sourceRevision, signal: ownedSignal });
      scan.loadedFiles++;
      if (!current || current.contentHash !== source.contentHash)
        throw toolFailure('登记资料已变化，请重新建立索引。', 'STALE_RETRIEVAL_SOURCE', 409);
      return current;
    } };
  }

  conversationSources(relationship, entries, messages, settings) {
    this.trimCaches(settings.cache.memoryLimitBytes);
    const revision = sourceIdentity(visibleScopes(relationship), entries.map(entry => [entry.id, entry.revision, entry.active, entry.content]),
      messages.map(message => [message.Id, message.Role, message.Status, message.Content]));
    const key = relationship.conversationId;
    const cached = this.conversationCache.get(key);
    if (cached?.revision === revision && performance.now() - cached.capturedAt < SOURCE_CACHE_TTL_MS) return cached.sources;
    const sources = projectConversationSources(relationship, entries, messages);
    const memoryBytes = sources.reduce((total, source) => total + source.text.length * 2, 0);
    if (!this.closed) this.conversationCache.set(key, { revision, capturedAt: performance.now(), sources, memoryBytes });
    this.trimCaches(settings.cache.memoryLimitBytes);
    return sources;
  }

  async mountedSources(projectId, settings, signal) {
    const snapshot = await this.mountedSnapshot(projectId, settings, signal);
    const sources = [];
    for (const source of snapshot.sources) sources.push(await snapshot.loadSource(source, signal));
    return sources;
  }

  mountedSnapshot(projectId, settings, signal) {
    projectId = projectId?.toLowerCase() ?? null;
    const previous = this.mountedOperations.get(projectId) ?? Promise.resolve();
    const pending = previous.catch(() => {}).then(() => this.captureMounted(projectId, settings, signal));
    this.mountedOperations.set(projectId, pending);
    pending.finally(() => {
      if (this.mountedOperations.get(projectId) === pending) this.mountedOperations.delete(projectId);
    }).catch(() => {});
    return pending;
  }

  async foregroundMountedSnapshot(projectId, settings, signal) {
    signal?.throwIfAborted();
    if (this.closed) throw toolFailure('资料同步已经关闭。', 'RETRIEVAL_CLOSED', 409);
    if (!settings.projectIndexing?.mountedFolder) return this.mountedSnapshot(projectId, settings, signal);
    projectId = projectId?.toLowerCase() ?? null;
    // Continue owned discovery after the short foreground grace period, never abandon its errors or lifecycle.
    // 前台短暂等待后由服务继续拥有目录发现，异常和关闭生命周期不能被丢弃。
    const pending = this.mountedOperations.get(projectId) ?? this.mountedSnapshot(projectId, settings, this.shutdown.signal);
    let timeout;
    const foreground = pending.catch(error => {
      signal?.throwIfAborted();
      if (this.closed || this.shutdown.signal.aborted) throw error;
      // Optional discovery failure remains a failed index job, never an interruption of an unrelated user conversation.
      // 可选来源发现失败仍由索引任务明确记录，不能中断无关用户对话；已有来源仍须逐项复核当前状态。
      this.lastDiscoveryError = error.name === 'AbortError' ? 'RETRIEVAL_DISCOVERY_CANCELLED'
        : error.code ?? 'RETRIEVAL_DISCOVERY_FAILED';
      return null;
    });
    const result = await Promise.race([foreground, new Promise(resolveDeadline => {
      timeout = setTimeout(() => resolveDeadline(null), 100);
    })]).finally(() => clearTimeout(timeout));
    signal?.throwIfAborted();
    if (this.closed) throw toolFailure('资料同步已经关闭。', 'RETRIEVAL_CLOSED', 409);
    if (result) { this.lastDiscoveryError = undefined; return result; }
    if (!this.foregroundContinuations.has(projectId)) {
      const continuation = pending.then(() => {
        this.lastDiscoveryError = undefined;
        if (!this.closed) return this.onFolderChanged?.(projectId);
      }).catch(error => {
        if (error.name !== 'AbortError') this.lastDiscoveryError = error.code ?? 'RETRIEVAL_DISCOVERY_FAILED';
      }).finally(() => {
        if (this.foregroundContinuations.get(projectId) === continuation) this.foregroundContinuations.delete(projectId);
      });
      this.foregroundContinuations.set(projectId, continuation);
    }
    const project = await this.getProject(projectId);
    signal?.throwIfAborted();
    if (!project?.FolderPath) return { sources: [], scan: { backgroundPending: true }, loadSource: async source => source };
    const root = resolve(project.FolderPath), bindingRevision = settings.projectIndexing.bindingRevision;
    try { await inspectLocalPath(root); }
    catch (error) {
      signal?.throwIfAborted();
      return { sources: [], scan: { backgroundPending: true, diagnosticCode: error.code ?? 'RETRIEVAL_DISCOVERY_FAILED' },
        loadSource: async source => source };
    }
    const state = this.mountedStates.get(projectId);
    const validState = state?.root === root && state.bindingRevision === bindingRevision;
    const metadata = validState ? new Map(state.discoveryFiles ?? state.files) : new Map();
    const limits = sourceLimits(settings);
    const withinBudget = metadata.size <= limits.maximumFiles &&
      [...metadata.values()].reduce((sum, file) => sum + file.textBytes, 0) <= limits.maximumBytes;
    const sources = withinBudget ? mountedDescriptors([...metadata.values()], projectId, root, bindingRevision) : [];
    return this.snapshotFor(sources, projectId, root, bindingRevision, settings,
      { backgroundPending: true, fileReads: 0, reusedFiles: sources.length, scannedFiles: 0,
        coverage: { discovered: metadata.size, readable: metadata.size, complete: false },
        ...(this.lastDiscoveryError ? { diagnosticCode: this.lastDiscoveryError } : {}) }, metadata);
  }

  async captureMounted(projectId, settings, signal) {
    signal?.throwIfAborted();
    if (this.closed) throw toolFailure('资料同步已经关闭。', 'RETRIEVAL_CLOSED', 409);
    projectId = projectId?.toLowerCase() ?? null;
    const project = await this.getProject(projectId);
    signal?.throwIfAborted();
    if (!project?.FolderPath || !settings.projectIndexing?.mountedFolder) {
      this.watchers.get(projectId)?.close();
      this.watchers.delete(projectId);
      this.mountedCache.delete(projectId);
      this.mountedStates.delete(projectId);
      return { sources: [], loadSource: async source => source, scan: { fileReads: 0, reusedFiles: 0, scannedFiles: 0 } };
    }
    const root = resolve(project.FolderPath);
    const bindingRevision = settings.projectIndexing.bindingRevision;
    const limits = sourceLimits(settings), limitsKey = JSON.stringify(limits);
    await inspectLocalPath(root);
    signal?.throwIfAborted();
    this.watchFolder(projectId, root);
    const cached = this.mountedCache.get(projectId);
    const currentState = this.mountedStates.get(projectId);
    const unchangedState = currentState?.root === root && currentState.bindingRevision === bindingRevision &&
      currentState.limitsKey === limitsKey && currentState.generation === cached?.generation &&
      !currentState.fullScan && currentState.dirtyPaths.size === 0;
    // Watch events and the bounded reconciliation timer invalidate descriptors; a TTL is not a reason to rescan a warm tree.
    // 文件事件和有界定时核对负责使描述失效，不能只因缓存到期就重新扫描热目录。
    if (cached?.root === root && cached.bindingRevision === bindingRevision && cached.limitsKey === limitsKey &&
        unchangedState)
      return this.snapshotFor(cached.sources, projectId, root, bindingRevision, settings, { fileReads: 0, reusedFiles: cached.sources.length,
        scannedFiles: 0, visitedEntries: 0, bytesRead: 0, cached: true,
        ...(cached.scanStats?.failures ? { failures: cached.scanStats.failures, failedFiles: cached.scanStats.failedFiles } : {}),
        ...(cached.scanStats?.coverage ? { coverage: cached.scanStats.coverage } : {}) }, cached.metadataFiles);
    const binding = { projectId, root, bindingRevision };
    let state = this.mountedStates.get(projectId);
    if (!state || state.root !== root || state.bindingRevision !== bindingRevision) {
      let stored;
      try { stored = await this.manifest?.read(binding, { signal }); }
      catch (error) { if (signal?.aborted) throw error; stored = null; }
      state = { root, bindingRevision, files: new Map((stored?.files ?? []).map(file => [file.relativePath, file])),
        failureCache: new Map(),
        generation: 0, dirtyPaths: new Set(), fullScan: true, limitsKey, capturedAt: performance.now(),
        memoryBytes: (stored?.files ?? []).reduce((total, file) => total + JSON.stringify(file).length * 2 + 256, 0) };
      this.mountedStates.set(projectId, state);
    }
    if (state.limitsKey !== limitsKey) state.fullScan = true;
    const generation = state.generation, dirtyPaths = new Set(state.dirtyPaths), stats = {};
    const options = { ...limits, root, excludedRoots: this.excludedRoots, signal, resourceService: this.resources,
      previousFiles: state.files, changedPaths: dirtyPaths, stats, failureCache: state.failureCache, allowPartial: true };
    let files;
    if (this.readTree !== readSourceTree) {
      files = await this.readTree(root, options);
      stats.fileReads = files.length; stats.reusedFiles = 0; stats.scannedFiles = files.length;
    } else {
      const next = state.fullScan || !dirtyPaths.size ? new Map() : new Map(state.files);
      // Discovery publishes only verified descriptors; callers still see a partial view until the scan settles.
      // 目录发现只逐步提供已复核描述，扫描结束前调用方始终收到部分覆盖状态。
      state.discoveryFiles = next;
      const scan = async iterable => {
        for await (const file of iterable) {
          const relativePath = relative(root, file.path);
          if (file.missing) next.delete(relativePath);
          else if (file.failed) continue;
          else next.set(relativePath, { relativePath, contentHash: file.contentHash, textBytes: file.textBytes, metadata: file.metadata,
            ...(file.extraction === undefined ? {} : { extraction: file.extraction }), ...(file.windows ? { windows: file.windows } : {}) });
        }
      };
      try { await scan(state.fullScan || !dirtyPaths.size ? scanSourceTree(root, options) : scanSourcePaths(root, dirtyPaths, options)); }
      catch (error) {
        if (error.code !== 'RETRIEVAL_FULL_SCAN_REQUIRED') throw error;
        next.clear();
        await scan(scanSourceTree(root, options));
      }
      // Failed reads are not evidence of deletion; retain old metadata only as unavailable derivation hints.
      // 读取失败不能证明文件删除，旧元信息只保留为不可用派生线索，不能冒充本轮读取成功。
      for (const [path, file] of state.files) if (!next.has(path) &&
        (stats.failures?.some(failure => path === failure.relativePath || path.startsWith(`${failure.relativePath}/`) || path.startsWith(`${failure.relativePath}\\`)))) next.set(path, file);
      const bytes = [...next.values()].reduce((total, file) => total + file.textBytes, 0);
      if (next.size > limits.maximumFiles || bytes > limits.maximumBytes)
        throw toolFailure('挂载资料超过当前来源或字节预算。', 'RETRIEVAL_SCAN_LIMIT', 413);
      await this.manifest?.write(binding, [...next.values()], { signal });
      state.files = next;
      state.discoveryFiles = undefined;
      state.memoryBytes = [...next.values()].reduce((total, file) => total + JSON.stringify(file).length * 2 + 256, 0);
      files = [...next.values()].map(file => ({ ...file, path: resolve(root, file.relativePath), title: file.relativePath }));
    }
    signal?.throwIfAborted();
    const sources = mountedDescriptors(files, projectId, root, bindingRevision, stats.failures);
    stats.coverage = { discovered: stats.discoveredFiles ?? stats.scannedFiles ?? files.length,
      readable: stats.scannedFiles ?? files.length, failed: stats.failedFiles ?? 0, complete: !stats.failedFiles && !stats.incomplete,
      ...(stats.limit ? { limit: stats.limit } : {}) };
    const memoryBytes = sources.reduce((total, source) => total + (source.text?.length ?? 0) * 2 +
      JSON.stringify(source.locator).length * 2 + 512, 0);
    if (state.generation === generation) { state.dirtyPaths.clear(); state.fullScan = false; }
    state.limitsKey = limitsKey;
    state.capturedAt = performance.now();
    if (!this.closed && memoryBytes <= settings.cache.memoryLimitBytes)
      this.mountedCache.set(projectId, { root, bindingRevision, capturedAt: performance.now(), sources, memoryBytes,
        metadataFiles: state.files, limitsKey, generation: state.generation, scanStats: stats });
    this.trimCaches(settings.cache.memoryLimitBytes);
    return this.snapshotFor(sources, projectId, root, bindingRevision, settings, stats, state.files);
  }

  snapshotFor(sources, projectId, root, bindingRevision, settings, scan, metadataFiles) {
    const isCurrent = async (source, signal) => {
      signal?.throwIfAborted();
      if (source.unavailable) return false;
      const project = await this.getProject(projectId);
      if (this.closed || !project?.FolderPath || resolve(project.FolderPath) !== root || source.bindingRevision !== bindingRevision)
        return false;
      try {
        const expected = metadataFiles?.get(source.locator.relativePath), current = await inspectLocalPath(source.locator.path);
        signal?.throwIfAborted();
        if (source.locator.fileWindow && expected?.windows?.some(window => window.contentHash === source.contentHash && window.startOffset === source.locator.fileWindow.startOffset))
          return current.isFile() && sameSourceMetadata(source.locator.fileWindow.metadata, { sizeBytes: current.size, mtimeMs: current.mtimeMs,
            ctimeMs: current.ctimeMs, device: current.dev, inode: current.ino });
        if (expected && expected.contentHash === source.contentHash && sourceFileRevision(expected) === source.sourceRevision &&
            (expected.extraction === undefined || expected.extraction.version === sourceExtractionVersion(source.locator.path)))
          return current.isFile() && sameSourceMetadata(expected.metadata, { sizeBytes: current.size, mtimeMs: current.mtimeMs,
            ctimeMs: current.ctimeMs, device: current.dev, inode: current.ino });
        const file = source.locator.fileWindow ? await readSourceFileWindow(source.locator.path, source.locator.fileWindow,
          { ...sourceLimits(settings), root, excludedRoots: this.excludedRoots, signal }) : await readSourceFile(source.locator.path,
          { ...sourceLimits(settings), root, excludedRoots: this.excludedRoots, signal, resourceService: this.resources });
        return file.contentHash === source.contentHash && sourceFileRevision(file) === source.sourceRevision;
      } catch (error) { if (signal?.aborted) throw error; return false; }
    };
    return { sources, scan, isCurrent, loadSource: async (source, signal) => {
      signal?.throwIfAborted();
      if (source.unavailable) throw toolFailure('本次来源读取失败，旧派生只保留待修复。', source.errorCode ?? 'RETRIEVAL_SOURCE_FAILED', 409);
      if (this.readTree !== readSourceTree && source.text !== undefined) return source;
      const project = await this.getProject(projectId);
      if (!project?.FolderPath || resolve(project.FolderPath) !== root || source.bindingRevision !== bindingRevision)
        throw toolFailure('挂载文件夹已经变化。', 'STALE_RETRIEVAL_SOURCE', 409);
      const file = source.locator.fileWindow ? await readSourceFileWindow(source.locator.path, source.locator.fileWindow,
        { ...sourceLimits(settings), root, excludedRoots: this.excludedRoots, signal }) : await readSourceFile(source.locator.path,
        { ...sourceLimits(settings), root, excludedRoots: this.excludedRoots, signal, resourceService: this.resources });
      scan.loadedFiles = (scan.loadedFiles ?? 0) + 1;
      scan.loadedBytes = (scan.loadedBytes ?? 0) + file.metadata.sizeBytes;
      if (file.contentHash !== source.contentHash || sourceFileRevision(file) !== source.sourceRevision)
        throw toolFailure('工作文件内容已经变化。', 'STALE_RETRIEVAL_SOURCE', 409);
      return { ...source, text: file.text };
    } };
  }

  markChanged(projectId, filename) {
    const state = this.mountedStates.get(projectId);
    const name = filename?.toString(), root = state?.root ?? this.watchers.get(projectId)?.root;
    // Managed files are excluded from both acquisition and notifications, so writing a manifest cannot schedule itself.
    // 应用管理文件同时排除读取和变化通知，写入清单不能再次调度清单自身。
    if (name && root && this.excludedRoots.some(folder => within(folder, resolve(root, name)))) return false;
    this.mountedCache.delete(projectId);
    if (!state) return true;
    state.generation++;
    const changedPath = name ? relative(state.root, resolve(state.root, name)) : null;
    const deletedDirectory = !state.fullScan && changedPath && !state.files.has(changedPath) &&
      [...state.files.keys()].some(path => path.startsWith(`${changedPath}\\`) || path.startsWith(`${changedPath}/`));
    if (!name || /(?:^|[\\/])(?:\.gitignore|\.git)(?:[\\/]|$)/u.test(name) ||
        !within(state.root, resolve(state.root, name)) || deletedDirectory || state.dirtyPaths.size >= MAX_DIRTY_PATHS) {
      state.fullScan = true; state.dirtyPaths.clear();
    } else state.dirtyPaths.add(changedPath);
    return true;
  }

  watchFolder(projectId, root) {
    if (this.closed) return;
    const current = this.watchers.get(projectId);
    if (current?.root === root) return;
    current?.close();
    let timer, watcher, watcherError;
    const notify = filename => {
      // The first manifest may be written before a state exists; the watcher still knows its bound root.
      // 首次清单写入时状态可能尚未建立，监听器仍拥有已绑定的根目录用于排除路径。
      const name = filename?.toString();
      if (name && this.excludedRoots.some(folder => within(folder, resolve(root, name)))) return;
      if (this.markChanged(projectId, filename) === false) return;
      clearTimeout(timer);
      timer = setTimeout(() => {
        if (!this.closed) Promise.resolve().then(() => this.onFolderChanged?.(projectId)).catch(() => {});
      }, 800);
      timer.unref();
    };
    const reconciliation = setInterval(() => notify(), RECONCILIATION_INTERVAL_MS);
    reconciliation.unref();
    try {
      watcher = this.watchFactory(root, { recursive: true, persistent: false }, (_event, filename) => notify(filename));
      watcher.on('error', error => { watcherError = error.code ?? 'WATCH_UNAVAILABLE'; watcher.close(); notify(); });
    } catch (error) {
      watcherError = error.code ?? 'WATCH_UNAVAILABLE';
      // Freshness is still validated on reads when folder notifications are unavailable.
      // 文件夹监听不可用时，资料回读仍复核实际版本，不伪造同步成功。
    }
    this.watchers.set(projectId, { root, get state() { return watcherError ? 'polling' : 'watching'; },
      get diagnosticCode() { return watcherError; },
      close: () => { clearTimeout(timer); clearInterval(reconciliation); watcher?.close(); } });
  }

  status() {
    return { watchers: [...this.watchers.values()].map(watcher => ({ state: watcher.state,
      ...(watcher.diagnosticCode ? { diagnosticCode: watcher.diagnosticCode } : {}) })),
      reconciliationIntervalMs: RECONCILIATION_INTERVAL_MS, manifest: this.manifest ? 'durable-metadata' : 'memory-only' };
  }

  close() {
    this.closed = true;
    this.shutdown.abort();
    for (const watcher of this.watchers.values()) watcher.close();
    this.watchers.clear();
    this.libraryCache.clear();
    this.conversationCache.clear();
    this.mountedCache.clear();
    this.mountedStates.clear();
  }
}

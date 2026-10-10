import { validateId } from '../../platform/conversation-id.mjs';
import { toolFailure } from '../../platform/tool-paths.mjs';
import { normalizeRetrievalPath } from '../../data/retrieval/retrieval-contracts.mjs';

// Full per-source coverage lives in SQLite; job history keeps only counts and a bounded recent view.
// 完整来源覆盖保存在 SQLite；作业历史只保留统计及有界近期视图。
function coverageSummary(coverage) {
  return { ...coverage, sources: (coverage.sources ?? []).slice(-50), failures: (coverage.failures ?? []).slice(-50),
    reportTruncated: Boolean(coverage.reportTruncated) || (coverage.sources?.length ?? 0) > 50 || (coverage.failures?.length ?? 0) > 50 };
}

/** Owns scheduled indexing and durable terminal states, without owning model/index resources.
 * 拥有索引调度与持久终态，不接管模型服务或索引资源的释放权。 */
export class IndexJobService {
  constructor({ jobs, validateProject, scopeIdentity, prepareSources, publishSources, restoreSources, reuseSources, finalizeSources, refreshSources }) {
    this.jobs = jobs;
    this.validateProject = validateProject;
    this.scopeIdentity = scopeIdentity;
    this.prepareSources = prepareSources;
    this.publishSources = publishSources;
    this.restoreSources = restoreSources;
    this.reuseSources = reuseSources;
    this.finalizeSources = finalizeSources;
    this.refreshSources = refreshSources;
    this.active = new Map();
    this.admissionQueue = Promise.resolve();
    this.executionQueue = Promise.resolve();
    this.closed = false;
    this.lastFailure = undefined;
    this.taskPriorities = new Map();
    this.automaticStops = new Map();
  }

  prioritiesFor(projectId) {
    const key = projectId?.toLowerCase() ?? null;
    const running = [...this.active.values()].find(job => job.projectId === key && !job.controller.signal.aborted);
    if (running?.priorities) return running.priorities;
    let priorities = this.taskPriorities.get(key);
    if (!priorities) {
      priorities = { revision: 0, paths: new Set(), recentPaths: new Set() };
      this.taskPriorities.set(key, priorities);
      while (this.taskPriorities.size > 32) this.taskPriorities.delete(this.taskPriorities.keys().next().value);
    }
    return priorities;
  }

  prioritize(projectId, path, { recent = false } = {}) {
    if (this.closed || !path) return;
    const priorities = this.prioritiesFor(projectId);
    const normalized = normalizeRetrievalPath(path);
    // File notifications have a separate bound and cannot evict the user's current explicit targets.
    // 文件通知使用独立限额，不能挤掉用户当前明确指定的目标。
    const paths = recent ? (priorities.recentPaths ??= new Set()) : priorities.paths;
    if (recent || !paths.has(normalized)) {
      paths.delete(normalized);
      paths.add(normalized);
      while (paths.size > 16) paths.delete(paths.values().next().value);
      priorities.revision++;
    }
  }

  /** Retire an obsolete binding without turning it into a persistent user cancellation.
   * 旧目录绑定失效时停止其任务，不把目录替换误记为用户持久取消。 */
  supersede(projectId) {
    for (const active of this.active.values()) if (active.projectId === projectId && !active.cancelledByUser) {
      active.superseded = true;
      active.suspend = false;
      active.acceptingRefresh = false;
      active.controller.abort();
      if (!active.hasStarted) this.cancelQueued(active).catch(error => { this.lastFailure ??= error; });
    }
  }

  initialize() {
    if (!this.initialization) this.initialization = this.jobs.recover({ resumable: true }).then(async recovered => {
      const history = (await this.jobs.list?.() ?? []).sort((left, right) => left.createdAt.localeCompare(right.createdAt));
      const latest = new Map();
      for (const job of history) {
        latest.set(job.projectId, job);
        if (job.automaticRebuildBlocked === true) this.automaticStops.set(job.projectId, job);
      }
      // Older cancellation receipts acquire the same durable policy; ordinary history trimming must not forget it.
      // 旧版取消回执沿用同一持久策略，不能被普通任务历史清理忘掉。
      for (const job of latest.values()) if (job.status === 'cancelled' && job.error === 'INDEX_CANCELLED' &&
          job.automaticRebuildBlocked === undefined) {
        this.automaticStops.set(job.projectId, job);
        await this.jobs.update(job.jobId, { automaticRebuildBlocked: true });
      }
      for (const job of recovered ?? []) if (!this.closed) {
        const stopped = this.automaticStops.get(job.projectId);
        if (stopped && (!stopped.automaticRebuildScope || stopped.automaticRebuildScope === job.automaticRebuildScope)) {
          await this.jobs.update(job.jobId, { status: 'cancelled', error: 'INDEX_CANCELLED',
            automaticRebuildBlocked: true, finishedAt: new Date().toISOString() });
          continue;
        }
        this.admit(job, undefined, { checkpoint: job.checkpoint });
      }
    });
    return this.initialization;
  }

  stopAutomatic(active) {
    if (!active.cancelMarker) {
      this.automaticStops.set(active.projectId, { jobId: active.jobId, projectId: active.projectId,
        automaticRebuildScope: active.automaticRebuildScope });
      active.cancelMarker = this.jobs.update(active.jobId, { automaticRebuildBlocked: true });
      active.cancelMarker.catch(error => { this.lastFailure ??= error; });
    }
    return active.cancelMarker;
  }

  admit(job, signal, recovered) {
    const controller = new AbortController();
    const active = { jobId: job.jobId, projectId: job.projectId, controller, acceptingRefresh: true,
      automaticRebuildScope: job.automaticRebuildScope,
      needsRefresh: false, recovered, completedSources: job.completedSources, totalSources: job.totalSources,
      suspend: false, cancelledByUser: false, hasStarted: false, priorities: this.prioritiesFor(job.projectId) };
    const abort = () => {
      active.cancelledByUser = true;
      active.suspend = false;
      this.stopAutomatic(active);
      controller.abort(signal?.reason);
      if (!active.hasStarted) this.cancelQueued(active).catch(error => { this.lastFailure ??= error; });
    };
    active.detachAbort = () => signal?.removeEventListener('abort', abort);
    signal?.addEventListener('abort', abort, { once: true });
    if (signal?.aborted) abort();
    else if (this.closed) controller.abort();
    this.active.set(job.jobId, active);
    active.promise = this.executionQueue.catch(() => {}).then(() => {
      if (active.queuedCancellation) return active.queuedCancellation;
      active.hasStarted = true;
      return this.run(active);
    }).finally(() => {
      active.detachAbort();
      if (this.active.get(job.jobId) === active) this.active.delete(job.jobId);
    });
    // Only background derivation uses this slot; foreground retrieval keeps its own queue.
    // 此执行槽只串行化后台派生，前台检索仍使用独立队列。
    this.executionQueue = active.promise;
    // The owning service observes task failures even when the HTTP caller has returned.
    // HTTP 请求返回后仍由服务观察任务失败，避免丢弃后台 Promise 的异常。
    active.promise.catch(error => { this.lastFailure ??= error; });
    return active;
  }

  cancelQueued(active) {
    if (!active.queuedCancellation) {
      active.acceptingRefresh = false;
      // Queued work has admitted no reads or publications, so cancellation need not wait for another project.
      // 排队任务尚未接纳读取或发布，取消无需等待其他项目的运行任务。
      active.queuedCancellation = Promise.resolve(active.cancelMarker).then(() => this.jobs.update(active.jobId, {
        status: 'cancelled', error: active.superseded && !active.cancelledByUser ? 'INDEX_SUPERSEDED' : 'INDEX_CANCELLED',
        finishedAt: new Date().toISOString() })).then(receipt => {
        active.detachAbort();
        if (this.active.get(active.jobId) === active) this.active.delete(active.jobId);
        return receipt;
      });
    }
    return active.queuedCancellation;
  }

  rebuild({ projectId = null, signal, dirty = false, automatic = false } = {}) {
    const pending = this.admissionQueue.catch(() => {}).then(async () => {
      signal?.throwIfAborted();
      if (this.closed) throw toolFailure('检索服务已关闭。', 'RETRIEVAL_CLOSED', 409);
      await this.initialize();
      if (projectId) {
        projectId = validateId(projectId).toLowerCase();
        await this.validateProject(projectId);
      }
      signal?.throwIfAborted();
      if (this.closed) throw toolFailure('检索服务已关闭。', 'RETRIEVAL_CLOSED', 409);
      const automaticRebuildScope = await this.scopeIdentity?.(projectId, signal);
      signal?.throwIfAborted();
      const stopped = this.automaticStops.get(projectId);
      if (automatic && stopped && (!stopped.automaticRebuildScope || stopped.automaticRebuildScope === automaticRebuildScope)) {
        const receipt = await this.jobs.get(stopped.jobId);
        return { ...receipt, automaticRebuildBlocked: true, automaticRebuildState: 'cancelled-by-user' };
      }
      const current = [...this.active.values()].find(job => job.projectId === projectId &&
        job.acceptingRefresh && !job.controller.signal.aborted);
      if (current) {
        if (dirty) current.needsRefresh = true;
        return this.jobs.get(current.jobId);
      }
      // Explicit resumption must not clear the durable stop before the cancelled owner's work has drained.
      // 显式恢复先等待被取消任务排空，不能提前清除持久停止标记并让重启复活旧任务。
      if (!automatic) await Promise.all([...this.active.values()].filter(job => job.projectId === projectId &&
        job.cancelledByUser && job.controller.signal.aborted).map(job => job.promise));
      signal?.throwIfAborted();
      const job = await this.jobs.create(projectId, { automaticRebuildScope, resumeAutomatic: !automatic });
      if (!automatic) this.automaticStops.delete(projectId);
      this.admit(job, signal);
      return job;
    });
    this.admissionQueue = pending;
    return pending;
  }

  async run(active) {
    const signal = active.controller.signal;
    try {
      signal.throwIfAborted();
      if (active.projectId) await this.validateProject(active.projectId);
      signal.throwIfAborted();
      await this.jobs.update(active.jobId, { status: 'running', startedAt: new Date().toISOString(), error: undefined, finishedAt: undefined });
      const plannedSourceIds = new Set(), completedSourceIds = new Set();
      const priorDiagnosticCodes = new Set();
      let previousPassDiagnostics = [];
      let stalePasses = 0;
      do {
        signal.throwIfAborted();
        active.needsRefresh = false;
        const prepared = await this.prepareSources(active.projectId, signal);
        signal.throwIfAborted();
        if (active.recovered) {
          const saved = active.recovered.checkpoint, current = prepared.checkpoint;
          if (!current || saved.checkpointId !== current.checkpointId || saved.settingsSignature !== current.settingsSignature ||
              saved.root !== current.root || saved.bindingRevision !== current.bindingRevision ||
              saved.preparationVersion !== current.preparationVersion)
            throw toolFailure('索引来源配置已变化，旧检查点不能恢复。', 'INDEX_CHECKPOINT_STALE', 409);
          const records = await this.jobs.checkpointSources(active.jobId, saved);
          signal.throwIfAborted();
          await this.restoreSources?.(prepared.sources, prepared.settings, records, signal);
          active.recovered = undefined;
        } else await this.reuseSources?.(prepared, active.projectId, signal);
        for (const source of prepared.sources) plannedSourceIds.add(source.sourceId);
        for (const failure of prepared.sourceScan?.failures ?? []) if (failure.sourceId && !failure.directory)
          plannedSourceIds.add(failure.sourceId);
        active.totalSources = Math.max(active.totalSources, plannedSourceIds.size);
        await this.jobs.update(active.jobId, { totalSources: active.totalSources });
        for (const code of previousPassDiagnostics) if (priorDiagnosticCodes.size < 8) priorDiagnosticCodes.add(code);
        const semanticProgress = semantic => ({ ...semantic, priorDiagnosticCodes: [...priorDiagnosticCodes] });
        let acknowledgedSources = 0;
        const report = await this.publishSources(prepared.sources, prepared.settings, signal, (completedSources, progress) => {
          if (progress?.processedSourceIds) for (const sourceId of progress.processedSourceIds) completedSourceIds.add(sourceId);
          else for (const source of prepared.sources.slice(acknowledgedSources, completedSources)) completedSourceIds.add(source.sourceId);
          acknowledgedSources = completedSources;
          active.completedSources = Math.max(active.completedSources, completedSourceIds.size);
          const patch = { completedSources: active.completedSources,
            ...(progress?.coverage ? { coverage: coverageSummary(progress.coverage) } : {}),
            ...(progress?.semantic ? { semantic: semanticProgress(progress.semantic) } : {}) };
          if (prepared.checkpoint && progress?.checkpointSources?.length) {
            return this.jobs.commitBatch(active.jobId, { ...prepared.checkpoint, updatedAt: new Date().toISOString() },
              progress.checkpointSources, patch).then(receipt => { active.hasCheckpoint = true; return receipt; });
          }
          return this.jobs.update(active.jobId, patch);
        }, { loadSource: prepared.loadSource, isCurrent: prepared.isCurrent,
          priorities: active.priorities, sourceFailures: prepared.sourceScan?.failures ?? [] });
        signal.throwIfAborted();
        if (report?.coverage || prepared.sourceScan?.coverage) {
          const coverage = report?.coverage ?? { discovered: prepared.sources.length, lexical: 0, semantic: 0,
            failed: 0, skipped: 0, partial: 0, complete: false, sources: [] };
          const scan = prepared.sourceScan;
          const failures = scan?.failures ?? [];
          active.coverage = { ...coverage,
            discovered: Math.max(coverage.discovered, scan?.coverage?.discovered ?? 0),
            failed: Math.max(coverage.failed, scan?.coverage?.failed ?? failures.length),
            skipped: Math.max(coverage.skipped, scan?.coverage?.skipped ?? 0),
            partial: coverage.partial ?? 0, failures: failures.slice(0, 50), sources: (coverage.sources ?? []).slice(-50),
            complete: coverage.complete && scan?.coverage?.complete !== false && !failures.length && !scan?.truncated && !scan?.backgroundPending,
            ...(scan?.coverage?.limit ? { limit: scan.coverage.limit } : {}),
            ...(scan?.coverage?.limits ? { limits: scan.coverage.limits.slice(0, 50) } : {}),
            ...(scan?.coverage?.effectiveLimits ? { effectiveLimits: scan.coverage.effectiveLimits } : {}),
            reportTruncated: failures.length > 50 || (coverage.sources?.length ?? 0) > 50 || Boolean(coverage.reportTruncated) };
          await this.jobs.update(active.jobId, { coverage: active.coverage });
        }
        await this.finalizeSources?.(prepared, signal);
        if (report?.semantic) {
          previousPassDiagnostics = report.semantic.diagnosticCodes;
          await this.jobs.update(active.jobId, { semantic: semanticProgress(report.semantic) });
          if (report.semantic.diagnosticCodes.includes('STALE_RETRIEVAL_SOURCE')) {
            if (++stalePasses > 2)
              throw toolFailure('资料持续变化，已保留完成的索引，请稍后重试。', 'INDEX_SOURCE_UNSTABLE', 409);
            await this.refreshSources?.(active.projectId);
            active.needsRefresh = true;
          } else stalePasses = 0;
        }
      } while (active.needsRefresh && !signal.aborted);
      signal.throwIfAborted();
      // Seal admission synchronously before terminal persistence; newer dirtiness starts a successor job.
      // 终态落盘前同步封闭追加入口，此后到来的来源变更另起后继作业，不恢复已结束状态。
      active.acceptingRefresh = false;
      const partial = active.coverage && (!active.coverage.complete || active.coverage.failed || active.coverage.skipped);
      await this.jobs.update(active.jobId, { status: partial ? 'partial' : 'completed', finishedAt: new Date().toISOString() });
    } catch (error) {
      await active.cancelMarker;
      const superseded = active.superseded && !active.cancelledByUser;
      const suspended = signal.aborted && !active.cancelledByUser && active.suspend && (active.hasCheckpoint || active.recovered);
      await this.jobs.update(active.jobId, { status: suspended ? 'paused' : signal.aborted ? 'cancelled' : 'failed',
        error: superseded ? 'INDEX_SUPERSEDED' : suspended ? 'INDEX_SUSPENDED' : signal.aborted ? 'INDEX_CANCELLED' : typeof error.code === 'string' ? error.code : 'INDEX_FAILED',
        finishedAt: new Date().toISOString() });
    }
  }

  async cancelJob(id) {
    id = validateId(id);
    const admitted = this.active.get(id);
    // Abort an owned live task before waiting for disk reads that may queue behind progress writes.
    // 先取消已知的本进程任务，再等待可能排在进度写入之后的磁盘读取。
    if (admitted) {
      admitted.cancelledByUser = true; admitted.suspend = false;
      this.stopAutomatic(admitted); admitted.controller.abort();
    }
    const queuedCancellation = admitted && !admitted.hasStarted ? this.cancelQueued(admitted) : null;
    await this.initialize();
    if (queuedCancellation) return queuedCancellation;
    const job = await this.jobs.get(id);
    const active = this.active.get(job.jobId);
    if (active) {
      active.suspend = false;
      active.cancelledByUser = true;
      this.stopAutomatic(active);
      active.controller.abort();
      if (!active.hasStarted) return this.cancelQueued(active);
      // Cancellation is acknowledged only after admitted reads/publications have drained.
      // 已接纳的读取和发布排空后才确认取消，之后不会再发布此任务的批次。
      await active.promise;
    } else if (job.status === 'paused') {
      this.automaticStops.set(job.projectId, job);
      await this.jobs.update(job.jobId, { status: 'cancelled', error: 'INDEX_CANCELLED',
        automaticRebuildBlocked: true, finishedAt: new Date().toISOString() });
    }
    return this.jobs.get(job.jobId);
  }

  beginClose() {
    this.closed = true;
    this.taskPriorities.clear();
    for (const active of this.active.values()) {
      // Shutdown may resume only derived indexing; explicit cancellation remains terminal.
      // 关闭时仅允许派生索引恢复，用户明确取消仍保留终态。
      active.suspend = !active.cancelledByUser;
      active.controller.abort();
    }
  }

  async drain() {
    await this.admissionQueue.catch(() => {});
    for (const active of this.active.values()) active.controller.abort();
    const results = await Promise.allSettled([...this.active.values()].map(active => active.promise));
    const failure = results.find(result => result.status === 'rejected')?.reason ?? this.lastFailure;
    if (failure) throw failure;
  }
}

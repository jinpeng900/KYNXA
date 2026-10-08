import { retrievalFailure } from './retrieval-contracts.mjs';

const MAX_QUEUED_BUILDS = 64;
const MAX_FAILURE_RECORDS = 64;
const RETRY_WINDOW_MS = 60000;

function sameBuild(left, right) {
  return left.generation === right.generation && left.count === right.count && left.buildBudgetKey === right.buildBudgetKey;
}

/** Derived graphs are replaceable jobs; foreground queries never await a large cold build.
 * 派生图是可替换的后台作业；前台查询不等待大型冷图构建。 */
export class AnnBuildQueue {
  constructor({ now = Date.now, concurrency = 1 } = {}) {
    this.jobs = new Map();
    this.active = new Map();
    this.concurrency = concurrency;
    this.scheduled = null;
    this.closed = false;
    this.completed = 0;
    this.failed = 0;
    this.lastError = null;
    this.failures = new Map();
    this.now = now;
  }

  get running() { return this.active.size ? Promise.all([...this.active.values()].map(job => job.promise)) : null; }
  get runningJob() { return this.active.values().next().value ?? null; }
  configure(concurrency) {
    this.concurrency = Number.isSafeInteger(concurrency) ? Math.max(1, Math.min(4, concurrency)) : 1;
    this._schedule();
  }

  enqueue(key, descriptor, operation) {
    if (this.closed) return false;
    const previous = this.jobs.get(key);
    if (previous && !previous.controller.signal.aborted && sameBuild(previous.descriptor, descriptor)) return true;
    const failure = this.failure(key, descriptor);
    if (failure && this.now() < failure.retryAt) return false;
    if (!previous && this.jobs.size >= MAX_QUEUED_BUILDS)
      throw retrievalFailure('ANN build queue is full. / 向量建图队列已满。', 'RETRIEVAL_ANN_QUEUE_FULL');
    // Replacement stays queued until the cancelled owner has actually settled.
    // 新版本保留在队列中，等待被取消的旧所有者真正结束后执行，不能静默丢掉预热请求。
    previous?.controller.abort();
    const job = { key, descriptor, scopeKey: descriptor.scope_key, generation: descriptor.generation,
      totalVectors: descriptor.count, completedVectors: 0, state: 'queued', controller: new AbortController(), operation };
    this.jobs.set(key, job);
    this._schedule();
    return true;
  }

  failure(key, descriptor) {
    const failure = this.failures.get(key);
    if (!failure || !sameBuild(failure.descriptor, descriptor)) return null;
    return failure;
  }

  _schedule() {
    if (this.active.size >= this.concurrency || this.scheduled || this.closed || this.draining || !this.jobs.size) return;
    this.scheduled = setImmediate(() => {
      this.scheduled = null;
      const job = [...this.jobs.values()].find(item => item.state === 'queued' && !this.active.has(item.key));
      if (!job || this.closed) return;
      job.state = 'building';
      this.active.set(job.key, job);
      job.promise = Promise.resolve().then(() => job.operation(job)).then(() => {
        if (!job.controller.signal.aborted) { this.completed++; this.lastError = null; this.failures.delete(job.key); }
      }).catch(error => {
        if (error.name !== 'AbortError' && !job.controller.signal.aborted) {
          this.failed++; this.lastError = error.code ?? 'RETRIEVAL_ANN_BUILD_FAILED';
          const previous = this.failure(job.key, job.descriptor), now = this.now();
          const windowStartedAt = previous && now - previous.windowStartedAt < RETRY_WINDOW_MS ? previous.windowStartedAt : now;
          const attempts = windowStartedAt === previous?.windowStartedAt ? previous.attempts + 1 : 1;
          // Queries can request preparation frequently; failures must not turn them into an endless retry storm.
          // 查询可能频繁请求预热，失败时须退避，避免每次查询都触发无限重试风暴。
          const retryAt = attempts >= 3 ? windowStartedAt + RETRY_WINDOW_MS : now + 1000 * 2 ** (attempts - 1);
          this.failures.delete(job.key);
          this.failures.set(job.key, { descriptor: job.descriptor, attempts, windowStartedAt, retryAt, code: this.lastError });
          while (this.failures.size > MAX_FAILURE_RECORDS) this.failures.delete(this.failures.keys().next().value);
        }
      }).finally(() => {
        if (this.jobs.get(job.key) === job) this.jobs.delete(job.key);
        this.active.delete(job.key);
        this._schedule();
      });
      this._schedule();
    });
  }

  cancelScopes(scopes) {
    for (const job of this.active.values()) if (scopes.includes(job.scopeKey)) job.controller.abort();
    for (const [key, job] of this.jobs) if (scopes.includes(job.scopeKey)) {
      job.controller.abort();
      if (job.state === 'queued') this.jobs.delete(key);
    }
  }

  async drain() {
    this.draining = true;
    for (const job of this.active.values()) job.controller.abort();
    this.cancelScopes([...new Set([...this.jobs.values()].map(job => job.scopeKey))]);
    if (this.scheduled) { clearImmediate(this.scheduled); this.scheduled = null; }
    try { await this.running; } finally { this.draining = false; this._schedule(); }
  }

  status() {
    const jobs = [...this.jobs.values()];
    for (const job of this.active.values()) if (!jobs.includes(job)) jobs.unshift(job);
    return { pendingBuilds: jobs.length, activeBuilds: this.active.size, buildConcurrency: this.concurrency,
      maximumQueuedBuilds: MAX_QUEUED_BUILDS, completedBuilds: this.completed, failedBuilds: this.failed,
      retryingBuilds: [...this.failures.values()].filter(failure => failure.retryAt > this.now()).length,
      buildErrorCode: this.lastError, buildProgress: jobs.map(({ scopeKey, generation, state, totalVectors, completedVectors, controller }) =>
        ({ scopeKey, generation, state: controller.signal.aborted ? 'cancelling' : state, totalVectors, completedVectors })) };
  }

  async close() { this.closed = true; await this.drain(); }
}

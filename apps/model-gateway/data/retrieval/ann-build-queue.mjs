import { retrievalFailure } from './retrieval-contracts.mjs';

/** Derived graphs are replaceable jobs; foreground queries never await a large cold build.
 * 派生图是可替换的后台作业；前台查询不等待大型冷图构建。 */
export class AnnBuildQueue {
  constructor() {
    this.jobs = new Map();
    this.running = null;
    this.scheduled = null;
    this.closed = false;
    this.completed = 0;
    this.failed = 0;
    this.lastError = null;
  }

  enqueue(key, descriptor, operation) {
    if (this.closed || this.jobs.has(key)) return;
    if (this.jobs.size >= 16) throw retrievalFailure('ANN build queue is full. / 向量建图队列已满。', 'RETRIEVAL_ANN_QUEUE_FULL');
    const job = { key, descriptor, scopeKey: descriptor.scope_key, generation: descriptor.generation,
      totalVectors: descriptor.count, completedVectors: 0, state: 'queued', controller: new AbortController(), operation };
    this.jobs.set(key, job);
    this._schedule();
  }

  _schedule() {
    if (this.running || this.scheduled || this.closed || !this.jobs.size) return;
    this.scheduled = setImmediate(() => {
      this.scheduled = null;
      const job = this.jobs.values().next().value;
      if (!job || this.closed) return;
      job.state = 'building';
      this.running = Promise.resolve().then(() => job.operation(job)).then(() => {
        if (!job.controller.signal.aborted) { this.completed++; this.lastError = null; }
      }).catch(error => {
        if (error.name !== 'AbortError') { this.failed++; this.lastError = error.code ?? 'RETRIEVAL_ANN_BUILD_FAILED'; }
      }).finally(() => {
        if (this.jobs.get(job.key) === job) this.jobs.delete(job.key);
        this.running = null;
        this._schedule();
      });
    });
  }

  cancelScopes(scopes) {
    for (const [key, job] of this.jobs) if (scopes.includes(job.scopeKey)) {
      job.controller.abort();
      if (job.state === 'queued') this.jobs.delete(key);
    }
  }

  async drain() {
    this.cancelScopes([...new Set([...this.jobs.values()].map(job => job.scopeKey))]);
    if (this.scheduled) { clearImmediate(this.scheduled); this.scheduled = null; }
    await this.running;
  }

  status() {
    return { pendingBuilds: this.jobs.size, completedBuilds: this.completed, failedBuilds: this.failed,
      buildErrorCode: this.lastError, buildProgress: [...this.jobs.values()].map(({ scopeKey, generation, state, totalVectors, completedVectors }) =>
        ({ scopeKey, generation, state, totalVectors, completedVectors })) };
  }

  async close() { this.closed = true; await this.drain(); }
}

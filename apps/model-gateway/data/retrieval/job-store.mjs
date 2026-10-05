import { randomUUID } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { atomicJson } from '../../platform/atomic-json.mjs';
import { ensureLocalDirectory, inspectLocalPath, toolFailure } from '../../platform/tool-paths.mjs';

/** Durable indexing status, independent of chat existence and action execution receipts.
 * 持久索引状态独立于聊天是否存在，也不能替代副作用执行回执。 */
export class RetrievalJobStore {
  constructor(root) { this.folder = join(root, 'Retrieval'); this.file = join(this.folder, 'jobs.json'); this.queue = Promise.resolve(); }
  _run(operation) { const pending = this.queue.catch(() => {}).then(operation); this.queue = pending; return pending; }
  async _read() {
    await ensureLocalDirectory(this.folder);
    if (!await inspectLocalPath(this.file, { allowMissing: true })) return { schemaVersion: 1, jobs: [] };
    const document = JSON.parse(await readFile(this.file, 'utf8'));
    if (document.schemaVersion !== 1 || !Array.isArray(document.jobs))
      throw toolFailure('索引任务文件版本无效。', 'UNSUPPORTED_RETRIEVAL_SCHEMA', 409);
    return document;
  }
  list() { return this._run(async () => structuredClone((await this._read()).jobs)); }
  get(jobId) {
    return this._run(async () => {
      const job = (await this._read()).jobs.find(item => item.jobId === jobId);
      if (!job) throw toolFailure('索引任务不存在。', 'RETRIEVAL_JOB_NOT_FOUND', 404);
      return structuredClone(job);
    });
  }
  create(projectId = null) {
    return this._run(async () => {
      const document = await this._read();
      const job = { jobId: randomUUID(), projectId, status: 'queued', completedSources: 0, totalSources: 0,
        createdAt: new Date().toISOString() };
      document.jobs = [...document.jobs.filter(item => ['queued', 'running'].includes(item.status)),
        ...document.jobs.filter(item => !['queued', 'running'].includes(item.status)).slice(-95), job];
      await atomicJson(this.file, document); return structuredClone(job);
    });
  }
  update(jobId, patch) {
    return this._run(async () => {
      const document = await this._read(), job = document.jobs.find(item => item.jobId === jobId);
      if (!job) throw toolFailure('索引任务不存在。', 'RETRIEVAL_JOB_NOT_FOUND', 404);
      Object.assign(job, patch); await atomicJson(this.file, document); return structuredClone(job);
    });
  }
  async recover() {
    const jobs = await this.list();
    for (const job of jobs) if (['running', 'queued'].includes(job.status)) await this.update(job.jobId,
      { status: 'failed', error: 'INDEX_JOB_INTERRUPTED', finishedAt: new Date().toISOString() });
  }
}

import { createHash, randomUUID } from 'node:crypto';
import { open, readFile, stat, unlink } from 'node:fs/promises';
import { join } from 'node:path';
import { atomicJson } from '../../platform/atomic-json.mjs';
import { ensureLocalDirectory, inspectLocalPath, toolFailure } from '../../platform/tool-paths.mjs';
import { validateDocumentCoverage } from './document-coverage.mjs';

const ACTIVE_JOB_STATUSES = new Set(['queued', 'running', 'paused']);
const MAX_CHECKPOINT_BYTES = 128 * 1024 * 1024;
const MAX_CHECKPOINT_RECORDS = 200000;
const SHA256_PATTERN = /^[a-f0-9]{64}$/u;
const SEMANTIC_STATES = new Set(['disabled', 'complete', 'partial', 'unavailable']);
const MAX_SEMANTIC_DIAGNOSTICS = 8;
const JOB_TRANSITIONS = {
  queued: new Set(['queued', 'running', 'cancelled', 'failed']),
  running: new Set(['running', 'paused', 'completed', 'partial', 'cancelled', 'failed']),
  paused: new Set(['paused', 'running', 'cancelled', 'failed']),
  completed: new Set(['completed']),
  partial: new Set(['partial']),
  cancelled: new Set(['cancelled']),
  failed: new Set(['failed'])
};

function validateCheckpoint(value) {
  const fields = ['version', 'kind', 'checkpointId', 'settingsSignature', 'root', 'bindingRevision', 'preparationVersion', 'updatedAt'];
  if (!value || typeof value !== 'object' || Array.isArray(value) || value.version !== 1 || value.kind !== 'source-index' ||
      Object.keys(value).some(field => !fields.includes(field)) || !SHA256_PATTERN.test(value.checkpointId ?? '') ||
      !SHA256_PATTERN.test(value.settingsSignature ?? '') ||
      !(value.root === null || typeof value.root === 'string' && value.root.length <= 4096) ||
      !Number.isSafeInteger(value.bindingRevision) || value.bindingRevision < 0 ||
      !(value.preparationVersion === null || typeof value.preparationVersion === 'string' && value.preparationVersion.length <= 512) ||
      typeof value.updatedAt !== 'string' || !Number.isFinite(Date.parse(value.updatedAt)))
    throw toolFailure('索引恢复检查点无效。', 'INVALID_RETRIEVAL_CHECKPOINT', 400);
  return structuredClone(value);
}

function validateCheckpointSource(value) {
  const fields = ['sourceId', 'inputSignature', 'fingerprint', 'semantic', 'preparationVersion', 'derivationSignature',
    'embeddingInputSignature', 'chunkCount', 'vectorChunks', 'embeddingProfileId', 'embeddingModelVersion', 'embeddingSpaceId', 'vectorDimensions'];
  if (!value || typeof value !== 'object' || Array.isArray(value) || Object.keys(value).some(field => !fields.includes(field)) ||
      typeof value.sourceId !== 'string' || !value.sourceId || value.sourceId.length > 256 ||
      ['inputSignature', 'fingerprint', 'derivationSignature', 'embeddingInputSignature'].some(field => !SHA256_PATTERN.test(value[field] ?? '')) ||
      typeof value.semantic !== 'boolean' || typeof value.preparationVersion !== 'string' || !value.preparationVersion || value.preparationVersion.length > 512 ||
      !Number.isSafeInteger(value.chunkCount) || value.chunkCount < 0 || !Number.isSafeInteger(value.vectorChunks) ||
      value.vectorChunks < 0 || value.vectorChunks > value.chunkCount ||
      !(value.vectorDimensions === null || Number.isSafeInteger(value.vectorDimensions) && value.vectorDimensions > 0 && value.vectorDimensions <= 4096))
    throw toolFailure('索引批次回执无效。', 'INVALID_RETRIEVAL_CHECKPOINT', 400);
  for (const field of ['embeddingProfileId', 'embeddingModelVersion', 'embeddingSpaceId'])
    if (!(value[field] === null || typeof value[field] === 'string' && value[field].length <= 512))
      throw toolFailure('索引嵌入回执无效。', 'INVALID_RETRIEVAL_CHECKPOINT', 400);
  return structuredClone(value);
}

function validateSemanticProgress(value) {
  if (!value || typeof value !== 'object' || Array.isArray(value) || typeof value.requested !== 'boolean' ||
      !(value.profileId === null || typeof value.profileId === 'string' && value.profileId.length <= 256) ||
      !SEMANTIC_STATES.has(value.state))
    throw toolFailure('索引语义进度无效。', 'INVALID_RETRIEVAL_JOB_UPDATE', 400);
  const fields = ['requested', 'profileId', 'state', 'totalChunks', 'vectorChunks', 'cachedChunks',
    'diagnosticCodes', 'priorDiagnosticCodes', 'skippedSources', 'skippedChunks'];
  if (Object.keys(value).some(field => !fields.includes(field)))
    throw toolFailure('索引语义进度字段无效。', 'INVALID_RETRIEVAL_JOB_UPDATE', 400);
  for (const field of ['totalChunks', 'vectorChunks', 'cachedChunks', 'skippedSources', 'skippedChunks']) {
    if (value[field] === undefined && ['skippedSources', 'skippedChunks'].includes(field)) continue;
    if (!Number.isSafeInteger(value[field]) || value[field] < 0)
      throw toolFailure('索引语义数量无效。', 'INVALID_RETRIEVAL_JOB_UPDATE', 400);
  }
  for (const field of ['diagnosticCodes', 'priorDiagnosticCodes']) {
    if (field === 'priorDiagnosticCodes' && value[field] === undefined) continue;
    if (!Array.isArray(value[field]) || value[field].length > MAX_SEMANTIC_DIAGNOSTICS ||
        value[field].some(code => typeof code !== 'string' || !/^[A-Z][A-Z0-9_]{0,127}$/u.test(code)))
      throw toolFailure('索引语义诊断无效。', 'INVALID_RETRIEVAL_JOB_UPDATE', 400);
  }
  return structuredClone(value);
}

function validateCoverageProgress(value) {
  const fields = ['discovered', 'files', 'lexical', 'semantic', 'failed', 'skipped', 'partial', 'complete', 'sources', 'failures',
    'reportTruncated', 'limit', 'limits', 'effectiveLimits'];
  if (!value || typeof value !== 'object' || Array.isArray(value) || Object.keys(value).some(key => !fields.includes(key)) ||
      typeof value.complete !== 'boolean') throw toolFailure('索引覆盖进度无效。', 'INVALID_RETRIEVAL_JOB_UPDATE', 400);
  for (const key of ['discovered', 'lexical', 'semantic', 'failed', 'skipped'])
    if (!Number.isSafeInteger(value[key]) || value[key] < 0) throw toolFailure('索引覆盖数量无效。', 'INVALID_RETRIEVAL_JOB_UPDATE', 400);
  for (const key of ['files', 'partial']) if (value[key] !== undefined && (!Number.isSafeInteger(value[key]) || value[key] < 0))
    throw toolFailure('索引覆盖数量无效。', 'INVALID_RETRIEVAL_JOB_UPDATE', 400);
  const states = new Set(['pending', 'ready', 'partial', 'failed', 'skipped', 'unverified', 'disabled']);
  if (value.sources !== undefined && (!Array.isArray(value.sources) || value.sources.length > 1000 || value.sources.some(source =>
    !source || typeof source.sourceId !== 'string' || source.sourceId.length > 256 || typeof source.relativePath !== 'string' || source.relativePath.length > 4096 ||
    ['status', 'lexical', 'semantic', 'parser'].some(key => !states.has(source[key])) ||
    source.errorCode !== undefined && !/^[A-Z][A-Z0-9_]{0,127}$/u.test(source.errorCode))))
    throw toolFailure('索引来源覆盖记录无效。', 'INVALID_RETRIEVAL_JOB_UPDATE', 400);
  if (value.failures !== undefined && (!Array.isArray(value.failures) || value.failures.length > 1000 || value.failures.some(item =>
    !item || typeof item.relativePath !== 'string' || item.relativePath.length > 4096 || !/^[A-Z][A-Z0-9_]{0,127}$/u.test(item.errorCode ?? ''))))
    throw toolFailure('索引来源失败记录无效。', 'INVALID_RETRIEVAL_JOB_UPDATE', 400);
  if (value.reportTruncated !== undefined && typeof value.reportTruncated !== 'boolean')
    throw toolFailure('索引覆盖截断标记无效。', 'INVALID_RETRIEVAL_JOB_UPDATE', 400);
  for (const entry of [...(value.sources ?? []), ...(value.failures ?? [])])
    if (entry.documentCoverage !== undefined) validateDocumentCoverage(entry.documentCoverage);
  const limits = value.limits ?? [];
  if (!Array.isArray(limits) || limits.length > 50 || [...limits, ...(value.limit ? [value.limit] : [])].some(limit => !limit || typeof limit.dimension !== 'string' ||
      limit.dimension.length > 64 || !Number.isSafeInteger(limit.limit) || limit.limit < 1 ||
      !Number.isSafeInteger(limit.observed) || limit.observed < 0))
    throw toolFailure('索引覆盖限额无效。', 'INVALID_RETRIEVAL_JOB_UPDATE', 400);
  if (value.effectiveLimits !== undefined && (!value.effectiveLimits || typeof value.effectiveLimits !== 'object' ||
      Array.isArray(value.effectiveLimits) || Object.keys(value.effectiveLimits).some(key => !['maximumFiles', 'maximumSourceBytes',
        'maximumBytes', 'maximumEntries', 'maximumDocumentInputBytes', 'maximumDocumentOutputBytes', 'maximumPdfPages'].includes(key)) ||
      Object.values(value.effectiveLimits).some(limit => !Number.isSafeInteger(limit) || limit < 1)))
    throw toolFailure('索引有效限额无效。', 'INVALID_RETRIEVAL_JOB_UPDATE', 400);
  return structuredClone(value);
}

/** Durable indexing status, independent of chat existence and action execution receipts.
 * 持久索引状态独立于聊天是否存在，也不能替代副作用执行回执。 */
export class RetrievalJobStore {
  constructor(root) {
    this.folder = join(root, 'Retrieval'); this.file = join(this.folder, 'jobs.json');
    this.checkpointFolder = join(this.folder, 'checkpoints'); this.queue = Promise.resolve();
  }
  _run(operation) { const pending = this.queue.catch(() => {}).then(operation); this.queue = pending; return pending; }
  async _read() {
    await ensureLocalDirectory(this.folder);
    if (!await inspectLocalPath(this.file, { allowMissing: true })) return { schemaVersion: 1, jobs: [] };
    const document = JSON.parse(await readFile(this.file, 'utf8'));
    if (document.schemaVersion !== 1 || !Array.isArray(document.jobs))
      throw toolFailure('索引任务文件版本无效。', 'UNSUPPORTED_RETRIEVAL_SCHEMA', 409);
    for (const job of document.jobs) if (job.coverage !== undefined) validateCoverageProgress(job.coverage);
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
      const previous = document.jobs;
      document.jobs = [...document.jobs.filter(item => ACTIVE_JOB_STATUSES.has(item.status)),
        ...document.jobs.filter(item => !ACTIVE_JOB_STATUSES.has(item.status)).slice(-95), job];
      const retained = new Set(document.jobs.map(item => item.jobId));
      // Retired terminal checkpoints are rebuildable metadata, never original sources or active work.
      // 淘汰的终态检查点只是可重建元信息，不是原始资料，也不包含仍在运行的任务。
      for (const retired of previous) if (!retained.has(retired.jobId)) {
        const path = this._checkpointPath(retired.jobId);
        if (await inspectLocalPath(path, { allowMissing: true })) await unlink(path);
      }
      await atomicJson(this.file, document); return structuredClone(job);
    });
  }
  update(jobId, patch) {
    return this._run(async () => {
      const document = await this._read(), job = document.jobs.find(item => item.jobId === jobId);
      if (!job) throw toolFailure('索引任务不存在。', 'RETRIEVAL_JOB_NOT_FOUND', 404);
      if (patch.jobId !== undefined && patch.jobId !== job.jobId || patch.projectId !== undefined && patch.projectId !== job.projectId)
        throw toolFailure('索引任务身份不能修改。', 'INVALID_RETRIEVAL_JOB_UPDATE', 409);
      if (patch.status !== undefined && !JOB_TRANSITIONS[job.status]?.has(patch.status))
        throw toolFailure('索引任务已结束，不能恢复运行。', 'RETRIEVAL_JOB_STATE_CONFLICT', 409);
      for (const field of ['completedSources', 'totalSources']) if (patch[field] !== undefined &&
          (!Number.isSafeInteger(patch[field]) || patch[field] < 0 || field === 'completedSources' && patch[field] < job.completedSources))
        throw toolFailure('索引任务进度无效。', 'INVALID_RETRIEVAL_JOB_UPDATE', 400);
      if (patch.semantic !== undefined) patch = { ...patch, semantic: validateSemanticProgress(patch.semantic) };
      if (patch.coverage !== undefined) patch = { ...patch, coverage: validateCoverageProgress(patch.coverage) };
      if (patch.checkpoint !== undefined) patch = { ...patch, checkpoint: validateCheckpoint(patch.checkpoint) };
      Object.assign(job, patch); await atomicJson(this.file, document); return structuredClone(job);
    });
  }
  commitBatch(jobId, checkpoint, sources, patch = {}) {
    return this._run(async () => {
      const document = await this._read(), job = document.jobs.find(item => item.jobId === jobId);
      if (!job || job.status !== 'running')
        throw toolFailure('索引任务未运行，不能记录批次。', 'RETRIEVAL_JOB_STATE_CONFLICT', 409);
      checkpoint = validateCheckpoint(checkpoint);
      if (!Array.isArray(sources) || sources.length > 128)
        throw toolFailure('索引批次回执过大。', 'INVALID_RETRIEVAL_CHECKPOINT', 400);
      sources = sources.map(validateCheckpointSource);
      for (const field of ['completedSources', 'totalSources']) if (patch[field] !== undefined &&
          (!Number.isSafeInteger(patch[field]) || patch[field] < 0 || field === 'completedSources' && patch[field] < job.completedSources))
        throw toolFailure('索引任务进度无效。', 'INVALID_RETRIEVAL_JOB_UPDATE', 400);
      if (patch.semantic !== undefined) patch = { ...patch, semantic: validateSemanticProgress(patch.semantic) };
      if (patch.coverage !== undefined) patch = { ...patch, coverage: validateCoverageProgress(patch.coverage) };
      const record = { jobId, checkpointId: checkpoint.checkpointId, sources };
      const payload = JSON.stringify(record);
      const line = JSON.stringify({ ...record, checksum: createHash('sha256').update(payload).digest('hex') }) + '\n';
      await ensureLocalDirectory(this.checkpointFolder);
      const path = this._checkpointPath(jobId);
      const exists = await inspectLocalPath(path, { allowMissing: true });
      if ((exists ? (await stat(path)).size : 0) + Buffer.byteLength(line) > MAX_CHECKPOINT_BYTES)
        throw toolFailure('索引恢复日志超过本地预算。', 'RETRIEVAL_CHECKPOINT_TOO_LARGE', 413);
      // Fsync the bounded receipt before announcing progress; no source text or vectors enter this log.
      // 有界批次回执先同步到磁盘再公布进度，此日志不保存正文或向量。
      const handle = await open(path, 'a');
      try { await handle.writeFile(line, { encoding: 'utf8' }); await handle.sync(); }
      finally { await handle.close(); }
      Object.assign(job, patch, { checkpoint });
      await atomicJson(this.file, document);
      return structuredClone(job);
    });
  }

  _checkpointPath(jobId) {
    if (typeof jobId !== 'string' || !/^[a-z0-9-]{1,128}$/iu.test(jobId))
      throw toolFailure('索引任务身份无效。', 'INVALID_RETRIEVAL_CHECKPOINT', 400);
    return join(this.checkpointFolder, `${jobId}.jsonl`);
  }

  checkpointSources(jobId, checkpoint) {
    return this._run(async () => {
      checkpoint = validateCheckpoint(checkpoint);
      const path = this._checkpointPath(jobId);
      if (!await inspectLocalPath(path, { allowMissing: true })) return [];
      if ((await stat(path)).size > MAX_CHECKPOINT_BYTES)
        throw toolFailure('索引恢复日志超过本地预算。', 'RETRIEVAL_CHECKPOINT_TOO_LARGE', 413);
      const content = await readFile(path, 'utf8'), records = new Map();
      const lines = content.split('\n');
      // An interrupted last write is not a committed record; validate every complete preceding line.
      // 中断时未写完整的末行不算回执，之前所有完整行都必须通过完整性校验。
      lines.pop();
      for (const line of lines) {
        if (!line) continue;
        let record;
        try { record = JSON.parse(line); } catch { throw toolFailure('索引恢复日志损坏。', 'INVALID_RETRIEVAL_CHECKPOINT', 409); }
        const payload = JSON.stringify({ jobId: record.jobId, checkpointId: record.checkpointId, sources: record.sources });
        if (record.jobId !== jobId || !SHA256_PATTERN.test(record.checkpointId ?? '') ||
            record.checksum !== createHash('sha256').update(payload).digest('hex') ||
            !Array.isArray(record.sources) || record.sources.length > 128)
          throw toolFailure('索引恢复日志损坏。', 'INVALID_RETRIEVAL_CHECKPOINT', 409);
        const sources = record.sources.map(validateCheckpointSource);
        if (record.checkpointId === checkpoint.checkpointId)
          for (const source of sources) records.set(source.sourceId, source);
        if (records.size > MAX_CHECKPOINT_RECORDS)
          throw toolFailure('索引恢复来源超过预算。', 'RETRIEVAL_CHECKPOINT_TOO_LARGE', 413);
      }
      const boundary = content.lastIndexOf('\n') + 1;
      if (boundary < content.length) {
        // Remove only the uncommitted tail before the next append, preserving the verified prefix.
        // 下次追加前只删除未提交尾部，已校验的完整回执保持原样。
        const handle = await open(path, 'r+');
        try { await handle.truncate(Buffer.byteLength(content.slice(0, boundary))); await handle.sync(); }
        finally { await handle.close(); }
      }
      return [...records.values()];
    });
  }

  recover({ resumable = false } = {}) {
    return this._run(async () => {
      const document = await this._read();
      let changed = false;
      const pending = [];
      for (const job of document.jobs) if (ACTIVE_JOB_STATUSES.has(job.status)) {
        if (resumable && job.checkpoint) {
          try { validateCheckpoint(job.checkpoint); pending.push(structuredClone(job)); continue; }
          catch { /* Invalid headers never authorize an automatic restart. 无效头部不能触发自动恢复。 */ }
        }
        Object.assign(job, { status: 'failed', error: 'INDEX_JOB_INTERRUPTED', finishedAt: new Date().toISOString() });
        changed = true;
      }
      if (changed) await atomicJson(this.file, document);
      return pending;
    });
  }
}

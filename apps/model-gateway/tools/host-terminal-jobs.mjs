import { randomUUID } from 'node:crypto';
import { boundedInteger, toolFailure } from '../platform/tool-paths.mjs';

const MAX_OUTPUT_CHARACTERS = 262144;
const MAX_RETAINED_JOBS = 32;
const MAX_JOB_TIMEOUT_MS = 21600000;

function receiptMetadata(result) {
  if (!result) return undefined;
  const { stdout, stderr, consoleText, ...value } = result.value ?? {};
  return { ...value, isError: result.isError, code: result.code, status: result.status };
}

/**
 * Running jobs belong to their chat and the gateway lifetime; model request completion is not cancellation.
 * 运行任务归属原聊天及网关生命周期，单轮模型请求结束不应取消后台任务。
 */
export class HostTerminalJobs {
  constructor({ runner, maximumRunning = 4 } = {}) {
    if (!runner?.run) throw new TypeError('Host terminal runner is required.');
    this.runner = runner;
    this.maximumRunning = maximumRunning;
    this.jobs = new Map();
    this.closed = false;
  }

  _get(context, jobId) {
    const job = this.jobs.get(jobId);
    if (!job || job.conversationId !== context.conversationId)
      throw toolFailure('当前聊天中没有此终端任务。', 'HOST_TERMINAL_JOB_NOT_FOUND', 404);
    return job;
  }

  _view(job) {
    return { jobId: job.id, status: job.status, running: ['starting', 'running'].includes(job.status),
      processId: job.processId, shell: job.shell, cwd: job.cwd, startedAt: job.startedAt, finishedAt: job.finishedAt,
      timeoutMs: job.timeoutMs, totalCharacters: job.totalCharacters, earliestOffset: job.baseOffset,
      receipt: receiptMetadata(job.result),
      ...(job.failure ? { error: { code: job.failure.code ?? 'HOST_TERMINAL_FAILED', message: job.failure.message } } : {}) };
  }

  async start(context, request, signal) {
    signal?.throwIfAborted();
    if (this.closed) throw toolFailure('后台终端任务服务已经关闭。', 'HOST_TERMINAL_UNAVAILABLE', 503);
    if (!context.conversationId) throw toolFailure('后台终端任务需要聊天身份。', 'HOST_TERMINAL_INVALID_REQUEST');
    if ([...this.jobs.values()].filter(job => ['starting', 'running'].includes(job.status)).length >= this.maximumRunning)
      throw toolFailure('后台终端任务已达到并发上限。', 'HOST_TERMINAL_CAPACITY', 429);
    for (const [id, job] of this.jobs) {
      if (this.jobs.size < MAX_RETAINED_JOBS) break;
      if (!['starting', 'running'].includes(job.status)) this.jobs.delete(id);
    }
    const timeoutMs = boundedInteger(request.timeoutMs, 1800000, 100, MAX_JOB_TIMEOUT_MS);
    const controller = new AbortController();
    const job = { id: randomUUID(), conversationId: context.conversationId, controller, shell: request.shell, cwd: request.cwd,
      status: 'starting', startedAt: new Date().toISOString(), timeoutMs, output: '', totalCharacters: 0, baseOffset: 0 };
    this.jobs.set(job.id, job);
    let resolveStarted, rejectStarted;
    const started = new Promise((resolve, reject) => { resolveStarted = resolve; rejectStarted = reject; });
    const abortStartup = () => controller.abort(signal.reason);
    const rejectAbortedStartup = () => rejectStarted(Object.assign(new Error('后台终端任务启动已取消。'), { name: 'AbortError' }));
    signal?.addEventListener('abort', abortStartup, { once: true });
    controller.signal.addEventListener('abort', rejectAbortedStartup, { once: true });
    const deadline = setTimeout(() => controller.abort(new Error('Background terminal startup acknowledgement timed out.')), 15000);
    const append = event => {
      if (event.replace || typeof event.text !== 'string') return;
      job.output += event.text;
      job.totalCharacters += event.text.length;
      if (job.output.length > MAX_OUTPUT_CHARACTERS) job.output = job.output.slice(-MAX_OUTPUT_CHARACTERS);
      if (/^[\uDC00-\uDFFF]/.test(job.output)) job.output = job.output.slice(1);
      job.baseOffset = job.totalCharacters - job.output.length;
    };
    // Observe and retain every completion, including cancellations and startup failures.
    // 每次结束都保存真实回执，包括取消和启动失败；不自动重放已经提交的命令。
    job.completion = Promise.resolve().then(() => this.runner.run({ ...request, visible: false, backgroundJob: true, timeoutMs },
      controller.signal, append, receipt => {
        if (job.status !== 'starting' || !Number.isSafeInteger(receipt.processId) || receipt.processId <= 0) return;
        job.processId = receipt.processId;
        job.status = 'running';
        resolveStarted();
      })).then(result => {
      job.result = result;
      job.status = result.status === 'unknown' ? 'unknown' : result.isError ? 'error' : 'completed';
      if (!job.processId) rejectStarted(Object.assign(toolFailure(
        `后台终端任务没有可核验的启动确认，请用 terminal.host.read 查询任务 ${job.id}，不要自动重新执行命令。`,
        'HOST_TERMINAL_INVALID_RESULT', 502), { jobId: job.id, outcomeUnknown: result.status === 'unknown' || result.value?.completed === true }));
    }, error => {
      job.failure = error;
      job.status = error.name === 'AbortError' ? 'cancelled' : 'error';
      rejectStarted(error);
    }).finally(() => { job.finishedAt = new Date().toISOString(); });
    try {
      await started;
      signal?.throwIfAborted();
      return this._view(job);
    } catch (error) {
      // Retain the broker's handle even when a startup acknowledgement is lost; it never authorizes a replay.
      // 启动确认丢失时仍保留代理分配的任务句柄；句柄只供查询，不授权重放命令。
      if (error && typeof error === 'object') error.jobId = job.id;
      throw error;
    } finally {
      clearTimeout(deadline);
      signal?.removeEventListener('abort', abortStartup);
      controller.signal.removeEventListener('abort', rejectAbortedStartup);
    }
  }

  read(context, { jobId, offset = 0, limit = 16000 }) {
    const job = this._get(context, jobId);
    offset = boundedInteger(offset, 0, 0, Number.MAX_SAFE_INTEGER);
    limit = boundedInteger(limit, 16000, 1, 64000);
    if (offset > job.totalCharacters) throw toolFailure('输出位置超出终端任务记录。', 'HOST_TERMINAL_INVALID_OFFSET');
    const actualOffset = Math.max(offset, job.baseOffset), relativeOffset = actualOffset - job.baseOffset;
    if (relativeOffset > 0 && /[\uD800-\uDBFF]/.test(job.output[relativeOffset - 1]) && /[\uDC00-\uDFFF]/.test(job.output[relativeOffset] ?? ''))
      throw toolFailure('输出位置不能位于字符代理对中间，请使用上一页的 nextOffset。', 'HOST_TERMINAL_INVALID_OFFSET');
    let nextOffset = Math.min(job.totalCharacters, actualOffset + limit);
    const relativeEnd = nextOffset - job.baseOffset;
    if (relativeEnd < job.output.length && /[\uD800-\uDBFF]/.test(job.output[relativeEnd - 1]) && /[\uDC00-\uDFFF]/.test(job.output[relativeEnd]))
      nextOffset += nextOffset - actualOffset === 1 ? 1 : -1;
    return { ...this._view(job), output: job.output.slice(actualOffset - job.baseOffset, nextOffset - job.baseOffset),
      offset: actualOffset, nextOffset, hasMore: nextOffset < job.totalCharacters, truncated: offset < job.baseOffset };
  }

  async stop(context, { jobId }, signal) {
    signal?.throwIfAborted();
    const job = this._get(context, jobId);
    if (['starting', 'running'].includes(job.status)) job.controller.abort();
    await job.completion;
    return this._view(job);
  }

  async close() {
    this.closed = true;
    for (const job of this.jobs.values()) if (['starting', 'running'].includes(job.status)) job.controller.abort();
    await Promise.allSettled([...this.jobs.values()].map(job => job.completion));
  }
}

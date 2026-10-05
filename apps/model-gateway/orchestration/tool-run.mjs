import { estimateTokens } from '../models/context-tokens.mjs';
import { StreamFailure } from '../models/streaming.mjs';

// Limits belong to one continuous request, independently of a model's output ceiling.
// 限制属于一次连续请求，与单轮模型输出上限无关。
export const DEFAULT_TOOL_RUN_LIMITS = Object.freeze({ maxRounds: 64, maxToolCalls: 256,
  maxGeneratedTokens: 1_048_576, maxDurationMs: 1_800_000 });

export function toolRunLimits(input = {}) {
  const bounds = { maxRounds: [1, 128], maxToolCalls: [1, 512],
    maxGeneratedTokens: [1024, 4_194_304], maxDurationMs: [1000, 3_600_000] };
  if (!input || typeof input !== 'object' || Array.isArray(input) || Object.keys(input).some(key => !Object.hasOwn(bounds, key)))
    throw Object.assign(new Error('连续执行预算格式无效。'), { code: 'INVALID_TOOL_RUN_LIMITS', statusCode: 400 });
  const result = { ...DEFAULT_TOOL_RUN_LIMITS, ...input };
  for (const [key, [min, max]] of Object.entries(bounds)) {
    if (!Number.isSafeInteger(result[key]) || result[key] < min || result[key] > max)
      throw Object.assign(new Error('连续执行预算超出允许范围。'), { code: 'INVALID_TOOL_RUN_LIMITS', statusCode: 400 });
  }
  return result;
}

export function runLimitFailure(code) {
  return Object.assign(new StreamFailure('连续执行达到预算上限，已保留回复和工具执行记录。', 'interrupted'), { code });
}

const metricFailure = () => Object.assign(new Error('执行耗时诊断格式无效。'), { code: 'INVALID_TOOL_RUN_DIAGNOSTICS' });
function metricInteger(value) {
  if (!Number.isSafeInteger(value) || value < 0) throw metricFailure();
  return value;
}
function metricSum(left, right) { return metricInteger(left + right); }
function metricRound(value) {
  if (!Number.isSafeInteger(value) || value < 1 || value > 128) throw metricFailure();
  return value;
}

/**
 * Caller ends this timer at the real operation boundary; wall-clock/date edits cannot change elapsed time.
 * 调用方在操作实际结束时停止计时，系统日期或时钟调整不改变已用时。
 */
export function startRunTimer() {
  const started = performance.now();
  return () => Math.max(0, Math.ceil(performance.now() - started));
}

/**
 * Small public projection; native provider continuation and credentials never enter it.
 * 公开视图保持精简，供应商原生续传状态和凭据不进入其中。
 */
export class ToolRunProgress {
  constructor(limits, persist = async () => {}) {
    this.limits = toolRunLimits(limits);
    this.persist = persist;
    this.startedAt = new Date().toISOString();
    this.generatedTokens = 0;
    this.rounds = 0;
    this.toolCalls = 0;
    this.diagnostics = { version: 1, totalModelMs: 0, totalToolMs: 0, totalApprovalWaitMs: 0,
      maxModelMs: 0, maxToolMs: 0, maxApprovalWaitMs: 0, modelCalls: 0,
      executedToolCalls: 0, reusedToolCalls: 0, noProgressRounds: 0, modelRounds: [], toolCallsTiming: [] };
  }

  observeTurn(turn) {
    this.generatedTokens += estimateTokens(turn.content ?? '') + estimateTokens(turn.reasoning ?? '')
      + estimateTokens(JSON.stringify(turn.calls ?? []));
    if (this.generatedTokens > this.limits.maxGeneratedTokens) throw runLimitFailure('TOOL_RUN_OUTPUT_LIMIT');
  }

  recordModel(durationMs, round = this.rounds) {
    durationMs = metricInteger(durationMs); round = metricRound(round);
    const previous = this.diagnostics;
    this.diagnostics = { ...previous, totalModelMs: metricSum(previous.totalModelMs, durationMs),
      maxModelMs: Math.max(previous.maxModelMs, durationMs), modelCalls: metricSum(previous.modelCalls, 1),
      modelRounds: [...previous.modelRounds, { round, durationMs }].slice(-64) };
  }

  /**
   * durationMs is the complete call span. Tool totals/maxima exclude measured approval wait.
   * Concurrent tool spans are summed processing time, never the request's elapsed wall time.
   * Reused calls measure lookup/projection overhead and do not increment executedToolCalls.
   * durationMs 表示完整调用跨度；工具总用时和最大用时扣除已测审批等待，并发跨度之和不是请求墙钟用时；复用调用只计查找与投影开销，不增加 executedToolCalls。
   */
  recordTool({ id, round, durationMs, approvalMs = 0, reused = false }) {
    durationMs = metricInteger(durationMs); approvalMs = metricInteger(approvalMs); round = metricRound(round);
    if (typeof id !== 'string' || !id || id.length > 200 || /[\0\r\n]/.test(id) || typeof reused !== 'boolean' || approvalMs > durationMs)
      throw metricFailure();
    const previous = this.diagnostics, executionMs = durationMs - approvalMs;
    this.diagnostics = { ...previous, totalToolMs: metricSum(previous.totalToolMs, executionMs),
      totalApprovalWaitMs: metricSum(previous.totalApprovalWaitMs, approvalMs),
      maxToolMs: Math.max(previous.maxToolMs, executionMs), maxApprovalWaitMs: Math.max(previous.maxApprovalWaitMs, approvalMs),
      executedToolCalls: metricSum(previous.executedToolCalls, reused ? 0 : 1),
      reusedToolCalls: metricSum(previous.reusedToolCalls, reused ? 1 : 0),
      toolCallsTiming: [...previous.toolCallsTiming, { toolCallId: id, round, durationMs, approvalMs, reused }].slice(-256) };
  }

  observeNoProgress(count = 1) {
    const previous = this.diagnostics;
    this.diagnostics = { ...previous, noProgressRounds: metricSum(previous.noProgressRounds, metricInteger(count)) };
  }

  async save(phase, { toolCallId = null, code = null } = {}) {
    await this.persist({ version: 1, phase, rounds: this.rounds, toolCalls: this.toolCalls,
      estimatedGeneratedTokens: this.generatedTokens, limits: this.limits,
      startedAt: this.startedAt, updatedAt: new Date().toISOString(), toolCallId, code,
      diagnostics: structuredClone(this.diagnostics) });
  }
}

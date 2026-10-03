import { estimateTokens } from './context-tokens.mjs';
import { StreamFailure } from './streaming.mjs';

// Limits belong to one continuous request, independently of a model's output ceiling.
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

/** Small public projection; native provider continuation and credentials never enter it. */
export class ToolRunProgress {
  constructor(limits, persist = async () => {}) {
    this.limits = toolRunLimits(limits);
    this.persist = persist;
    this.startedAt = new Date().toISOString();
    this.generatedTokens = 0;
    this.rounds = 0;
    this.toolCalls = 0;
  }

  observeTurn(turn) {
    this.generatedTokens += estimateTokens(turn.content ?? '') + estimateTokens(turn.reasoning ?? '')
      + estimateTokens(JSON.stringify(turn.calls ?? []));
    if (this.generatedTokens > this.limits.maxGeneratedTokens) throw runLimitFailure('TOOL_RUN_OUTPUT_LIMIT');
  }

  async save(phase, { toolCallId = null, code = null } = {}) {
    await this.persist({ version: 1, phase, rounds: this.rounds, toolCalls: this.toolCalls,
      estimatedGeneratedTokens: this.generatedTokens, limits: this.limits,
      startedAt: this.startedAt, updatedAt: new Date().toISOString(), toolCallId, code });
  }
}

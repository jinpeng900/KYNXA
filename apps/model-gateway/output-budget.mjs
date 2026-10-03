// Output is a ceiling, not a target length. Keep it independent of history size.
export const DEFAULT_MAX_OUTPUT_TOKENS = 262_144;
export const MIN_CONFIGURED_OUTPUT_TOKENS = 1_024;
export const MAX_CONFIGURED_OUTPUT_TOKENS = 262_144;

export class OutputBudgetError extends Error {
  constructor(message, code = 'INVALID_OUTPUT_BUDGET') {
    super(message);
    this.name = 'OutputBudgetError';
    this.code = code;
    this.statusCode = 400;
  }
}

export function validateOutputTokens(value = DEFAULT_MAX_OUTPUT_TOKENS) {
  if (!Number.isInteger(value) || value < MIN_CONFIGURED_OUTPUT_TOKENS || value > MAX_CONFIGURED_OUTPUT_TOKENS) {
    throw new OutputBudgetError('maxOutputTokens 必须是 1,024 到 262,144 之间的整数。');
  }
  return value;
}

export function resolveOutputBudget({ contextWindowTokens, requestedOutputTokens, requiredInputTokens = 0,
  providerMaxOutputTokens, providerMaxInputTokens }) {
  if (!Number.isInteger(contextWindowTokens) || contextWindowTokens < 2_048 || contextWindowTokens > 2_000_000) {
    throw new OutputBudgetError('contextWindowTokens 必须是 2,048 到 2,000,000 之间的整数。', 'INVALID_CONTEXT_WINDOW');
  }
  if (!Number.isInteger(requiredInputTokens) || requiredInputTokens < 0) {
    throw new OutputBudgetError('requiredInputTokens 必须是非负整数。');
  }
  if (providerMaxOutputTokens !== undefined && (!Number.isInteger(providerMaxOutputTokens) || providerMaxOutputTokens < 256))
    throw new OutputBudgetError('模型实际输出上限必须是至少 256 的整数。');
  if (providerMaxInputTokens !== undefined && (!Number.isInteger(providerMaxInputTokens) || providerMaxInputTokens < 256))
    throw new OutputBudgetError('模型实际输入上限必须是至少 256 的整数。');
  const requested = validateOutputTokens(requestedOutputTokens);
  const safetyMarginTokens = Math.min(8_192, Math.max(256, Math.ceil(contextWindowTokens * 0.1)));
  const usableTokens = contextWindowTokens - safetyMarginTokens;
  const providerInputBudget = providerMaxInputTokens === undefined ? Infinity : providerMaxInputTokens - safetyMarginTokens;
  if (requiredInputTokens > providerInputBudget)
    throw new OutputBudgetError('当前消息、系统指令或工具目录超过模型实际输入上限，请缩小输入。', 'CONTEXT_INPUT_TOO_LARGE');
  // Small local windows keep their existing split. Larger windows protect history
  // and code inputs instead of reserving half the window for hypothetical output.
  // This is an application allocation, independent of a provider's actual limit.
  const outputShare = contextWindowTokens <= 32_768 ? .5 : .3;
  const contextOutputLimit = Math.min(Math.floor(usableTokens * outputShare), usableTokens - requiredInputTokens);
  const maxOutputTokens = Math.min(requested, providerMaxOutputTokens ?? Infinity, contextOutputLimit);
  if (maxOutputTokens < 256) {
    throw new OutputBudgetError('当前消息、系统指令或工具目录超过上下文预算，请增大模型实际支持的上下文或减少输入。', 'CONTEXT_INPUT_TOO_LARGE');
  }
  return {
    requestedOutputTokens: requested,
    maxOutputTokens,
    safetyMarginTokens,
    inputBudgetTokens: Math.min(usableTokens - maxOutputTokens, providerInputBudget),
    ...(providerMaxOutputTokens === undefined ? {} : { providerMaxOutputTokens }),
    ...(providerMaxInputTokens === undefined ? {} : { providerMaxInputTokens }),
    outputBudgetReduced: maxOutputTokens < requested,
    outputBudgetReductionReason: maxOutputTokens < requested
      ? providerMaxOutputTokens < requested && providerMaxOutputTokens <= contextOutputLimit ? 'provider_limit' : 'context_window'
      : null,
  };
}

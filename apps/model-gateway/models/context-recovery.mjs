import { localToolGrammarRejection } from './local-tool-compatibility.mjs';

const rejectionCodes = new Set(['context_length_exceeded', 'context_window_exceeded',
  'model_context_window_exceeded', 'input_too_long', 'prompt_too_long']);
const validLimit = value => Number.isSafeInteger(value) && value >= 2048 && value <= 2_000_000;

/** Read only bounded HTTP rejection diagnostics; never expose upstream text, credentials or reflected inputs.
 * 仅读取有界 HTTP 拒绝诊断，不公开上游原文、凭据或被回显的输入。 */
export async function readContextRejection(response) {
  if (![400, 413, 422].includes(response.status) || !response.body) return null;
  const reader = response.body.getReader();
  const chunks = [];
  let bytes = 0, complete = false;
  try {
    while (bytes <= 65_536) {
      const item = await reader.read();
      if (item.done) { complete = true; break; }
      bytes += item.value.byteLength;
      if (bytes > 65_536) break;
      chunks.push(Buffer.from(item.value));
    }
  } finally {
    if (!complete) await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
  if (!complete) return null;
  let body;
  try { body = JSON.parse(Buffer.concat(chunks).toString('utf8')); } catch { return null; }
  return contextRejection(body) ?? localToolGrammarRejection(body, { status: response.status });
}

/** Extract limits only from a recognized rejection, not arbitrary /models attributes or error numbers.
 * 仅从明确的超限拒绝中提取额度，不采信任意 /models 属性或错误里的其他数字。 */
export function contextRejection(body) {
  const error = body?.error;
  const message = typeof error === 'string' ? error : typeof error?.message === 'string' ? error.message
    : typeof body?.message === 'string' ? body.message : '';
  const code = String(error?.code ?? error?.type ?? body?.code ?? '').toLowerCase();
  const contextMatch = /(?:maximum|max)\s+context\s+(?:length|window)(?:\s+(?:is|of))?\s*[:=]?\s*([\d,]+)\s*tokens?/iu.exec(message);
  const inputMatch = /(?:prompt|input)\s+is\s+too\s+long\s*:\s*[\d,]+\s*tokens?\s*>\s*([\d,]+)\s*(?:maximum|max)/iu.exec(message);
  const numericContext = error?.maximum_context_length ?? error?.max_context_tokens;
  const contextWindowTokens = validLimit(numericContext) ? numericContext : Number(contextMatch?.[1]?.replaceAll(',', ''));
  const outputMatch = /\bmax_(?:output_tokens|completion_tokens|tokens)\b[^\n]{0,120}?(?:at most|less than or equal to|between\s+\d+\s+and|<=)\s*([\d,]+)/iu.exec(message);
  const providerMaxOutputTokens = Number(outputMatch?.[1]?.replaceAll(',', ''));
  if (outputMatch && Number.isSafeInteger(providerMaxOutputTokens) && providerMaxOutputTokens >= 256 && providerMaxOutputTokens <= 2_000_000)
    return { kind: 'output', providerMaxOutputTokens, verified: true };
  const providerMaxInputTokens = Number(inputMatch?.[1]?.replaceAll(',', ''));
  if (inputMatch && validLimit(providerMaxInputTokens)) return { kind: 'input', providerMaxInputTokens, verified: true };
  if (!rejectionCodes.has(code) && !contextMatch) return null;
  return { kind: 'context', ...(validLimit(contextWindowTokens) ? { contextWindowTokens, verified: true } : { verified: false }) };
}

/** Retry only a rejected model step, with a strictly smaller limit; callers retain tools and durable receipts.
 * 只重试被拒绝的模型步骤，额度必须严格缩小；调用方保留工具身份和正式执行回执。 */
export function contextRecoveryLimits(rejection, current) {
  if (rejection?.kind === 'input') {
    const maximum = rejection.providerMaxInputTokens;
    if (!validLimit(maximum) || maximum >= (current.providerMaxInputTokens ?? Infinity)) return null;
    return { kind: 'input', contextWindowTokens: current.contextWindowTokens, providerMaxInputTokens: maximum,
      verified: rejection.verified };
  }
  if (rejection?.kind === 'output') {
    const maximum = rejection.providerMaxOutputTokens;
    if (!Number.isSafeInteger(maximum) || maximum < 256 || maximum > 2_000_000 || maximum >= current.maxOutputTokens) return null;
    return { kind: 'output', contextWindowTokens: current.contextWindowTokens, providerMaxOutputTokens: maximum,
      verified: rejection.verified };
  }
  if (rejection?.kind !== 'context') return null;
  const smaller = rejection.contextWindowTokens ?? Math.floor(current.contextWindowTokens / 2);
  const contextWindowTokens = Math.min(current.contextWindowTokens, smaller);
  if (!validLimit(contextWindowTokens) || contextWindowTokens >= current.contextWindowTokens) return null;
  return { kind: 'context', contextWindowTokens, verified: rejection.verified === true };
}

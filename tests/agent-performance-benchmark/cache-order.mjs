import { createHash } from 'node:crypto';
import { SYSTEM_TASKS, verifySystemTask } from './system-suite.mjs';

export const CACHE_ORDER_VERSION = 'native-tool-order-v1';
export const CACHE_ORDER_TASKS = Object.freeze(SYSTEM_TASKS.filter(task => [
  'greeting-zh', 'english-to-chinese', 'multi-source-join', 'followup-entity',
  'dependent-file-cycle', 'recover-missing-file'
].includes(task.id)));
const integer = value => Number.isSafeInteger(value) && value >= 0 ? value : null;
const sumKnown = (values, field) => values.reduce((sum, value) => sum + (integer(value[field]) ?? 0), 0);
const completeSum = (values, field) => values.length > 0 && values.every(value => integer(value[field]) !== null)
  ? sumKnown(values, field) : null;
export const fixtureId = value => {
  const hash = createHash('sha256').update(`cache-order-v1:${value}`).digest('hex').slice(0, 32);
  // Native catalog admission canonicalizes UUIDs; URL read routes expect that same hyphenated identity.
  // 原生目录准入会规范化 UUID；URL 读取路由必须使用同一带连字符身份，不能沿用未规范化的 32 位值。
  return `${hash.slice(0, 8)}-${hash.slice(8, 12)}-${hash.slice(12, 16)}-${hash.slice(16, 20)}-${hash.slice(20)}`;
};
export const fixtureHash = value => createHash('sha256').update(JSON.stringify(value)).digest('hex');

export function cacheOrderPlan({ repeats = 2, taskIds = CACHE_ORDER_TASKS.map(task => task.id) } = {}) {
  return { schemaVersion: 1, benchmarkVersion: CACHE_ORDER_VERSION, measurementKind: 'plan-only', realCalls: 0,
    repetitions: repeats, taskIds, arms: { A: 'relevance', B: 'stable' },
    sequence: Array.from({ length: repeats }, (_, index) => index % 2 === 0 ? ['A', 'B'] : ['B', 'A']),
    fixtureHash: fixtureHash(CACHE_ORDER_TASKS.filter(task => taskIds.includes(task.id))),
    metrics: ['taskSuccessRate', 'toolSelectionAccuracy', 'inputTokens', 'cacheReadTokens', 'cacheWriteTokens',
      'firstVisibleTextMs', 'durationMs', 'modelRequests', 'estimatedTokenCost'],
    instruction: 'Explicit --execute --gateway-bin PATH --application-root PATH --connection-file PATH --model ID required. '
      + 'Starts isolated native gateways only; no paid calls, process launch or data writes in plan mode.' };
}

/** Read the existing native reservations once; retries, review and compaction are not discarded.
 * 只读既有原生预约；失败重试、完成核验及压缩请求均保留，不创建第二份产品账本。 */
export function projectReservations(reservations) {
  const unique = new Map(reservations.map(item => [`${item.taskId}:${item.reservationId}`, item]));
  return [...unique.values()].filter(item => item.dispatched !== false && item.state !== 'released').map(item => {
    const provider = item.providerUsage ?? {}, usage = item.usage ?? {};
    const hasKnownUsage = usage.unknownTokenRequests === 0;
    return { reservationId: item.reservationId, state: item.state,
      dispatchKnown: item.dispatched === true,
      inputTokens: integer(provider.inputTokens) ?? (hasKnownUsage ? integer(usage.inputTokens) : null),
      outputTokens: integer(provider.outputTokens) ?? (hasKnownUsage ? integer(usage.outputTokens) : null),
      cacheReadTokens: integer(provider.cacheReadTokens), cacheWriteTokens: integer(provider.cacheCreationTokens),
      uncachedInputTokens: integer(provider.uncachedInputTokens),
      firstVisibleTextMs: integer(provider.clientMetrics?.firstVisibleTextMs),
      reservationElapsedMs: integer(item.updatedAtMs - item.createdAtMs),
      cachePolicy: provider.clientMetrics?.cachePolicy ?? null };
  });
}

/** Prices are explicit per-million token rates, never inferred from a model name or presented as an invoice.
 * 价格使用显式的每百万 token 单价，不能从模型名猜测，也不冒充供应商账单。 */
export function tokenCost(calls, price) {
  if (!price) return { estimatedTokenCost: null, knownEstimatedTokenCost: null, callsMissingPriceOrUsage: calls.length,
    currency: null, costKind: 'unpriced', reason: 'No explicit pricing table.' };
  for (const name of ['inputPerMillion', 'outputPerMillion']) {
    if (!Number.isFinite(price[name]) || price[name] < 0) throw new Error('INVALID_BENCHMARK_PRICE');
  }
  for (const name of ['cacheReadPerMillion', 'cacheWritePerMillion']) {
    if (price[name] != null && (!Number.isFinite(price[name]) || price[name] < 0)) throw new Error('INVALID_BENCHMARK_PRICE');
  }
  if (typeof price.currency !== 'string' || !price.currency.trim()) throw new Error('INVALID_BENCHMARK_CURRENCY');
  const readPrice = price.cacheReadPerMillion ?? price.inputPerMillion;
  const writePrice = price.cacheWritePerMillion ?? price.inputPerMillion;
  const amounts = calls.map(call => {
    if (!call.dispatchKnown || call.inputTokens === null || call.outputTokens === null
      || readPrice !== price.inputPerMillion && call.cacheReadTokens === null
      || writePrice !== price.inputPerMillion && call.cacheWriteTokens === null) return null;
    if (call.cacheReadTokens > call.inputTokens || call.cacheWriteTokens > call.inputTokens
      || call.cacheReadTokens !== null && call.cacheWriteTokens !== null
        && call.cacheReadTokens + call.cacheWriteTokens > call.inputTokens) return null;
    return (call.inputTokens * price.inputPerMillion + call.outputTokens * price.outputPerMillion
      + (call.cacheReadTokens ?? 0) * (readPrice - price.inputPerMillion)
      + (call.cacheWriteTokens ?? 0) * (writePrice - price.inputPerMillion)) / 1_000_000;
  });
  return { estimatedTokenCost: amounts.every(value => value !== null) ? amounts.reduce((sum, value) => sum + value, 0) : null,
    knownEstimatedTokenCost: amounts.reduce((sum, value) => sum + (value ?? 0), 0),
    callsMissingPriceOrUsage: amounts.filter(value => value === null).length, currency: price.currency,
    costKind: 'explicit-rate-token-estimate', reason: 'Includes every recorded dispatch; excludes unmeasured cache storage, '
      + 'tax and provider-specific billing adjustments. Unknown attempts prevent a complete estimate.' };
}

export function toolSelection(task, assistants) {
  const activities = assistants.flatMap(item => item.ToolActivities ?? []);
  const allowed = task.greeting ? new Set() : new Set(['filesystem.read', 'filesystem.list', 'filesystem.search', 'filesystem.stat',
    'knowledge.search', 'knowledge.read', 'tool.search', 'tool.load', 'tool.result.read',
    ...(task.messages.length > 1 ? ['conversation.history.search', 'conversation.history.read'] : []),
    ...(task.effect ? ['filesystem.write'] : [])]);
  const correct = activities.filter(item => allowed.has(item.name)).length;
  return { observed: activities.length, correct, accuracy: activities.length ? correct / activities.length : task.greeting ? 1 : null,
    method: 'Frozen synthetic task allow-list; discovery tools allowed; not a general semantic quality judge.' };
}

export function assessNativeTask(task, assistants, disk) {
  // Preserve the existing fixed gold, sources and real write/read-back checks; do not lower the scorer for A/B.
  // 沿用既有金标、来源与真实写入读回校验；A/B 不放宽评分，也不把模型自报成功当作证据。
  return { ...verifySystemTask(task, { assistants, disk }), toolSelection: toolSelection(task, assistants) };
}

export function summarizeCacheOrder(runs, price = null) {
  const calls = runs.flatMap(run => run.modelCalls ?? []), visible = calls.filter(call => call.firstVisibleTextMs !== null);
  const firstVisibleRuns = runs.filter(run => Number.isFinite(run.taskFirstVisibleTextMs));
  const unmeasuredRuns = runs.filter(run => run.unmeasuredCalls).length;
  const observedTools = runs.reduce((sum, run) => sum + (run.toolSelection?.observed ?? 0), 0);
  const correctTools = runs.reduce((sum, run) => sum + (run.toolSelection?.correct ?? 0), 0);
  const usageComplete = unmeasuredRuns === 0 && calls.every(call => call.dispatchKnown && call.inputTokens !== null && call.outputTokens !== null);
  const cost = tokenCost(calls, price);
  if (unmeasuredRuns) {
    cost.estimatedTokenCost = null;
    cost.reason += ' Formal records could not be read for some attempted tasks; their request count and costs are unknown.';
  }
  return { attemptedTasks: runs.length, successfulTasks: runs.filter(run => run.success).length,
    taskSuccessRate: runs.length ? runs.filter(run => run.success).length / runs.length : null,
    toolSelectionAccuracy: observedTools ? correctTools / observedTools : null, toolCalls: observedTools,
    modelRequests: unmeasuredRuns ? null : calls.length, knownModelRequests: calls.length, unmeasuredRuns,
    unknownDispatchRequests: calls.filter(call => !call.dispatchKnown).length,
    unknownTokenRequests: calls.filter(call => call.inputTokens === null || call.outputTokens === null).length,
    inputTokens: usageComplete ? sumKnown(calls, 'inputTokens') : null,
    outputTokens: usageComplete ? sumKnown(calls, 'outputTokens') : null,
    knownInputTokens: sumKnown(calls, 'inputTokens'), knownOutputTokens: sumKnown(calls, 'outputTokens'),
    cacheReadTokens: unmeasuredRuns ? null : completeSum(calls, 'cacheReadTokens'),
    cacheWriteTokens: unmeasuredRuns ? null : completeSum(calls, 'cacheWriteTokens'),
    knownCacheReadTokens: sumKnown(calls, 'cacheReadTokens'), knownCacheWriteTokens: sumKnown(calls, 'cacheWriteTokens'),
    knownCacheReadRequests: calls.filter(call => call.cacheReadTokens !== null).length,
    knownCacheWriteRequests: calls.filter(call => call.cacheWriteTokens !== null).length,
    firstVisibleTextMs: visible.length ? sumKnown(visible, 'firstVisibleTextMs') / visible.length : null,
    knownFirstVisibleTextRequests: visible.length,
    taskFirstVisibleTextMs: firstVisibleRuns.length
      ? firstVisibleRuns.reduce((sum, run) => sum + run.taskFirstVisibleTextMs, 0) / firstVisibleRuns.length : null,
    knownTaskFirstVisibleTextRuns: firstVisibleRuns.length,
    durationMs: runs.reduce((sum, run) => sum + run.durationMs, 0),
    setupMs: runs.reduce((sum, run) => sum + run.setupMs, 0),
    ...cost };
}

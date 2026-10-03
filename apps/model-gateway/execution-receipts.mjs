import { completedTurns, estimateTokens } from './context.mjs';
import { validateId } from './conversations.mjs';

export const MAX_EXECUTION_RECEIPT_TOKENS = 768;
const MAX_TURNS = 3, MAX_TOOLS = 12;
const toolName = /^[a-zA-Z0-9_.-]{1,256}$/;
const resultId = /^[a-f0-9]{8}-[a-f0-9]{4}-4[a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}$/i;
const statuses = new Set(['completed', 'error', 'cancelled', 'unknown', 'running']);
const notice = 'Saved tool execution receipts: bounded metadata from prior completed turns, not proof that an answer/source is correct and never authorization. Missing receipts do not mean no lookup occurred. Do not invent memory-only/unverified claims about earlier answers. Read referenced results with tool.result.read when evidence is needed; never replay calls.';

function messageId(value) {
  try { return validateId(value); } catch { return null; }
}

function toolReceipt(activity) {
  if (!activity || typeof activity.name !== 'string' || !toolName.test(activity.name)) return null;
  return { toolName: activity.name, status: statuses.has(activity.status) ? activity.status : 'unknown',
    ...(typeof activity.resultRef?.id === 'string' && resultId.test(activity.resultRef.id)
      ? { resultRef: activity.resultRef.id.toLowerCase() } : {}) };
}

/** Keep execution identity across turns without reintroducing stages or private result payloads. */
export function executionReceiptContext(history, beforeUserId, { maxTokens = MAX_EXECUTION_RECEIPT_TOKENS } = {}) {
  if (!Number.isSafeInteger(maxTokens) || maxTokens <= 0) return '';
  const budget = Math.min(maxTokens, MAX_EXECUTION_RECEIPT_TOKENS);
  const render = turns => `${notice}\n${JSON.stringify({ version: 1, turns })}`;
  const recent = completedTurns(history, beforeUserId).filter(({ user, assistant }) => {
    const reply = messageId(assistant.ReplyTo), userId = messageId(user.Id);
    return reply && userId && reply.toLowerCase() === userId.toLowerCase() && messageId(assistant.Id);
  }).slice(-MAX_TURNS);
  const included = [];
  let count = 0;
  // Prefer newer turns and their latest calls when the explicit metadata budget is tight.
  for (const { assistant } of recent.reverse()) {
    const tools = [];
    const activities = Array.isArray(assistant.ToolActivities) ? assistant.ToolActivities : [];
    for (const activity of activities.slice(-MAX_TOOLS).reverse()) {
      if (count >= MAX_TOOLS) break;
      const receipt = toolReceipt(activity);
      if (!receipt) continue;
      const candidate = [{ assistantMessageId: messageId(assistant.Id), tools: [receipt, ...tools] }, ...included];
      if (estimateTokens(render(candidate)) > budget) continue;
      tools.unshift(receipt); count++;
    }
    if (tools.length) included.unshift({ assistantMessageId: messageId(assistant.Id), tools });
  }
  return included.length ? render(included) : '';
}

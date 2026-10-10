import { performance } from 'node:perf_hooks';
import { buildSemanticSummaryRequest, finalizeSemanticSummary } from '../models/semantic-summary.mjs';
import { ModelHistoryProjection } from '../models/model-history.mjs';
import { finalParts } from '../models/streaming.mjs';

/** One optional summary call, never a second intent router or a business-tool replay.
 * 可选摘要最多调用一次，不另建意图路由模型，也不重放业务工具；失败继续使用原文摘录。 */
export async function runSemanticSummary({ plan, connection, generate, repository, projectionOptions,
  cooldowns, signal, now = Date.now() }) {
  const key = plan.conversationId.toLowerCase();
  const previous = cooldowns.get(key);
  if (previous && previous.retryAfter > now) return { audit: { state: 'deferred', reason: 'failure-cooldown', modelCalls: 0 } };
  const started = performance.now();
  const audit = { state: 'requested', sourceHash: plan.sourceHash, coveredTurns: plan.coveredTurnCount,
    maxOutputTokens: plan.maxOutputTokens, modelCalls: 0, usageKnown: false };
  try {
    signal?.throwIfAborted();
    const raw = await generate(buildSemanticSummaryRequest(plan), () => { audit.modelCalls++; });
    const parts = finalParts(connection.protocol, raw);
    // Rejected navigation still consumed a model request; retain returned usage separately from checkpoint success.
    // 被拒绝的导航仍消耗模型请求，计量与检查点成功状态分开保存。
    audit.usage = Object.fromEntries(Object.entries(raw.usage ?? {}).filter(([name, value]) =>
      ['input_tokens', 'output_tokens', 'prompt_tokens', 'completion_tokens', 'total_tokens'].includes(name) &&
      Number.isSafeInteger(value) && value >= 0));
    audit.usageKnown = Object.keys(audit.usage).length > 0;
    signal?.throwIfAborted();
    const toolCalls = connection.protocol === 'anthropic-messages' ? raw.content?.filter(item => item.type === 'tool_use') :
      connection.protocol === 'openai-responses' ? raw.output?.filter(item => item.type === 'function_call') :
        raw.choices?.[0]?.message?.tool_calls;
    const response = { status: 'completed', finishReason: parts.finish ??
      (connection.protocol === 'openai-responses' ? raw.status : undefined), content: parts.content, toolCalls };
    const sourcesFor = history => new ModelHistoryProjection({ ...projectionOptions, history }).summarySources();
    const finalized = finalizeSemanticSummary({ plan, response, currentSourceTurns: sourcesFor(projectionOptions.history) });
    if (!finalized.value) throw Object.assign(new Error('Summary rejected'), { code: 'SUMMARY_REJECTED', reason: finalized.reason });
    await repository.writeSummary(plan.conversationId, finalized.value, {
      signal,
      // Recheck the same candidate inside the source lock before replacing a reusable checkpoint.
      // 在来源锁内复验同一候选，再替换可复用检查点；源变更时保留旧状态。
      validateCurrent: async history => {
        signal?.throwIfAborted();
        const current = finalizeSemanticSummary({ plan, response, currentSourceTurns: sourcesFor(history) });
        return current.value?.sourceHash === finalized.value.sourceHash && current.value?.contentHash === finalized.value.contentHash;
      }
    });
    audit.state = 'saved'; audit.contentTokens = finalized.contentTokens;
    cooldowns.delete(key);
    return { value: finalized.value, audit };
  } catch (error) {
    if (signal?.aborted) {
      audit.state = 'cancelled'; audit.reason = 'SUMMARY_CANCELLED';
      throw Object.assign(new Error(signal.reason instanceof Error ? signal.reason.message : 'Summary cancelled before publication'), { name: 'AbortError',
        code: 'SUMMARY_CANCELLED', semanticSummaryAudit: audit });
    }
    audit.state = 'fallback'; audit.reason = error.reason ?? (/^[A-Z0-9_]{1,100}$/u.test(error.code ?? '') ? error.code : 'SUMMARY_GENERATION_FAILED');
    cooldowns.delete(key); cooldowns.set(key, { retryAfter: now + 300000 });
    while (cooldowns.size > 64) cooldowns.delete(cooldowns.keys().next().value);
    return { audit };
  } finally { audit.durationMs = Math.max(0, Math.round(performance.now() - started)); }
}

import { createHash } from 'node:crypto';
import { StreamFailure } from './streaming.mjs';
import { estimateMessageTokens, estimateTokens } from './context.mjs';
import { appendToolResults } from './tool-protocols.mjs';

export function toolPolicyHash(context) {
  return createHash('sha256').update(JSON.stringify([context.permissionMode, context.workspaceRoot,
    context.projectId, 'broker-v1-appcontainer-workspace-copy'])).digest('hex');
}

/** Bounded model → broker → model loop. Persist before side effects and after each result. */
export async function runToolLoop({ protocol, messages, system, declarations, inputBudgetTokens,
  context, service, requestTurn, emit, saveActivity, onRoundComplete = () => {}, signal, interactive = false }) {
  const seenIds = new Set();
  let content = '', reasoning = '', callsRun = 0;
  const schemaCost = estimateTokens(JSON.stringify(declarations));
  for (let round = 0; round < 12; round++) {
    signal?.throwIfAborted();
    if (estimateMessageTokens([], system) + estimateTokens(JSON.stringify(messages)) + schemaCost > inputBudgetTokens)
      throw new StreamFailure('工具结果超过本次上下文预算，请缩小读取范围或提高模型上下文配置。', 'interrupted');
    const turn = await requestTurn(messages);
    content += turn.content; reasoning += turn.reasoning;
    onRoundComplete({ content, reasoning });
    if (!turn.calls.length) return { content, reasoning };
    const results = [];
    for (const call of turn.calls) {
      signal?.throwIfAborted();
      if (seenIds.has(call.id) || ++callsRun > 32) throw new StreamFailure('工具调用重复或超过本次执行上限。', 'interrupted');
      seenIds.add(call.id);
      const activity = { toolCallId: call.id, name: call.name, arguments: call.arguments,
        status: 'running', summary: call.name, workspaceRoot: context.workspaceRoot ?? null };
      await saveActivity(activity);
      emit({ type: 'tool_call', tool: activity });
      const result = await service.execute(context, call, { signal, interactive, emit: event => {
        // The approval token is ephemeral; only the call itself is durable.
        emit(event);
      } });
      signal?.throwIfAborted();
      if (typeof result.content !== 'string' || result.content.length > 65536)
        throw new StreamFailure('工具结果超过大小限制。', 'interrupted');
      const completed = { ...activity, status: result.isError ? 'error' : 'completed', result: result.content,
        ...(result.sandbox ? { sandbox: result.sandbox } : {}),
        ...(result.outsideWorkspace != null ? { outsideWorkspace: result.outsideWorkspace } : {}) };
      await saveActivity(completed);
      emit({ type: 'tool_result', tool: completed });
      results.push({ call, result });
    }
    messages = appendToolResults(protocol, messages, turn, results);
  }
  throw new StreamFailure('工具执行达到本次轮次上限，已保留执行记录。', 'interrupted');
}

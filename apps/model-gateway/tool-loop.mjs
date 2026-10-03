import { createHash } from 'node:crypto';
import { StreamFailure } from './streaming.mjs';
import { appendToolResults } from './tool-protocols.mjs';
import { ToolContextProjection } from './tool-context.mjs';
import { ToolRunProgress, runLimitFailure } from './tool-run.mjs';
import { AssistantSegments } from './assistant-segments.mjs';
import { canRunInParallel } from './tool-scheduling.mjs';

export function toolPolicyHash(context) {
  return createHash('sha256').update(JSON.stringify([context.permissionMode, context.workspaceRoot,
    context.projectId, 'broker-v1-appcontainer-workspace-copy'])).digest('hex');
}

/** Bounded model → broker → model loop. Persist before side effects and after each result. */
export async function runToolLoop({ protocol, messages, system, declarations, inputBudgetTokens,
  context, service, requestTurn, emit, saveActivity, onRoundComplete = () => {}, declarationsForRound, signal, interactive = false,
  historySources, onContextCompacted = () => {}, limits, saveRunState, saveModelRound = async () => {} }) {
  const seenIds = new Set();
  const projection = new ToolContextProjection({ protocol, messages, historySources, conversationId: context.conversationId });
  let callsRun = 0;
  const segments = new AssistantSegments(emit);
  const progress = new ToolRunProgress(limits, saveRunState);
  const deadline = AbortSignal.timeout(progress.limits.maxDurationMs);
  signal = signal ? AbortSignal.any([signal, deadline]) : deadline;
  try {
    for (let round = 0; round < progress.limits.maxRounds; round++) {
      signal?.throwIfAborted();
      progress.rounds = round + 1;
      segments.start(round + 1);
      await progress.save('model');
      const roundDeclarations = declarationsForRound?.() ?? declarations;
      const compacted = projection.compact(messages, { system, declarations: roundDeclarations, inputBudgetTokens });
      messages = compacted.messages;
      if (compacted.metrics) await onContextCompacted(compacted.metrics);
      const turn = await requestTurn(messages, roundDeclarations, signal, event => segments.receive(event));
      // Persist the decoded model step before executing its effects. Results remain owned by saveActivity.
      await saveModelRound({ round: round + 1, turn, messages, system, declarations: roundDeclarations });
      segments.finish(turn);
      await onRoundComplete({ content: segments.text(), reasoning: segments.reasoning() });
      progress.observeTurn(turn);
      if (!turn.calls.length) {
        await progress.save('finalizing');
        return { content: turn.content, reasoning: segments.reasoning(), assistantSegments: segments.snapshot(), toolStreamProtocol: 3 };
      }
      const results = [];
      let starts = Promise.resolve();
      const executeCall = async call => {
        signal?.throwIfAborted();
        if (seenIds.has(call.id)) throw new StreamFailure('工具调用 ID 重复，已停止执行。', 'interrupted');
        if (++callsRun > progress.limits.maxToolCalls) throw runLimitFailure('TOOL_RUN_CALL_LIMIT');
        seenIds.add(call.id);
        const activity = { toolCallId: call.id, name: call.name, arguments: call.arguments,
          status: 'running', summary: call.name, workspaceRoot: context.workspaceRoot ?? null,
          round: round + 1, order: segments.order++ };
        // Publish call starts in their assigned order, even when a storage callback is slow.
        const start = starts.then(async () => {
          await saveActivity(activity);
          progress.toolCalls = callsRun;
          await progress.save('tool', { toolCallId: call.id });
          emit({ type: 'tool_call', tool: activity });
        });
        starts = start;
        await start;
        const result = await service.execute(context, call, { signal, interactive, emit: event => {
          // The approval token is ephemeral; only the call itself is durable.
          emit({ ...event, ...(event.tool ? { tool: { ...event.tool, round: activity.round, order: activity.order } } : {}) });
        } });
        if (typeof result.content !== 'string' || result.content.length > 65536)
          throw new StreamFailure('工具结果超过大小限制。', 'interrupted');
        const completed = { ...activity, status: result.status ?? (result.code === 'TOOL_CANCELLED' ? 'cancelled' : result.isError ? 'error' : 'completed'), result: result.content,
          ...(result.resultRef ? { resultRef: result.resultRef } : {}), ...(result.code ? { code: result.code } : {}),
          ...(result.sandbox ? { sandbox: result.sandbox } : {}),
          ...(result.outsideWorkspace != null ? { outsideWorkspace: result.outsideWorkspace } : {}) };
        await saveActivity(completed);
        emit({ type: 'tool_result', tool: completed });
        await progress.save('continuing', { toolCallId: call.id });
        if (['AGENT_CONFIG_CHANGED', 'MCP_CATALOG_CHANGED'].includes(result.code))
          throw Object.assign(new StreamFailure(result.content, 'interrupted'), { code: result.code });
        // Once execution returned, record its known outcome before honoring stop.
        // The same cancellation prevents subsequent effects, never this receipt.
        signal?.throwIfAborted();
        return { call, result };
      };
      for (let index = 0; index < turn.calls.length;) {
        let end = index + 1;
        if (canRunInParallel(turn.calls[index])) {
          while (end < turn.calls.length && end - index < 4 && canRunInParallel(turn.calls[end])) end++;
        }
        // Unknown tools and effects stay serial. Every accepted read still passes the same permission broker.
        // Settle the whole batch before failing so cancellation never drops a finished receipt.
        const batch = await Promise.allSettled(turn.calls.slice(index, end).map(executeCall));
        const failed = batch.find(item => item.status === 'rejected');
        if (failed) throw failed.reason;
        results.push(...batch.map(item => item.value));
        index = end;
      }
      messages = appendToolResults(protocol, messages, turn, results,
        { onResult: (message, pair) => projection.observeResult(message, pair, round) });
    }
    throw runLimitFailure('TOOL_RUN_ROUND_LIMIT');
  } catch (error) {
    segments.interrupt();
    if (deadline.aborted && signal.reason === deadline.reason && ['AbortError', 'TimeoutError'].includes(error?.name))
      error = runLimitFailure('TOOL_RUN_TIME_LIMIT');
    await progress.save('interrupted', { code: error.code ?? null });
    throw error;
  }
}

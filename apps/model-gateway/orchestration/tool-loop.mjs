import { createHash } from 'node:crypto';
import { StreamFailure } from '../models/streaming.mjs';
import { appendToolResults } from '../models/tool-protocols.mjs';
import { ToolContextProjection } from '../models/tool-context.mjs';
import { ToolRunProgress, runLimitFailure, startRunTimer } from './tool-run.mjs';
import { AssistantSegments } from '../platform/assistant-segments.mjs';
import { canRunInParallel } from '../tools/tool-scheduling.mjs';
import { ToolProgressGuard, ToolReadFailureGuard } from '../tools/tool-observations.mjs';
import { isDesktopObservation } from '../tools/tool-outcomes.mjs';

export function toolPolicyHash(context) {
  return createHash('sha256').update(JSON.stringify([context.permissionMode, context.workspaceRoot,
    context.projectId, 'broker-v1-appcontainer-workspace-copy'])).digest('hex');
}

/**
 * Bounded model → broker → model loop. Persist before side effects and after each result.
 * 模型到代理再到模型的循环有明确上限，副作用前及每次结果返回后都先持久化。
 */
export async function runToolLoop({ protocol, messages, system, declarations, inputBudgetTokens,
  context, service, requestTurn, emit, saveActivity, onRoundComplete = () => {}, declarationsForRound, signal, interactive = false,
  historySources, onContextCompacted = () => {}, limits, saveRunState, saveModelRound = async () => {} }) {
  const seenIds = new Set();
  const projection = new ToolContextProjection({ protocol, messages, historySources, conversationId: context.conversationId });
  let callsRun = 0;
  const segments = new AssistantSegments(emit);
  const progress = new ToolRunProgress(limits, saveRunState);
  const observations = new ToolProgressGuard();
  const readFailures = new ToolReadFailureGuard();
  let summarizeOnly = false;
  const deadline = AbortSignal.timeout(progress.limits.maxDurationMs);
  signal = signal ? AbortSignal.any([signal, deadline]) : deadline;
  try {
    for (let round = 0; round < progress.limits.maxRounds; round++) {
      signal?.throwIfAborted();
      progress.rounds = round + 1;
      segments.start(round + 1);
      await progress.save('model');
      const roundDeclarations = summarizeOnly ? [] : declarationsForRound?.() ?? declarations;
      const compacted = projection.compact(messages, { system, declarations: roundDeclarations, inputBudgetTokens });
      messages = compacted.messages;
      if (compacted.metrics) await onContextCompacted(compacted.metrics);
      const modelElapsed = startRunTimer();
      let turn;
      try { turn = await requestTurn(messages, roundDeclarations, signal, event => segments.receive(event)); }
      finally { progress.recordModel(modelElapsed()); }
      // Persist the decoded model step before executing its effects. Results remain owned by saveActivity.
      // 执行副作用前先保存已解码模型步骤，结果仍由 saveActivity 负责保存。
      await saveModelRound({ round: round + 1, turn, messages, system, declarations: roundDeclarations });
      segments.finish(turn);
      await onRoundComplete({ content: segments.text(), reasoning: segments.reasoning() });
      progress.observeTurn(turn);
      if (summarizeOnly && turn.calls.length)
        throw Object.assign(new StreamFailure('连续读取没有新增信息，已停止重复调用并保留已有结果。请调整查询或补充条件。', 'interrupted'), { code: 'TOOL_RUN_NO_PROGRESS' });
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
        // 即使存储回调较慢，仍按已分配顺序发布调用开始事件。
        const start = starts.then(async () => {
          await saveActivity(activity);
          progress.toolCalls = callsRun;
          await progress.save('tool', { toolCallId: call.id });
          emit({ type: 'tool_call', tool: activity });
        });
        starts = start;
        await start;
        const toolElapsed = startRunTimer();
        let approvalMs = 0, result;
        try {
          result = await service.execute(context, call, { signal, interactive,
            onApprovalWait: durationMs => { approvalMs += durationMs; }, emit: event => {
              // The approval token is ephemeral; only the call itself is durable.
              // 审批令牌是临时数据，只持久化调用本身。
              emit({ ...event, ...(event.tool ? { tool: { ...event.tool, round: activity.round, order: activity.order } } : {}) });
            } });
        }
        finally { progress.recordTool({ id: call.id, round: round + 1, durationMs: toolElapsed(), approvalMs, reused: result?.reused === true }); }
        if (isDesktopObservation(call.name) && result.status === 'unknown')
          result = { ...result, status: result.code === 'TOOL_CANCELLED' ? 'cancelled' : 'error' };
        if (typeof result.content !== 'string' || result.content.length > 65536)
          throw new StreamFailure('工具结果超过大小限制。', 'interrupted');
        const completed = { ...activity, status: result.status ?? (result.code === 'TOOL_CANCELLED' ? 'cancelled' : result.isError ? 'error' : 'completed'), result: result.content,
          ...(result.resultRef ? { resultRef: result.resultRef } : {}), ...(result.code ? { code: result.code } : {}),
          ...(result.sandbox ? { sandbox: result.sandbox } : {}),
          ...(result.outsideWorkspace != null ? { outsideWorkspace: result.outsideWorkspace } : {}) };
        if (result.reused) { completed.reused = true; completed.observationCapturedAt = result.observationCapturedAt; }
        await saveActivity(completed);
        emit({ type: 'tool_result', tool: completed });
        await progress.save('continuing', { toolCallId: call.id });
        if (['AGENT_CONFIG_CHANGED', 'MCP_CATALOG_CHANGED'].includes(result.code))
          throw Object.assign(new StreamFailure(result.content, 'interrupted'), { code: result.code });
        if (call.name === 'terminal.host.run' && result.status === 'unknown')
          throw Object.assign(new StreamFailure('本机命令结果尚未确认，已保留执行记录。请核验已执行的操作后再继续。', 'interrupted'),
            { code: 'HOST_TERMINAL_OUTCOME_UNKNOWN' });
        if (call.name.startsWith('computer.') && !isDesktopObservation(call.name) && result.status === 'unknown')
          throw Object.assign(new StreamFailure('本机操作结果尚未确认，已保留执行记录。请检查窗口状态后再继续。', 'interrupted'),
            { code: 'DESKTOP_OUTCOME_UNKNOWN' });
        if (call.name.startsWith('mcp.') && result.status === 'unknown')
          throw Object.assign(new StreamFailure('浏览器或外部工具操作结果尚未确认，已保留执行记录。请核验页面状态后再继续。', 'interrupted'),
            { code: 'MCP_OUTCOME_UNKNOWN' });
        // Once execution returned, record its known outcome before honoring stop.
        // The same cancellation prevents subsequent effects, never this receipt.
        // 执行返回后先保存已知结果，再处理停止信号；取消阻止后续副作用，不能丢失本次回执。
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
        // 未知工具和有副作用操作保持串行，允许并发的读取仍经过相同权限代理；整批全部结算后再报告失败，取消也不能丢失已完成回执。
        const batch = await Promise.allSettled(turn.calls.slice(index, end).map(executeCall));
        const failed = batch.find(item => item.status === 'rejected');
        if (failed) throw failed.reason;
        results.push(...batch.map(item => item.value));
        index = end;
      }
      messages = appendToolResults(protocol, messages, turn, results,
        { onResult: (message, pair) => projection.observeResult(message, pair, round) });
      const state = observations.observeRound(results);
      const failures = readFailures.observeRound(results);
      if (state.repeated) progress.observeNoProgress();
      if (failures.repeated) progress.observeNoProgress();
      if (failures.warning || failures.finalize) {
        summarizeOnly = failures.finalize;
        messages.push({ role: 'user', content: failures.finalize
          ? '[KYNXA_READ_FAILURE_FINAL] The same observation target failed three times. Tools are disabled for this final response. Explain the concrete blocker and preserve verified findings. Do not claim that the page was read, the task completed, or the user cancelled. The conversation and saved history remain available.'
          : '[KYNXA_READ_FAILURE_WARNING] Reading the same target failed twice. Change the connection, use browser DOM/frames instead of desktop UIA, or report the blocker. Do not repeat the same failing read or reactivate the browser just to retry it. This is runtime guidance, not a new user task.' });
      } else if (state.warning || state.finalize) {
        summarizeOnly = state.finalize;
        messages.push({ role: 'user', content: state.finalize
          ? '[KYNXA_NO_PROGRESS_FINAL] Repeated successful read/search calls produced no new observations. Tools are disabled for this final response. Answer the original task using the existing evidence and actual source URLs. If unresolved, state the specific missing evidence or blocker. Do not claim unsupported completion or repeat the process.'
          : '[KYNXA_NO_PROGRESS_WARNING] Repeated successful read/search calls produced no new observations. Reassess the original task: answer now if evidence is sufficient; otherwise change the query, page, or approach to obtain genuinely new evidence. Repeating the same observations will end tool execution. This is runtime guidance, not a new user task.' });
      }
      await progress.save('continuing', { toolCallId: results.at(-1)?.call.id ?? null });
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

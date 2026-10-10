import assert from 'node:assert/strict';
import { test } from 'node:test';
import { setTimeout as delay } from 'node:timers/promises';
import { DEFAULT_TOOL_RUN_LIMITS, ToolRunProgress, startRunTimer } from '../orchestration/tool-run.mjs';
import { runToolLoop } from '../orchestration/tool-loop.mjs';
import { validateAssistantSegments } from '../platform/assistant-segments.mjs';

// Synthetic broker snapshots exercise the version contract, not real file hashing or repository-wide coverage.
// 合成代理快照只验证版本合同，不冒充真实文件哈希计算或全仓覆盖验收。
function codeVersionSnapshot(signature = 'a'.repeat(64)) {
  return { signature, complete: true, files: [{ pathId: 'b'.repeat(64), state: 'present', sha256: signature }],
    coverage: 'observed-filesystem-targets', exhaustive: false };
}

test('a draft followed by required validation does not seal final segments before the task finishes', async () => {
  const events = [], activities = [], checks = [];
  const turns = [
    { content: 'Changing the file.', calls: [{ id: 'edit-1', name: 'filesystem.edit', arguments: {} }] },
    { content: 'Draft before verification.', calls: [] },
    { content: 'Running the relevant check.', calls: [{ id: 'check-1', name: 'terminal.host.run', arguments: { script: 'node --test check.mjs' } }] },
    { content: 'Verified final result.', calls: [] }
  ].map(turn => ({ ...turn, reasoning: '', continuation: [{ role: 'assistant', content: turn.content,
    ...(turn.calls.length ? { tool_calls: turn.calls.map(call => ({ id: call.id, type: 'function',
      function: { name: call.name, arguments: JSON.stringify(call.arguments) } })) } : {}) }] }));
  const result = await runToolLoop({ protocol: 'openai-completions', context: { message: '修复代码并验证' },
    messages: [], system: '', declarations: [], inputBudgetTokens: 32768,
    service: { captureTaskCodeVersion: async () => codeVersionSnapshot(), execute: async (_context, call) => {
      checks.push(call.id);
      return { status: 'completed', isError: false, content: JSON.stringify({ exitCode: 0 }) };
    } }, requestTurn: async () => turns.shift(), emit: event => events.push(event),
    saveActivity: async activity => activities.push({ ...activity }) });
  assert.equal(result.content, 'Verified final result.');
  assert.equal(result.taskCompletion.pendingValidation, false);
  assert.deepEqual(checks, ['edit-1', 'check-1']);
  assert.deepEqual(validateAssistantSegments(result.assistantSegments).map(segment => segment.phase),
    ['commentary', 'commentary', 'commentary', 'final_answer']);
  assert.ok(events.some(event => event.segment?.content === 'Draft before verification.' && event.segment.phase === 'commentary'));
  assert.equal(activities.filter(activity => activity.status === 'completed').length, 2);
});

test('validation records require executed commands and a later edit invalidates earlier passed checks', async () => {
  const states = [], progress = new ToolRunProgress(undefined, state => states.push(state));
  const versions = { beforeVersion: codeVersionSnapshot(), afterVersion: codeVersionSnapshot() };
  const receipt = (exitCode, extra = {}) => ({ status: 'completed', isError: exitCode !== 0,
    content: JSON.stringify({ exitCode, ...extra }) });
  progress.observeOutcome({ id: 'write-1', name: 'filesystem.write', arguments: {} }, receipt(0));
  progress.observeOutcome({ id: 'source-only', name: 'knowledge.search', arguments: {} }, receipt(0));
  progress.observeOutcome({ id: 'not-a-test', name: 'terminal.host.run', arguments: { command: 'echo npm test' } }, receipt(0));
  await progress.save('continuing');
  assert.equal(states.at(-1).verification.pendingValidation, true);
  assert.deepEqual(states.at(-1).verification.receipts, []);
  progress.observeOutcome({ id: 'failed-test', name: 'terminal.host.run', arguments: { command: 'npm test' } }, receipt(1));
  assert.equal(progress.checkedMutationRevision, 0);
  progress.observeOutcome({ id: 'unbound-test', name: 'terminal.host.run', arguments: { command: 'node --test test.mjs' } }, receipt(0));
  assert.equal(progress.verification().receipts.at(-1).passed, true);
  assert.equal(progress.verification().receipts.at(-1).coversObservedRevision, false);
  assert.equal(progress.verification().state, 'checks-version-unverified');
  assert.equal(progress.verification().pendingValidation, true);
  assert.equal(progress.needsValidation('修复代码'), false, 'missing hash coverage alone must not repeat a passing command');
  progress.observeOutcome({ id: 'passed-test', name: 'terminal.host.run', arguments: { command: 'node --test test.mjs' } }, receipt(0), versions);
  await progress.save('continuing');
  assert.equal(states.at(-1).verification.pendingValidation, false);
  assert.equal(states.at(-1).verification.receipts.length, 3);
  assert.equal(states.at(-1).verification.codeVersionCoverage.actualFileHashesVerified, true);
  assert.equal(states.at(-1).verification.codeVersionCoverage.wholeRepositoryCertified, false);
  assert.equal(states.at(-1).verification.conclusion, 'task-correctness-not-certified');
  progress.observeOutcome({ id: 'write-2', name: 'filesystem.edit', arguments: {} }, receipt(0));
  await progress.save('finalizing');
  assert.equal(states.at(-1).verification.pendingValidation, true);
  progress.observeOutcome({ id: 'cmd-validation', name: 'terminal.run', arguments: { command: 'cmd', args: ['/d', '/c', 'node --test check.mjs'] } },
    receipt(0, { sandbox: 'appcontainer', tokenVerified: true, workspaceCopy: true }), versions);
  assert.equal(progress.verification().receipts.at(-1).passed, true);
  assert.equal(progress.verification().receipts.at(-1).codeVersionCoverage.sandboxSnapshotVersion, 'unverified');
  assert.equal(progress.verification().pendingValidation, true, 'host hashes cannot certify the starting version of a sandbox copy');
  progress.observeOutcome({ id: 'current-host-test', name: 'terminal.host.run', arguments: { command: 'node --test check.mjs' } }, receipt(0), versions);
  assert.equal(progress.verification().pendingValidation, false);
  progress.observeFinalCodeVersion(codeVersionSnapshot('c'.repeat(64)));
  assert.equal(progress.verification().pendingValidation, true, 'an external change invalidates the earlier host check');
  assert.equal(progress.verification().codeVersionCoverage.actualFileHashesVerified, false);
  progress.observeOutcome({ id: 'latest-failure', name: 'terminal.host.run', arguments: { script: 'node --test check.mjs' } }, receipt(1));
  assert.equal(progress.verification().state, 'checks-failed');
  assert.equal(progress.verification().pendingValidation, true);
});

test('unknown, cancelled and timed-out operations cannot retain a prior version-bound pass', () => {
  const versions = { beforeVersion: codeVersionSnapshot(), afterVersion: codeVersionSnapshot() };
  const check = { id: 'initial-check', name: 'terminal.host.run', arguments: { script: 'node --test check.mjs' } };
  const passing = { status: 'completed', isError: false, content: JSON.stringify({ exitCode: 0 }) };
  const outcomes = [
    { status: 'unknown', code: 'TOOL_OUTCOME_UNKNOWN', expected: 'unknown', output: {} },
    { status: 'cancelled', code: 'TOOL_CANCELLED', expected: 'cancelled', output: { exitCode: 0, cancelled: true } },
    { status: 'error', code: 'TOOL_TIMED_OUT', expected: 'timed-out', output: { exitCode: 0, timedOut: true } }
  ];
  for (const outcome of outcomes) {
    const progress = new ToolRunProgress();
    progress.observeOutcome(check, passing, versions);
    assert.equal(progress.verification().pendingValidation, false);
    progress.observeOutcome({ ...check, id: 'interrupted-check' }, { status: outcome.status, code: outcome.code,
      isError: true, content: JSON.stringify(outcome.output) }, versions);
    const verification = progress.verification();
    assert.equal(verification.receipts.at(-1).status, outcome.expected);
    assert.notEqual(verification.receipts.at(-1).status, 'failed', 'missing or interrupted execution is not an assertion failure');
    assert.equal(verification.pendingValidation, true);
    assert.equal(verification.state, 'execution-unconfirmed');
    assert.equal(verification.codeVersionCoverage.actualFileHashesVerified, false);
    assert.equal(progress.needsValidation('修复代码'), false, 'unconfirmed effects require state verification rather than a replay');
  }
  for (const status of ['unknown', 'cancelled']) {
    const progress = new ToolRunProgress();
    progress.observeOutcome(check, passing, versions);
    progress.observeOutcome({ id: 'uncertain-write', name: 'filesystem.edit', arguments: {} },
      { status, isError: true, content: 'Write outcome is unconfirmed.' });
    assert.equal(progress.verification().state, 'execution-unconfirmed');
    assert.equal(progress.verification().codeVersionCoverage.actualFileHashesVerified, false);
  }
});

test('numeric diagnostics distinguish model, execution and approval spans without treating parallel totals as wall time', async () => {
  const saved = [], progress = new ToolRunProgress(undefined, state => saved.push(state));
  progress.rounds = 1; progress.recordModel(100);
  progress.recordTool({ id: 'call-a', round: 1, durationMs: 120, approvalMs: 90 });
  progress.recordTool({ id: 'call-b', round: 1, durationMs: 80 });
  progress.recordTool({ id: 'call-c', round: 1, durationMs: 2, reused: true });
  progress.recordModel(150, 2); progress.observeNoProgress(); progress.observeNoProgress(2);
  assert.equal(saved.length, 0, 'recording does not cause extra or per-token writes');
  await progress.save('continuing');
  assert.deepEqual(saved[0].diagnostics, { version: 1, totalModelMs: 250, totalToolMs: 112, totalApprovalWaitMs: 90,
    maxModelMs: 150, maxToolMs: 80, maxApprovalWaitMs: 90, modelCalls: 2, executedToolCalls: 2,
    reusedToolCalls: 1, noProgressRounds: 3, modelRounds: [{ round: 1, durationMs: 100 }, { round: 2, durationMs: 150 }],
    toolCallsTiming: [{ toolCallId: 'call-a', round: 1, durationMs: 120, approvalMs: 90, reused: false },
      { toolCallId: 'call-b', round: 1, durationMs: 80, approvalMs: 0, reused: false },
      { toolCallId: 'call-c', round: 1, durationMs: 2, approvalMs: 0, reused: true }] });
  assert.equal(saved[0].version, 1); assert.deepEqual(saved[0].limits, DEFAULT_TOOL_RUN_LIMITS);
});

test('bounded timing lists keep the latest entries while total counters include all calls', async () => {
  const saved = [], progress = new ToolRunProgress(undefined, state => saved.push(state));
  for (let round = 1; round <= 128; round++) progress.recordModel(round, round);
  for (let index = 0; index < 512; index++) progress.recordTool({ id: 'call-' + index, round: index % 128 + 1, durationMs: 1 });
  await progress.save('finalizing'); const diagnostics = saved[0].diagnostics;
  assert.equal(diagnostics.modelCalls, 128); assert.equal(diagnostics.totalModelMs, 8256);
  assert.equal(diagnostics.modelRounds.length, 64); assert.equal(diagnostics.modelRounds[0].round, 65);
  assert.equal(diagnostics.executedToolCalls, 512); assert.equal(diagnostics.totalToolMs, 512);
  assert.equal(diagnostics.toolCallsTiming.length, 256); assert.equal(diagnostics.toolCallsTiming[0].toolCallId, 'call-256');
  progress.recordModel(1, 128);
  assert.equal(diagnostics.modelCalls, 128, 'published diagnostics snapshots remain immutable after recording');
  diagnostics.modelRounds[0].durationMs = 9999;
  assert.equal(progress.diagnostics.modelRounds[0].durationMs, 66, 'published lists cannot mutate ongoing run metrics');
});

test('timing records whitelist fields rather than persist private inputs or tool result text', async () => {
  const saved = [], progress = new ToolRunProgress(undefined, state => saved.push(state));
  progress.recordTool({ id: 'formal-call-id', round: 1, durationMs: 5, name: 'PRIVATE_TOOL_NAME',
    url: 'https://private.invalid', arguments: { apiKey: 'PRIVATE_KEY' }, result: 'PRIVATE_RESULT', _meta: 'PRIVATE_META' });
  await progress.save('tool');
  assert.doesNotMatch(JSON.stringify(saved[0].diagnostics), /PRIVATE_|https:|arguments|name|_meta|result/);
});

test('invalid metrics and arithmetic overflow fail before changing diagnostics', () => {
  const progress = new ToolRunProgress(); progress.rounds = 1;
  const before = JSON.stringify(progress.diagnostics);
  for (const duration of [-1, .5, NaN, Infinity, '1', Number.MAX_SAFE_INTEGER + 1]) {
    assert.throws(() => progress.recordModel(duration), { code: 'INVALID_TOOL_RUN_DIAGNOSTICS' });
    assert.throws(() => progress.recordTool({ id: 'call', round: 1, durationMs: duration }), { code: 'INVALID_TOOL_RUN_DIAGNOSTICS' });
  }
  for (const round of [0, 129, '1', 1.5]) assert.throws(() => progress.recordModel(1, round), { code: 'INVALID_TOOL_RUN_DIAGNOSTICS' });
  for (const input of [{ id: '', round: 1, durationMs: 1 }, { id: 'bad\0id', round: 1, durationMs: 1 },
    { id: 'call', round: 1, durationMs: 1, approvalMs: 2 }, { id: 'call', round: 1, durationMs: 1, reused: 'yes' }])
    assert.throws(() => progress.recordTool(input), { code: 'INVALID_TOOL_RUN_DIAGNOSTICS' });
  assert.throws(() => progress.observeNoProgress(-1), { code: 'INVALID_TOOL_RUN_DIAGNOSTICS' });
  assert.equal(JSON.stringify(progress.diagnostics), before);
  progress.recordModel(Number.MAX_SAFE_INTEGER); const huge = JSON.stringify(progress.diagnostics);
  assert.throws(() => progress.recordModel(1), { code: 'INVALID_TOOL_RUN_DIAGNOSTICS' });
  assert.equal(JSON.stringify(progress.diagnostics), huge);
});

test('the elapsed timer stays monotonic when wall time moves backwards', async t => {
  const timer = startRunTimer();
  let wallTime = 100000; t.mock.method(Date, 'now', () => wallTime);
  await delay(15); const first = timer(); wallTime = -100000;
  await delay(15); const second = timer();
  assert.ok(Number.isSafeInteger(first) && first >= 1); assert.ok(second >= first);
});

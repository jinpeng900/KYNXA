import assert from 'node:assert/strict';
import { test } from 'node:test';
import { setTimeout as delay } from 'node:timers/promises';
import { DEFAULT_TOOL_RUN_LIMITS, ToolRunProgress, startRunTimer } from '../orchestration/tool-run.mjs';

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

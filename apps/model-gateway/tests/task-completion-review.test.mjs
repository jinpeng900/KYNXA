import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { test } from 'node:test';
import { runToolLoop } from '../orchestration/tool-loop.mjs';
import { ToolCallDecodeFailure } from '../models/tool-call-validation.mjs';

const reviewMarker = '[KYNXA_TASK_COMPLETION_REVIEW]';
const originalGoal = '完成原始任务的全部三部分，不要让我回复继续。';

function footer(state, fields = {}) {
  return `<kynxa_task>${JSON.stringify({ state, ...fields })}</kynxa_task>`;
}

function textTurn(content, fields = {}) {
  return { content, reasoning: '', calls: [], finish: 'stop', ...fields };
}

// Use isolated in-memory archives and request adapters; no real model, process, or user data is touched.
// 使用隔离的内存归档与请求适配器，不调用真实模型、进程或用户数据。
async function runScenario(plan, { signal, events = [], states = [], onExecute = () => {}, limits = {} } = {}) {
  const requests = [];
  const results = { save: async (_context, _call, value) => ({ id: randomUUID(),
    bytes: Buffer.byteLength(JSON.stringify(value)), sha256: 'a'.repeat(64) }) };
  const result = await runToolLoop({ protocol: 'openai-completions',
    messages: [{ role: 'user', content: originalGoal }], declarations: [], inputBudgetTokens: 6000,
    context: { conversationId: randomUUID(), message: originalGoal }, signal,
    limits: { maxRounds: 12, maxDurationMs: 5000, ...limits },
    service: { results, execute: async call => { onExecute(call); return { status: 'completed', content: 'Saved.' }; } },
    emit: event => events.push(event), saveActivity: async () => {},
    saveRunState: async state => states.push(structuredClone(state)),
    requestTurn: async (messages, declarations, _signal, receive) => {
      const isReview = messages.at(-1)?.role === 'user' && messages.at(-1).content.includes(reviewMarker);
      const request = { index: requests.length + 1, isReview, messages, declarations };
      requests.push(request);
      const turn = await plan(request);
      if (turn.content) receive({ type: 'text_delta', delta: turn.content });
      return turn;
    } });
  return { result, requests, states, events };
}

function checkpointMetrics(messages) {
  for (const message of messages) {
    if (typeof message.content !== 'string') continue;
    let checkpoint;
    try { checkpoint = JSON.parse(message.content); } catch { continue; }
    if (checkpoint?.outputMetrics) return checkpoint.outputMetrics;
  }
  return null;
}

for (const ending of ['missing-state', 'false-complete']) {
  test(`an active task with ${ending} is reviewed against the original goal and resumes before sealing`, async () => {
    let reviews = 0;
    const { result, requests, states, events } = await runScenario(({ index, isReview, messages, declarations }) => {
      if (isReview) {
        reviews++;
        assert.deepEqual(declarations, [], 'completion review cannot grant executable tools');
        assert.ok(messages.some(message => message.role === 'user' && message.content === originalGoal));
        const metrics = checkpointMetrics(messages);
        assert.ok(metrics, 'the review receives actual output metrics');
        assert.equal(metrics.hanCharacters, reviews === 1 ? 4 : 6);
        assert.ok(Number.isSafeInteger(metrics.unicodeCharacters));
        assert.ok(Number.isSafeInteger(metrics.lineBreaks));
        return textTurn(reviews === 1
          ? footer('continue', { next: '完成尚未写出的第三部分' })
          : footer('complete', { reason: '原始要求的三部分已经全部交付' }));
      }
      if (index === 1) return textTurn(`甲乙。\n${footer('continue', { next: '继续完成第二部分' })}`);
      if (index === 2) return textTurn('丙丁。' + (ending === 'false-complete'
        ? `\n${footer('complete', { reason: '模型先前误认为全部完成' })}` : ''));
      assert.equal(index, 4);
      return textTurn(`戊己。\n${footer('complete', { reason: '第三部分已经写出' })}`);
    });
    assert.equal(requests.length, 5);
    assert.equal(reviews, 2);
    assert.equal(result.content, '甲乙。\n\n丙丁。\n\n戊己。');
    assert.notEqual(result.completionStatus, 'interrupted');
    assert.equal(states.at(-1).diagnostics.modelCalls, 5);
    assert.equal(states.at(-1).taskState.completionReview.outputMetrics.hanCharacters, 6);
    assert.equal(states.at(-1).taskState.completionReview.correctnessCertified, false);
    assert.ok(!JSON.stringify(events).includes('原始要求的三部分已经全部交付'));
  });
}

test('a native output-limit resume also requires explicit completion review after a normal stop', async () => {
  const { result, requests } = await runScenario(({ index, isReview }) => {
    if (index === 1) return textTurn('甲乙', { finish: 'length', outputTruncated: true });
    if (index === 2) return textTurn('丙丁');
    assert.equal(index, 3);
    assert.ok(isReview);
    return textTurn(footer('complete', { reason: '原始任务要求的文本已经完成' }));
  });
  assert.equal(requests.length, 3);
  assert.equal(result.content, '甲乙丙丁');
  assert.notEqual(result.completionStatus, 'interrupted');
});

test('the total round budget preserves a terminal draft when no review request fits', async () => {
  const { result, requests, states } = await runScenario(({ index, isReview }) => {
    assert.ok(!isReview);
    return textTurn(index === 1 ? `甲乙。\n${footer('continue', { next: '补充第二部分' })}` : '丙丁。');
  }, { limits: { maxRounds: 2 } });
  assert.equal(requests.length, 2);
  assert.equal(result.completionStatus, 'interrupted');
  for (const body of ['甲乙。', '丙丁。']) assert.equal(result.content.split(body).length - 1, 1);
  assert.equal(states.at(-1).taskState.reviewRequests, 0);
});

test('a real blocker identified during review preserves the aggregate and reports the missing requirement', async () => {
  const { result, requests } = await runScenario(({ index, isReview }) => {
    if (index === 1) return textTurn(`甲乙。\n${footer('continue', { next: '补充第二部分' })}`);
    if (index === 2) return textTurn('丙丁。');
    assert.ok(isReview);
    return textTurn(footer('blocked', { reason: '第三部分依赖的资料尚未提供' }));
  });
  assert.equal(requests.length, 3);
  assert.equal(result.completionStatus, 'interrupted');
  assert.ok(result.content.includes('第三部分依赖的资料尚未提供'));
  for (const body of ['甲乙。', '丙丁。']) assert.equal(result.content.split(body).length - 1, 1);
});

test('empty decoded review failures consume the same bounded attempts and cannot dispatch tools', async () => {
  const { result, requests, states } = await runScenario(({ index, isReview }) => {
    if (index === 1) return textTurn(`甲乙。\n${footer('continue', { next: '补充第二部分' })}`);
    if (index === 2) return textTurn('丙丁。');
    assert.ok(isReview);
    throw new ToolCallDecodeFailure('empty review', 'MODEL_RESPONSE_EMPTY');
  });
  assert.equal(requests.length, 4);
  assert.equal(result.completionStatus, 'interrupted');
  assert.equal(states.at(-1).diagnostics.modelCalls, 4);
  for (const body of ['甲乙。', '丙丁。']) assert.equal(result.content.split(body).length - 1, 1);
});

test('two missing or invalid review verdicts stop with the preserved draft and complete call accounting', async () => {
  const { result, requests, states, events } = await runScenario(({ index, isReview }) => {
    if (index === 1) return textTurn(`甲乙。\n${footer('continue', { next: '补充第二部分' })}`);
    if (index === 2) return textTurn('丙丁。');
    assert.ok(isReview);
    return textTurn(index === 3 ? '不应混入正文的核对说明。' : footer('complete'));
  });
  assert.equal(requests.length, 4);
  assert.equal(result.completionStatus, 'interrupted');
  for (const body of ['甲乙。', '丙丁。']) assert.equal(result.content.split(body).length - 1, 1);
  assert.ok(!result.content.includes('不应混入正文的核对说明'));
  assert.ok(!result.content.includes('kynxa_task'));
  assert.ok(!JSON.stringify(events).includes('不应混入正文的核对说明'));
  assert.equal(states.at(-1).diagnostics.modelCalls, 4);
  assert.ok(states.at(-1).estimatedGeneratedTokens > 0);
});

test('injected tool calls during completion review never dispatch a write and consume a bounded repair attempt', async () => {
  let executions = 0;
  const { result, requests, states } = await runScenario(({ index, isReview, declarations }) => {
    if (index === 1) return textTurn(`甲乙。\n${footer('continue', { next: '补充第二部分' })}`);
    if (index === 2) return textTurn('丙丁。');
    assert.ok(isReview);
    assert.deepEqual(declarations, []);
    if (index === 3) return textTurn('', { finish: 'tool_calls', calls: [{ id: 'review-write',
      name: 'filesystem.write', arguments: { path: 'note.txt', content: 'unexpected', expectedHash: null } }] });
    assert.equal(index, 4);
    return textTurn(footer('complete', { reason: '原始内容已经完整，核对不需要写入文件' }));
  }, { onExecute: () => { executions++; } });
  assert.equal(requests.length, 4);
  assert.equal(executions, 0);
  assert.equal(states.at(-1).diagnostics.executedToolCalls, 0);
  assert.equal(result.content, '甲乙。\n\n丙丁。');
});

test('cancellation during completion review prevents any following model request or operation', async () => {
  const controller = new AbortController();
  let requests = 0, executions = 0;
  await assert.rejects(runScenario(({ index, isReview }) => {
    requests++;
    if (index === 1) return textTurn(`甲乙。\n${footer('continue', { next: '补充第二部分' })}`);
    if (index === 2) return textTurn('丙丁。');
    assert.ok(isReview);
    controller.abort();
    return textTurn(footer('continue', { next: '不应执行的后续步骤' }));
  }, { signal: controller.signal, onExecute: () => { executions++; } }), { name: 'AbortError' });
  assert.equal(requests, 3);
  assert.equal(executions, 0);
});

test('ordinary legacy text has no extra completion-review call', async () => {
  const { result, requests } = await runScenario(({ isReview }) => {
    assert.ok(!isReview);
    return textTurn('你好！');
  });
  assert.equal(requests.length, 1);
  assert.equal(result.content, '你好！');
});

for (const hasReviewProse of [false, true]) {
  test(`a valid ${hasReviewProse ? 'prose-bearing' : 'footer-only'} review preserves the draft without a no-progress failure`, async () => {
    const { result, requests, states } = await runScenario(({ index, isReview }) => {
      if (index === 1) return textTurn(`甲乙。\n${footer('continue', { next: '补充第二部分' })}`);
      if (index === 2) return textTurn(`丙丁。\n${footer('complete', { reason: '已写出最后部分' })}`);
      assert.ok(isReview);
      return textTurn((hasReviewProse ? '应被忽略的核对解释。\n' : '')
        + footer('complete', { reason: '原始要求已经完整实现' }));
    });
    assert.equal(requests.length, 3);
    assert.equal(result.content, '甲乙。\n\n丙丁。');
    assert.notEqual(result.completionStatus, 'interrupted');
    assert.equal(states.at(-1).diagnostics.modelCalls, 3);
    assert.ok(!result.content.includes('应被忽略的核对解释'));
  });
}

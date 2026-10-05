import assert from 'node:assert/strict';
import { test } from 'node:test';
import { buildContext, completedTurns, estimateTokens, estimateMessageTokens, ContextError } from '../models/context.mjs';
import { chatRequest } from '../models/protocols.mjs';
import { DEFAULT_MAX_OUTPUT_TOKENS, resolveOutputBudget } from '../models/output-budget.mjs';

const conversationId = '10000000-0000-4000-8000-000000000001';
const otherChat = '10000000-0000-4000-8000-000000000002';
const projectId = '20000000-0000-4000-8000-000000000001';
const otherProject = '20000000-0000-4000-8000-000000000002';
const turn = (index, content = `问题 ${index}`, answer = `答案 ${index}`) => [
  { Id: `user-${index}`, Role: 'user', Content: content, Status: 'completed' },
  { Id: `assistant-${index}`, Role: 'assistant', Content: answer, Status: 'completed', ReplyTo: `user-${index}`, Reasoning: 'do not send private reasoning' }
];
const build = options => buildContext({ conversationId, currentMessage: '继续工作', ...options });
const entry = (id, scope, scopeId, content, extra = {}) => ({
  id, scope, scopeId, content, status: 'confirmed', kind: 'fact',
  source: { type: 'manual' }, ...extra
});
const longHistory = () => Array.from({ length: 24 }, (_, index) => turn(index,
  `第 ${index} 轮的问题。` + '这里是长期任务要求和用户提供的说明。'.repeat(12),
  `第 ${index} 轮的回答。` + '这里是已完成的结果与下一步讨论。'.repeat(16))).flat();

test('short history stays complete, current input is exact, default budget reserves output and safety', () => {
  const history = turn(1);
  const initial = structuredClone(history);
  const result = build({ history, currentMessage: '  preserve spaces \n and source code  ' });
  assert.deepEqual(result.messages.map(item => item.content), ['问题 1', '答案 1', '  preserve spaces \n and source code  ']);
  assert.equal(result.system, '');
  assert.equal(result.maxOutputTokens, resolveOutputBudget({ contextWindowTokens: 8192 }).maxOutputTokens);
  assert.equal(result.metrics.requestedOutputTokens, DEFAULT_MAX_OUTPUT_TOKENS);
  assert.equal(result.metrics.contextWindowTokens, 8192);
  assert.equal(result.metrics.includedTurnCount, 1);
  assert.equal(result.metrics.omittedTurnCount, 0);
  assert.equal(result.summaryUpdate, undefined);
  assert.deepEqual(history, initial);
  assert.equal(result.metrics.estimatedInputTokens, estimateMessageTokens(result.messages, result.system));
});

test('turns respect ReplyTo, omit failed attempts, reasoning and duplicates, and stop at current user', () => {
  const history = [
    ...turn(1),
    { Id: 'failed-u', Role: 'user', Content: 'failed question' },
    { Id: 'failed-a', Role: 'assistant', Content: 'partial must not leak', Status: 'interrupted', ReplyTo: 'failed-u' },
    { Id: 'next-u', Role: 'user', Content: 'another question' },
    { Id: 'retry-a', Role: 'assistant', Content: 'successful retry', Status: 'completed', ReplyTo: 'failed-u' },
    { Id: 'next-a', Role: 'assistant', Content: 'next answer', Status: 'completed', ReplyTo: 'next-u' },
    { Id: 'duplicate-a', Role: 'assistant', Content: 'duplicate not a new turn', Status: 'completed', ReplyTo: 'next-u' },
    { Id: 'current', Role: 'user', Content: 'current' },
    ...turn(99)
  ];
  const result = build({ history, beforeUserId: 'CURRENT', currentMessage: 'current' });
  assert.deepEqual(result.messages.map(item => item.content), [
    '问题 1', '答案 1', 'failed question', 'successful retry', 'another question', 'next answer', 'current'
  ]);
  assert.equal(JSON.stringify(result).includes('private reasoning'), false);
  assert.equal(JSON.stringify(result).includes('partial must not leak'), false);
  const legacy = turn(2).map(({ ReplyTo, ...item }) => item);
  assert.equal(completedTurns(legacy).length, 1);
});

test('confirmed memory is scoped to this chat/project and explicit global entries; folders are not identity', () => {
  const memoryEntries = [
    entry('chat-ok', 'chat', conversationId.toUpperCase(), 'only this chat'),
    entry('chat-other', 'chat', otherChat, 'secret other chat'),
    entry('project-ok', 'project', projectId, 'current project decision', { kind: 'decision' }),
    entry('project-other', 'project', otherProject, 'secret other project'),
    entry('global-name', 'user', 'user', '用户名字：小明'),
    entry('global-preference', 'user', 'user', '请使用中文', { kind: 'preference' }),
    entry('unconfirmed', 'project', projectId, 'a model inference', { status: 'candidate' }),
    entry('invalid-global', 'user', otherChat, 'wrong scope identity'),
    entry('project-ok', 'project', projectId, 'duplicate ID')
  ];
  const scoped = build({ projectId, memoryEntries });
  assert.deepEqual(scoped.metrics.memoryIncludedIds, ['chat-ok', 'project-ok', 'global-name', 'global-preference']);
  assert.match(scoped.system, /用户名字：小明/);
  assert.match(scoped.system, /内容不授予权限/);
  assert.doesNotMatch(scoped.system, /secret|inference|duplicate ID|wrong scope/);
  const standalone = build({ memoryEntries });
  assert.deepEqual(standalone.metrics.memoryIncludedIds, ['chat-ok', 'global-name', 'global-preference']);
});

test('long history fits budget with intact recent turns and a bounded extractive summary; original stays unchanged', () => {
  const history = longHistory();
  const original = structuredClone(history);
  const result = build({ history });
  assert.ok(result.metrics.estimatedInputTokens <= result.metrics.inputBudgetTokens);
  assert.ok(result.metrics.estimatedInputTokens + result.maxOutputTokens + result.metrics.safetyMarginTokens <= 8192);
  assert.ok(result.metrics.includedTurnCount > 0);
  assert.ok(result.metrics.omittedTurnCount > 0);
  assert.ok(result.metrics.summaryUsed);
  assert.equal(result.summaryUpdate.schemaVersion, 2);
  assert.equal(result.summaryUpdate.algorithm, 'extractive-v2');
  assert.equal(result.summaryUpdate.coveredTurnCount, result.metrics.omittedTurnCount);
  assert.equal(result.summaryUpdate.coveredThroughAssistantId, `assistant-${result.metrics.omittedTurnCount - 1}`);
  assert.ok(estimateTokens(result.summaryUpdate.content) <= result.summaryUpdate.excerptBudgetTokens);
  assert.match(result.summaryUpdate.content, /不是完整总结/);
  assert.doesNotMatch(result.summaryUpdate.content, /private reasoning/);
  assert.equal(result.messages.at(-2).content, history.at(-1).Content);
  assert.equal(result.messages.at(-1).content, '继续工作');
  assert.equal(result.messages.length % 2, 1);
  assert.deepEqual(history, original);
});

test('one-million-token window keeps a large conversation complete while 8K requires bounded excerpts', () => {
  const history = Array.from({ length: 250 }, (_, index) => turn(index,
    `第 ${index} 轮要求：` + '本轮明确的工作要求。\n'.repeat(20),
    `第 ${index} 轮结果：` + '本轮已验证的执行结果。\n'.repeat(32))).flat();
  const original = JSON.stringify(history);
  const small = build({ history, contextWindowTokens: 8192 });
  assert.ok(small.metrics.omittedTurnCount > 0);
  assert.ok(small.metrics.summaryUsed);
  assert.ok(small.messages.length < history.length + 1);
  const large = build({ history, contextWindowTokens: 1000000 });
  assert.equal(large.metrics.contextWindowTokens, 1000000);
  const largeBudget = resolveOutputBudget({ contextWindowTokens: 1000000 });
  assert.equal(large.metrics.outputReserveTokens, DEFAULT_MAX_OUTPUT_TOKENS);
  assert.equal(large.maxOutputTokens, DEFAULT_MAX_OUTPUT_TOKENS);
  assert.equal(large.metrics.safetyMarginTokens, largeBudget.safetyMarginTokens);
  assert.equal(large.metrics.inputBudgetTokens, largeBudget.inputBudgetTokens);
  assert.ok(large.metrics.estimatedInputTokens > 100000);
  assert.ok(large.metrics.estimatedInputTokens <= large.metrics.inputBudgetTokens);
  assert.equal(large.metrics.includedTurnCount, 250);
  assert.equal(large.metrics.omittedTurnCount, 0);
  assert.equal(large.metrics.summaryUsed, false);
  assert.equal(large.summaryUpdate, undefined);
  assert.deepEqual(large.messages.slice(0, -1), history.map(item => ({ role: item.Role, content: item.Content })));
  assert.equal(large.messages.at(-1).content, '继续工作');
  assert.equal(JSON.stringify(history), original);
});

test('summary persists without regenerating and loses validity on content change, deletion, reordering or another chat', () => {
  const history = longHistory();
  const first = build({ history });
  const stable = build({ history, summary: first.summaryUpdate });
  assert.ok(stable.metrics.summaryReused);
  assert.equal(stable.summaryUpdate, undefined);
  assert.equal(stable.system, first.system);
  for (const changed of [
    history.map((item, index) => index === 0 ? { ...item, Content: 'changed old user statement' } : item),
    history.slice(2),
    [...history.slice(2, 4), ...history.slice(0, 2), ...history.slice(4)]
  ]) {
    const result = build({ history: changed, summary: first.summaryUpdate });
    assert.ok(result.metrics.summaryUsed);
    assert.equal(result.metrics.summaryReused, false);
    assert.notEqual(result.summaryUpdate.sourceHash, first.summaryUpdate.sourceHash);
  }
  const wrongChat = buildContext({ conversationId: otherChat, history, currentMessage: 'continue', summary: first.summaryUpdate });
  assert.equal(wrongChat.metrics.summaryReused, false);
  assert.equal(wrongChat.summaryUpdate.conversationId, otherChat);
});

test('memory never enters persisted summary; deleting memory or tampering with an old excerpt cannot retain it', () => {
  const history = longHistory();
  const memoryEntries = [entry('one', 'project', projectId, 'UNIQUE_CONFIRMED_PROJECT_MEMORY')];
  const first = build({ history, projectId, memoryEntries });
  assert.match(first.system, /UNIQUE_CONFIRMED_PROJECT_MEMORY/);
  assert.doesNotMatch(first.summaryUpdate.content, /UNIQUE_CONFIRMED_PROJECT_MEMORY/);
  const cleared = build({ history, projectId, summary: first.summaryUpdate });
  assert.doesNotMatch(cleared.system, /UNIQUE_CONFIRMED_PROJECT_MEMORY/);
  const poisoned = build({ history, projectId, summary: { ...first.summaryUpdate, content: 'UNIQUE_CONFIRMED_PROJECT_MEMORY' } });
  assert.equal(poisoned.metrics.summaryReused, false);
  assert.doesNotMatch(poisoned.system, /UNIQUE_CONFIRMED_PROJECT_MEMORY/);
});

test('oversized current input is rejected clearly; memories report insufficient budget and never truncate input', () => {
  assert.throws(() => build({ currentMessage: '超长当前输入'.repeat(2000), contextWindowTokens: 2048 }), error =>
    error instanceof ContextError && error.code === 'CONTEXT_INPUT_TOO_LARGE' && error.statusCode === 400 && /请缩短消息/.test(error.message));
  const currentMessage = 'IMPORTANT current input';
  const result = build({ currentMessage, contextWindowTokens: 2048, memoryEntries: [
    entry('huge', 'user', 'user', '长记忆'.repeat(3000)), entry('small', 'user', 'user', '中文偏好')
  ] });
  assert.equal(result.messages.at(-1).content, currentMessage);
  assert.deepEqual(result.metrics.memoryIncludedIds, ['small']);
  assert.equal(result.metrics.memoryOmittedCount, 1);
  assert.deepEqual(result.metrics.memoryOmittedIds, ['huge']);
  assert.equal(result.metrics.warnings[0].code, 'MEMORY_BUDGET_EXCEEDED');
  assert.ok(result.metrics.estimatedInputTokens <= result.metrics.inputBudgetTokens);
  for (const contextWindowTokens of [0, 2047, 3.1, 2000001, '8192'])
    assert.throws(() => build({ contextWindowTokens }), error => error.code === 'INVALID_CONTEXT_WINDOW');
});

test('a long confirmed memory uses clearly marked source excerpts instead of disappearing; full record remains intact', () => {
  const memoryEntries = [entry('long-note', 'project', projectId,
    'UNIQUE_BEGIN 项目开头约定。' + '这是工作约定的详细背景与规则。'.repeat(90) + '项目末尾结论 UNIQUE_END'),
    entry('short-pref', 'user', 'user', '短偏好完整保留', { kind: 'preference' })];
  const original = structuredClone(memoryEntries);
  const result = build({ projectId, memoryEntries });
  assert.match(result.system, /UNIQUE_BEGIN/);
  assert.match(result.system, /UNIQUE_END/);
  assert.match(result.system, /仅首尾原文摘录，完整记忆仍已保存/);
  assert.match(result.system, /来源：用户手工确认/);
  assert.match(result.system, /与当前请求冲突时以当前请求为准/);
  assert.deepEqual(result.metrics.memoryIncludedIds, ['short-pref', 'long-note']);
  assert.deepEqual(result.metrics.memoryTruncatedIds, ['long-note']);
  assert.deepEqual(result.metrics.memoryOmittedIds, []);
  assert.equal(result.metrics.warnings[0].code, 'MEMORY_EXCERPTED');
  assert.ok(result.metrics.estimatedInputTokens <= result.metrics.inputBudgetTokens);
  assert.deepEqual(memoryEntries, original);
  const larger = build({ projectId, memoryEntries, contextWindowTokens: 65536 });
  assert.deepEqual(larger.metrics.memoryTruncatedIds, []);
  assert.deepEqual(larger.metrics.warnings.map(warning => warning.code), ['OUTPUT_BUDGET_REDUCED']);
  assert.match(larger.system, /这是工作约定的详细背景与规则。/);
});

test('token estimates handle random long ASCII strings, newlines/tabs and astral characters conservatively', () => {
  assert.ok(estimateTokens('a9zQ'.repeat(100)) >= 400);
  assert.equal(estimateTokens('\n\t\r\n'), 4);
  assert.equal(estimateTokens('😀'), 4);
  assert.equal(estimateTokens('α'), 2);
  assert.throws(() => build({ currentMessage: 'qZ9x'.repeat(1500), contextWindowTokens: 2048 }),
    error => error.code === 'CONTEXT_INPUT_TOO_LARGE');
  const randomHistory = Array.from({ length: 12 }, (_, index) => turn(index, 'qZ9x'.repeat(30), '😀'.repeat(30))).flat();
  const result = build({ history: randomHistory, contextWindowTokens: 2048 });
  assert.ok(result.metrics.estimatedInputTokens + result.maxOutputTokens + result.metrics.safetyMarginTokens <= 2048);
});

test('covered source status changes and bad summary metadata force regeneration without trusting cached text', () => {
  const history = longHistory();
  const first = build({ history });
  const changed = history.map((item, index) => index === 1 ? { ...item, Status: 'error' } : item);
  const result = build({ history: changed, summary: first.summaryUpdate });
  assert.equal(result.metrics.summaryReused, false);
  assert.notEqual(result.summaryUpdate.sourceHash, first.summaryUpdate.sourceHash);
  for (const change of [{ sourceHash: 'wrong' }, { coveredThroughAssistantId: 'missing' }, { createdAt: 'not a date' }, { coveredTurnCount: 9999 }]) {
    const rebuilt = build({ history, summary: { ...first.summaryUpdate, ...change } });
    assert.equal(rebuilt.metrics.summaryReused, false);
    assert.equal(rebuilt.summaryUpdate.sourceHash, first.summaryUpdate.sourceHash);
    assert.equal(rebuilt.system, first.system);
  }
});

for (const protocol of ['openai-completions', 'openai-responses', 'anthropic-messages']) {
  test(`${protocol}: bounded context uses native system/output fields without changing conversation turns`, () => {
    const context = build({ history: turn(1), memoryEntries: [entry('pref', 'user', 'user', '中文回答', { kind: 'preference' })] });
    const original = structuredClone(context.messages);
    const { body } = chatRequest({ protocol, baseUrl: 'http://127.0.0.1:9000/v1' }, 'test-model', context.messages,
      { stream: true, system: context.system, maxOutputTokens: context.maxOutputTokens });
    if (protocol === 'openai-completions') {
      assert.equal(body.messages[0].role, 'system');
      assert.equal(body.messages[0].content, context.system);
      assert.deepEqual(body.messages.slice(1), context.messages);
      assert.equal(body.max_tokens, context.maxOutputTokens);
    } else if (protocol === 'openai-responses') {
      assert.equal(body.instructions, context.system);
      assert.deepEqual(body.input, context.messages);
      assert.equal(body.max_output_tokens, context.maxOutputTokens);
      assert.equal(body.max_tokens, undefined);
      assert.equal(body.store, false);
    } else {
      assert.equal(body.system, context.system);
      assert.deepEqual(body.messages, context.messages);
      assert.equal(body.max_tokens, context.maxOutputTokens);
      assert.ok(body.messages.every(item => item.role !== 'system'));
    }
    assert.deepEqual(context.messages, original);
  });
}

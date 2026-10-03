import assert from 'node:assert/strict';
import { test } from 'node:test';
import { buildContext, completedTurns, estimateTokens, estimateMessageTokens } from '../context.mjs';

const history = Array.from({ length: 8 }, (_, index) => [
  { Id: `u-${index}`, Role: 'user', Content: `Keep code constraint ${index}`, Status: 'completed' },
  { Id: `a-${index}`, Role: 'assistant', Content: `Result ${index}`, Status: 'completed', ReplyTo: `u-${index}` }
]).flat();
const projectTurn = turn => [
  { role: 'user', content: turn.user.Content },
  { role: 'assistant', content: 'Reading code', tool_calls: [{ id: turn.assistant.Id, type: 'function', function: {
    name: 'file.read', arguments: JSON.stringify({ path: `${turn.user.Id}.py` })
  } }] },
  { role: 'tool', tool_call_id: turn.assistant.Id, content: '# source code\n'.repeat(160) },
  { role: 'assistant', content: turn.assistant.Content }
];
const estimate = (messages, system = '') => messages.reduce((sum, message) =>
  sum + 8 + estimateTokens(JSON.stringify(message)), 0) + estimateMessageTokens([], system);

test('context accounts for complete call/result history rather than only final text', () => {
  const original = structuredClone(history);
  const result = buildContext({ conversationId: 'projection-test', historyTurns: completedTurns(history),
    currentMessage: 'Continue code', contextWindowTokens: 32768, maxOutputTokens: 4096,
    projectTurn, estimateContextMessages: estimate });
  assert.equal(result.messages.length, 33);
  assert.equal(result.historySources.length, result.messages.length);
  assert.equal(result.metrics.estimatedInputTokens, estimate(result.messages, result.system));
  assert.deepEqual(result.historySources.slice(0, 4).map(source => [source.messageId, source.turnIndex]),
    [['u-0', 0], ['a-0', 0], ['a-0', 0], ['a-0', 0]]);
  assert.deepEqual(history, original);
});

test('tight projection keeps whole recent turns and summarizes only public conversation text', () => {
  const result = buildContext({ conversationId: 'projection-test', historyTurns: completedTurns(history),
    currentMessage: 'Continue code', contextWindowTokens: 8192, maxOutputTokens: 2048,
    projectTurn, estimateContextMessages: estimate });
  assert.ok(result.metrics.includedTurnCount > 0 && result.metrics.omittedTurnCount > 0);
  assert.equal((result.messages.length - 1) % 4, 0);
  for (let offset = 0; offset < result.messages.length - 1; offset += 4) {
    assert.equal(result.messages[offset + 1].tool_calls[0].id, result.messages[offset + 2].tool_call_id);
    assert.equal(result.historySources[offset].turnIndex, result.historySources[offset + 3].turnIndex);
  }
  assert.equal(result.messages.at(-2).content, 'Result 7');
  assert.doesNotMatch(result.system, /file\.read|# source code|tool_calls/);
  assert.ok(result.metrics.estimatedInputTokens + result.maxOutputTokens + result.metrics.safetyMarginTokens <= 8192);
});

test('provider output limit and large code input coexist within the actual window', () => {
  const result = buildContext({ conversationId: 'projection-test', currentMessage: 'Review code',
    contextWindowTokens: 1_000_000, providerMaxOutputTokens: 128_000 });
  assert.equal(result.maxOutputTokens, 128_000);
  assert.equal(result.metrics.providerMaxOutputTokens, 128_000);
  assert.equal(result.metrics.outputBudgetReductionReason, 'provider_limit');
  assert.ok(result.metrics.inputBudgetTokens > 850_000);
  assert.match(result.metrics.warnings[0].message, /模型实际输出能力/);
});

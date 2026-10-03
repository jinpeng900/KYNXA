import assert from 'node:assert/strict';
import { test } from 'node:test';
import { buildContext, completedTurns } from '../context.mjs';
import { resolveOutputBudget } from '../output-budget.mjs';
import { estimateToolMessageTokens } from '../tool-protocols.mjs';

function freeze(value) {
  if (value && typeof value === 'object') {
    Object.values(value).forEach(freeze);
    Object.freeze(value);
  }
  return value;
}

const history = freeze(Array.from({ length: 112 }, (_, index) => [
  { Id: `boundary-user-${index}`, Role: 'user', Content: `必须保留 constraint ${index}`, Status: 'completed' },
  { Id: `boundary-assistant-${index}`, Role: 'assistant', Content: `Public result ${index}`,
    Status: 'completed', ReplyTo: `boundary-user-${index}`, Reasoning: 'PRIVATE_UI_REASONING',
    NativeFixture: { signature: `OPAQUE_PROVIDER_SIGNATURE_${index}`, output: 'Stored output line.\n'.repeat(75) } }
]).flat());

const projections = {
  responses(turn) {
    const callId = `call-${turn.assistant.Id}`;
    return [
      { role: 'user', content: turn.user.Content },
      { type: 'reasoning', id: `reasoning-${turn.assistant.Id}`, encrypted_content: turn.assistant.NativeFixture.signature },
      { type: 'function_call', id: `function-${turn.assistant.Id}`, call_id: callId, name: 'read_file', arguments: '{"path":"fixture.txt"}' },
      { type: 'function_call_output', call_id: callId, output: turn.assistant.NativeFixture.output },
      { role: 'assistant', content: turn.assistant.Content }
    ];
  },
  anthropic(turn) {
    const callId = `call-${turn.assistant.Id}`;
    return [
      { role: 'user', content: turn.user.Content },
      { role: 'assistant', content: [
        { type: 'thinking', thinking: 'PUBLIC_PROVIDER_SUMMARY', signature: turn.assistant.NativeFixture.signature },
        { type: 'tool_use', id: callId, name: 'read_file', input: { path: 'fixture.txt' } }
      ] },
      { role: 'user', content: [{ type: 'tool_result', tool_use_id: callId, content: turn.assistant.NativeFixture.output }] },
      { role: 'assistant', content: turn.assistant.Content }
    ];
  }
};

for (const [protocol, projectTurn] of Object.entries(projections)) {
  test(`${protocol}: independent input cap budgets complete native turns with schemas, system and memory`, () => {
    const original = JSON.stringify(history), turns = freeze(completedTurns(history));
    const result = buildContext({
      conversationId: 'projection-boundary', historyTurns: turns, currentMessage: 'Continue current implementation.',
      contextWindowTokens: 64_000, maxOutputTokens: 32_768,
      providerMaxInputTokens: 16_384, providerMaxOutputTokens: 4_096,
      additionalSystem: 'Trusted application instruction.\n'.repeat(30), reservedInputTokens: 1_024,
      memoryEntries: [{ id: 'confirmed-boundary', status: 'confirmed', kind: 'decision', scope: 'chat',
        scopeId: 'projection-boundary', content: 'Keep the confirmed public contract.' }],
      projectTurn, estimateContextMessages: estimateToolMessageTokens
    });
    assert.ok(result.metrics.includedTurnCount > 0 && result.metrics.omittedTurnCount > 0);
    assert.equal(result.maxOutputTokens, 4_096);
    assert.equal(result.metrics.inputBudgetTokens, 16_384 - result.metrics.safetyMarginTokens);
    assert.ok(result.metrics.estimatedInputTokens + result.metrics.reservedToolTokens <= result.metrics.inputBudgetTokens);
    assert.ok(result.metrics.estimatedInputTokens + result.metrics.reservedToolTokens + result.maxOutputTokens
      + result.metrics.safetyMarginTokens <= 64_000);
    assert.deepEqual(result.metrics.memoryIncludedIds, ['confirmed-boundary']);
    assert.equal(result.metrics.estimatedInputTokens, estimateToolMessageTokens(result.messages, result.system));
    const included = turns.slice(result.metrics.omittedTurnCount), projectedLength = projectTurn(turns[0]).length;
    assert.equal(result.messages.length, included.length * projectedLength + 1);
    assert.deepEqual(result.messages.slice(0, -1), included.flatMap(projectTurn));
    for (let offset = 0; offset < result.messages.length - 1; offset += projectedLength) {
      const turn = result.messages.slice(offset, offset + projectedLength);
      if (protocol === 'responses') {
        assert.equal(turn[2].call_id, turn[3].call_id);
        assert.match(turn[1].encrypted_content, /^OPAQUE_PROVIDER_SIGNATURE_/);
      } else {
        assert.equal(turn[1].content[1].id, turn[2].content[0].tool_use_id);
        assert.match(turn[1].content[0].signature, /^OPAQUE_PROVIDER_SIGNATURE_/);
      }
      const sources = result.historySources.slice(offset, offset + projectedLength);
      assert.ok(sources.every(source => source.turnIndex === sources[0].turnIndex));
    }
    assert.doesNotMatch(result.system, /OPAQUE_PROVIDER_SIGNATURE|Stored output|PRIVATE_UI_REASONING|PUBLIC_PROVIDER_SUMMARY/);
    assert.equal(result.messages.at(-1).content, 'Continue current implementation.');
    assert.equal(JSON.stringify(history), original, 'all 224 frozen original messages remain unchanged');
  });
}

test('exact minimum generation boundary and independent input cap reject the first excess token', () => {
  const exact = resolveOutputBudget({ contextWindowTokens: 2_048, requestedOutputTokens: 1_024, requiredInputTokens: 1_536 });
  assert.equal(exact.maxOutputTokens, 256);
  assert.equal(exact.inputBudgetTokens, 1_536);
  assert.equal(exact.inputBudgetTokens + exact.maxOutputTokens + exact.safetyMarginTokens, 2_048);
  assert.throws(() => resolveOutputBudget({ contextWindowTokens: 2_048, requestedOutputTokens: 1_024,
    requiredInputTokens: 1_537 }), { code: 'CONTEXT_INPUT_TOO_LARGE' });
  const inputExact = resolveOutputBudget({ contextWindowTokens: 2_048, providerMaxInputTokens: 900, requiredInputTokens: 644 });
  assert.equal(inputExact.inputBudgetTokens, 644);
  assert.throws(() => resolveOutputBudget({ contextWindowTokens: 2_048, providerMaxInputTokens: 900,
    requiredInputTokens: 645 }), { code: 'CONTEXT_INPUT_TOO_LARGE' });
});

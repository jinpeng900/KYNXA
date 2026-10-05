import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { test } from 'node:test';
import { ModelHistoryProjection } from '../models/model-history.mjs';
import { ToolContextProjection } from '../models/tool-context.mjs';
import { estimateToolMessageTokens } from '../models/tool-protocols.mjs';
import { estimateTokens } from '../models/context.mjs';

const protocols = ['openai-completions', 'openai-responses', 'anthropic-messages'];
const reference = () => ({ id: randomUUID(), bytes: 100_000, sha256: 'a'.repeat(64) });

function formalTurn(index, { argumentText = 'file.txt', resultText = 'Recorded output.', userText = 'Read the mounted file.',
  finalText = 'The recorded file was read.', resultCount = 1, resultRef = true } = {}) {
  const user = { Id: `user-${index}`, Role: 'user', Content: userText, Status: 'completed' };
  const calls = Array.from({ length: resultCount }, (_, callIndex) => ({ id: `call-${index}-${callIndex}`,
    name: 'filesystem.read', arguments: { path: argumentText } }));
  const assistant = { Id: `assistant-${index}`, Role: 'assistant', ReplyTo: user.Id, Status: 'completed', Content: finalText,
    ModelTranscript: { rounds: [{ round: 1, text: 'Reading the requested file.', calls }] },
    ToolActivities: calls.map(call => ({ toolCallId: call.id, name: call.name, arguments: call.arguments,
      round: 1, status: 'completed', result: resultText, ...(resultRef ? { resultRef: reference() } : {}) })) };
  return [user, assistant];
}

function projectedHistory(protocol, turnOptions) {
  const history = turnOptions.flatMap((options, index) => formalTurn(index, options));
  const projection = new ModelHistoryProjection({ history, protocol, inputBudgetTokens: 1_000_000,
    availableTools: [{ name: 'filesystem.read' }] });
  const messages = [], sources = [];
  projection.historyTurns.forEach((turn, turnIndex) => {
    const items = projection.projectTurn(turn);
    messages.push(...items);
    sources.push(...items.map((message, position) => ({ turnIndex, role: message.role ?? 'assistant',
      messageId: position === 0 ? turn.user.Id : turn.assistant.Id })));
  });
  messages.push({ role: 'user', content: 'Continue the current task.' });
  sources.push({ messageId: 'current-user', role: 'user', turnIndex: null });
  return { history, messages, sources: projection.historySources(messages, sources) };
}

function nativeCalls(messages) {
  return messages.flatMap(message => [...(message.tool_calls ?? []).map(call => call.id),
    ...(message.type === 'function_call' ? [message.call_id] : []),
    ...(Array.isArray(message.content) ? message.content.filter(block => block.type === 'tool_use').map(block => block.id) : [])]);
}
function nativeOutputs(messages) {
  return messages.flatMap(message => message.type === 'function_call_output' ? [{ id: message.call_id, text: message.output }]
    : message.role === 'tool' ? [{ id: message.tool_call_id, text: message.content }]
      : Array.isArray(message.content) ? message.content.filter(block => block.type === 'tool_result')
        .map(block => ({ id: block.tool_use_id, text: block.content })) : []);
}
function assertPairs(messages) {
  assert.deepEqual(nativeCalls(messages).toSorted(), nativeOutputs(messages).map(item => item.id).toSorted());
}

function activeNativePair(protocol) {
  const args = { path: 'current.txt', exact: 'Current arguments must remain byte-for-byte unchanged.' };
  if (protocol === 'openai-responses') return [{ type: 'reasoning', encrypted_content: 'opaque-current-state', summary: [] },
    { type: 'function_call', call_id: 'current-call', name: 'current_wire', arguments: JSON.stringify(args) },
    { type: 'function_call_output', call_id: 'current-call', output: 'Current result.' }];
  if (protocol === 'anthropic-messages') return [{ role: 'assistant', content: [
    { type: 'thinking', thinking: 'Private current reasoning.', signature: 'opaque-current-signature' },
    { type: 'tool_use', id: 'current-call', name: 'current_wire', input: args }] },
  { role: 'user', content: [{ type: 'tool_result', tool_use_id: 'current-call', content: 'Current result.' }] }];
  return [{ role: 'assistant', content: 'Current phase.', reasoning_content: 'Private current reasoning.',
    tool_calls: [{ id: 'current-call', type: 'function', function: { name: 'current_wire', arguments: JSON.stringify(args) } }] },
  { role: 'tool', tool_call_id: 'current-call', content: 'Current result.' }];
}

for (const protocol of protocols) {
  test(`${protocol} masks older cross-message observations before atomically replacing their complete turn`, () => {
    const f = projectedHistory(protocol, [{ argumentText: 'large historical call argument '.repeat(1600),
      resultText: 'Old archived observation words 😀 '.repeat(2200), resultCount: 2 },
    { resultText: 'Latest complete paired evidence.' }]);
    const original = structuredClone(f.messages), formal = structuredClone(f.history);
    const projection = new ToolContextProjection({ protocol, messages: f.messages, historySources: f.sources, conversationId: 'chat-history' });
    const active = activeNativePair(protocol), recent = f.messages.filter((_, index) => f.sources[index].turnIndex === 1);
    const result = projection.compact([...f.messages, ...active], { system: 'Fixed current permissions.', inputBudgetTokens: 6500 });
    assert.ok(result.metrics.compactedToolResultCount >= 2, 'old archived observations are considered before call-bearing history');
    assert.equal(result.metrics.compactedHistoryTurnCount, 1);
    assert.ok(estimateToolMessageTokens(result.messages, 'Fixed current permissions.') <= 6500);
    assertPairs(result.messages);
    assert.deepEqual(result.messages.slice(2, 2 + recent.length), recent, 'the latest complete turn remains verbatim');
    assert.deepEqual(result.messages.slice(-active.length), active, 'current arguments and private continuation remain untouched');
    assert.equal(nativeCalls(result.messages).length, 2, 'only recent and current calls remain, with their complete result pairs');
    for (const message of result.messages.slice(0, 2)) {
      const compacted = JSON.parse(message.content);
      assert.equal(compacted.pairedHistoryCompacted, true);
      assert.equal(compacted.navigation.arguments.includeTools, true);
      assert.equal(compacted.source.conversationId, 'chat-history');
      assert.ok(compacted.originalMessageCount > 2);
    }
    assert.equal(JSON.parse(result.messages[0].content).source.messageId, 'user-0');
    assert.equal(JSON.parse(result.messages[1].content).source.messageId, 'assistant-0');
    assert.deepEqual(f.messages, original); assert.deepEqual(f.history, formal);
  });

  test(`${protocol} one latest historical result remains paired as a head/tail preview with its real result reference`, () => {
    const resultText = 'DIAGNOSTIC_HEAD\n' + 'Unneeded middle log text 😀 '.repeat(3500) + '\nDIAGNOSTIC_TAIL';
    const f = projectedHistory(protocol, [{ resultText }]);
    const projection = new ToolContextProjection({ protocol, messages: f.messages, historySources: f.sources, conversationId: 'chat-history' });
    const result = projection.compact(f.messages, { inputBudgetTokens: 4500 });
    assertPairs(result.messages);
    assert.equal(result.metrics.compactedLatestResultCount, 1);
    assert.equal(result.metrics.compactedHistoryTurnCount, 0);
    const output = nativeOutputs(result.messages)[0], preview = JSON.parse(output.text);
    assert.equal(preview.status, 'completed');
    assert.equal(preview.resultRef.id, f.history[1].ToolActivities[0].resultRef.id);
    assert.match(preview.excerpt, /DIAGNOSTIC_HEAD/); assert.match(preview.excerpt, /DIAGNOSTIC_TAIL/);
    assert.match(preview.excerpt, /middle omitted/);
    assert.equal(preview.navigation.tool, 'tool.result.read');
    assert.deepEqual(result.messages.at(-1), f.messages.at(-1));
  });

  test(`${protocol} cloned result owners retain group identity through later whole-turn and staged compression`, () => {
    const f = projectedHistory(protocol, [{ argumentText: 'historical argument words '.repeat(1100),
      resultText: 'Archived large result words '.repeat(3200), userText: 'Historical user instructions and context '.repeat(240),
      finalText: 'Historical public final text '.repeat(250), resultCount: 2 }]);
    const projection = new ToolContextProjection({ protocol, messages: f.messages, historySources: f.sources, conversationId: 'chat-history' });
    const baseline = structuredClone(f.messages);
    for (const message of baseline) {
      if (message.role === 'tool') message.content = 'Bounded output.';
      if (message.type === 'function_call_output') message.output = 'Bounded output.';
      if (Array.isArray(message.content)) for (const block of message.content)
        if (block.type === 'tool_result') block.content = 'Bounded output.';
    }
    const first = projection.compact(f.messages, { inputBudgetTokens: estimateToolMessageTokens(baseline) + 8000 });
    assert.equal(first.metrics.compactedHistoryTurnCount, 0, 'initial pressure only needs archived output masking');
    assertPairs(first.messages);
    const second = projection.compact(first.messages, { inputBudgetTokens: 1800 });
    assert.equal(second.messages.length, 3, 'entire call-bearing turn becomes one identified user/assistant excerpt pair');
    assert.equal(second.metrics.compactedHistoryTurnCount, 1);
    assert.equal(nativeCalls(second.messages).length, 0); assert.equal(nativeOutputs(second.messages).length, 0);
    const declarations = [{ description: 'new current schema words '.repeat(25) }];
    const third = projection.compact(second.messages, { inputBudgetTokens: 950, declarations });
    assert.equal(third.messages.length, 3);
    assert.ok(estimateToolMessageTokens(third.messages) + estimateTokens(JSON.stringify(declarations)) <= 950);
    assert.ok(JSON.parse(third.messages[1].content).excerpt.length <= 128, 'later pressure advances the same historical group to a smaller stage');
    assert.equal(JSON.parse(third.messages[1].content).navigation.arguments.messageId, 'assistant-0');
    assert.equal(JSON.parse(third.messages[1].content).navigation.arguments.includeTools, true);
    assert.equal(JSON.parse(third.messages[1].content).originalMessageCount, f.messages.length - 1);
    assert.deepEqual(third.messages.at(-1), f.messages.at(-1));
  });

  test(`${protocol} leaves the newest complete historical pair verbatim when it fits within the hard budget`, () => {
    const f = projectedHistory(protocol, [{ resultText: 'Recent authoritative observation '.repeat(160) }]);
    const projection = new ToolContextProjection({ protocol, messages: f.messages, historySources: f.sources, conversationId: 'chat-history' });
    const cost = estimateToolMessageTokens(f.messages), hardLimit = Math.ceil(cost / .95);
    const result = projection.compact(f.messages, { inputBudgetTokens: hardLimit });
    assert.ok(cost > hardLimit * .90, 'the pressure trigger is reached');
    assert.equal(result.messages, f.messages, 'a target below the most recent pair is not permission to damage it');
    assertPairs(result.messages);
  });
}

test('incomplete historical tool pairs and unlabeled current provider state cannot be hidden by history compaction', () => {
  const f = projectedHistory('openai-completions', [{ argumentText: 'unpaired historical call words '.repeat(1500), resultRef: false }]);
  const outputIndex = f.messages.findIndex(message => message.role === 'tool');
  f.messages.splice(outputIndex, 1); f.sources.splice(outputIndex, 1);
  const original = structuredClone(f.messages);
  const projection = new ToolContextProjection({ protocol: 'openai-completions', messages: f.messages,
    historySources: f.sources, conversationId: 'chat-history' });
  assert.throws(() => projection.compact(f.messages, { inputBudgetTokens: 1100 }), { code: 'TOOL_CONTEXT_BUDGET_EXCEEDED' });
  assert.deepEqual(f.messages, original);
  const current = activeNativePair('openai-responses');
  current[1].arguments = JSON.stringify({ path: 'large current immutable arguments '.repeat(1200) });
  const currentProjection = new ToolContextProjection({ protocol: 'openai-responses', messages: [{ role: 'user', content: 'Current request.' }] });
  assert.throws(() => currentProjection.compact(current, { inputBudgetTokens: 1000 }), { code: 'TOOL_CONTEXT_BUDGET_EXCEEDED' });
  assert.match(current[0].encrypted_content, /opaque-current/);
});

test('history without available tool declarations compacts the whole low-trust chronology, not an intermediate assistant/user slice', () => {
  const history = formalTurn(0, { argumentText: 'historical arguments '.repeat(2500), resultText: 'Archived observation '.repeat(2500) });
  const model = new ModelHistoryProjection({ history, protocol: 'openai-responses', inputBudgetTokens: 1_000_000, availableTools: [] });
  const messages = [...model.projectTurn(model.historyTurns[0]), { role: 'user', content: 'Current request.' }];
  const base = messages.map((message, index) => ({ turnIndex: index === messages.length - 1 ? null : 0,
    messageId: index === 0 ? 'user-0' : 'assistant-0', role: message.role ?? 'assistant' }));
  const projection = new ToolContextProjection({ protocol: 'openai-responses', messages,
    historySources: model.historySources(messages, base), conversationId: 'chat-history' });
  const result = projection.compact(messages, { inputBudgetTokens: 1800 });
  assert.equal(result.messages.length, 3);
  assert.equal(result.metrics.compactedHistoryTurnCount, 1);
  assert.equal(JSON.parse(result.messages[0].content).source.messageId, 'user-0');
  assert.equal(JSON.parse(result.messages[1].content).source.messageId, 'assistant-0');
  assert.deepEqual(result.messages.at(-1), messages.at(-1));
});

test('history projection never restores a result reference that archive ownership validation cleared', () => {
  const f = projectedHistory('openai-responses', [{ resultText: 'Observation without a trusted archive '.repeat(2400) }]);
  for (const source of f.sources) if (source.historicalResult) source.historicalResult.resultRef = null;
  const projection = new ToolContextProjection({ protocol: 'openai-responses', messages: f.messages,
    historySources: f.sources, conversationId: 'chat-history' });
  const result = projection.compact(f.messages, { inputBudgetTokens: 1800 });
  assert.equal(result.metrics.compactedToolResultCount, 0, 'no guessed archive can support observation masking');
  assert.equal(result.messages.length, 3, 'the full historical pair can still be replaced by formal-message navigation');
  assert.doesNotMatch(JSON.stringify(result.messages), /tool\.result\.read/);
  assert.equal(JSON.parse(result.messages[1].content).navigation.arguments.includeTools, true);
});

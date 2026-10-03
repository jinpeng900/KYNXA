import assert from 'node:assert/strict';
import { test } from 'node:test';
import { AssistantSegments, applyAssistantSegmentEvent, assistantSegmentText, validateAssistantSegments } from '../assistant-segments.mjs';
import { runToolLoop } from '../tool-loop.mjs';

test('three rounds expose ordered public segments, tools and an independent final answer', async () => {
  const events = [], saved = [], assistant = {};
  let rounds = 0;
  const result = await runToolLoop({ protocol: 'openai-completions', messages: [{ role: 'user', content: 'Find the current fact.' }],
    system: '', declarations: [], inputBudgetTokens: 32000, context: {},
    emit: event => { events.push(event); applyAssistantSegmentEvent(assistant, event); },
    saveActivity: async item => saved.push(item),
    service: { execute: async () => ({ content: 'Official source available.' }) },
    requestTurn: async (_messages, _declarations, _signal, emit) => {
      const round = ++rounds, content = round === 3 ? 'Short verified answer.' : `Stage ${round}.`;
      emit({ type: 'reasoning_delta', delta: `Public summary ${round}.` }); emit({ type: 'text_delta', delta: content });
      const call = { id: `call-${round}`, name: 'search', arguments: { round } };
      return { content, reasoning: `Public summary ${round}.`, calls: round < 3 ? [call] : [],
        continuation: [{ role: 'assistant', content, tool_calls: [{ id: call.id, type: 'function',
          function: { name: 'search', arguments: JSON.stringify(call.arguments) } }] }] };
    } });
  assert.equal(result.content, 'Short verified answer.');
  assert.equal(result.toolStreamProtocol, 3);
  assert.deepEqual(result.assistantSegments.map(item => [item.round, item.order, item.phase, item.status]),
    [[1, 0, 'commentary', 'completed'], [2, 2, 'commentary', 'completed'], [3, 4, 'final_answer', 'completed']]);
  assert.deepEqual(saved.filter(item => item.status === 'completed').map(item => [item.round, item.order]), [[1, 1], [2, 3]]);
  assert.equal(assistantSegmentText(assistant), 'Stage 1.\n\nStage 2.\n\nShort verified answer.');
  assert.deepEqual(validateAssistantSegments(result.assistantSegments), result.assistantSegments);
  for (const event of events.filter(item => item.type.endsWith('_delta')))
    assert.ok(assistant.AssistantSegments.some(segment => segment.id === event.segmentId));
});

test('an interrupted revised second round preserves both public stages and excludes private protocol fields', async () => {
  const events = [], message = {};
  const segments = new AssistantSegments(event => { events.push(event); applyAssistantSegmentEvent(message, event); });
  segments.start(1); segments.receive({ type: 'text_delta', delta: 'Earlier stage.' });
  segments.finish({ content: 'Earlier stage.', reasoning: '', calls: [{ id: 'call' }] });
  segments.order++;
  segments.start(2); segments.receive({ type: 'text_delta', delta: 'draft' });
  segments.receive({ type: 'content_snapshot', content: 'Revised partial.', reasoning: 'Public summary' });
  segments.interrupt();
  assert.equal(assistantSegmentText(message), 'Earlier stage.\n\nRevised partial.');
  assert.equal(message.AssistantSegments[1].status, 'interrupted');
  assert.equal(message.AssistantSegments[1].phase, 'commentary');
  const source = { ...message.AssistantSegments[0], signature: 'PRIVATE', encrypted_content: 'PRIVATE' };
  assert.ok(!JSON.stringify(validateAssistantSegments([source])).includes('PRIVATE'));
  assert.deepEqual(events.filter(item => item.type === 'assistant_segment').map(item => item.segment.status),
    ['streaming', 'completed', 'streaming', 'interrupted']);
});

test('invalid identities, ordering and final boundaries cannot become formal segments', () => {
  const segment = { id: 'segment', round: 1, order: 0, phase: 'commentary', status: 'completed', content: 'text', reasoning: '' };
  for (const value of [null, {}, [null], [{ ...segment, round: 0 }], [{ ...segment, phase: 'invented' }],
    [{ ...segment, order: -1 }], [{ ...segment, reasoning: 2 }], [segment, { ...segment, round: 2, order: 1 }],
    [{ ...segment, phase: 'final_answer' }, { ...segment, id: 'later', round: 2, order: 1 }]])
    assert.throws(() => validateAssistantSegments(value), { code: 'INVALID_CONVERSATION_DATA' });
  assert.throws(() => applyAssistantSegmentEvent({}, { type: 'text_delta', segmentId: 'unknown', delta: 'text' }),
    { code: 'INVALID_ASSISTANT_SEGMENT' });
});

test('invalidated tool configurations stop the loop after saving the first failed receipt', async () => {
  for (const code of ['AGENT_CONFIG_CHANGED', 'MCP_CATALOG_CHANGED']) {
    const activities = [], events = [];
    let rounds = 0, executions = 0;
    await assert.rejects(runToolLoop({ protocol: 'openai-completions', messages: [], system: '', declarations: [],
      inputBudgetTokens: 32000, context: {}, emit: event => events.push(event), saveActivity: async item => activities.push(item),
      service: { execute: async () => { executions++; return { content: 'The tool configuration changed.', isError: true, code }; } },
      requestTurn: async () => {
        rounds++; return { content: 'Checking the source.', reasoning: '', calls: [
          { id: 'first', name: 'search', arguments: {} }, { id: 'second', name: 'search', arguments: {} }] };
      } }), { code, type: 'interrupted' });
    assert.equal(rounds, 1); assert.equal(executions, 1);
    assert.deepEqual(activities.map(item => item.status), ['running', 'error']);
    assert.equal(events.at(-1).type, 'tool_result');
  }
});

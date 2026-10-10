import assert from 'node:assert/strict';
import { createHash, randomUUID } from 'node:crypto';
import { test } from 'node:test';
import { OutputContinuation } from '../orchestration/output-continuation.mjs';
import { runToolLoop } from '../orchestration/tool-loop.mjs';
import { decodeToolTurn } from '../models/tool-protocols.mjs';
import { readToolStream } from '../models/tool-streaming.mjs';

function archiveFixture() {
  const records = [];
  return { records, async save(context, call, canonical) {
    records.push({ context, call, canonical });
    const source = JSON.stringify(canonical);
    return { id: randomUUID(), bytes: Buffer.byteLength(source), sha256: createHash('sha256').update(source).digest('hex') };
  } };
}

function native(protocol, content, limited) {
  if (protocol === 'anthropic-messages') return { stop_reason: limited ? 'max_tokens' : 'end_turn', content: [{ type: 'text', text: content }] };
  if (protocol === 'openai-responses') return { status: limited ? 'incomplete' : 'completed',
    ...(limited ? { incomplete_details: { reason: 'max_output_tokens' } } : {}),
    output: [{ type: 'message', content: [{ type: 'output_text', text: content }] }] };
  return { choices: [{ finish_reason: limited ? 'length' : 'stop', message: { role: 'assistant', content } }] };
}

for (const protocol of ['openai-completions', 'openai-responses', 'anthropic-messages']) {
  test(`${protocol}: explicit length receipt continues in one request and preserves the complete final body`, async () => {
    const results = archiveFixture(), activities = [], states = [], events = [];
    let requests = 0;
    const first = '1 第一行\n2 第二行\n';
    const result = await runToolLoop({ protocol, messages: [{ role: 'user', content: '写三行，自动继续。' }],
      declarations: [], system: '', inputBudgetTokens: 2048,
      context: { conversationId: randomUUID(), requestId: randomUUID(), message: '写三行，自动继续。' },
      service: { results, execute: () => assert.fail('text continuation grants no executable tool') },
      emit: event => events.push(event), saveActivity: async activity => activities.push(activity),
      saveRunState: async state => states.push(state),
      requestTurn: async messages => {
        requests++;
        if (requests === 2) {
          assert.ok(messages.some(message => message.content?.includes('[KYNXA_OUTPUT_CONTINUATION]')));
          assert.ok(!messages.some(message => message.content === first), 'raw output replaced only after archival');
        }
        return decodeToolTurn(protocol, native(protocol, requests === 1 ? first : '3 第三行', requests === 1), [], { allowTruncatedText: true });
      } });
    assert.equal(requests, 2); assert.equal(result.content, first + '3 第三行');
    assert.equal(result.assistantSegments.at(-1).phase, 'final_answer');
    assert.equal(result.assistantSegments.at(-1).content, result.content);
    assert.equal(results.records[0].canonical.content, first);
    assert.ok(activities.some(activity => activity.name === 'context.compact' && activity.status === 'completed'));
    assert.equal(states.at(-1).diagnostics.executedToolCalls, 0);
    assert.ok(events.some(event => event.type === 'tool_result' && event.tool.name === 'context.compact'));
  });
  test(`${protocol}: truncated tool calls remain non-executable with automatic text continuation enabled`, () => {
    const raw = native(protocol, 'Partial draft', true);
    if (protocol === 'anthropic-messages') raw.content.push({ type: 'tool_use', id: 'unsafe', name: 'write', input: {} });
    else if (protocol === 'openai-responses') raw.output.push({ type: 'function_call', call_id: 'unsafe', name: 'write', arguments: '{}' });
    else raw.choices[0].message.tool_calls = [{ id: 'unsafe', function: { name: 'write', arguments: '{}' } }];
    assert.throws(() => decodeToolTurn(protocol, raw, [], { allowTruncatedText: true }), { code: 'MODEL_TOOL_OUTPUT_TRUNCATED' });
  });
}

test('streaming length receipts continue while a dropped stream is not mistaken for a length receipt', async () => {
  const frame = value => `data: ${JSON.stringify(value)}\n\n`;
  const response = new Response(frame({ choices: [{ index: 0, delta: { content: 'partial' }, finish_reason: 'length' }] }),
    { headers: { 'content-type': 'text/event-stream' } });
  const turn = await readToolStream(response, 'openai-completions', [], () => {}, () => {}, { allowTruncatedText: true });
  assert.equal(turn.outputTruncated, true); assert.equal(turn.content, 'partial');
  const dropped = new Response(frame({ choices: [{ index: 0, delta: { content: 'partial' } }] }),
    { headers: { 'content-type': 'text/event-stream' } });
  await assert.rejects(readToolStream(dropped, 'openai-completions', [], () => {}, () => {}, { allowTruncatedText: true }));
});

test('rolling checkpoints remain bounded, keep the original task, and preserve complete archived output', async () => {
  const results = archiveFixture(), original = { role: 'user', content: 'Keep the original task.' };
  const continuation = new OutputContinuation({ context: {}, resultStore: results });
  let messages = [original];
  for (let round = 1; round <= 4; round++) {
    const resumed = await continuation.resume(messages, { content: `${round}:` + '正文'.repeat(2000),
      finish: 'length', outputTruncated: true, calls: [] }, { round, inputBudgetTokens: 2048 });
    messages = resumed.messages;
    assert.equal(messages.length, 3); assert.equal(messages[0], original);
    assert.ok(resumed.audit.checkpointCharacters < 2000);
  }
  assert.ok(results.records.at(-1).canonical.content.length > 16000);
});

test('archive failure or cancellation prevents another model dispatch', async () => {
  const cancelled = new AbortController(); cancelled.abort();
  const continuation = new OutputContinuation({ resultStore: { save: () => assert.fail('cancelled') } });
  await assert.rejects(continuation.resume([], { content: 'text', outputTruncated: true, finish: 'length', calls: [] },
    { signal: cancelled.signal }), { name: 'AbortError' });
  const unavailable = new OutputContinuation();
  await assert.rejects(unavailable.resume([], { content: 'text', outputTruncated: true, finish: 'length', calls: [] }),
    { code: 'OUTPUT_CONTINUATION_ARCHIVE_UNAVAILABLE' });
});

test('a lookup between chunks preserves the draft and the complete existing call/result pair', async () => {
  const results = archiveFixture(), original = { role: 'user', content: 'Write a long answer using verified evidence.' };
  const callMessage = { role: 'assistant', tool_calls: [{ id: 'read-1', type: 'function',
    function: { name: 'read', arguments: '{}' } }] };
  const resultMessage = { role: 'tool', tool_call_id: 'read-1', content: 'Verified receipt.' };
  const continuation = new OutputContinuation({ resultStore: results });
  let resumed = await continuation.resume([original, callMessage, resultMessage], {
    content: 'Beginning.', calls: [], outputTruncated: true, finish: 'length' }, { round: 1 });
  assert.ok(resumed.messages.includes(callMessage)); assert.ok(resumed.messages.includes(resultMessage));
  assert.equal(continuation.finalText({ content: 'Read evidence.', calls: [{ name: 'read' }] }), 'Read evidence.');
  assert.equal(continuation.finalText({ content: 'Conclusion.', calls: [] }), 'Beginning.Conclusion.');
});

import assert from 'node:assert/strict';
import { test } from 'node:test';
import { readToolStream } from '../models/tool-streaming.mjs';
import { wireCatalog } from '../models/tool-protocols.mjs';

const frame = (item, type) => `${type ? `event: ${type}\n` : ''}data: ${JSON.stringify(item)}\n\n`;
const response = body => new Response(body, { headers: { 'content-type': 'text/event-stream' } });

test('tool-enabled Chat Completions retains text and reasoning content arrays', async () => {
  const events = [];
  const body = frame({ choices: [{ index: 0, delta: {
    content: [{ type: 'text', text: 'Array content ' }, { type: 'text', text: 'fixture' }],
    reasoning_content: [{ type: 'text', text: 'Public array summary' }]
  } }] }) + frame({ choices: [{ index: 0, delta: {}, finish_reason: 'stop' }] });
  const turn = await readToolStream(response(body), 'openai-completions', [], event => events.push(event));
  assert.equal(turn.content, 'Array content fixture');
  assert.equal(turn.reasoning, 'Public array summary');
  assert.equal(events.filter(event => event.type === 'text_delta').map(event => event.delta).join(''), turn.content);
  assert.equal(events.filter(event => event.type === 'reasoning_delta').map(event => event.delta).join(''), turn.reasoning);
  assert.equal(turn.calls.length, 0);
});

test('tool-enabled Responses accepts SSE event names when JSON omits type', async () => {
  const events = [];
  const body = frame({ delta: 'Event-only content' }, 'response.output_text.delta') +
    frame({ delta: 'Public event summary' }, 'response.reasoning_summary_text.delta') +
    frame({ response: { status: 'completed', output: [
      { type: 'message', role: 'assistant', content: [{ type: 'output_text', text: 'Event-only content' }] },
      { type: 'reasoning', summary: [{ type: 'summary_text', text: 'Public event summary' }] }
    ] } }, 'response.completed');
  const turn = await readToolStream(response(body), 'openai-responses', [], event => events.push(event));
  assert.equal(turn.content, 'Event-only content');
  assert.equal(turn.reasoning, 'Public event summary');
  assert.deepEqual(events.map(event => event.type), ['text_delta', 'reasoning_delta']);
  assert.equal(turn.calls.length, 0);
});

test('Anthropic max_tokens takes precedence over unfinished JSON arguments and keeps returned text', async () => {
  const events = [], catalog = wireCatalog([{ name: 'filesystem.write', description: 'Write', inputSchema: { type: 'object' } }]);
  const body = frame({ type: 'content_block_start', index: 0, content_block: { type: 'text', text: 'partial answer' } })
    + frame({ type: 'content_block_start', index: 1, content_block: { type: 'tool_use', id: 'call1', name: catalog[0].wireName, input: {} } })
    + frame({ type: 'content_block_delta', index: 1, delta: { type: 'input_json_delta', partial_json: '{"path":' } })
    + frame({ type: 'message_delta', delta: { stop_reason: 'max_tokens' } }) + frame({ type: 'message_stop' });
  await assert.rejects(readToolStream(response(body), 'anthropic-messages', catalog, item => events.push(item)),
    error => error.type === 'interrupted' && /上限/.test(error.message));
  assert.equal(events.find(item => item.type === 'text_delta').delta, 'partial answer');
});

test('Responses incomplete final snapshot is retained without accepting valid-looking tool calls', async () => {
  const events = [], catalog = wireCatalog([{ name: 'filesystem.write', description: 'Write', inputSchema: { type: 'object' } }]);
  const body = frame({ type: 'response.output_text.delta', delta: 'draft' }) + frame({ type: 'response.incomplete', response: {
    status: 'incomplete', incomplete_details: { reason: 'max_output_tokens' }, output: [
      { type: 'message', content: [{ type: 'output_text', text: 'full partial response' }] },
      { type: 'function_call', call_id: 'call1', name: catalog[0].wireName, arguments: '{"path":"never.txt","content":"never"}' }
    ] } });
  await assert.rejects(readToolStream(response(body), 'openai-responses', catalog, item => events.push(item)),
    error => error.type === 'interrupted');
  assert.equal(events.at(-1).type, 'content_snapshot');
  assert.equal(events.at(-1).content, 'full partial response');
});

test('tool argument buffers remain charged when stream identity fails before a complete call', async () => {
  const catalog = wireCatalog([{ name: 'filesystem.write', description: 'Write', inputSchema: { type: 'object' } }]);
  const body = frame({ choices: [{ index: 0, delta: { tool_calls: [{ index: 0, id: 'first',
    function: { name: catalog[0].wireName, arguments: JSON.stringify({ content: 'generated argument '.repeat(1000) }) } }] } }] })
    + frame({ choices: [{ index: 0, delta: { tool_calls: [{ index: -1, id: 'invalid', function: { arguments: '{}' } }] } }] });
  await assert.rejects(readToolStream(response(body), 'openai-completions', catalog), error =>
    error.code === 'MODEL_TOOL_INDEX_INVALID' && error.executed === false && error.estimatedGeneratedTokens > 1024);
});

test('unbound Anthropic tool arguments and duplicate tool blocks request repair without guessing identity', async () => {
  const unbound = frame({ type: 'content_block_delta', index: 99,
    delta: { type: 'input_json_delta', partial_json: '{"path":"never.txt"}' } });
  await assert.rejects(readToolStream(response(unbound), 'anthropic-messages', []),
    { code: 'MODEL_TOOL_IDENTITY_INVALID', executed: false });
  const tool = frame({ type: 'content_block_start', index: 99,
    content_block: { type: 'tool_use', id: 'first', name: 'synthetic', input: {} } });
  await assert.rejects(readToolStream(response(tool + tool), 'anthropic-messages', []),
    { code: 'MODEL_TOOL_INDEX_INVALID', executed: false });
});

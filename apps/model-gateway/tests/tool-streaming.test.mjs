import assert from 'node:assert/strict';
import { test } from 'node:test';
import { readToolStream } from '../tool-streaming.mjs';

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

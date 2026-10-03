import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { randomUUID } from 'node:crypto';
import { writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from 'node:test';
import { setTimeout as delay } from 'node:timers/promises';
import { ModelRuntime } from '../runtime.mjs';
import { ModelStore } from '../store.mjs';
import { storedReplyDurationMs } from '../reply-timing.mjs';
import { createModelServer } from '../server.mjs';
import { readSse } from '../streaming.mjs';
import { toolFixture } from './tool-fixture.mjs';

function text(protocol, content, truncated = false) {
  if (protocol === 'anthropic-messages') return { stop_reason: truncated ? 'max_tokens' : 'end_turn', content: [{ type: 'text', text: content }] };
  if (protocol === 'openai-responses') return { status: truncated ? 'incomplete' : 'completed', ...(truncated ? { incomplete_details: { reason: 'max_output_tokens' } } : {}),
    output: [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: content }] }] };
  return { choices: [{ finish_reason: truncated ? 'length' : 'stop', message: { role: 'assistant', content } }] };
}
function call(protocol, name) {
  if (protocol === 'anthropic-messages') return { stop_reason: 'tool_use', content: [{ type: 'tool_use', id: 'duration_call', name, input: { path: 'note.txt' } }] };
  if (protocol === 'openai-responses') return { status: 'completed', output: [{ type: 'function_call', call_id: 'duration_call', name, arguments: '{"path":"note.txt"}' }] };
  return { choices: [{ finish_reason: 'tool_calls', message: { role: 'assistant', content: '', tool_calls: [{ id: 'duration_call', type: 'function',
    function: { name, arguments: '{"path":"note.txt"}' } }] } }] };
}
async function fixture(t, protocol, { tool = false, truncated = false } = {}) {
  const f = await toolFixture(t); await writeFile(join(f.workspace, 'note.txt'), 'Public tool output');
  let requests = 0;
  const upstream = createServer(async (request, response) => {
    let raw = ''; for await (const chunk of request) raw += chunk;
    const body = JSON.parse(raw); requests++;
    await delay(80);
    const descriptor = body.tools?.find(item => (item.description ?? item.function?.description).startsWith('filesystem.read:'));
    const result = tool && requests === 1 ? call(protocol, descriptor.name ?? descriptor.function.name) : text(protocol, 'Final answer.', truncated);
    response.writeHead(200, { 'Content-Type': 'application/json' }); response.end(JSON.stringify(result));
  });
  await new Promise(resolve => upstream.listen(0, '127.0.0.1', resolve));
  const store = new ModelStore({ dataHome: f.dataHome });
  await store.save({ providerId: 'duration-fixture', displayName: 'Fixture', protocol, baseUrl: `http://127.0.0.1:${upstream.address().port}/v1`,
    models: ['model'], contextWindowTokens: 32768, maxOutputTokens: 2048 });
  const runtime = new ModelRuntime({ modelStore: store, dataHome: f.dataHome, conversationStore: f.conversations, toolService: f.service });
  t.after(async () => { await runtime.close(); upstream.closeAllConnections(); await new Promise(resolve => upstream.close(resolve)); });
  const input = { conversationId: f.conversationId, requestId: randomUUID(), userMessageId: randomUUID(), provider: 'duration-fixture', model: 'model',
    message: 'Read this note.', ...(tool ? { permissionMode: 'ask' } : {}) };
  return { ...f, runtime, input, requests: () => requests };
}

async function gateway(t, runtime) {
  const server = createModelServer({ modelStore: runtime.store, modelRuntime: runtime });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(async () => { server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); });
  return `http://127.0.0.1:${server.address().port}`;
}

test('real HTTP and SSE terminal events transmit the same saved duration on cross-transport replay', async t => {
  const f = await fixture(t, 'openai-completions'), url = await gateway(t, f.runtime);
  const response = await fetch(url + '/api/chat', { method: 'POST', body: JSON.stringify(f.input) });
  assert.equal(response.status, 200); const reply = await response.json(); assert.ok(reply.durationMs >= 70);
  const responseStream = await fetch(url + '/api/chat/stream', { method: 'POST', body: JSON.stringify(f.input) });
  const events = []; for await (const event of readSse(responseStream.body)) events.push(JSON.parse(event.data));
  assert.equal(events.at(-1).type, 'completed'); assert.equal(events.at(-1).durationMs, reply.durationMs);
  assert.equal(f.requests(), 1);
});

test('real failed SSE terminal reports the persisted interrupted duration', async t => {
  const f = await fixture(t, 'openai-completions', { truncated: true }), url = await gateway(t, f.runtime);
  const response = await fetch(url + '/api/chat/stream', { method: 'POST', body: JSON.stringify(f.input) });
  const events = []; for await (const event of readSse(response.body)) events.push(JSON.parse(event.data));
  assert.equal(events.at(-1).type, 'interrupted'); assert.ok(events.at(-1).durationMs >= 70);
  const saved = (await f.conversations.readMessages(f.conversationId)).at(-1);
  assert.equal(saved.Status, 'interrupted'); assert.equal(saved.DurationMs, events.at(-1).durationMs);
});

for (const protocol of ['openai-completions', 'openai-responses', 'anthropic-messages']) {
  test(`${protocol}: nonstreaming timing is stored once and replayed without a new generation`, async t => {
    const f = await fixture(t, protocol), reply = await f.runtime.replyResult(f.input);
    assert.equal(reply.content, 'Final answer.'); assert.ok(Number.isSafeInteger(reply.durationMs) && reply.durationMs >= 70);
    const saved = (await f.conversations.readMessages(f.conversationId)).at(-1);
    assert.equal(saved.DurationMs, reply.durationMs); assert.equal(saved.ReasoningDurationMs, 0);
    await delay(30);
    assert.deepEqual(await f.runtime.replyResult(f.input), reply); assert.equal(f.requests(), 1);
    assert.equal((await f.runtime.replyStream(f.input, () => {})).durationMs, reply.durationMs);
  });
  test(`${protocol}: stream timing includes all model rounds and tool execution and remains stable on replay`, async t => {
    const f = await fixture(t, protocol, { tool: true });
    const execute = f.service.execute.bind(f.service);
    f.service.execute = async (...args) => { await delay(70); return execute(...args); };
    const reply = await f.runtime.replyStream(f.input, () => {});
    assert.ok(reply.durationMs >= 220); assert.equal(f.requests(), 2);
    const saved = (await f.conversations.readMessages(f.conversationId)).at(-1);
    assert.equal(saved.DurationMs, reply.durationMs); assert.equal(saved.Status, 'completed'); assert.equal(saved.ToolActivities[0].status, 'completed');
    assert.equal((await f.runtime.replyResult(f.input)).durationMs, reply.durationMs); assert.equal(f.requests(), 2);
  });
}

test('cancel after a returned tool result retains its completed receipt and records interrupted elapsed time', async t => {
  const f = await fixture(t, 'openai-completions', { tool: true }), controller = new AbortController();
  const execute = f.service.execute.bind(f.service);
  f.service.execute = async (...args) => { await delay(70); const result = await execute(...args); controller.abort(); return result; };
  let failure;
  await assert.rejects(f.runtime.replyStream(f.input, () => {}, controller.signal), error => { failure = error; return error.type === 'interrupted'; });
  const saved = (await f.conversations.readMessages(f.conversationId)).at(-1);
  assert.ok(failure.durationMs >= 140); assert.equal(saved.DurationMs, failure.durationMs);
  assert.equal(saved.Status, 'interrupted'); assert.equal(saved.ToolActivities[0].status, 'completed'); assert.equal(f.requests(), 1);
});

test('truncation preserves failure duration without claiming completion', async t => {
  const f = await fixture(t, 'openai-completions', { truncated: true });
  let failure;
  await assert.rejects(f.runtime.replyResult(f.input), error => { failure = error; return error.type === 'interrupted'; });
  const saved = (await f.conversations.readMessages(f.conversationId)).at(-1);
  assert.ok(failure.durationMs >= 70); assert.equal(saved.DurationMs, failure.durationMs); assert.equal(saved.Status, 'interrupted');
});

test('duration validation and legacy projection never use the current time or unfinished tool receipts', async t => {
  const f = await toolFixture(t);
  assert.equal(storedReplyDurationMs({ CreatedAt: '2000-01-01T00:00:00Z' }), 0);
  const old = { Role: 'assistant', Status: 'completed', ToolRun: { version: 1, phase: 'completed', startedAt: '2020-01-01T00:00:00.000Z', updatedAt: '2020-01-01T00:00:02.345Z' } };
  assert.equal(storedReplyDurationMs(old), 2345); assert.equal(storedReplyDurationMs({ ...old, ToolRun: { ...old.ToolRun, phase: 'model' } }), 0);
  assert.equal(storedReplyDurationMs({ ...old, DurationMs: 500 }), 500);
  for (const value of [-1, 1.5, '3', Number.MAX_SAFE_INTEGER + 1])
    await assert.rejects(f.conversations.upsertMessage(f.conversationId, { Id: randomUUID(), Role: 'assistant', Content: 'Invalid time', DurationMs: value }),
      error => error.code === 'INVALID_CONVERSATION_DATA');
  await f.conversations.upsertMessage(f.conversationId, { ...old, Id: randomUUID(), Content: 'Legacy answer' });
  assert.equal((await f.conversations.readMessages(f.conversationId)).at(-1).DurationMs, 2345);
});

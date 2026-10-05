import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { randomUUID } from 'node:crypto';
import { readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { test } from 'node:test';
import { ConversationStore } from '../data/conversations.mjs';
import { ModelRuntime } from '../orchestration/runtime.mjs';
import { ModelStore } from '../models/store.mjs';
import { createModelServer } from '../server.mjs';
import { readSse } from '../models/streaming.mjs';
import { toolFixture } from './tool-fixture.mjs';

const protocols = ['openai-completions', 'openai-responses', 'anthropic-messages'];
const evidence = 'Stable isolated source evidence.';
const answer = 'The source says: ' + evidence;
async function listen(server) {
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  return `http://127.0.0.1:${server.address().port}`;
}
async function close(server) {
  server.closeAllConnections(); await new Promise(resolve => server.close(resolve));
}
function nativeTool(protocol, round, name, args) {
  const id = 'progress_call_' + round, content = 'Public step ' + round + '.';
  if (protocol === 'anthropic-messages') return { stop_reason: 'tool_use', content: [
    { type: 'thinking', thinking: 'Public thinking summary', signature: 'PRIVATE_SIGNATURE_' + round },
    { type: 'text', text: content }, { type: 'tool_use', id, name, input: args }] };
  if (protocol === 'openai-responses') return { status: 'completed', output: [
    { type: 'reasoning', id: 'reasoning_' + round, encrypted_content: 'PRIVATE_ENCRYPTED_' + round, summary: [] },
    { type: 'message', role: 'assistant', content: [{ type: 'output_text', text: content }] },
    { type: 'function_call', id: 'function_' + round, call_id: id, name, arguments: JSON.stringify(args) }] };
  return { choices: [{ finish_reason: 'tool_calls', message: { role: 'assistant', content, reasoning_content: 'Private native continuation only',
    tool_calls: [{ id, type: 'function', function: { name, arguments: JSON.stringify(args) } }] } }] };
}
function nativeText(protocol, content) {
  if (protocol === 'anthropic-messages') return { stop_reason: 'end_turn', content: [{ type: 'text', text: content }] };
  if (protocol === 'openai-responses') return { status: 'completed', output: [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: content }] }] };
  return { choices: [{ finish_reason: 'stop', message: { role: 'assistant', content } }] };
}
function assertPaired(body, protocol, expected) {
  const messages = body.messages ?? body.input;
  let calls, results;
  if (protocol === 'anthropic-messages') {
    const blocks = messages.flatMap(message => Array.isArray(message.content) ? message.content : []);
    calls = blocks.filter(block => block.type === 'tool_use').map(block => block.id);
    results = blocks.filter(block => block.type === 'tool_result').map(block => block.tool_use_id);
    assert.equal(blocks.filter(block => block.type === 'thinking').length, expected);
    for (let round = 1; round <= expected; round++) assert.ok(blocks.some(block => block.signature === 'PRIVATE_SIGNATURE_' + round));
  } else if (protocol === 'openai-responses') {
    calls = messages.filter(message => message.type === 'function_call').map(message => message.call_id);
    results = messages.filter(message => message.type === 'function_call_output').map(message => message.call_id);
    for (let round = 1; round <= expected; round++) assert.ok(messages.some(message => message.encrypted_content === 'PRIVATE_ENCRYPTED_' + round));
  } else {
    calls = messages.flatMap(message => (message.tool_calls ?? []).map(call => call.id));
    results = messages.filter(message => message.role === 'tool').map(message => message.tool_call_id);
    assert.equal(messages.filter(message => message.reasoning_content === 'Private native continuation only').length, expected);
  }
  assert.deepEqual(calls, Array.from({ length: expected }, (_, index) => 'progress_call_' + (index + 1)));
  assert.deepEqual(results, calls); assert.equal(new Set(calls).size, calls.length);
  if (expected) assert.match(JSON.stringify(messages), /Stable isolated source evidence|made\.txt/);
}

async function fixture(t, protocol, { ignoreFinal = false, write = false } = {}) {
  const f = await toolFixture(t); await writeFile(join(f.workspace, 'note.txt'), evidence);
  const seen = []; let wireName;
  const upstream = createServer(async (request, response) => {
    try {
      let raw = ''; for await (const chunk of request) raw += chunk;
      const body = JSON.parse(raw); seen.push(body);
      const round = seen.length, stopRound = write ? 2 : 5;
      assertPaired(body, protocol, round - 1);
      await delay(10);
      if (round === 1) {
        const descriptor = body.tools.find(tool => (tool.description ?? tool.function?.description).startsWith((write ? 'filesystem.write' : 'filesystem.read') + ':'));
        assert.ok(descriptor); wireName = descriptor.name ?? descriptor.function.name;
      }
      if (!write && round === 4) assert.match(JSON.stringify(body), /KYNXA_NO_PROGRESS_WARNING/);
      if (!write && round === 5) {
        assert.equal((body.tools ?? []).length, 0); assert.match(JSON.stringify(body), /KYNXA_NO_PROGRESS_FINAL/);
      }
      const shouldCall = round < stopRound || ignoreFinal;
      const result = shouldCall ? nativeTool(protocol, round, wireName,
        write ? { path: 'made.txt', content: 'Written through an approved broker.', expectedHash: null } : { path: 'note.txt' })
        : nativeText(protocol, write ? 'The approved file was written.' : answer);
      response.writeHead(200, { 'Content-Type': 'application/json' }); response.end(JSON.stringify(result));
    } catch (error) {
      response.writeHead(500, { 'Content-Type': 'application/json' }); response.end(JSON.stringify({ error: error.message }));
    }
  });
  const upstreamUrl = await listen(upstream); t.after(() => close(upstream));
  const models = new ModelStore({ dataHome: f.dataHome });
  await models.save({ providerId: 'progress-fixture', displayName: 'Progress fixture', protocol,
    baseUrl: upstreamUrl + '/v1', models: ['model'], contextWindowTokens: 32768, maxOutputTokens: 2048 });
  const runtime = new ModelRuntime({ modelStore: models, dataHome: f.dataHome, conversationStore: f.conversations, toolService: f.service });
  const gateway = createModelServer({ modelStore: models, modelRuntime: runtime }), address = await listen(gateway);
  t.after(async () => { await gateway.shutdownModelRuntime(); await close(gateway); });
  let executions = 0; const execute = f.service.execute.bind(f.service);
  f.service.execute = async (...args) => { executions++; return execute(...args); };
  const input = { conversationId: f.conversationId, requestId: randomUUID(), userMessageId: randomUUID(), provider: 'progress-fixture',
    model: 'model', message: write ? 'Write the approved workspace file.' : 'Read this source and answer from evidence.', permissionMode: 'ask' };
  const post = (path, payload) => fetch(address + path, { method: 'POST', body: JSON.stringify(payload) });
  async function saved() { return (await f.conversations.readMessages(f.conversationId)).find(message => message.Id === input.requestId); }
  async function run() {
    const response = await fetch(address + '/api/conversations/' + f.conversationId + '/runs/' + input.requestId);
    assert.equal(response.status, 200); return (await response.json()).run;
  }
  return { ...f, runtime, input, seen, post, saved, run, executions: () => executions };
}

for (const protocol of protocols) test(`${protocol}: real HTTP stops stagnant file reads, returns evidence and persists numeric diagnostics with native pairs intact`, async t => {
  const f = await fixture(t, protocol), response = await f.post('/api/chat', f.input);
  assert.equal(response.status, 200); const reply = await response.json(); assert.equal(reply.content, answer);
  assert.equal(f.seen.length, 5); assert.equal(f.executions(), 4);
  const saved = await f.saved(), run = await f.run(), diagnostics = run.diagnostics;
  assert.equal(saved.Status, 'completed'); assert.equal(saved.Content, answer); assert.equal(saved.DurationMs, reply.durationMs);
  assert.equal(saved.ToolActivities.length, 4); assert.ok(saved.ToolActivities.every(tool => tool.status === 'completed' && tool.resultRef && !tool.reused));
  assert.equal(saved.AssistantSegments.length, 5); assert.equal(saved.AssistantSegments.at(-1).phase, 'final_answer');
  assert.equal(saved.ToolRun.phase, 'completed'); assert.deepEqual(saved.ToolRun, run);
  assert.equal(diagnostics.modelCalls, 5); assert.equal(diagnostics.executedToolCalls, 4); assert.equal(diagnostics.reusedToolCalls, 0);
  assert.equal(diagnostics.noProgressRounds, 3); assert.equal(diagnostics.totalApprovalWaitMs, 0);
  assert.ok(diagnostics.totalModelMs >= 40); assert.ok(diagnostics.totalToolMs > 0);
  assert.equal(diagnostics.modelRounds.length, 5); assert.equal(diagnostics.toolCallsTiming.length, 4);
  assert.doesNotMatch(JSON.stringify(saved), /PRIVATE_SIGNATURE|PRIVATE_ENCRYPTED/);
  assert.doesNotMatch(JSON.stringify(diagnostics), /note\.txt|filesystem\.read|Stable isolated|PRIVATE_/);
  const reopened = new ConversationStore({ dataHome: f.dataHome, legacyDesktopDirectory: null });
  assert.deepEqual((await reopened.readMessages(f.conversationId)).find(message => message.Id === f.input.requestId).ToolRun, run);
  const repeated = await f.post('/api/chat', f.input); assert.equal(repeated.status, 200); assert.deepEqual((await repeated.json()).content, answer);
  assert.equal(f.seen.length, 5); assert.equal(f.executions(), 4);
});

test('a model ignoring the tools-disabled final round completes with a limitation and no fifth execution', async t => {
  const f = await fixture(t, 'openai-completions', { ignoreFinal: true });
  const response = await f.post('/api/chat', f.input), reply = await response.json();
  assert.equal(response.ok, true); assert.match(reply.content, /unavailable/);
  assert.equal(f.seen.length, 5); assert.equal(f.executions(), 4);
  const saved = await f.saved(), run = await f.run();
  assert.equal(saved.Status, 'completed'); assert.equal(run.phase, 'completed');
  assert.equal(saved.Content, reply.content); assert.equal(saved.AssistantSegments.at(-1).phase, 'final_answer');
  assert.equal(saved.ToolActivities.length, 5);
  assert.ok(saved.ToolActivities.slice(0, 4).every(tool => tool.status === 'completed' && tool.resultRef));
  const rejected = saved.ToolActivities.at(-1);
  assert.equal(rejected.status, 'error'); assert.equal(rejected.code, 'MODEL_TOOL_UNAVAILABLE');
  assert.equal(JSON.parse(rejected.result).executed, false);
  assert.equal(run.diagnostics.modelCalls, 5); assert.equal(run.diagnostics.executedToolCalls, 4); assert.equal(run.diagnostics.noProgressRounds, 3);
  const privateMessages = await f.conversations.readModelMessages(f.conversationId);
  assert.equal(privateMessages.find(message => message.Id === f.input.requestId).ModelTranscript.rounds.at(-1).calls[0].id, 'progress_call_5');
  const next = await f.runtime.prepare({ ...f.input, requestId: randomUUID(), userMessageId: randomUUID(),
    message: 'Continue the same conversation.' }, f.conversationId);
  assert.ok(JSON.stringify(next.messages).includes(saved.Content));
  assert.ok(JSON.stringify(next.messages).includes('MODEL_TOOL_UNAVAILABLE'));
  await f.runtime.tools.releaseContext(next.toolContext);
  const retry = await f.post('/api/chat', f.input); assert.equal(retry.ok, true);
  assert.equal((await retry.json()).content, reply.content); assert.equal(f.executions(), 4); assert.equal(f.seen.length, 5);
});

test('a real Ask approval measures only the actual approval wait and persists it through SSE completion', async t => {
  const f = await fixture(t, 'openai-completions', { write: true });
  const response = await f.post('/api/chat/stream', f.input), events = [];
  for await (const raw of readSse(response.body)) {
    const event = JSON.parse(raw.data); events.push(event);
    if (event.type === 'approval_required') {
      await assert.rejects(readFile(join(f.workspace, 'made.txt')), { code: 'ENOENT' });
      await delay(75);
      const approval = await f.post('/api/agent/approvals', { conversationId: f.conversationId, requestId: f.input.requestId,
        toolCallId: event.tool.toolCallId, approvalId: event.tool.approvalId, approved: true });
      assert.equal(approval.status, 200);
    }
  }
  assert.equal(events.at(-1).type, 'completed'); assert.equal(events.at(-1).toolStreamProtocol, 3);
  assert.equal(events.at(-1).assistantSegments.at(-1).phase, 'final_answer');
  assert.equal(await readFile(join(f.workspace, 'made.txt'), 'utf8'), 'Written through an approved broker.');
  const saved = await f.saved(), run = await f.run(), diagnostics = run.diagnostics;
  assert.equal(saved.DurationMs, events.at(-1).durationMs); assert.equal(saved.ToolActivities[0].status, 'completed');
  assert.ok(diagnostics.totalApprovalWaitMs >= 70); assert.equal(diagnostics.executedToolCalls, 1); assert.equal(diagnostics.modelCalls, 2);
  assert.equal(diagnostics.totalToolMs + diagnostics.totalApprovalWaitMs, diagnostics.toolCallsTiming[0].durationMs);
  assert.equal(diagnostics.maxApprovalWaitMs, diagnostics.totalApprovalWaitMs); assert.equal(diagnostics.noProgressRounds, 0);
  assert.doesNotMatch(JSON.stringify(diagnostics), /approvalId|made\.txt|Written through|filesystem\.write/);
});

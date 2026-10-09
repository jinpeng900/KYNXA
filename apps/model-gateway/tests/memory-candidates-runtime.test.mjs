import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { randomUUID } from 'node:crypto';
import { stat } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from 'node:test';
import { MemoryService } from '../data/memory-service.mjs';
import { ModelRuntime } from '../orchestration/runtime.mjs';
import { ModelStore } from '../models/store.mjs';
import { createModelServer } from '../orchestration/server.mjs';
import { toolFixture } from './tool-fixture.mjs';

async function runtimeFixture(t) {
  const f = await toolFixture(t), requests = [];
  const upstream = createServer(async (request, response) => {
    let body = ''; for await (const part of request) body += part;
    requests.push(JSON.parse(body));
    response.setHeader('Content-Type', 'application/json');
    response.end(JSON.stringify({ choices: [{ finish_reason: 'stop', message: { role: 'assistant', content: 'Fixture answer; model claims are not memory.' } }] }));
  });
  await new Promise(resolve => upstream.listen(0, '127.0.0.1', resolve));
  t.after(() => new Promise(resolve => { upstream.closeAllConnections(); upstream.close(resolve); }));
  const store = new ModelStore({ dataHome: f.dataHome });
  await store.save({ providerId: 'fixture', displayName: 'Fixture', baseUrl: `http://127.0.0.1:${upstream.address().port}/v1`,
    models: ['mock-model'], contextWindowTokens: 32768, maxOutputTokens: 1024 });
  // Empty resources and a mocked change consumer forbid native resource/model admission in this fixture.
  // 空资源适配器和模拟变更消费者确保夹具不启动原生资源服务或离线模型。
  const runtime = new ModelRuntime({ modelStore: store, conversationStore: f.conversations, toolService: f.service,
    dataHome: f.dataHome, resourceService: {} });
  const changes = [];
  runtime.retrieval.onMemoryChanged = async event => { changes.push(event); };
  await runtime.retrieval.settings.patchGlobal({ expectedRevision: 0, patch: { local: { semantic: 'off' } } });
  t.after(() => runtime.close());
  return { ...f, runtime, store, requests, changes };
}

for (const streamed of [false, true]) test(`${streamed ? 'stream' : 'JSON'} production replies schedule user/completed candidates only once after durable records`, async t => {
  const f = await runtimeFixture(t), phases = [];
  const schedule = f.runtime.memory.scheduleCandidates.bind(f.runtime.memory);
  f.runtime.memory.scheduleCandidates = (id, options) => { phases.push(options.phase); return schedule(id, options); };
  const input = { conversationId: f.conversationId, requestId: randomUUID(), userMessageId: randomUUID(),
    provider: 'fixture', model: 'mock-model', message: '纠正：只在当前聊天保留候选原话。' };
  const reply = () => streamed ? f.runtime.replyStream(input, () => {}) : f.runtime.reply(input);
  await reply(); await f.runtime.memory.flushCandidates();
  assert.deepEqual(phases, ['user', 'completed']); assert.equal(f.requests.length, 1);
  const messages = await f.conversations.readMessages(f.conversationId);
  assert.equal(messages.find(message => message.Id === input.requestId).Status, 'completed');
  const document = await f.runtime.memory.repository.read(f.conversationId, 'chat');
  assert.equal(document.entries.length, 1); assert.equal(document.entries[0].status, 'draft');
  assert.equal(document.entries[0].candidate.quotes[0].text, input.message);
  assert.equal(f.changes.length, 1); assert.equal(f.changes[0].status, 'draft');
  await reply(); await f.runtime.memory.flushCandidates();
  assert.deepEqual(phases, ['user', 'completed']); assert.equal(f.requests.length, 1);
  assert.deepEqual((await f.runtime.memory.contextFor(f.conversationId)).entries, []);
});

test('completed runtime stages flag only explicit execution with broker-owned passed checks', () => {
  const scheduled = [], runtime = Object.create(ModelRuntime.prototype);
  runtime.scheduleMemoryCandidates = (id, options) => scheduled.push({ id, ...options });
  const turn = { conversationId: 'chat', memoryTaskExecution: true, assistant: { ReplyTo: 'source' } };
  runtime.scheduleCompletedMemory(turn, { taskCompletion: { state: 'checks-passed' } });
  runtime.scheduleCompletedMemory(turn, { taskCompletion: { state: 'needs-validation' } });
  runtime.scheduleCompletedMemory({ ...turn, memoryTaskExecution: false }, { taskCompletion: { state: 'checks-passed' } });
  runtime.scheduleCompletedMemory(turn, { completionStatus: 'interrupted', taskCompletion: { state: 'checks-passed' } });
  assert.deepEqual(scheduled.map(options => options.taskCompleted), [true, false, false]);
});

test('shutdown drains accepted memory jobs before unsubscribing and retiring retrieval', async () => {
  const events = [], runtime = Object.create(ModelRuntime.prototype);
  runtime.shutdown = new AbortController(); runtime.queues = new Map(); runtime.ownsResources = false;
  runtime.tools = { close: async () => { events.push('tools'); } };
  runtime.memory = { flushCandidates: async () => { events.push('memory-drained'); } };
  runtime.memoryChangeUnsubscribe = () => events.push('unsubscribe');
  runtime.externalAdmissions = { close: async () => events.push('external') };
  runtime.retrieval = { close: async () => events.push('retrieval') };
  await runtime.close();
  assert.deepEqual(events, ['memory-drained', 'unsubscribe', 'tools', 'memory-drained', 'external', 'retrieval']);
});

async function httpFixture(t) {
  const f = await toolFixture(t), memory = new MemoryService({ conversationStore: f.conversations });
  const store = new ModelStore({ dataHome: f.dataHome });
  const server = createModelServer({ modelStore: store, modelRuntime: { memory, conversations: f.conversations } });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(() => new Promise(resolve => { server.closeAllConnections(); server.close(resolve); }));
  const base = `http://127.0.0.1:${server.address().port}`;
  const api = async (method, path, input) => {
    const response = await fetch(base + path, { method, headers: { 'Content-Type': 'application/json' },
      body: input === undefined ? undefined : JSON.stringify(input) });
    return { status: response.status, body: await response.json() };
  };
  return { ...f, memory, api };
}

test('candidate settings HTTP persists only in the existing atomic user memory document and keeps a separate settings revision', async t => {
  const f = await httpFixture(t), path = '/api/memory/candidates/settings';
  const initial = await f.api('GET', path);
  assert.equal(initial.status, 200); assert.equal(initial.body.settings.enabled, true); assert.equal(initial.body.settingsRevision, 0);
  await assert.rejects(stat(join(f.conversations.root, 'Memory', 'entries.json')), { code: 'ENOENT' });
  assert.equal((await f.api('GET', '/health')).body.memoryCandidateProtocol, 1);
  const updated = await f.api('PATCH', path, { expectedRevision: 0, patch: { enabled: false, maxTurns: 10 } });
  assert.equal(updated.status, 200); assert.equal(updated.body.settingsRevision, 1); assert.equal(updated.body.settings.enabled, false);
  await f.memory.createScope('user', 'user', { content: 'Unrelated user memory' });
  assert.equal((await f.api('PATCH', path, { expectedRevision: 1, patch: { enabled: true } })).status, 200);
  assert.equal((await f.api('PATCH', path, { expectedRevision: 0, patch: { enabled: true } })).body.code, 'MEMORY_CONFLICT');
  assert.equal((await f.api('PATCH', path, { patch: { enabled: true } })).status, 400);
  const reopened = new MemoryService({ conversationStore: f.conversations });
  await reopened.initializeCandidates();
  assert.equal(reopened.candidateStatus().settings.maxTurns, 10); assert.equal(reopened.candidateStatus().settingsRevision, 2);
  assert.equal((await reopened.listScope('user', 'user')).entries[0].content, 'Unrelated user memory');
});

test('persisted disabled policy takes effect before the first scheduled user extraction after restart', async t => {
  const f = await httpFixture(t);
  await f.memory.updateCandidateSettings({ expectedRevision: 0, patch: { enabled: false } });
  const reopened = new MemoryService({ conversationStore: f.conversations });
  const messageId = randomUUID();
  await f.conversations.upsertMessage(f.conversationId, { Id: messageId, Role: 'user', Status: 'completed', Content: '纠正：持久禁用时不能提取' });
  assert.equal((await reopened.scheduleCandidates(f.conversationId, { phase: 'user', userMessageId: messageId })).reason, 'disabled');
  assert.deepEqual((await reopened.repository.read(f.conversationId, 'chat')).entries, []);
});

test('the existing HTTP PATCH confirms drafts without changing scope and returns CAS conflicts for stale editors', async t => {
  const f = await httpFixture(t), messageId = randomUUID(), raw = '工作决定：确认前仍是候选';
  await f.conversations.upsertMessage(f.conversationId, { Id: messageId, Role: 'user', Status: 'completed', Content: raw });
  await f.memory.scheduleCandidates(f.conversationId, { phase: 'user', userMessageId: messageId });
  const document = await f.memory.listScope('project', f.projectId), entry = document.entries[0];
  const path = `/api/projects/${f.projectId}/memory/${entry.id}`;
  const confirmed = await f.api('PATCH', path, { expectedRevision: document.revision, status: 'confirmed' });
  assert.equal(confirmed.status, 200); assert.equal(confirmed.body.entries[0].active, true); assert.equal(confirmed.body.entries[0].scopeId, f.projectId);
  assert.equal((await f.api('PATCH', path, { expectedRevision: document.revision, status: 'confirmed' })).status, 409);
});

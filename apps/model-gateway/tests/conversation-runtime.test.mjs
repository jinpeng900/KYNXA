import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { randomUUID } from 'node:crypto';
import { mkdtemp, access } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { ModelRuntime, completedContext } from '../orchestration/runtime.mjs';
import { ModelStore } from '../models/store.mjs';
import { createModelServer } from '../server.mjs';

async function listen(server, t) {
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(() => { server.closeAllConnections(); server.close(); });
  return `http://127.0.0.1:${server.address().port}`;
}

async function setup(t) {
  const seen = [];
  const endpoint = await listen(createServer(async (request, response) => {
    let body = '';
    for await (const chunk of request) body += chunk;
    seen.push(JSON.parse(body));
    response.setHeader('Content-Type', 'application/json');
    response.end(JSON.stringify({ choices: [{ message: { content: `answer-${seen.length}` } }] }));
  }), t);
  const dataHome = await mkdtemp(join(tmpdir(), 'kynxa-conversation-runtime-'));
  const modelStore = new ModelStore({ dataHome });
  for (const providerId of ['provider-a', 'provider-b'])
    await modelStore.save({ providerId, displayName: providerId, baseUrl: endpoint, models: ['model-a', 'model-b'] });
  let runtime = new ModelRuntime({ modelStore, dataHome });
  t.after(() => runtime.close());
  return { seen, dataHome, modelStore, get runtime() { return runtime; },
    restart: async () => { await runtime.close(); runtime = new ModelRuntime({ modelStore, dataHome }); } };
}

test('one transcript survives model/provider switches and restart; other conversations stay isolated', async t => {
  const f = await setup(t), conversationId = randomUUID();
  const first = { conversationId, provider: 'provider-a', model: 'model-a', message: 'remember my project', requestId: randomUUID() };
  await f.runtime.reply(first);
  await f.restart();
  await f.runtime.replyStream({ ...first, provider: 'provider-b', model: 'model-b', message: 'continue', requestId: randomUUID() }, () => {});
  // Derived task hints are separate system data; the provider-neutral dialogue remains exact.
  // 派生任务线索放在独立系统数据中，跨供应商对话原文仍须逐条保持一致。
  assert.deepEqual(f.seen[1].messages.filter(item => item.role !== 'system').map(item => item.content),
    ['remember my project', 'answer-1', 'continue']);
  assert.match(f.seen[1].messages.find(item => item.role === 'system').content, /"relation":"continue"/u);
  await f.runtime.reply({ ...first, message: 'back to first model', requestId: randomUUID() });
  assert.deepEqual(f.seen[2].messages.map(item => item.content), ['remember my project', 'answer-1', 'continue', 'answer-2', 'back to first model']);
  const log = await f.runtime.conversations.readMessages(conversationId);
  assert.equal(log.length, 6);
  assert.deepEqual(log.filter(item => item.Role === 'assistant').map(item => item.Model), ['model-a', 'model-b', 'model-a']);
  await f.runtime.reply({ ...first, conversationId: randomUUID(), message: 'different conversation', requestId: randomUUID() });
  assert.equal(f.seen.at(-1).messages.length, 1);
  await assert.rejects(access(join(f.dataHome, 'sessions')), { code: 'ENOENT' });
});

test('desktop catalog and transport share user IDs and final messages; stale UI cannot overwrite a reply', async t => {
  const f = await setup(t);
  const base = await listen(createModelServer({ modelStore: f.modelStore, modelRuntime: f.runtime }), t);
  const get = () => fetch(`${base}/api/conversations/catalog`).then(response => response.json());
  const save = body => fetch(`${base}/api/conversations/catalog`, { method: 'PUT', body: JSON.stringify(body) });
  const initial = await get();
  const projectId = randomUUID(), conversationId = randomUUID(), userMessageId = randomUUID(), requestId = randomUUID();
  const project = { Id: projectId, Name: '项目一', FolderPath: 'D:\\example', Chats: [
    { Id: conversationId, Title: 'first', Messages: [{ Id: userMessageId, Role: 'user', Content: 'hello' }] }
  ] };
  const saved = await save({ Revision: initial.Revision, Projects: [project] });
  assert.equal(saved.status, 200);
  const stale = await saved.json();
  await f.runtime.reply({ conversationId, userMessageId, requestId, provider: 'provider-a', model: 'model-a', message: 'hello' });
  project.Name = 'renamed';
  project.Chats[0].Messages.push({ Id: requestId, Role: 'assistant', Content: 'stale incomplete answer', Status: 'streaming' });
  assert.equal((await save({ Revision: stale.Revision, Projects: [project] })).status, 200);
  const updated = await get();
  assert.equal(updated.Projects[0].Name, 'renamed');
  assert.deepEqual(updated.Projects[0].Chats[0].Messages.map(message => message.Content), ['hello', 'answer-1']);
  assert.equal(updated.Projects[0].Chats[0].Messages[1].Status, 'completed');
  await f.runtime.reply({ conversationId, requestId: randomUUID(), provider: 'provider-b', model: 'model-b', message: 'again' });
  assert.deepEqual(f.seen[1].messages.map(item => item.content), ['hello', 'answer-1', 'again']);
  assert.equal((await save({ Revision: initial.Revision, Projects: [] })).status, 409);
});

test('deletion removes model context access and undo restores the same authoritative transcript', async t => {
  const f = await setup(t), conversationId = randomUUID();
  const input = { conversationId, provider: 'provider-a', model: 'model-a', message: 'keep this', requestId: randomUUID() };
  await f.runtime.reply(input);
  const before = await f.runtime.conversations.catalog();
  await f.runtime.conversations.saveCatalog({ Revision: before.Revision, Chats: [] });
  await assert.rejects(f.runtime.reply({ ...input, message: 'must not recreate', requestId: randomUUID() }));
  assert.equal(f.seen.length, 1);
  const deleted = await f.runtime.conversations.catalog();
  assert.equal(deleted.Chats.length, 0);
  await f.runtime.conversations.saveCatalog({ Revision: deleted.Revision, Chats: before.Chats });
  await f.runtime.reply({ ...input, message: 'restored', requestId: randomUUID() });
  assert.deepEqual(f.seen.at(-1).messages.map(item => item.content), ['keep this', 'answer-1', 'restored']);
});

test('simultaneous turns across different models serialize by conversation, not model', async t => {
  const f = await setup(t), conversationId = randomUUID();
  await Promise.all([
    f.runtime.reply({ conversationId, provider: 'provider-a', model: 'model-a', message: 'first' }),
    f.runtime.reply({ conversationId, provider: 'provider-b', model: 'model-b', message: 'second' })
  ]);
  assert.deepEqual(f.seen[1].messages.map(item => item.content), ['first', 'answer-1', 'second']);
});

test('non-stream HTTP response identifies the same persisted assistant and replays without another model call', async t => {
  const f = await setup(t);
  const base = await listen(createModelServer({ modelStore: f.modelStore, modelRuntime: f.runtime }), t);
  const input = { conversationId: randomUUID(), provider: 'provider-a', model: 'model-a', message: 'one' };
  const post = body => fetch(`${base}/api/chat`, { method: 'POST', body: JSON.stringify(body) }).then(response => response.json());
  const first = await post(input);
  const log = await f.runtime.conversations.readMessages(input.conversationId);
  assert.equal(first.requestId, log[1].Id);
  const replay = await post({ ...input, requestId: first.requestId });
  assert.equal(replay.requestId, first.requestId);
  assert.equal(replay.content, first.content);
  assert.equal(f.seen.length, 1);
  const uppercase = await post({ ...input, requestId: first.requestId.toUpperCase(), userMessageId: log[0].Id.toUpperCase() });
  assert.equal(uppercase.content, first.content);
  assert.equal(f.seen.length, 1);
});

test('context keeps complete turns, excludes reasoning and failed attempts, without deleting durable history', () => {
  const source = [
    { Id: 'u1', Role: 'user', Content: 'one' },
    { Id: 'a1', Role: 'assistant', Content: 'answer', Status: 'completed', Reasoning: 'private reasoning' },
    { Id: 'u2', Role: 'user', Content: 'failed question' },
    { Id: 'a2', Role: 'assistant', Content: 'partial', Status: 'interrupted' },
    { Id: 'u3', Role: 'user', Content: 'current' }
  ];
  assert.deepEqual(completedContext(source, 'u3'), [{ role: 'user', content: 'one' }, { role: 'assistant', content: 'answer' }]);
  assert.equal(source.length, 5);
});

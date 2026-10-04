import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { randomUUID } from 'node:crypto';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { after, test } from 'node:test';
import { ConversationStore } from '../conversations.mjs';
import { ModelRuntime } from '../runtime.mjs';
import { ModelStore } from '../store.mjs';
import { readSse } from '../streaming.mjs';

// Avoid resolving the user's storage pointer when server.mjs constructs its unused defaults.
// server.mjs 创建未使用的默认实例时，避免解析用户的存储指针。
const importRoot = await mkdtemp(join(tmpdir(), 'kynxa-memory-effect-import-'));
const previousDataRoot = process.env.KYNXA_DATA_HOME;
process.env.KYNXA_DATA_HOME = importRoot;
const { createModelServer } = await import('../server.mjs');
if (previousDataRoot === undefined) delete process.env.KYNXA_DATA_HOME;
else process.env.KYNXA_DATA_HOME = previousDataRoot;

async function removeTempRoot(root) {
  const suffix = relative(resolve(tmpdir()), resolve(root));
  assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`), 'cleanup stays inside the owned temporary root');
  await rm(root, { recursive: true, force: true });
}
after(() => removeTempRoot(importRoot));

// Gemini currently uses the desktop preset's OpenAI-compatible /v1beta/openai endpoint.
// This suite intentionally does not invent a Gemini-native systemInstruction adapter.
// Gemini 沿用桌面预设的 OpenAI 兼容端点；本套件不虚构 Gemini 原生 systemInstruction 适配器。
const providers = [
  { id: 'effect-openai', name: 'OpenAI Chat Completions', protocol: 'openai-completions', basePath: '/openai/v1', path: '/chat/completions' },
  { id: 'effect-responses', name: 'OpenAI Responses', protocol: 'openai-responses', basePath: '/responses/v1', path: '/responses' },
  { id: 'effect-anthropic', name: 'Anthropic Messages', protocol: 'anthropic-messages', basePath: '/anthropic/v1', path: '/messages' },
  { id: 'effect-gemini', name: 'Gemini OpenAI compatibility', protocol: 'openai-completions', basePath: '/gemini/v1beta/openai', path: '/chat/completions' }
];
const markers = {
  global: 'MEM_EFF_GLOBAL_V1', globalUpdated: 'MEM_EFF_GLOBAL_V2',
  work: 'MEM_EFF_WORK_V1', workUpdated: 'MEM_EFF_WORK_V2',
  chat: 'MEM_EFF_CHAT_V1', chatUpdated: 'MEM_EFF_CHAT_V2',
  folderless: 'MEM_EFF_FOLDERLESS_CHAT', source: 'MEM_EFF_SAVED_SOURCE'
};
const rememberedMarkers = system => [...new Set(system.match(/\bMEM_EFF_[A-Z0-9_]+\b/g) ?? [])].sort();
const user = Content => ({ Id: randomUUID(), Role: 'user', Content, Status: 'completed' });
const chat = title => ({ Id: randomUUID(), Title: title, Messages: [user(`Synthetic source for ${title}`)] });
const project = (name, Chats, extra = {}) => ({ Id: randomUUID(), Name: name, FolderPath: null, Chats, ...extra });
const frame = value => `data: ${typeof value === 'string' ? value : JSON.stringify(value)}\n\n`;

function nativeSystem(provider, body) {
  if (provider.protocol === 'anthropic-messages') return body.system ?? '';
  if (provider.protocol === 'openai-responses') return body.instructions ?? '';
  return body.messages.filter(item => item.role === 'system').map(item => item.content).join('\n');
}

function nativeReply(protocol, content) {
  if (protocol === 'anthropic-messages') return { content: [{ type: 'text', text: content }], stop_reason: 'end_turn' };
  if (protocol === 'openai-responses') return { status: 'completed', output: [
    { type: 'message', content: [{ type: 'output_text', text: content }] }
  ] };
  return { choices: [{ message: { role: 'assistant', content }, finish_reason: 'stop' }] };
}

function streamingReply(protocol, content) {
  const split = Math.max(1, Math.floor(content.length / 2)), deltas = [content.slice(0, split), content.slice(split)];
  if (protocol === 'anthropic-messages') return deltas.map(text => frame({ type: 'content_block_delta', index: 0,
    delta: { type: 'text_delta', text } })).join('') +
    frame({ type: 'message_delta', delta: { stop_reason: 'end_turn' } }) + frame({ type: 'message_stop' });
  if (protocol === 'openai-responses') return deltas.map(delta => frame({ type: 'response.output_text.delta', delta })).join('') +
    frame({ type: 'response.completed', response: nativeReply(protocol, content) });
  return deltas.map(delta => frame({ choices: [{ index: 0, delta: { content: delta } }] })).join('') +
    frame({ choices: [{ index: 0, delta: {}, finish_reason: 'stop' }] }) + frame('[DONE]');
}

async function listen(server) {
  await new Promise(ready => server.listen(0, '127.0.0.1', ready));
  return `http://127.0.0.1:${server.address().port}`;
}

async function closeServer(server) {
  server.closeAllConnections();
  await new Promise(complete => server.close(complete));
}

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-memory-effect-')), dataHome = join(root, 'Models');
  const seen = [];
  const upstream = createServer(async (request, response) => {
    try {
      const chunks = [];
      for await (const chunk of request) chunks.push(chunk);
      const body = JSON.parse(Buffer.concat(chunks).toString('utf8'));
      const provider = providers.find(item => request.url === item.basePath + item.path);
      assert.ok(provider, 'only explicitly configured loopback fixture endpoints may receive requests');
      assert.equal(request.method, 'POST');
      const system = nativeSystem(provider, body);
      assert.equal(typeof system, 'string');
      // The observable reply depends solely on the actual system field received over HTTP.
      // Old assistant answers may remain in this chat's history after memory edits/deletions.
      // They must not masquerade as the current confirmed-memory projection in this probe.
      // 可观察的回复仅取决于 HTTP 实际收到的 system 字段；编辑或删除记忆后，历史回答可以保留，但不能在此探针中冒充当前已确认记忆的投影。
      const content = JSON.stringify({ memory: rememberedMarkers(system) });
      seen.push({ provider, path: request.url, body, system });
      if (body.stream) {
        response.writeHead(200, { 'Content-Type': 'text/event-stream' });
        response.end(streamingReply(provider.protocol, content));
      } else {
        response.writeHead(200, { 'Content-Type': 'application/json' });
        response.end(JSON.stringify(nativeReply(provider.protocol, content)));
      }
    } catch (error) {
      response.writeHead(500, { 'Content-Type': 'application/json' });
      response.end(JSON.stringify({ error: error.message }));
    }
  });
  const endpoint = await listen(upstream);
  let modelStore = new ModelStore({ dataHome });
  for (const provider of providers) await modelStore.save({ providerId: provider.id, displayName: provider.name,
    protocol: provider.protocol, baseUrl: endpoint + provider.basePath, models: ['effect-test-model'], contextWindowTokens: 16384 });
  const openRuntime = () => new ModelRuntime({ modelStore, dataHome,
    conversationStore: new ConversationStore({ dataHome, legacyDesktopDirectory: null }) });
  let runtime = openRuntime(), gateway = createModelServer({ modelStore, modelRuntime: runtime }), base = await listen(gateway);
  t.after(async () => {
    await runtime.close();
    await closeServer(gateway);
    await closeServer(upstream);
    await removeTempRoot(root);
  });
  const a = chat('Work A / private'), sibling = chat('Work A / sibling'), other = chat('Work B / isolated');
  const folderlessA = chat('Folderless / A'), folderlessB = chat('Folderless / B'), ordinary = chat('Ordinary');
  const work = project('Work A', [a, sibling]), otherWork = project('Work B', [other]);
  const folderlessWork = project('Folderless', [folderlessA, folderlessB], { IsFolderlessWorkspace: true });
  const raw = async (method, path, input) => {
    const response = await fetch(base + path, { method, headers: { 'Content-Type': 'application/json' },
      body: input === undefined ? undefined : JSON.stringify(input) });
    return { status: response.status, body: await response.json() };
  };
  const api = async (method, path, input) => {
    const result = await raw(method, path, input);
    assert.ok(result.status >= 200 && result.status < 300, `${method} ${path}: ${JSON.stringify(result)}`);
    return result.body;
  };
  const mutateCatalog = async mutate => {
    const catalog = await api('GET', '/api/conversations/catalog');
    mutate(catalog);
    return api('PUT', '/api/conversations/catalog', { Revision: catalog.Revision, Projects: catalog.Projects, Chats: catalog.Chats });
  };
  const seedCatalog = (includeChats = true) => mutateCatalog(catalog => {
    catalog.Projects = [work, otherWork, folderlessWork].map(item => ({ ...item, Chats: includeChats ? item.Chats : [] }));
    catalog.Chats = includeChats ? [ordinary] : [];
  });
  const readMemory = async (path, scope) => {
    const result = await api('GET', path);
    return result.scopes ? result.scopes.find(document => document.scope === scope) : result;
  };
  const createMemory = async (path, scope, content, extra = {}) => {
    const document = await readMemory(path, scope);
    const written = await api('POST', path, { scope, content, expectedRevision: document.revision, ...extra });
    const entry = written.entries.find(item => item.content === content);
    assert.ok(entry, 'created memory must have a gateway-owned identity');
    return { path, scope, id: entry.id, source: entry.source };
  };
  const updateMemory = async (memory, content) => {
    const document = await readMemory(memory.path, memory.scope);
    return api('PATCH', `${memory.path}/${memory.id}`, { scope: memory.scope, content, expectedRevision: document.revision });
  };
  const deleteMemory = async memory => {
    const document = await readMemory(memory.path, memory.scope);
    return api('DELETE', `${memory.path}/${memory.id}`, { scope: memory.scope, expectedRevision: document.revision });
  };
  const ask = async (conversationId, expected, { provider = providers[0], stream = false, ...extra } = {}) => {
    const input = { conversationId, requestId: randomUUID(), userMessageId: randomUUID(), provider: provider.id,
      model: 'effect-test-model', message: '请报告当前有效记忆中的约定标识。', ...extra };
    const before = seen.length;
    let content;
    if (stream) {
      const response = await fetch(base + '/api/chat/stream', { method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(input) });
      assert.equal(response.status, 200);
      assert.match(response.headers.get('content-type'), /text\/event-stream/);
      const events = [];
      for await (const frame of readSse(response.body)) events.push(JSON.parse(frame.data));
      assert.equal(events.at(-1).type, 'completed', JSON.stringify(events.at(-1)));
      const deltas = events.filter(event => event.type === 'text_delta');
      assert.ok(deltas.length >= 2, 'native upstream SSE must produce incremental gateway reply events');
      content = events.at(-1).content;
      assert.equal(deltas.map(event => event.delta).join(''), content);
    } else content = (await api('POST', '/api/chat', input)).content;
    assert.equal(seen.length, before + 1, 'fresh request IDs must reach the real runtime and mock upstream');
    const request = seen.at(-1);
    assert.equal(request.provider.id, provider.id);
    assert.equal(request.body.stream, stream);
    assert.deepEqual(rememberedMarkers(request.system), [...expected].sort(), 'only eligible memories enter the protocol system field');
    assert.deepEqual(JSON.parse(content).memory, [...expected].sort(), 'the received memory must determine the simulated model answer');
    const persisted = (await runtime.conversations.readMessages(conversationId)).find(message => message.Id === input.requestId);
    assert.equal(persisted?.Status, 'completed');
    assert.equal(persisted?.Content, content, 'the user-visible answer is the authoritative saved assistant reply');
    return request;
  };
  return { root, raw, api, work, otherWork, folderlessWork, a, sibling, other, folderlessA, folderlessB, ordinary,
    mutateCatalog, seedCatalog, readMemory, createMemory, updateMemory, deleteMemory, ask,
    chatPath: id => `/api/conversations/${id}/memory`, workPath: `/api/projects/${work.Id}/memory`,
    restart: async () => {
      await runtime.close();
      await closeServer(gateway);
      modelStore = new ModelStore({ dataHome });
      runtime = openRuntime();
      gateway = createModelServer({ modelStore, modelRuntime: runtime });
      base = await listen(gateway);
    } };
}

for (const provider of providers) {
  test(`${provider.name}: management-created memory changes real gateway answers, native SSE and answers after restart`, async t => {
    const f = await fixture(t);
    const global = await f.createMemory('/api/memory/user', 'user', markers.global);
    assert.equal(global.source.conversationId, undefined, 'global management must not anchor to a fake chat');
    assert.deepEqual((await f.api('GET', '/api/conversations/catalog')).Chats, []);
    await f.seedCatalog(false);
    const workMemory = await f.createMemory(f.workPath, 'project', markers.work);
    assert.equal(workMemory.source.conversationId, undefined, 'empty work management must not need a saved chat');
    await f.seedCatalog();
    await f.createMemory(f.chatPath(f.a.Id), 'chat', markers.chat);
    await f.ask(f.a.Id, [markers.global, markers.work, markers.chat], { provider });
    const sibling = await f.ask(f.sibling.Id, [markers.global, markers.work], { provider });
    const messages = sibling.body.messages ?? sibling.body.input;
    assert.ok(!JSON.stringify(messages.filter(message => message.role !== 'system')).includes(markers.chat),
      'a sibling must receive shared work memory without inheriting the private chat transcript');
    await f.ask(f.sibling.Id, [markers.global, markers.work], { provider, stream: true });
    await f.restart();
    await f.ask(f.sibling.Id, [markers.global, markers.work], { provider });
    const persisted = JSON.parse(await readFile(join(f.root, 'Memory', 'entries.json'), 'utf8'));
    assert.equal(persisted.entries[0].content, markers.global);
  });
}

test('observable answers keep work, private chat and folderless boundaries after independent management CRUD', async t => {
  const f = await fixture(t);
  await f.seedCatalog();
  await f.createMemory('/api/memory/user', 'user', markers.global);
  await f.createMemory(f.workPath, 'project', markers.work);
  await f.createMemory(f.chatPath(f.a.Id), 'chat', markers.chat);
  await f.createMemory(f.chatPath(f.folderlessA.Id), 'chat', markers.folderless);
  await f.ask(f.a.Id, [markers.global, markers.work, markers.chat]);
  await f.ask(f.sibling.Id, [markers.global, markers.work]);
  await f.ask(f.other.Id, [markers.global]);
  await f.ask(f.ordinary.Id, [markers.global]);
  await f.ask(f.folderlessA.Id, [markers.global, markers.folderless]);
  await f.ask(f.folderlessB.Id, [markers.global]);
  const forbidden = await f.raw('POST', `/api/projects/${f.folderlessWork.Id}/memory`, { content: 'MEM_EFF_FORBIDDEN_SHARED', expectedRevision: 0 });
  assert.equal(forbidden.status, 400);
  await f.ask(f.folderlessB.Id, [markers.global], { stream: true });
});

test('editing and deleting each memory scope changes the next answer without rewriting old transcripts', async t => {
  const f = await fixture(t);
  await f.seedCatalog();
  const global = await f.createMemory('/api/memory/user', 'user', markers.global);
  const work = await f.createMemory(f.workPath, 'project', markers.work);
  const privateChat = await f.createMemory(f.chatPath(f.a.Id), 'chat', markers.chat);
  await f.ask(f.a.Id, [markers.global, markers.work, markers.chat]);
  const eventsFile = join(f.root, 'Projects', f.work.Id, 'Sessions', f.a.Id, 'events.jsonl');
  const originalEvents = await readFile(eventsFile);
  await f.updateMemory(privateChat, markers.chatUpdated);
  await f.updateMemory(work, markers.workUpdated);
  await f.updateMemory(global, markers.globalUpdated);
  assert.deepEqual(await readFile(eventsFile), originalEvents, 'memory management cannot edit previous conversation replies');
  await f.ask(f.a.Id, [markers.globalUpdated, markers.workUpdated, markers.chatUpdated], { stream: true });
  await f.ask(f.sibling.Id, [markers.globalUpdated, markers.workUpdated]);
  await f.deleteMemory(privateChat);
  await f.ask(f.a.Id, [markers.globalUpdated, markers.workUpdated]);
  await f.deleteMemory(work);
  await f.ask(f.sibling.Id, [markers.globalUpdated], { stream: true });
  await f.deleteMemory(global);
  await f.ask(f.sibling.Id, []);
  await f.restart();
  await f.ask(f.a.Id, []);
});

test('source and work lifecycle immediately changes model answers while manual confirmations remain independent', async t => {
  const f = await fixture(t);
  await f.seedCatalog();
  await f.createMemory('/api/memory/user', 'user', markers.global);
  await f.createMemory(f.workPath, 'project', markers.work);
  const sourceMessage = user(`工作记住：${markers.source}`);
  await f.mutateCatalog(catalog => { catalog.Projects[0].Chats[0].Messages.push(sourceMessage); });
  const source = await f.createMemory(f.chatPath(f.a.Id), 'project', markers.source,
    { source: { type: 'user-message', role: 'user', conversationId: f.a.Id, messageId: sourceMessage.Id } });
  await f.ask(f.sibling.Id, [markers.global, markers.work, markers.source]);
  await f.mutateCatalog(catalog => { catalog.Projects[0].Chats[0].IsArchived = true; });
  assert.equal((await f.readMemory(f.workPath, 'project')).entries.find(entry => entry.id === source.id).sourceArchived, true);
  await f.ask(f.sibling.Id, [markers.global, markers.work, markers.source]);
  const savedSource = (await f.api('GET', '/api/conversations/catalog')).Projects[0].Chats[0];
  await f.mutateCatalog(catalog => { catalog.Projects[0].Chats = catalog.Projects[0].Chats.filter(item => item.Id !== f.a.Id); });
  let sourceStatus = (await f.readMemory(f.workPath, 'project')).entries.find(entry => entry.id === source.id);
  assert.equal(sourceStatus.active, false);
  assert.equal(sourceStatus.sourceAvailable, false);
  await f.ask(f.sibling.Id, [markers.global, markers.work], { stream: true });
  await f.mutateCatalog(catalog => { catalog.Projects[0].Chats.push(savedSource); });
  await f.ask(f.sibling.Id, [markers.global, markers.work, markers.source]);
  await f.mutateCatalog(catalog => {
    const original = catalog.Projects[0];
    catalog.Projects[1].Chats.push(original.Chats.find(item => item.Id === f.a.Id));
    original.Chats = original.Chats.filter(item => item.Id !== f.a.Id);
  });
  sourceStatus = (await f.readMemory(f.workPath, 'project')).entries.find(entry => entry.id === source.id);
  assert.equal(sourceStatus.active, false);
  assert.equal(sourceStatus.sourceAvailable, true);
  await f.ask(f.sibling.Id, [markers.global, markers.work]);
  await f.ask(f.other.Id, [markers.global]);
  await f.mutateCatalog(catalog => {
    const destination = catalog.Projects[1];
    catalog.Projects[0].Chats.push(destination.Chats.find(item => item.Id === f.a.Id));
    destination.Chats = destination.Chats.filter(item => item.Id !== f.a.Id);
    catalog.Projects[0].IsArchived = true;
  });
  await f.ask(f.sibling.Id, [markers.global]);
  await f.mutateCatalog(catalog => { catalog.Projects[0].IsArchived = false; });
  await f.ask(f.sibling.Id, [markers.global, markers.work, markers.source]);
  const deleted = await f.deleteMemory({ ...source, path: f.workPath });
  assert.ok(deleted.dismissedSources.some(item => item.conversationId === f.a.Id && item.messageId === sourceMessage.Id));
  await f.ask(f.a.Id, [markers.global, markers.work], { message: sourceMessage.Content, userMessageId: sourceMessage.Id });
  await f.ask(f.sibling.Id, [markers.global, markers.work]);
  assert.ok(!(await f.readMemory(f.workPath, 'project')).entries.some(entry => entry.id === source.id),
    'retrying the exact source instruction must not recreate an explicitly deleted memory');
});

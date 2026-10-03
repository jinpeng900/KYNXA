import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { randomUUID } from 'node:crypto';
import { mkdtemp, readFile, stat, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { ModelRuntime } from '../runtime.mjs';
import { ModelStore } from '../store.mjs';
import { readSse } from '../streaming.mjs';
import { estimateTokens, estimateMessageTokens } from '../context.mjs';
import { migrateStorage } from '../migrate-storage.mjs';

// server.mjs creates unused default objects when imported. Give those objects a
// synthetic root too, so this suite never resolves the user's storage pointer.
const importRoot = await mkdtemp(join(tmpdir(), 'kynxa-memory-integration-import-'));
const previousDataRoot = process.env.KYNXA_DATA_HOME;
process.env.KYNXA_DATA_HOME = importRoot;
const { createModelServer } = await import('../server.mjs');
if (previousDataRoot === undefined) delete process.env.KYNXA_DATA_HOME;
else process.env.KYNXA_DATA_HOME = previousDataRoot;

const assistantOnlyFact = 'ASSISTANT_ONLY_FACT_不要把模型回复自动记作用户事实';
const replyText = `记住：${assistantOnlyFact}`;
const protocols = ['openai-completions', 'openai-responses', 'anthropic-messages'];

async function listen(server, t) {
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(() => { server.closeAllConnections(); server.close(); });
  return `http://127.0.0.1:${server.address().port}`;
}

function nativeReply(protocol) {
  if (protocol === 'anthropic-messages') return { content: [{ type: 'text', text: replyText }] };
  if (protocol === 'openai-responses') return { status: 'completed', output: [
    { type: 'message', content: [{ type: 'output_text', text: replyText }] }
  ] };
  return { choices: [{ message: { content: replyText } }] };
}

function user(content, id = randomUUID()) {
  return { Id: id, Role: 'user', Content: content, Status: 'completed', CreatedAt: new Date().toISOString() };
}

function seedChat(title, options = {}) {
  return { Id: randomUUID(), Title: title, Messages: [user(`fixture seed: ${title}`)], ...options };
}

function upstreamMessages(request) { return request.body.messages ?? request.body.input ?? []; }

function systemText(request) {
  if (request.protocol === 'anthropic-messages') {
    return typeof request.body.system === 'string' ? request.body.system : JSON.stringify(request.body.system ?? '');
  }
  if (request.protocol === 'openai-responses') return request.body.instructions ?? '';
  return upstreamMessages(request).filter(item => ['system', 'developer'].includes(item.role))
    .map(item => typeof item.content === 'string' ? item.content : JSON.stringify(item.content)).join('\n');
}

function promptText(request) {
  return JSON.stringify({ system: request.body.system, instructions: request.body.instructions,
    messages: upstreamMessages(request) });
}

function deferred() {
  let resolve;
  const promise = new Promise(complete => { resolve = complete; });
  return { promise, resolve };
}

async function concurrentChatMemoryAndCatalogChange(f, content, changeCatalog) {
  // Read the intended metadata before blocking storage: fetching it while the
  // write guard is held would correctly queue and deadlock the test driver.
  const catalog = await f.api('GET', '/api/conversations/catalog');
  changeCatalog(catalog);
  const entered = deferred(), release = deferred();
  const repository = f.runtime.memory.repository;
  const originalSafe = repository._safe;
  const memoryFolder = join(f.root, 'Projects', f.project.Id, 'Sessions', f.chatA.Id, 'Memory');
  repository._safe = async function(path, options = {}) {
    if (path === memoryFolder && options.create) {
      entered.resolve();
      await release.promise;
    }
    return originalSafe.call(this, path, options);
  };
  try {
    const memoryWrite = f.raw('POST', f.memoryPath(f.chatA.Id), { scope: 'chat', content });
    await entered.promise;
    let catalogFinished = false;
    const catalogChange = f.raw('PUT', '/api/conversations/catalog', {
      Revision: catalog.Revision, Projects: catalog.Projects, Chats: catalog.Chats
    }).then(result => { catalogFinished = true; return result; });
    // Health does not enter the storage queue. Observing both active requests
    // proves the catalog RPC arrived, without depending on arbitrary sleeps.
    let simultaneous = false;
    for (let attempt = 0; attempt < 30; attempt++) {
      const health = await f.raw('GET', '/health');
      if (health.body.activeRequests >= 2) { simultaneous = true; break; }
      if (catalogFinished) break;
    }
    assert.ok(simultaneous, 'both HTTP mutations should be in flight before releasing the write');
    assert.equal(catalogFinished, false, 'catalog relocation/deletion must wait for an in-progress memory write');
    release.resolve();
    const [memoryResult, catalogResult] = await Promise.all([memoryWrite, catalogChange]);
    assert.equal(memoryResult.status, 201, JSON.stringify(memoryResult));
    assert.equal(catalogResult.status, 200, JSON.stringify(catalogResult));
  } finally {
    release.resolve();
    repository._safe = originalSafe;
  }
}

async function fixture(t, { contextWindowTokens = 8192 } = {}) {
  const seen = [];
  const endpoint = await listen(createServer(async (request, response) => {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const body = JSON.parse(Buffer.concat(chunks).toString('utf8'));
    const protocol = protocols.find(item => request.url.includes(`/${item}/`));
    assert.ok(protocol, `unexpected fixture endpoint ${request.url}`);
    seen.push({ protocol, path: request.url, body });
    // Real SSE on one path catches an implementation that adds memory only to
    // non-streaming calls. Other protocols also support JSON stream fallbacks.
    if (protocol === 'openai-completions' && body.stream) {
      response.writeHead(200, { 'Content-Type': 'text/event-stream' });
      response.end(`data: ${JSON.stringify({ choices: [{ delta: { content: replyText } }] })}\n\n` +
        `data: ${JSON.stringify({ choices: [{ delta: {}, finish_reason: 'stop' }] })}\n\n` + 'data: [DONE]\n\n');
    } else {
      response.setHeader('Content-Type', 'application/json');
      response.end(JSON.stringify(nativeReply(protocol)));
    }
  }), t);
  const root = await mkdtemp(join(tmpdir(), 'kynxa-memory-integration-'));
  const dataHome = join(root, 'Models');
  let modelStore = new ModelStore({ dataHome });
  for (const [index, protocol] of protocols.entries())
    await modelStore.save({ providerId: `fixture-${index}`, displayName: `Fixture ${index}`, protocol,
      baseUrl: `${endpoint}/v1/${protocol}`, models: ['model-a', 'model-b'], contextWindowTokens });
  let runtime = new ModelRuntime({ modelStore, dataHome });
  let base = await listen(createModelServer({ modelStore, modelRuntime: runtime }), t);
  t.after(() => runtime.close());
  const chatA = seedChat('P / A'), chatB = seedChat('P / B');
  const otherProjectChat = seedChat('Q / A');
  const folderlessA = seedChat('Folderless / A'), folderlessB = seedChat('Folderless / B');
  const ordinary = seedChat('Ordinary');
  const project = { Id: randomUUID(), Name: 'Synthetic project P', FolderPath: null, Chats: [chatA, chatB] };
  const otherProject = { Id: randomUUID(), Name: 'Synthetic project Q', FolderPath: null, Chats: [otherProjectChat] };
  const folderless = { Id: randomUUID(), Name: 'Synthetic folderless', IsFolderlessWorkspace: true,
    Chats: [folderlessA, folderlessB] };

  const raw = async (method, path, body) => {
    const response = await fetch(`${base}${path}`, { method,
      headers: { 'Content-Type': 'application/json' }, body: body === undefined ? undefined : JSON.stringify(body) });
    if (response.headers.get('Content-Type')?.includes('text/event-stream')) {
      const events = [];
      for await (const event of readSse(response.body)) events.push(JSON.parse(event.data));
      return { status: response.status, body: events };
    }
    const text = await response.text();
    return { status: response.status, body: text ? JSON.parse(text) : null };
  };
  const api = async (method, path, body) => {
    const result = await raw(method, path, body);
    assert.ok(result.status >= 200 && result.status < 300,
      `${method} ${path}: HTTP ${result.status}: ${JSON.stringify(result.body)}`);
    return result.body;
  };
  const initial = await api('GET', '/api/conversations/catalog');
  await api('PUT', '/api/conversations/catalog', { Revision: initial.Revision,
    Projects: [project, otherProject, folderless], Chats: [ordinary] });
  const memoryPath = id => `/api/conversations/${id}/memory`;
  const getMemory = id => api('GET', memoryPath(id));
  const scopeDocument = async (id, scope) => {
    const document = (await getMemory(id)).scopes.find(item => item.scope === scope);
    assert.ok(document, `missing ${scope} scope for ${id}`);
    assert.ok(Array.isArray(document.entries));
    assert.ok(Number.isSafeInteger(document.revision));
    return document;
  };
  const addMemory = async (id, scope, content, extra = {}) => {
    await api('POST', memoryPath(id), { scope, content, ...extra });
    const document = await scopeDocument(id, scope);
    const entry = document.entries.find(item => item.content === content);
    assert.ok(entry, `missing saved memory ${content}`);
    assert.equal(typeof entry.id, 'string');
    return { document, entry };
  };
  const reply = async (id, message, { protocol = 'openai-completions', model = 'model-a',
    userMessageId = randomUUID(), requestId = randomUUID(), stream = false } = {}) => {
    const input = { conversationId: id, provider: `fixture-${protocols.indexOf(protocol)}`, model,
      message, userMessageId, requestId };
    if (stream) {
      const response = await fetch(`${base}/api/chat/stream`, { method: 'POST',
        headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(input) });
      assert.equal(response.status, 200);
      const events = [];
      for await (const event of readSse(response.body)) events.push(JSON.parse(event.data));
      assert.equal(events.at(-1).type, 'completed', JSON.stringify(events.at(-1)));
      return { input, result: events.at(-1) };
    }
    return { input, result: await api('POST', '/api/chat', input) };
  };
  const mutateCatalog = async mutation => {
    const catalog = await api('GET', '/api/conversations/catalog');
    mutation(catalog);
    return api('PUT', '/api/conversations/catalog', { Revision: catalog.Revision,
      Projects: catalog.Projects, Chats: catalog.Chats });
  };
  return { root, dataHome, get modelStore() { return modelStore; }, seen, project, otherProject, folderless,
    chatA, chatB, otherProjectChat, folderlessA, folderlessB, ordinary,
    get runtime() { return runtime; }, raw, api, getMemory, scopeDocument, addMemory, reply, mutateCatalog, memoryPath,
    restart: async (newDataHome = dataHome) => {
      await runtime.close();
      if (modelStore.dataHome !== newDataHome) modelStore = new ModelStore({ dataHome: newDataHome });
      runtime = new ModelRuntime({ modelStore, dataHome: newDataHome });
      base = await listen(createModelServer({ modelStore, modelRuntime: runtime }), t);
    } };
}

test('declared project memory reaches sibling chat, survives model/provider switches and restart, and stays isolated', async t => {
  const f = await fixture(t), fact = 'PROJECT_FACT_工作 P 的发布代号是翠鸟';
  await f.reply(f.chatA.Id, `记住：${fact}`);
  const saved = await f.scopeDocument(f.chatA.Id, 'project');
  assert.equal(saved.entries.filter(item => item.content === fact).length, 1);
  assert.ok(!JSON.stringify(saved).includes(assistantOnlyFact));
  await f.reply(f.chatB.Id, '给出本工作发布代号', { stream: true });
  assert.ok(systemText(f.seen.at(-1)).includes(fact));
  assert.ok(!upstreamMessages(f.seen.at(-1)).some(item => item.role === 'user' && item.content === `记住：${fact}`),
    'a sibling transcript must not be concatenated into this chat');
  await f.restart();
  await f.reply(f.chatB.Id, '继续本工作', { protocol: 'openai-responses', model: 'model-b' });
  assert.ok(systemText(f.seen.at(-1)).includes(fact));
  for (const chat of [f.otherProjectChat, f.ordinary, f.folderlessA, f.folderlessB]) {
    await f.reply(chat.Id, '检查隔离边界');
    assert.ok(!promptText(f.seen.at(-1)).includes(fact), chat.Title);
  }
});

test('ordinary and folderless declarations are chat-local while explicit user memory is shared', async t => {
  const f = await fixture(t);
  const ordinaryFact = 'ORDINARY_FACT_普通聊天私有';
  const folderlessFact = 'FOLDERLESS_FACT_无文件夹聊天 A 私有';
  const globalFact = 'USER_FACT_用户明确要求全局共享';
  await f.reply(f.ordinary.Id, `记住：${ordinaryFact}`);
  await f.reply(f.folderlessA.Id, `记住：${folderlessFact}`);
  await f.reply(f.ordinary.Id, `全局记住：${globalFact}`);
  assert.ok((await f.scopeDocument(f.ordinary.Id, 'chat')).entries.some(item => item.content === ordinaryFact));
  assert.ok((await f.scopeDocument(f.folderlessA.Id, 'chat')).entries.some(item => item.content === folderlessFact));
  for (const id of [f.folderlessA.Id, f.folderlessB.Id, f.chatA.Id, f.otherProjectChat.Id]) {
    await f.reply(id, '验证当前作用域');
    const system = systemText(f.seen.at(-1));
    assert.ok(system.includes(globalFact));
    assert.equal(system.includes(folderlessFact), id === f.folderlessA.Id);
    assert.ok(!system.includes(ordinaryFact));
  }
  for (const id of [f.ordinary.Id, f.folderlessA.Id, f.folderlessB.Id])
    assert.ok(!(await f.getMemory(id)).scopes.some(item => item.scope === 'project'));
  const prohibited = await f.raw('POST', f.memoryPath(f.folderlessA.Id), { scope: 'project', content: '不得伪共享' });
  assert.ok(prohibited.status >= 400 && prohibited.status < 500);
});

test('explicit chat and project prefixes select the intended scope; assistant and quoted instructions do not extract', async t => {
  const f = await fixture(t);
  const chatFact = 'CHAT_OVERRIDE_工作里的本聊天私有';
  const projectFact = 'PROJECT_OVERRIDE_明确工作级';
  const quoted = 'QUOTED_FACT_引用内容不能产生记忆';
  await f.reply(f.chatA.Id, `聊天记住：${chatFact}`);
  await f.reply(f.chatA.Id, `工作记住：${projectFact}`);
  await f.reply(f.chatA.Id, `这只是引用“记住：${quoted}”，没有要求存储。`);
  const view = await f.getMemory(f.chatA.Id);
  assert.ok(view.scopes.find(item => item.scope === 'chat').entries.some(item => item.content === chatFact));
  assert.ok(view.scopes.find(item => item.scope === 'project').entries.some(item => item.content === projectFact));
  assert.ok(!JSON.stringify(view).includes(quoted));
  assert.ok(!JSON.stringify(view).includes(assistantOnlyFact));
  await f.reply(f.chatB.Id, '兄弟聊天只继承工作记忆');
  assert.ok(systemText(f.seen.at(-1)).includes(projectFact));
  assert.ok(!promptText(f.seen.at(-1)).includes(chatFact));
});

test('a desktop-saved declaration and replay use one source identity without duplicating memory', async t => {
  const f = await fixture(t), fact = 'IDEMPOTENT_FACT_同一已保存用户消息只提取一次';
  const message = user(`记住：${fact}`);
  await f.mutateCatalog(catalog => catalog.Projects.find(item => item.Id === f.project.Id)
    .Chats.find(item => item.Id === f.chatA.Id).Messages.push(message));
  const requestId = randomUUID();
  await f.reply(f.chatA.Id, message.Content, { userMessageId: message.Id, requestId });
  const requests = f.seen.length;
  await f.reply(f.chatA.Id, message.Content, { userMessageId: message.Id, requestId });
  assert.equal(f.seen.length, requests);
  const entries = (await f.scopeDocument(f.chatA.Id, 'project')).entries.filter(item => item.content === fact);
  assert.equal(entries.length, 1);
  assert.equal(entries[0].source.conversationId, f.chatA.Id);
  assert.equal(entries[0].source.messageId, message.Id);
  // A user deletion must remain effective if the same declaration is retried
  // with a fresh request ID (completed-request replay itself does not extract).
  const document = await f.scopeDocument(f.chatA.Id, 'project');
  await f.api('DELETE', `${f.memoryPath(f.chatA.Id)}/${entries[0].id}`,
    { scope: 'project', expectedRevision: document.revision });
  await f.reply(f.chatA.Id, message.Content, { userMessageId: message.Id });
  assert.ok(!(await f.scopeDocument(f.chatA.Id, 'project')).entries.some(item => item.content === fact));
});

test('confirmed memory survives source archive, pauses on source deletion, and returns on undo; manual entries remain independent', async t => {
  const f = await fixture(t), fact = 'SOURCE_FACT_来源生命周期';
  const manualFact = 'MANUAL_FACT_独立手工工作事实';
  await f.reply(f.chatA.Id, `记住：${fact}`);
  await f.addMemory(f.chatB.Id, 'project', manualFact);
  await f.mutateCatalog(catalog => { catalog.Projects.find(item => item.Id === f.project.Id)
    .Chats.find(item => item.Id === f.chatA.Id).IsArchived = true; });
  await f.reply(f.chatB.Id, '检查归档来源');
  assert.ok(systemText(f.seen.at(-1)).includes(fact));
  const archivedEntry = (await f.scopeDocument(f.chatB.Id, 'project')).entries.find(item => item.content === fact);
  assert.equal(archivedEntry.active, true);
  assert.equal(archivedEntry.sourceAvailable, true);
  assert.equal(archivedEntry.sourceArchived, true);
  const beforeDeletion = await f.api('GET', '/api/conversations/catalog');
  const source = beforeDeletion.Projects.find(item => item.Id === f.project.Id).Chats.find(item => item.Id === f.chatA.Id);
  await f.mutateCatalog(catalog => {
    const project = catalog.Projects.find(item => item.Id === f.project.Id);
    project.Chats = project.Chats.filter(item => item.Id !== f.chatA.Id);
  });
  await f.reply(f.chatB.Id, '检查删除来源');
  assert.ok(!systemText(f.seen.at(-1)).includes(fact));
  assert.ok(systemText(f.seen.at(-1)).includes(manualFact));
  const unavailableEntry = (await f.scopeDocument(f.chatB.Id, 'project')).entries.find(item => item.content === fact);
  assert.equal(unavailableEntry.active, false);
  assert.equal(unavailableEntry.sourceAvailable, false);
  await f.mutateCatalog(catalog => { catalog.Projects.find(item => item.Id === f.project.Id).Chats.push(source); });
  await f.reply(f.chatB.Id, '检查撤销删除');
  assert.ok(systemText(f.seen.at(-1)).includes(fact));
  assert.ok(systemText(f.seen.at(-1)).includes(manualFact));
});

test('memory edits and deletes apply immediately across sibling chats and reject stale revisions', async t => {
  const f = await fixture(t), original = 'EDIT_FACT_旧的项目规范', replacement = 'EDIT_FACT_新的项目规范';
  const { entry, document } = await f.addMemory(f.chatA.Id, 'project', original);
  await f.api('PATCH', `${f.memoryPath(f.chatA.Id)}/${entry.id}`,
    { scope: 'project', expectedRevision: document.revision, content: replacement });
  const updated = await f.scopeDocument(f.chatB.Id, 'project');
  assert.ok(updated.revision > document.revision);
  assert.equal(updated.entries.find(item => item.id === entry.id).content, replacement);
  const stale = await f.raw('PATCH', `${f.memoryPath(f.chatA.Id)}/${entry.id}`,
    { scope: 'project', expectedRevision: document.revision, content: original });
  assert.equal(stale.status, 409);
  await f.reply(f.chatB.Id, '检查最新规范');
  assert.ok(systemText(f.seen.at(-1)).includes(replacement));
  assert.ok(!promptText(f.seen.at(-1)).includes(original));
  await f.api('DELETE', `${f.memoryPath(f.chatB.Id)}/${entry.id}`,
    { scope: 'project', expectedRevision: updated.revision });
  await f.reply(f.chatB.Id, '检查已删除规范');
  assert.ok(!promptText(f.seen.at(-1)).includes(replacement));
});

test('manual source references must identify a persisted user message in the requested scope', async t => {
  const f = await fixture(t);
  await f.reply(f.chatA.Id, '普通真实用户输入');
  const history = await f.runtime.conversations.readMessages(f.chatA.Id);
  const assistant = history.find(item => item.Role === 'assistant');
  for (const source of [
    { type: 'user-message', role: 'user', conversationId: f.chatA.Id, messageId: assistant.Id },
    { type: 'user-message', role: 'user', conversationId: f.chatA.Id, messageId: randomUUID() },
    { type: 'user-message', role: 'user', conversationId: f.otherProjectChat.Id, messageId: f.otherProjectChat.Messages[0].Id }
  ]) {
    const result = await f.raw('POST', f.memoryPath(f.chatA.Id), { scope: 'project', content: '伪造来源不得被采用', source });
    assert.ok(result.status >= 400 && result.status < 500, JSON.stringify(result));
  }
  assert.ok(!(await f.scopeDocument(f.chatA.Id, 'project')).entries.some(item => item.content === '伪造来源不得被采用'));
  const realUser = history.find(item => item.Role === 'user' && item.Content === '普通真实用户输入');
  const { entry } = await f.addMemory(f.chatA.Id, 'project', 'VALID_SOURCE_来源简写可规范化',
    { source: { conversationId: f.chatA.Id, messageId: realUser.Id } });
  assert.equal(entry.source.type, 'user-message');
  assert.equal(entry.source.role, 'user');
});

test('moving a source chat between works pauses its old project memory without sharing chat-local memory', async t => {
  const f = await fixture(t), projectFact = 'MOVED_SOURCE_FACT_属于原工作 P';
  const chatFact = 'MOVED_CHAT_FACT_跟随聊天但不共享';
  await f.reply(f.chatA.Id, `记住：${projectFact}`);
  await f.reply(f.chatA.Id, `聊天记住：${chatFact}`);
  await f.mutateCatalog(catalog => {
    const original = catalog.Projects.find(item => item.Id === f.project.Id);
    const destination = catalog.Projects.find(item => item.Id === f.otherProject.Id);
    destination.Chats.push(original.Chats.find(item => item.Id === f.chatA.Id));
    original.Chats = original.Chats.filter(item => item.Id !== f.chatA.Id);
  });
  await f.reply(f.chatB.Id, '原工作检查来源移走');
  assert.ok(!systemText(f.seen.at(-1)).includes(projectFact));
  const moved = (await f.scopeDocument(f.chatB.Id, 'project')).entries.find(item => item.content === projectFact);
  assert.equal(moved.active, false);
  assert.equal(moved.sourceAvailable, true);
  await f.reply(f.otherProjectChat.Id, '新工作不能自动继承旧工作事实');
  assert.ok(!promptText(f.seen.at(-1)).includes(projectFact));
  assert.ok(!promptText(f.seen.at(-1)).includes(chatFact));
  await f.reply(f.chatA.Id, '被移动的聊天仍有自己的记忆');
  assert.ok(systemText(f.seen.at(-1)).includes(chatFact));
  assert.ok(!systemText(f.seen.at(-1)).includes(projectFact));
  const related = await f.api('GET', `/api/conversations/${f.chatA.Id}/relationships`);
  assert.equal(related.projectId, f.otherProject.Id);
  await f.mutateCatalog(catalog => {
    const original = catalog.Projects.find(item => item.Id === f.project.Id);
    const destination = catalog.Projects.find(item => item.Id === f.otherProject.Id);
    original.Chats.push(destination.Chats.find(item => item.Id === f.chatA.Id));
    destination.Chats = destination.Chats.filter(item => item.Id !== f.chatA.Id);
  });
  await f.reply(f.chatB.Id, '移回原工作后恢复来源');
  assert.ok(systemText(f.seen.at(-1)).includes(projectFact));
});

test('a concurrent HTTP session move waits for chat memory IO and carries the newly written memory with the session', async t => {
  const f = await fixture(t), fact = 'CONCURRENT_MOVE_FACT_不能写回旧工作位置';
  await concurrentChatMemoryAndCatalogChange(f, fact, catalog => {
    const source = catalog.Projects.find(item => item.Id === f.project.Id);
    const destination = catalog.Projects.find(item => item.Id === f.otherProject.Id);
    destination.Chats.push(source.Chats.find(item => item.Id === f.chatA.Id));
    source.Chats = source.Chats.filter(item => item.Id !== f.chatA.Id);
  });
  assert.ok((await f.scopeDocument(f.chatA.Id, 'chat')).entries.some(item => item.content === fact));
  const moved = JSON.parse(await readFile(join(f.root, 'Projects', f.otherProject.Id, 'Sessions', f.chatA.Id, 'Memory', 'entries.json'), 'utf8'));
  assert.ok(moved.entries.some(item => item.content === fact));
  await assert.rejects(stat(join(f.root, 'Projects', f.project.Id, 'Sessions', f.chatA.Id)), { code: 'ENOENT' });
});

test('a concurrent HTTP chat deletion waits for memory IO and cannot recreate a removed session directory', async t => {
  const f = await fixture(t), fact = 'CONCURRENT_DELETE_FACT_跟随回收站而不是重建聊天';
  await concurrentChatMemoryAndCatalogChange(f, fact, catalog => {
    const source = catalog.Projects.find(item => item.Id === f.project.Id);
    source.Chats = source.Chats.filter(item => item.Id !== f.chatA.Id);
  });
  const missing = await f.raw('GET', f.memoryPath(f.chatA.Id));
  assert.equal(missing.status, 410);
  const deleted = JSON.parse(await readFile(join(f.root, 'Trash', f.chatA.Id, 'Memory', 'entries.json'), 'utf8'));
  assert.ok(deleted.entries.some(item => item.content === fact));
  await assert.rejects(stat(join(f.root, 'Projects', f.project.Id, 'Sessions', f.chatA.Id)), { code: 'ENOENT' });
});

test('concurrent user-memory HTTP mutations from different chat types share one document queue and revision', async t => {
  const f = await fixture(t);
  const ids = [f.chatA.Id, f.chatB.Id, f.otherProjectChat.Id, f.ordinary.Id, f.folderlessA.Id, f.folderlessB.Id];
  const contents = Array.from({ length: 12 }, (_, index) => `GLOBAL_CONCURRENT_FACT_${index}`);
  const results = await Promise.all(contents.map((content, index) =>
    f.raw('POST', f.memoryPath(ids[index % ids.length]), { scope: 'user', content })));
  assert.ok(results.every(result => result.status === 201));
  const documents = await Promise.all(ids.map(id => f.scopeDocument(id, 'user')));
  for (const document of documents) {
    assert.equal(document.scopeId, 'user');
    assert.equal(document.revision, contents.length);
    assert.equal(document.entries.length, contents.length);
    assert.ok(contents.every(content => document.entries.some(entry => entry.content === content)));
  }
  const snapshot = documents[0], entry = snapshot.entries[0];
  const competingEdits = await Promise.all([f.chatA.Id, f.folderlessB.Id].map((id, index) =>
    f.raw('PATCH', `${f.memoryPath(id)}/${entry.id}`, { scope: 'user', expectedRevision: snapshot.revision,
      content: `GLOBAL_REVISION_WINNER_${index}` })));
  assert.deepEqual(competingEdits.map(result => result.status).sort(), [200, 409]);
  const committed = await f.scopeDocument(f.ordinary.Id, 'user');
  assert.equal(committed.revision, snapshot.revision + 1);
  assert.equal(committed.entries.length, contents.length);
});

test('work relationships expose only same-work metadata and preserve identity when workspace folders change', async t => {
  const f = await fixture(t);
  await f.mutateCatalog(catalog => {
    const project = catalog.Projects.find(item => item.Id === f.project.Id);
    project.FolderPath = join(f.root, 'synthetic-workspace-relocated');
    project.Chats.find(item => item.Id === f.chatB.Id).IsArchived = true;
  });
  const related = await f.api('GET', `/api/conversations/${f.chatA.Id}/relationships`);
  assert.equal(related.conversationId, f.chatA.Id);
  assert.equal(related.projectId, f.project.Id);
  assert.equal(related.projectName, f.project.Name);
  assert.equal(related.isFolderlessWorkspace, false);
  assert.deepEqual(related.relatedConversations, [{ id: f.chatB.Id, title: f.chatB.Title, isArchived: true }]);
  assert.ok(!JSON.stringify(related).includes('fixture seed:'));
  assert.ok(!JSON.stringify(related).includes('Messages'));
  for (const id of [f.ordinary.Id, f.folderlessA.Id, f.folderlessB.Id]) {
    const value = await f.api('GET', `/api/conversations/${id}/relationships`);
    assert.deepEqual(value.relatedConversations, []);
    assert.equal(value.isFolderlessWorkspace, id !== f.ordinary.Id);
    assert.ok(!value.memoryScopes.some(item => (typeof item === 'string' ? item : item.scope) === 'project'));
  }
});

for (const protocol of protocols) {
  test(`${protocol}: injects confirmed memories in the native system field and reserves bounded output`, async t => {
    const f = await fixture(t, { contextWindowTokens: 2048 });
    const fact = `PROTOCOL_FACT_${protocol}_记忆必须到达实际模型请求`;
    await f.addMemory(f.chatA.Id, 'project', fact);
    await f.reply(f.chatB.Id, '读取工作记忆', { protocol });
    const request = f.seen.at(-1), system = systemText(request);
    assert.ok(system.includes(fact));
    const messages = upstreamMessages(request);
    assert.equal(messages.at(-1).role, 'user');
    assert.equal(messages.at(-1).content, '读取工作记忆');
    if (protocol === 'anthropic-messages') {
      assert.ok(request.body.system);
      assert.ok(!messages.some(item => ['system', 'developer'].includes(item.role)));
    } else if (protocol === 'openai-responses') {
      assert.ok(request.body.instructions);
      assert.ok(!messages.some(item => ['system', 'developer'].includes(item.role)));
    } else assert.ok(messages.some(item => ['system', 'developer'].includes(item.role)));
    const outputBudget = request.body.max_output_tokens ?? request.body.max_completion_tokens ?? request.body.max_tokens;
    assert.ok(Number.isInteger(outputBudget) && outputBudget > 0 && outputBudget < 2048,
      `invalid reserved output ${JSON.stringify(request.body)}`);
    const estimatedInput = estimateMessageTokens(messages, protocol === 'openai-completions' ? '' : system);
    assert.ok(estimatedInput + outputBudget + 256 <= 2048, 'input, reserved output and safety must fit the configured window');
  });
}

test('bounded session summary uses only this chat, excludes failures/reasoning/memory, and never truncates the authoritative log', async t => {
  const f = await fixture(t, { contextWindowTokens: 2048 });
  const fact = 'SUMMARY_MEMORY_FACT_删除后不可从会话摘要偷偷注入';
  const { entry } = await f.addMemory(f.chatA.Id, 'project', fact);
  const ownIds = [];
  for (let index = 0; index < 40; index++) {
    const question = user(`OWN_HISTORY_${index} ${'本聊天的历史记录 '.repeat(100)}`);
    const answer = { Id: randomUUID(), Role: 'assistant', Status: 'completed', Content: `OWN_ANSWER_${index} ${'回答内容 '.repeat(100)}`,
      Reasoning: 'PRIVATE_REASONING_SHOULD_NOT_BE_CONTEXT', ReplyTo: question.Id, CreatedAt: question.CreatedAt };
    ownIds.push(question.Id, answer.Id);
    await f.runtime.conversations.upsertMessage(f.chatB.Id, question);
    await f.runtime.conversations.upsertMessage(f.chatB.Id, answer);
  }
  const failedQuestion = user('FAILED_QUESTION_SHOULD_NOT_BE_CONTEXT');
  await f.runtime.conversations.upsertMessage(f.chatB.Id, failedQuestion);
  await f.runtime.conversations.upsertMessage(f.chatB.Id, { Id: randomUUID(), Role: 'assistant', Status: 'interrupted',
    Content: 'FAILED_PARTIAL_SHOULD_NOT_BE_CONTEXT', ReplyTo: failedQuestion.Id });
  await f.runtime.conversations.upsertMessage(f.otherProjectChat.Id, user('FOREIGN_HISTORY_SHOULD_NOT_BE_CONTEXT'));
  const logPath = join(f.root, 'Projects', f.project.Id, 'Sessions', f.chatB.Id, 'events.jsonl');
  const originalLog = await readFile(logPath);
  const beforeCount = (await f.runtime.conversations.readMessages(f.chatB.Id)).length;
  const sent = await f.reply(f.chatB.Id, 'CURRENT_INPUT_MUST_REMAIN');
  const request = f.seen.at(-1), serialized = promptText(request);
  assert.ok(serialized.includes('CURRENT_INPUT_MUST_REMAIN'));
  assert.ok(systemText(request).includes(fact));
  for (const forbidden of ['FOREIGN_HISTORY_SHOULD_NOT_BE_CONTEXT', 'FAILED_QUESTION_SHOULD_NOT_BE_CONTEXT',
    'FAILED_PARTIAL_SHOULD_NOT_BE_CONTEXT', 'PRIVATE_REASONING_SHOULD_NOT_BE_CONTEXT'])
    assert.ok(!serialized.includes(forbidden), forbidden);
  assert.ok(upstreamMessages(request).length < beforeCount, 'long history must be bounded, not sent in full');
  const history = await f.runtime.conversations.readMessages(f.chatB.Id);
  assert.equal(history.length, beforeCount + 2);
  assert.ok(ownIds.every(id => history.some(item => item.Id === id)));
  const updatedLog = await readFile(logPath);
  assert.ok(updatedLog.subarray(0, originalLog.length).equals(originalLog), 'existing JSONL bytes must stay unchanged');
  const summaryPath = join(f.root, 'Projects', f.project.Id, 'Sessions', f.chatB.Id, 'context.json');
  const summary = JSON.parse(await readFile(summaryPath, 'utf8'));
  assert.equal(summary.schemaVersion, 2);
  assert.ok(estimateTokens(summary.content) <= summary.excerptBudgetTokens, 'summary must stay within its own budget');
  assert.ok(!JSON.stringify(summary).includes(fact), 'long-term memory must not be copied into session summaries');
  assert.ok(!JSON.stringify(summary).includes('FOREIGN_HISTORY_SHOULD_NOT_BE_CONTEXT'));
  await writeFile(summaryPath, JSON.stringify({ ...summary, content: 'FORGED_SUMMARY_SCOPE_ESCAPE' }));
  // Retry the same input with a new assistant ID so the summarized history prefix
  // is identical: rejecting the injected text cannot be blamed on a new range.
  await f.reply(f.chatB.Id, 'CURRENT_INPUT_MUST_REMAIN', { userMessageId: sent.input.userMessageId });
  assert.ok(!promptText(f.seen.at(-1)).includes('FORGED_SUMMARY_SCOPE_ESCAPE'));
  assert.ok(!(await readFile(summaryPath, 'utf8')).includes('FORGED_SUMMARY_SCOPE_ESCAPE'));
  const document = await f.scopeDocument(f.chatB.Id, 'project');
  await f.api('DELETE', `${f.memoryPath(f.chatB.Id)}/${entry.id}`, { scope: 'project', expectedRevision: document.revision });
  await f.reply(f.chatB.Id, 'MEMORY_DELETED_CHECK');
  assert.ok(!promptText(f.seen.at(-1)).includes(fact));
});

test('memory candidates respect the request budget while all confirmed entries remain durable', async t => {
  const f = await fixture(t, { contextWindowTokens: 2048 });
  const facts = Array.from({ length: 12 }, (_, index) => `CANDIDATE_FACT_${index}:value-${index} ${'fact '.repeat(6)}`.trim());
  for (const fact of facts) await f.addMemory(f.chatA.Id, 'project', fact);
  await f.reply(f.chatB.Id, '优先满足当前输入，记忆不能撑破窗口');
  const request = f.seen.at(-1), injected = systemText(request);
  const injectedCount = facts.filter(fact => injected.includes(fact)).length;
  assert.ok(injectedCount > 0 && injectedCount < facts.length);
  assert.ok(estimateMessageTokens(upstreamMessages(request)) + request.body.max_tokens + 256 <= 2048);
  const document = await f.scopeDocument(f.chatB.Id, 'project');
  assert.equal(document.entries.length, facts.length);
  assert.ok(facts.every(fact => document.entries.some(item => item.content === fact)));
});

test('a verified Data migration preserves memory scopes, workspace relationships, and original conversation events', async t => {
  const f = await fixture(t), fact = 'MIGRATED_PROJECT_FACT_迁移后继续跨聊天使用';
  await f.reply(f.chatA.Id, `记住：${fact}`);
  await f.reply(f.ordinary.Id, '记住：MIGRATED_CHAT_FACT_普通聊天独立');
  const sourcePath = join(f.root, 'Projects', f.project.Id, 'Sessions', f.chatA.Id, 'events.jsonl');
  const sourceBytes = await readFile(sourcePath);
  const target = await mkdtemp(join(tmpdir(), 'kynxa-memory-integration-target-'));
  const pointer = join(f.root, 'synthetic-storage-pointer.json');
  await writeFile(pointer, JSON.stringify({ version: 1, dataRoot: f.root }));
  await f.runtime.close();
  await migrateStorage({ desktopSource: join(f.root, 'Desktop'), modelSource: f.dataHome, target, pointer });
  assert.equal(JSON.parse(await readFile(pointer, 'utf8')).dataRoot, target);
  await f.restart(join(target, 'Models'));
  await f.reply(f.chatB.Id, '迁移后读取工作事实');
  assert.ok(systemText(f.seen.at(-1)).includes(fact));
  assert.ok((await f.scopeDocument(f.chatB.Id, 'project')).entries.some(item => item.content === fact));
  assert.ok((await readFile(join(target, 'Projects', f.project.Id, 'Sessions', f.chatA.Id, 'events.jsonl'))).equals(sourceBytes));
  assert.ok((await readFile(sourcePath)).equals(sourceBytes), 'migration must keep its source untouched');
  const related = await f.api('GET', `/api/conversations/${f.chatB.Id}/relationships`);
  assert.equal(related.projectId, f.project.Id);
  await f.reply(f.ordinary.Id, '迁移后验证普通聊天');
  assert.ok(systemText(f.seen.at(-1)).includes('MIGRATED_CHAT_FACT_普通聊天独立'));
  assert.ok(!promptText(f.seen.at(-1)).includes(fact));
});

test('oversized current input fails before an upstream call and keeps the original submitted message intact', async t => {
  const f = await fixture(t, { contextWindowTokens: 2048 });
  const content = `OVERSIZED_CURRENT_${'内容'.repeat(9000)}`;
  const userMessageId = randomUUID(), requestId = randomUUID(), count = f.seen.length;
  const result = await f.raw('POST', '/api/chat', { conversationId: f.chatA.Id, provider: 'fixture-0', model: 'model-a',
    message: content, userMessageId, requestId });
  assert.ok(result.status >= 400 && result.status < 500, JSON.stringify(result));
  assert.equal(result.body.code, 'CONTEXT_INPUT_TOO_LARGE');
  assert.equal(f.seen.length, count);
  const messages = await f.runtime.conversations.readMessages(f.chatA.Id);
  assert.equal(messages.find(item => item.Id === userMessageId).Content, content);
  assert.equal(messages.find(item => item.Id === requestId).Status, 'error');
  const streamed = await f.raw('POST', '/api/chat/stream', { conversationId: f.chatA.Id,
    provider: 'fixture-0', model: 'model-a', message: content, userMessageId, requestId });
  assert.equal(streamed.status, 200);
  assert.deepEqual(streamed.body.map(event => event.type), ['started', 'error']);
  assert.equal(streamed.body.at(-1).code, 'CONTEXT_INPUT_TOO_LARGE');
  assert.equal(f.seen.length, count);
});

test('empty chats are not persisted and memory/relationship reads cannot create them', async t => {
  const f = await fixture(t), id = randomUUID();
  await f.mutateCatalog(catalog => { catalog.Projects.find(item => item.Id === f.project.Id)
    .Chats.push({ Id: id, Title: 'empty draft', Draft: 'unsent text', Messages: [] }); });
  const catalog = await f.api('GET', '/api/conversations/catalog');
  assert.ok(!catalog.Projects.some(project => project.Chats.some(chat => chat.Id === id)));
  for (const suffix of ['memory', 'relationships']) {
    const result = await f.raw('GET', `/api/conversations/${id}/${suffix}`);
    assert.equal(result.status, 404);
  }
  const directory = join(f.root, 'Projects', f.project.Id, 'Sessions', id);
  await assert.rejects(stat(directory), { code: 'ENOENT' });
});

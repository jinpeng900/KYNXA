import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtemp, readFile, readdir, rm, stat, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { after, test } from 'node:test';
import { ConversationStore } from '../data/conversations.mjs';
import { MemoryService } from '../data/memory-service.mjs';
import { ModelStore } from '../models/store.mjs';

// The server module's unused defaults must also resolve an isolated root at import time.
// server 模块未使用的默认实例在导入时也必须指向隔离的根目录。
const importRoot = await mkdtemp(join(tmpdir(), 'kynxa-memory-management-import-'));
const previousDataRoot = process.env.KYNXA_DATA_HOME;
process.env.KYNXA_DATA_HOME = importRoot;
const { createModelServer } = await import('../server.mjs');
if (previousDataRoot === undefined) delete process.env.KYNXA_DATA_HOME;
else process.env.KYNXA_DATA_HOME = previousDataRoot;

async function removeTempRoot(root) {
  const suffix = relative(resolve(tmpdir()), resolve(root));
  assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`), 'cleanup stays within the owned temp root');
  await rm(root, { recursive: true, force: true });
}
after(() => removeTempRoot(importRoot));

const user = (Id, Content) => ({ Id, Role: 'user', Content, Status: 'completed' });
const chat = () => ({ Id: randomUUID(), Title: 'Synthetic chat', Messages: [user(randomUUID(), 'Synthetic saved user message')] });
const project = (Chats = [], extra = {}) => ({ Id: randomUUID(), Name: 'Synthetic work', Chats, ...extra });

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-memory-management-'));
  const dataHome = join(root, 'Models');
  const conversations = new ConversationStore({ dataHome, legacyDesktopDirectory: null });
  const memory = new MemoryService({ conversationStore: conversations });
  const server = createModelServer({ modelStore: new ModelStore({ dataHome }), modelRuntime: { memory, conversations } });
  await new Promise(ready => server.listen(0, '127.0.0.1', ready));
  t.after(async () => {
    server.closeAllConnections();
    await new Promise(complete => server.close(complete));
    await removeTempRoot(root);
  });
  const base = `http://127.0.0.1:${server.address().port}`;
  const raw = async (method, path, input) => {
    const response = await fetch(`${base}${path}`, { method, headers: { 'Content-Type': 'application/json' },
      body: input === undefined ? undefined : JSON.stringify(input) });
    return { status: response.status, body: await response.json() };
  };
  const api = async (method, path, input) => {
    const result = await raw(method, path, input);
    assert.ok(result.status >= 200 && result.status < 300, `${method} ${path}: ${JSON.stringify(result)}`);
    return result.body;
  };
  const save = async mutation => {
    const catalog = await conversations.catalog();
    mutation(catalog);
    return conversations.saveCatalog(catalog);
  };
  return { root, conversations, memory, raw, api, save };
}

test('global HTTP CRUD works with an empty catalog, creates no chat, and preserves revisions across restart', async t => {
  const f = await fixture(t), path = '/api/memory/user';
  const initialCatalog = await f.conversations.catalog();
  const empty = await f.api('GET', path);
  assert.deepEqual(empty, { schemaVersion: 1, scope: 'user', scopeId: 'user', revision: 0, entries: [], dismissedSources: [] });
  await assert.rejects(stat(join(f.root, 'Memory', 'entries.json')), { code: 'ENOENT' });
  assert.equal((await f.api('GET', '/health')).memoryManagementProtocol, 1);
  const created = await f.api('POST', path, { content: '全局手动偏好', expectedRevision: 0 });
  const entry = created.entries[0];
  assert.equal(created.revision, 1);
  assert.equal(entry.kind, 'preference');
  assert.deepEqual(entry.source, { type: 'manual', role: 'user' });
  assert.equal(entry.active, true);
  assert.equal(entry.sourceAvailable, true);
  assert.equal(entry.sourceArchived, false);
  const file = join(f.root, 'Memory', 'entries.json');
  const persisted = JSON.parse(await readFile(file, 'utf8'));
  assert.equal(persisted.entries[0].source.conversationId, undefined);
  assert.equal(persisted.entries[0].active, undefined, 'availability is a response projection');
  const stale = await f.raw('PATCH', `${path}/${entry.id}`, { content: '旧版覆盖', expectedRevision: 0 });
  assert.equal(stale.status, 409);
  assert.equal(stale.body.code, 'MEMORY_CONFLICT');
  assert.equal((await f.raw('PATCH', `${path}/${entry.id}`, { content: '缺少版本' })).status, 400);
  const updated = await f.api('PATCH', `${path}/${entry.id}`, { content: '修改后的偏好', expectedRevision: 1 });
  assert.equal(updated.revision, 2);
  assert.equal(updated.entries[0].revision, 2);
  assert.equal(updated.entries[0].createdAt, entry.createdAt);
  assert.equal((await f.raw('DELETE', `${path}/${entry.id}`, { expectedRevision: 1 })).status, 409);
  const reopened = new MemoryService({ conversationStore: new ConversationStore({ dataHome: join(f.root, 'Models'), legacyDesktopDirectory: null }) });
  assert.equal((await reopened.listScope('user', 'user')).entries[0].content, '修改后的偏好');
  const deleted = await f.api('DELETE', `${path}/${entry.id}`, { expectedRevision: 2 });
  assert.equal(deleted.revision, 3);
  assert.deepEqual(deleted.entries, []);
  assert.deepEqual(await f.conversations.catalog(), initialCatalog);
  assert.deepEqual(await readdir(join(f.root, 'Chats')), []);
  assert.deepEqual(await readdir(join(f.root, 'Projects')), []);
});

test('work HTTP CRUD needs no chat, is isolated, and rejects folderless, unknown and removed work', async t => {
  const f = await fixture(t), work = project(), other = project(), folderless = project([], { IsFolderlessWorkspace: true });
  await f.save(catalog => { catalog.Projects = [work, other, folderless]; });
  const before = await f.conversations.catalog(), path = `/api/projects/${work.Id}/memory`;
  assert.equal((await f.api('GET', path)).revision, 0);
  const created = await f.api('POST', path, { content: '空工作中的决策', kind: 'decision', expectedRevision: 0,
    scopeId: other.Id, projectId: other.Id });
  assert.equal(created.scopeId, work.Id);
  assert.deepEqual(created.entries[0].source, { type: 'manual', role: 'user' });
  assert.deepEqual((await f.api('GET', `/api/projects/${other.Id}/memory`)).entries, []);
  assert.deepEqual(await f.conversations.catalog(), before);
  assert.deepEqual(await readdir(join(f.root, 'Projects', work.Id, 'Sessions')), []);
  assert.equal((await f.raw('POST', path, { content: '越界', scope: 'user' })).status, 400);
  assert.equal((await f.raw('GET', `/api/projects/${folderless.Id}/memory`)).status, 400);
  const unknown = randomUUID();
  const missing = await f.raw('POST', `/api/projects/${unknown}/memory`, { content: '不可创建工作' });
  assert.equal(missing.status, 404);
  assert.equal(missing.body.code, 'PROJECT_NOT_FOUND');
  await assert.rejects(stat(join(f.root, 'Projects', unknown)), { code: 'ENOENT' });
  const updated = await f.api('PATCH', `${path}/${created.entries[0].id}`, { content: '更新决策', expectedRevision: 1 });
  assert.equal(updated.revision, 2);
  await f.save(catalog => { catalog.Projects = catalog.Projects.filter(item => item.Id !== work.Id); });
  assert.equal((await f.raw('GET', path)).status, 404);
  assert.equal((await f.raw('DELETE', `${path}/${created.entries[0].id}`, { expectedRevision: 2 })).status, 404);
  assert.equal(JSON.parse(await readFile(join(f.root, 'Projects', work.Id, 'Memory', 'entries.json'), 'utf8')).revision, 2);
});

test('archived work remains editable through management while conversation context omits its shared memory', async t => {
  const f = await fixture(t), savedChat = chat(), work = project([savedChat], { IsArchived: true });
  await f.save(catalog => { catalog.Projects = [work]; });
  const path = `/api/projects/${work.Id}/memory`;
  const created = await f.api('POST', path, { content: '归档工作约定', expectedRevision: 0 });
  assert.equal(created.entries[0].active, true);
  assert.deepEqual((await f.memory.contextFor(savedChat.Id)).entries, []);
  assert.deepEqual((await f.api('GET', `/api/conversations/${savedChat.Id}/memory`)).scopes.map(scope => scope.scope), ['chat', 'user']);
  const updated = await f.api('PATCH', `${path}/${created.entries[0].id}`, { content: '归档后修改', expectedRevision: 1 });
  assert.equal(updated.entries[0].content, '归档后修改');
  await f.save(catalog => { catalog.Projects[0].IsArchived = false; });
  assert.equal((await f.memory.contextFor(savedChat.Id)).entries[0].content, '归档后修改');
  await f.api('DELETE', `${path}/${created.entries[0].id}`, { expectedRevision: 2 });
  assert.deepEqual((await f.memory.contextFor(savedChat.Id)).entries, []);
});

test('independent lists keep saved-message source status and deletion revocations compatible with conversation capture', async t => {
  const f = await fixture(t), savedChat = chat(), work = project([savedChat]), other = project();
  await f.save(catalog => { catalog.Projects = [work, other]; });
  const messageId = randomUUID(), instruction = '工作记住：明确来源的约定';
  await f.conversations.upsertMessage(savedChat.Id, user(messageId, instruction));
  const entry = await f.memory.captureExplicit(savedChat.Id, messageId, instruction);
  const path = `/api/projects/${work.Id}/memory`;
  let visible = await f.api('GET', path);
  assert.equal(visible.entries[0].active, true);
  await f.save(catalog => { catalog.Projects[0].Chats[0].IsArchived = true; });
  visible = await f.api('GET', path);
  assert.equal(visible.entries[0].active, true);
  assert.equal(visible.entries[0].sourceArchived, true);
  await f.save(catalog => { catalog.Projects[1].Chats.push(catalog.Projects[0].Chats.pop()); });
  visible = await f.api('GET', path);
  assert.equal(visible.entries[0].active, false);
  assert.equal(visible.entries[0].sourceAvailable, true);
  const movedChat = (await f.conversations.catalog()).Projects[1].Chats[0];
  await f.save(catalog => { catalog.Projects[1].Chats = []; });
  visible = await f.api('GET', path);
  assert.equal(visible.entries[0].sourceAvailable, false);
  assert.equal(visible.entries[0].active, false);
  await f.save(catalog => { catalog.Projects[0].Chats = [movedChat]; });
  visible = await f.api('GET', path);
  assert.equal(visible.entries[0].active, true);
  const deleted = await f.api('DELETE', `${path}/${entry.id}`, { expectedRevision: visible.revision });
  assert.equal(deleted.dismissedSources[0].messageId, messageId);
  assert.equal(await f.memory.captureExplicit(savedChat.Id, messageId, instruction), null);
  assert.deepEqual((await f.api('GET', path)).entries, []);
});

test('independent creation never weakens user-message verification and keeps old manual sources readable', async t => {
  const f = await fixture(t), savedChat = chat(), work = project([savedChat]);
  await f.save(catalog => { catalog.Projects = [work]; });
  const path = `/api/projects/${work.Id}/memory`, source = { type: 'user-message', role: 'user',
    conversationId: savedChat.Id, messageId: savedChat.Messages[0].Id };
  assert.equal((await f.raw('POST', path, { content: '不能旁路提取', source })).status, 400);
  assert.equal((await f.raw('POST', '/api/memory/user', { content: '缺少真实来源', source: { type: 'user-message' } })).status, 400);
  const chatPath = `/api/conversations/${savedChat.Id}/memory`;
  assert.equal((await f.raw('POST', chatPath, { scope: 'project', content: '伪造用户消息',
    source: { ...source, messageId: randomUUID() } })).status, 400);
  const legacy = await f.api('POST', chatPath, { scope: 'project', content: '旧手动来源' });
  assert.equal(legacy.entries[0].source.conversationId, savedChat.Id);
  assert.equal((await f.api('GET', path)).entries[0].source.conversationId, savedChat.Id);
  const current = await f.api('POST', path, { content: '新独立来源', expectedRevision: legacy.revision });
  assert.equal(current.entries[1].source.conversationId, undefined);
  const chatView = await f.api('GET', chatPath);
  assert.deepEqual(chatView.scopes.find(scope => scope.scope === 'project').entries.map(entry => entry.content), ['旧手动来源', '新独立来源']);
  assert.equal((await f.raw('PUT', '/api/memory/user', { content: 'unsupported' })).status, 405);
});

test('independent requests preserve future/corrupt files and concurrent edits report scope conflicts', async t => {
  const f = await fixture(t), path = '/api/memory/user';
  await f.api('GET', path);
  const file = join(f.root, 'Memory', 'entries.json'), future = '{"schemaVersion":99,"future":"preserve"}';
  await writeFile(file, future);
  const unsupported = await f.raw('POST', path, { content: '不能覆盖' });
  assert.equal(unsupported.status, 409);
  assert.equal(unsupported.body.code, 'UNSUPPORTED_MEMORY_VERSION');
  assert.equal(await readFile(file, 'utf8'), future);
  await writeFile(file, '{broken');
  assert.equal((await f.raw('GET', path)).body.code, 'CORRUPT_MEMORY');
  assert.equal(await readFile(file, 'utf8'), '{broken');
  await writeFile(file, JSON.stringify({ schemaVersion: 1, scope: 'user', scopeId: 'user', revision: 0, entries: [], dismissedSources: [] }));
  const results = await Promise.all([
    f.raw('POST', path, { content: '同时编辑 A', expectedRevision: 0 }),
    f.raw('POST', path, { content: '同时编辑 B', expectedRevision: 0 })
  ]);
  assert.deepEqual(results.map(result => result.status).sort(), [201, 409]);
  assert.equal((await f.api('GET', path)).entries.length, 1);
});

test('project removal waits for its in-flight independent memory mutation and never recreates a deleted work', async t => {
  const f = await fixture(t), work = project();
  await f.save(catalog => { catalog.Projects = [work]; });
  const catalog = await f.conversations.catalog();
  let notifyEntered, release;
  const entered = new Promise(complete => { notifyEntered = complete; });
  const paused = new Promise(complete => { release = complete; });
  const mutation = f.memory.repository.mutateScope('project', work.Id, 0, async document => {
    document.entries.push({ id: randomUUID(), scope: 'project', scopeId: work.Id, content: '事务内确认', kind: 'fact', status: 'confirmed',
      source: { type: 'manual', role: 'user' }, revision: 1, createdAt: new Date().toISOString(), updatedAt: new Date().toISOString() });
    notifyEntered();
    await paused;
  });
  await entered;
  catalog.Projects = [];
  let removed = false;
  const removal = f.conversations.saveCatalog(catalog).then(() => { removed = true; });
  try {
    await new Promise(complete => setImmediate(complete));
    assert.equal(removed, false);
  } finally {
    release();
    await Promise.all([mutation, removal]);
  }
  const persisted = JSON.parse(await readFile(join(f.root, 'Projects', work.Id, 'Memory', 'entries.json'), 'utf8'));
  assert.equal(persisted.entries[0].content, '事务内确认');
  const denied = await f.raw('POST', `/api/projects/${work.Id}/memory`, { content: '不能恢复工作' });
  assert.equal(denied.status, 404);
  assert.deepEqual((await f.conversations.catalog()).Projects, []);
});

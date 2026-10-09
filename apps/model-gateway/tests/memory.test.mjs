import assert from 'node:assert/strict';
import { cp, mkdtemp, readFile, readdir, rename, rm, stat, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { test } from 'node:test';
import { ConversationStore } from '../data/conversations.mjs';
import { MemoryService } from '../data/memory-service.mjs';
import { explicitMemoryInstruction } from '../data/memory-contracts.mjs';

const user = (Id, Content) => ({ Id, Role: 'user', Content, Status: 'completed' });
const chat = (Id, Messages = [user(`u-${Id}`, 'hello')]) => ({ Id, Title: Id, Messages });
const project = (Id, Chats, extra = {}) => ({ Id, Name: Id, FolderPath: null, Chats, ...extra });

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-memory-'));
  t.after(async () => {
    const suffix = relative(resolve(tmpdir()), resolve(root));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`), 'cleanup remains within owned temp root');
    await rm(root, { recursive: true, force: true });
  });
  const conversations = new ConversationStore({ dataHome: join(root, 'Models'), legacyDesktopDirectory: null });
  const initial = await conversations.catalog();
  await conversations.saveCatalog({ Revision: initial.Revision, Projects: [
    project('p-a', [chat('a1'), chat('a2')]), project('p-b', [chat('b1')]),
    project('folderless', [chat('f1'), chat('f2')], { IsFolderlessWorkspace: true })
  ], Chats: [chat('plain1'), chat('plain2')] });
  const memory = new MemoryService({ conversationStore: conversations });
  const capture = async (conversationId, messageId, message) => {
    await conversations.upsertMessage(conversationId, user(messageId, message));
    return memory.captureExplicit(conversationId, messageId, message);
  };
  const save = async document => conversations.saveCatalog({ ...document, Revision: (await conversations.catalog()).Revision });
  return { root, conversations, memory, capture, save };
}

test('explicit project memory is shared only inside its work; ordinary and folderless chats stay separate', async t => {
  const f = await fixture(t);
  const shared = await f.capture('a1', 'remember-project', '记住：项目统一使用 Python');
  assert.equal(shared.scope, 'project');
  assert.equal(shared.scopeId, 'p-a');
  assert.deepEqual((await f.memory.contextFor('a2')).entries.map(entry => entry.content), ['项目统一使用 Python']);
  for (const id of ['b1', 'plain1', 'plain2', 'f1', 'f2'])
    assert.deepEqual((await f.memory.contextFor(id)).entries, [], `project memory does not leak into ${id}`);
  assert.equal((await f.capture('plain1', 'remember-plain', '记住这个：只记在此聊天')).scope, 'chat');
  assert.equal((await f.capture('f1', 'remember-folderless', '记住：无文件夹聊天自己的约定')).scope, 'chat');
  assert.deepEqual((await f.memory.contextFor('plain2')).entries, []);
  assert.deepEqual((await f.memory.contextFor('f2')).entries, []);
  assert.deepEqual((await f.memory.listFor('f1')).scopes.map(item => item.scope), ['chat', 'user']);
  await assert.rejects(f.capture('f1', 'invalid-project', '项目记住：不能共享隐藏项目'), /未关联项目/);
});

test('explicit chat memory overrides project default; global memory requires its own prefix', async t => {
  const f = await fixture(t);
  await f.capture('a1', 'local', '聊天记住：此聊天的临时约定');
  assert.deepEqual((await f.memory.contextFor('a2')).entries, []);
  const global = await f.capture('plain1', 'global', '全局记住：用中文回答');
  assert.equal(global.scope, 'user');
  assert.equal(global.kind, 'preference');
  for (const id of ['a2', 'b1', 'plain2', 'f2'])
    assert.deepEqual((await f.memory.contextFor(id)).entries.map(item => item.content), ['用中文回答']);
  const view = await f.memory.listFor('a1');
  assert.equal(view.scopes.find(scope => scope.scope === 'chat').entries[0].content, '此聊天的临时约定');
});

test('projected memory snapshots detect edits and removal but ignore unrelated changes', async t => {
  const f = await fixture(t);
  const document = await f.memory.create('a1', { scope: 'project', content: 'Confirmed release requirement' });
  const id = document.entries[0].id;
  const context = await f.memory.contextFor('a2'), snapshot = f.memory.snapshotFor(context, [id]);
  assert.equal((await f.memory.validateSnapshot('a2', snapshot)).current, true);
  await f.memory.create('a1', { scope: 'chat', content: 'Private sibling note' });
  await f.memory.create('a2', { scope: 'project', content: 'Unrelated new record' });
  assert.equal((await f.memory.validateSnapshot('a2', snapshot)).current, true);
  await f.memory.update('a1', id, { scope: 'project', expectedRevision: 2, content: 'Updated release requirement' });
  const changed = await f.memory.validateSnapshot('a2', snapshot);
  assert.equal(changed.current, false);
  assert.equal(changed.invalidSources[0].code, 'MEMORY_SOURCE_CHANGED');
  assert.doesNotMatch(JSON.stringify(changed), /Confirmed release requirement|Updated release requirement|Private sibling note/);
  const refreshed = f.memory.snapshotFor(await f.memory.contextFor('a2'), [id]);
  await f.memory.delete('a1', id, { scope: 'project', expectedRevision: 3 });
  assert.equal((await f.memory.validateSnapshot('a2', refreshed)).invalidSources[0].code, 'MEMORY_SOURCE_UNAVAILABLE');
});

test('only saved user messages create automatic memory; exact replay is idempotent', async t => {
  const f = await fixture(t), message = '记住：保留来源';
  const first = await f.capture('a1', 'explicit-user', message);
  const repeated = await f.memory.captureExplicit('a1', 'explicit-user', message);
  assert.equal(repeated.id, first.id);
  assert.equal((await f.memory.listFor('a1')).scopes.find(item => item.scope === 'project').revision, 1);
  await f.conversations.upsertMessage('a1', { Id: 'assistant-source', Role: 'assistant', Content: message, Status: 'completed' });
  await assert.rejects(f.memory.captureExplicit('a1', 'assistant-source', message), /真实用户消息/);
  await assert.rejects(f.memory.captureExplicit('a1', 'explicit-user', '记住：伪造内容'), /真实用户消息/);
  await assert.rejects(f.memory.create('a1', { scope: 'project', content: '伪造源', source:
    { type: 'user-message', role: 'user', conversationId: 'a2', messageId: 'u-a2' } }), /当前聊天/);
  await assert.rejects(f.memory.create('a1', { scope: 'project', content: '伪造角色', source:
    { type: 'user-message', role: 'assistant', conversationId: 'a1', messageId: 'assistant-source' } }), /来源格式无效/);
  for (const content of ['请给我讲讲记住：这个词', '```text\n记住：代码示例\n```', '> 记住：引用内容', '普通问题'])
    assert.equal(explicitMemoryInstruction(content, true), null);
});

test('manual memory CRUD is versioned, isolated and available without a source message', async t => {
  const f = await fixture(t);
  const created = await f.memory.create('a1', { scope: 'project', content: '初始决策', kind: 'decision', projectId: 'p-b' });
  assert.equal(created.scopeId, 'p-a', 'client projectId cannot choose storage ownership');
  assert.equal(created.revision, 1);
  assert.equal(created.entries[0].source.type, 'manual');
  assert.deepEqual((await f.memory.contextFor('b1')).entries, []);
  const updated = await f.memory.update('a2', created.entries[0].id, { scope: 'project', expectedRevision: 1, content: '更新决策' });
  assert.equal(updated.revision, 2);
  assert.equal(updated.entries[0].revision, 2);
  assert.equal(updated.entries[0].createdAt, created.entries[0].createdAt);
  await assert.rejects(f.memory.update('a1', created.entries[0].id, { scope: 'project', expectedRevision: 1, content: '旧版本覆盖' }),
    { code: 'MEMORY_CONFLICT', statusCode: 409 });
  await assert.rejects(f.memory.delete('a1', created.entries[0].id, { scope: 'project' }), /expectedRevision/);
  const deleted = await f.memory.delete('a1', created.entries[0].id, { scope: 'project', expectedRevision: 2 });
  assert.equal(deleted.revision, 3);
  assert.deepEqual((await f.memory.contextFor('a2')).entries, []);
  assert.deepEqual(JSON.parse(await readFile(join(f.root, 'Projects', 'p-a', 'Memory', 'entries.json'), 'utf8')).entries, []);
});

test('deleting explicit memory stays deleted when a failed original turn is retried', async t => {
  const f = await fixture(t), instruction = '记住：允许删除的约定';
  const entry = await f.capture('a1', 'remember-delete', instruction);
  await f.memory.delete('a2', entry.id, { scope: 'project', expectedRevision: 1 });
  assert.equal(await f.memory.captureExplicit('a1', 'remember-delete', instruction), null);
  assert.deepEqual((await f.memory.contextFor('a2')).entries, []);
  assert.equal((await f.memory.listFor('a2')).scopes.find(item => item.scope === 'project').revision, 2);
  const newEntry = await f.capture('a1', 'remember-again', instruction);
  assert.notEqual(newEntry.id, entry.id, 'a new explicit user request can remember it again');
});

test('API shorthand source is normalized but explicit wrong roles remain rejected', async t => {
  const f = await fixture(t);
  const document = await f.memory.create('a1', { scope: 'chat', content: '确认来源',
    source: { conversationId: 'a1', messageId: 'u-a1' } });
  assert.equal(document.entries[0].source.type, 'user-message');
  assert.equal(document.entries[0].source.role, 'user');
  await assert.rejects(f.memory.create('a1', { scope: 'chat', content: '错误来源',
    source: { role: 'assistant', conversationId: 'a1', messageId: 'u-a1' } }), /来源格式无效/);
});

test('archived project does not contribute a shared memory scope', async t => {
  const f = await fixture(t);
  await f.capture('a1', 'before-archive', '记住：项目级约定');
  const catalog = await f.conversations.catalog();
  catalog.Projects[0].IsArchived = true;
  await f.save(catalog);
  assert.deepEqual((await f.memory.contextFor('a2')).entries, []);
  assert.deepEqual((await f.memory.listFor('a2')).scopes.map(item => item.scope), ['chat', 'user']);
  await assert.rejects(f.memory.create('a2', { scope: 'project', content: '归档项目不写新共享记忆' }), /未关联/);
});

test('archiving source keeps confirmed project memory; deletion pauses it and undo restores it', async t => {
  const f = await fixture(t);
  await f.capture('a1', 'remember-source', '工作记住：已确认约定');
  let catalog = await f.conversations.catalog();
  const savedSource = structuredClone(catalog.Projects[0].Chats[0]);
  catalog.Projects[0].Chats[0].IsArchived = true;
  await f.save(catalog);
  assert.equal((await f.memory.contextFor('a2')).entries.length, 1);
  await f.memory.create('a2', { scope: 'project', content: '手动约定独立于来源聊天' });
  catalog = await f.conversations.catalog();
  catalog.Projects[0].Chats = catalog.Projects[0].Chats.filter(item => item.Id !== 'a1');
  await f.save(catalog);
  assert.deepEqual((await f.memory.contextFor('a2')).entries.map(item => item.content), ['手动约定独立于来源聊天']);
  assert.equal((await f.memory.listFor('a2')).scopes.find(scope => scope.scope === 'project').entries.length, 2,
    'paused memory is retained for undo and can be reviewed/deleted');
  catalog = await f.conversations.catalog();
  catalog.Projects[0].Chats.push(savedSource);
  await f.save(catalog);
  assert.equal((await f.memory.contextFor('a2')).entries.length, 2);
});

test('moving a source chat preserves its own memory and summary but stops sharing old project memory', async t => {
  const f = await fixture(t);
  await f.capture('a1', 'project-memory', '记住：属于原项目');
  await f.capture('a1', 'chat-memory', '聊天记住：跟随此聊天');
  const summary = { schemaVersion: 2, conversationId: 'a1', algorithm: 'extractive-v2', content: '聊天摘要', sourceHash: 'hash' };
  await f.memory.repository.writeSummary('a1', summary);
  const catalog = await f.conversations.catalog();
  const moving = catalog.Projects[0].Chats.shift();
  catalog.Projects[1].Chats.push(moving);
  await f.save(catalog);
  assert.deepEqual((await f.memory.contextFor('a2')).entries, []);
  assert.deepEqual((await f.memory.contextFor('a1')).entries.map(entry => entry.content), ['跟随此聊天']);
  assert.deepEqual((await f.memory.contextFor('b1')).entries, [], 'old project memory is not copied into destination project');
  assert.deepEqual(await f.memory.repository.readSummary('a1'), summary);
  assert.ok(await readFile(join(f.root, 'Projects', 'p-b', 'Sessions', 'a1', 'Memory', 'entries.json')));
});

test('restart and data-root copy preserve memory; writing memory and summary never changes events.jsonl', async t => {
  const f = await fixture(t);
  const eventsFile = join(f.root, 'Projects', 'p-a', 'Sessions', 'a1', 'events.jsonl');
  const original = await readFile(eventsFile);
  await f.memory.create('a1', { scope: 'project', content: '跨重启记忆' });
  await f.memory.repository.writeSummary('a1', { schemaVersion: 2, conversationId: 'a1', algorithm: 'extractive-v2', content: '摘要' });
  assert.deepEqual(await readFile(eventsFile), original);
  const reopened = new ConversationStore({ dataHome: join(f.root, 'Models'), legacyDesktopDirectory: null });
  const memory = new MemoryService({ conversationStore: reopened });
  assert.equal((await memory.contextFor('a2')).entries[0].content, '跨重启记忆');
  assert.equal((await memory.repository.readSummary('a1')).content, '摘要');
  const migrated = join(f.root, 'copy-parent');
  // Copy only canonical components into a new root rather than recursively copying a root into itself.
  // 只将规范目录组件复制到新根目录，避免把根目录递归复制到自身。
  for (const component of ['Projects', 'Chats', 'Memory', 'catalog.json', 'settings.json', '.conversations-v1.json'])
    await cp(join(f.root, component), join(migrated, component), { recursive: true });
  const movedStore = new ConversationStore({ dataHome: join(migrated, 'Models'), legacyDesktopDirectory: null });
  const movedMemory = new MemoryService({ conversationStore: movedStore });
  assert.equal((await movedMemory.contextFor('a2')).entries[0].content, '跨重启记忆');
  assert.equal((await movedMemory.repository.readSummary('a1')).content, '摘要');
});

test('concurrent creates through separate repository instances serialize without losing entries', async t => {
  const f = await fixture(t), second = new MemoryService({ conversationStore: f.conversations });
  await Promise.all(Array.from({ length: 12 }, (_, index) => (index % 2 ? second : f.memory).create('a1',
    { scope: 'project', content: `记忆${index}` })));
  const document = (await f.memory.listFor('a1')).scopes.find(scope => scope.scope === 'project');
  assert.equal(document.revision, 12);
  assert.equal(document.entries.length, 12);
  assert.equal(new Set(document.entries.map(item => item.id)).size, 12);
});

test('unsupported or corrupt memory and summary versions are preserved and never overwritten', async t => {
  const f = await fixture(t);
  const memoryFile = join(f.root, 'Memory', 'entries.json'), future = '{"schemaVersion":99,"privateFutureData":"keep"}';
  await writeFile(memoryFile, future);
  await assert.rejects(f.memory.create('plain1', { scope: 'user', content: '不要覆盖' }), { code: 'UNSUPPORTED_MEMORY_VERSION' });
  assert.equal(await readFile(memoryFile, 'utf8'), future);
  await writeFile(memoryFile, '{broken');
  await assert.rejects(f.memory.listFor('plain1'), { code: 'CORRUPT_MEMORY' });
  assert.equal(await readFile(memoryFile, 'utf8'), '{broken');
  const summaryFile = join(f.root, 'Chats', 'plain1', 'context.json');
  await writeFile(summaryFile, '{"schemaVersion":99}');
  await assert.rejects(f.memory.repository.writeSummary('plain1', { schemaVersion: 2, conversationId: 'plain1', content: 'old' }),
    { code: 'UNSUPPORTED_SUMMARY_VERSION' });
  assert.equal(await readFile(summaryFile, 'utf8'), '{"schemaVersion":99}');
  await assert.rejects(f.memory.repository.writeSummary('plain1', { schemaVersion: 2, conversationId: 'plain2', content: 'wrong chat' }), /归属无效/);
});

test('linked memory directories and unsafe IDs are rejected before writing', async t => {
  const f = await fixture(t);
  await assert.rejects(f.memory.listFor('../outside'), /ID 无效/);
  const memoryFolder = join(f.root, 'Memory'), originalFolder = join(f.root, 'Memory-original');
  await rename(memoryFolder, originalFolder);
  try { await symlink(originalFolder, memoryFolder, process.platform === 'win32' ? 'junction' : 'dir'); }
  catch (error) {
    await rename(originalFolder, memoryFolder);
    if (['EPERM', 'EACCES'].includes(error.code)) return t.skip('symlink creation unavailable on this host');
    throw error;
  }
  await assert.rejects(f.memory.create('plain1', { scope: 'user', content: '不能写入链接目录' }), { code: 'INVALID_MEMORY_PATH' });
});

function memoryDocument(entries, dismissedSources = []) {
  return { schemaVersion: 1, scope: 'project', scopeId: 'p-a', revision: 1, entries, dismissedSources };
}

function memoryEntry(index, content = '字'.repeat(4000)) {
  return { id: `memory-${index}`, scope: 'project', scopeId: 'p-a', content, kind: 'fact', status: 'confirmed',
    source: { type: 'manual', role: 'user', conversationId: 'a1' }, revision: 1,
    createdAt: '2026-01-01T00:00:00.000Z', updatedAt: '2026-01-01T00:00:00.000Z' };
}

test('memory capacity measures written indented UTF-8 bytes; a refused large append leaves readable data', async t => {
  const f = await fixture(t), file = join(f.root, 'Projects', 'p-a', 'Memory', 'entries.json');
  const initial = memoryDocument(Array.from({ length: 677 }, (_, index) => memoryEntry(index)));
  const previous = JSON.stringify(initial, null, 2);
  assert.ok(Buffer.byteLength(previous) < 8 * 1024 * 1024);
  const overflowing = memoryDocument([...initial.entries, memoryEntry(677)]);
  assert.ok(Buffer.byteLength(JSON.stringify(overflowing)) < 8 * 1024 * 1024,
    'compact-size-only checks would incorrectly accept this append');
  assert.ok(Buffer.byteLength(JSON.stringify(overflowing, null, 2)) > 8 * 1024 * 1024);
  await writeFile(file, previous);
  await assert.rejects(f.memory.create('a1', { scope: 'project', content: '字'.repeat(4000) }), { code: 'MEMORY_CAPACITY_EXCEEDED' });
  assert.equal(await readFile(file, 'utf8'), previous);
  assert.equal((await f.memory.repository.read('a1', 'project')).entries.length, 677);
  await f.memory.create('a1', { scope: 'project', content: '短条目' });
  assert.ok((await stat(file)).size <= 8 * 1024 * 1024);
  assert.equal((await f.memory.repository.read('a1', 'project')).entries.length, 678, 'successful writes can always be read back');
});

test('summary size uses its actual written bytes and retains the old summary on overflow', async t => {
  const f = await fixture(t), old = { schemaVersion: 2, conversationId: 'a1', content: '原摘要' };
  await f.memory.repository.writeSummary('a1', old);
  const oversized = { schemaVersion: 2, conversationId: 'a1', content: 'x'.repeat(8 * 1024 * 1024 - 70000),
    diagnostic: Array.from({ length: 10000 }, () => 'x') };
  assert.ok(Buffer.byteLength(JSON.stringify(oversized)) < 8 * 1024 * 1024);
  assert.ok(Buffer.byteLength(JSON.stringify(oversized, null, 2)) > 8 * 1024 * 1024);
  await assert.rejects(f.memory.repository.writeSummary('a1', oversized), /摘要过大/);
  assert.deepEqual(await f.memory.repository.readSummary('a1'), old);
});

test('many remembered-source revocations remain bounded by byte capacity without silent eviction or resurrection', async t => {
  const f = await fixture(t), file = join(f.root, 'Projects', 'p-a', 'Memory', 'entries.json');
  const dismissed = Array.from({ length: 12000 }, (_, index) => ({
    conversationId: 'a1', messageId: `deleted-${index}`, deletedAt: '2026-01-01T00:00:00.000Z'
  }));
  await writeFile(file, JSON.stringify(memoryDocument([], dismissed), null, 2));
  await f.conversations.upsertMessage('a1', user('deleted-0', '记住：已撤销的最早约定'));
  assert.equal(await f.memory.captureExplicit('a1', 'deleted-0', '记住：已撤销的最早约定'), null,
    'oldest cancellations must not be evicted to allow original retries to resurrect memory');
  const entry = await f.capture('a1', 'new-source', '记住：新约定');
  const beforeDelete = (await stat(file)).size;
  const document = await f.memory.repository.read('a1', 'project');
  await f.memory.delete('a2', entry.id, { scope: 'project', expectedRevision: document.revision });
  assert.ok((await stat(file)).size < beforeDelete, 'deleting an entry frees bytes even when a source revocation is recorded');
  assert.equal((await f.memory.repository.read('a1', 'project')).dismissedSources.length, 12001);
  assert.ok((await stat(file)).size < 8 * 1024 * 1024);
});

test('summary reads and writes reject a linked session parent and preserve its external target', async t => {
  const f = await fixture(t), folder = join(f.root, 'Projects', 'p-a', 'Sessions', 'a1'), original = `${folder}-original`;
  const summary = { schemaVersion: 2, conversationId: 'a1', content: '外部内容不能改' };
  await f.memory.repository.writeSummary('a1', summary);
  await rename(folder, original);
  try { await symlink(original, folder, process.platform === 'win32' ? 'junction' : 'dir'); }
  catch (error) {
    await rename(original, folder);
    if (['EPERM', 'EACCES'].includes(error.code)) return t.skip('directory link creation unavailable on this host');
    throw error;
  }
  await assert.rejects(f.memory.repository.readSummary('a1'), /包含链接/);
  await assert.rejects(f.memory.repository.writeSummary('a1', { ...summary, content: '不得写入' }), /包含链接/);
  assert.deepEqual(JSON.parse(await readFile(join(original, 'context.json'), 'utf8')), summary);
});

test('summary reads and writes reject a linked file itself', async t => {
  const f = await fixture(t), file = join(f.root, 'Projects', 'p-a', 'Sessions', 'a1', 'context.json'), target = join(f.root, 'summary-target.json');
  const original = JSON.stringify({ schemaVersion: 2, conversationId: 'a1', content: '链接目标保留' });
  await writeFile(target, original);
  try { await symlink(target, file, 'file'); }
  catch (error) {
    if (['EPERM', 'EACCES'].includes(error.code)) return t.skip('file symlink creation unavailable on this host');
    throw error;
  }
  await assert.rejects(f.memory.repository.readSummary('a1'), /包含链接/);
  await assert.rejects(f.memory.repository.writeSummary('a1', { schemaVersion: 2, conversationId: 'a1', content: '不能写入' }), /包含链接/);
  assert.equal(await readFile(target, 'utf8'), original);
});

test('corrupt derived summary is backed up exactly once and can be rebuilt from the original chat', async t => {
  const f = await fixture(t), folder = join(f.root, 'Projects', 'p-a', 'Sessions', 'a1'), file = join(folder, 'context.json');
  const corrupt = Buffer.from('\uFEFF{"schemaVersion":1,"content":"中断的摘要', 'utf8');
  const originalEvents = await readFile(join(folder, 'events.jsonl'));
  await writeFile(file, corrupt);
  assert.deepEqual(await Promise.all(Array.from({ length: 8 }, () => f.memory.repository.readSummary('a1'))), Array(8).fill(null));
  await assert.rejects(readFile(file), { code: 'ENOENT' });
  const backups = (await readdir(folder)).filter(name => /^context\.corrupt-.*\.json$/.test(name));
  assert.equal(backups.length, 1, 'per-file queue allows only one corrupt canonical-file recovery');
  assert.deepEqual(await readFile(join(folder, backups[0])), corrupt);
  const rebuilt = { schemaVersion: 2, conversationId: 'a1', algorithm: 'extractive-v2', content: '从原始日志重建' };
  await f.memory.repository.writeSummary('a1', rebuilt);
  assert.deepEqual(await f.memory.repository.readSummary('a1'), rebuilt);
  assert.deepEqual(await readFile(join(folder, 'events.jsonl')), originalEvents);
});

test('a failed corrupt-summary diagnostic backup propagates without removing or overwriting the canonical file', async t => {
  const f = await fixture(t), folder = join(f.root, 'Projects', 'p-a', 'Sessions', 'a1'), file = join(folder, 'context.json');
  const corrupt = '{"schemaVersion":1,"unfinished":';
  await writeFile(file, corrupt);
  t.mock.method(f.memory.repository, '_preserveCorruptSummary', async () => { throw Object.assign(new Error('backup storage full'), { code: 'ENOSPC' }); });
  await assert.rejects(f.memory.repository.readSummary('a1'), { code: 'ENOSPC' });
  assert.equal(await readFile(file, 'utf8'), corrupt);
  await assert.rejects(f.memory.repository.writeSummary('a1', { schemaVersion: 2, conversationId: 'a1', content: '不得覆盖' }),
    { code: 'CORRUPT_SUMMARY' });
  assert.equal(await readFile(file, 'utf8'), corrupt);
  assert.deepEqual((await readdir(folder)).filter(name => name.startsWith('context.corrupt-')), []);
});

test('moving a chat waits for its in-flight memory mutation and carries the completed memory with the session', async t => {
  const f = await fixture(t), catalog = await f.conversations.catalog();
  const movingChat = catalog.Projects[0].Chats.shift();
  catalog.Projects[1].Chats.push(movingChat);
  let notifyEntered, release;
  const entered = new Promise(resolve => { notifyEntered = resolve; });
  const paused = new Promise(resolve => { release = resolve; });
  const mutation = f.memory.repository.mutate('a1', 'chat', undefined, async document => {
    document.entries.push({ ...memoryEntry(0, '移动时仍应保留'), scope: 'chat', scopeId: 'a1' });
    notifyEntered();
    await paused;
  });
  await entered;
  let moved = false;
  const move = f.conversations.saveCatalog(catalog).then(() => { moved = true; });
  try {
    await new Promise(resolve => setTimeout(resolve, 80));
    assert.equal(moved, false, 'catalog move cannot pass a paused memory transaction and strand its write in the old path');
  } finally {
    release();
    await Promise.all([mutation, move]);
  }
  assert.deepEqual((await f.memory.repository.read('a1', 'chat')).entries.map(entry => entry.content), ['移动时仍应保留']);
  await assert.rejects(stat(join(f.root, 'Projects', 'p-a', 'Sessions', 'a1')), { code: 'ENOENT' });
});

test('deleting a chat waits for its in-flight memory write, preserves it in Trash and does not recreate its old session', async t => {
  const f = await fixture(t), catalog = await f.conversations.catalog();
  const savedChat = catalog.Projects[0].Chats.shift();
  let notifyEntered, release;
  const entered = new Promise(resolve => { notifyEntered = resolve; });
  const paused = new Promise(resolve => { release = resolve; });
  const mutation = f.memory.repository.mutate('a1', 'chat', undefined, async document => {
    document.entries.push({ ...memoryEntry(0, '删除与撤销时保留'), scope: 'chat', scopeId: 'a1' });
    notifyEntered();
    await paused;
  });
  await entered;
  let deleted = false;
  const deleting = f.conversations.saveCatalog(catalog).then(() => { deleted = true; });
  try {
    await new Promise(resolve => setTimeout(resolve, 80));
    assert.equal(deleted, false, 'a delete transaction must wait until the dependent memory file is complete');
  } finally {
    release();
    await Promise.all([mutation, deleting]);
  }
  await assert.rejects(stat(join(f.root, 'Projects', 'p-a', 'Sessions', 'a1')), { code: 'ENOENT' });
  const trashed = JSON.parse(await readFile(join(f.root, 'Trash', 'a1', 'Memory', 'entries.json'), 'utf8'));
  assert.equal(trashed.entries[0].content, '删除与撤销时保留');
  await assert.rejects(f.memory.repository.read('a1', 'chat'), { code: 'CONVERSATION_DELETED' });
  await assert.rejects(f.memory.repository.create('a1', { scope: 'chat', content: '不能在已删除会话写回',
    source: { type: 'manual', role: 'user', conversationId: 'a1' } }), { code: 'CONVERSATION_DELETED' });
  const restore = await f.conversations.catalog();
  restore.Projects[0].Chats.push(savedChat);
  await f.conversations.saveCatalog(restore);
  assert.equal((await f.memory.repository.read('a1', 'chat')).entries[0].content, '删除与撤销时保留');
});

test('a pending summary write shares the same catalog guard and follows a subsequent chat move', async t => {
  const f = await fixture(t), catalog = await f.conversations.catalog(), originalSafe = f.memory.repository._safe.bind(f.memory.repository);
  const movingChat = catalog.Projects[0].Chats.shift();
  catalog.Projects[1].Chats.push(movingChat);
  let notifyEntered, release, pausedOnce = false;
  const entered = new Promise(resolve => { notifyEntered = resolve; });
  const paused = new Promise(resolve => { release = resolve; });
  t.mock.method(f.memory.repository, '_safe', async (file, options) => {
    if (!pausedOnce && file.endsWith(`${sep}context.json`)) {
      pausedOnce = true;
      notifyEntered();
      await paused;
    }
    return originalSafe(file, options);
  });
  const summary = { schemaVersion: 2, conversationId: 'a1', content: '跟随聊天的摘要' };
  const writing = f.memory.repository.writeSummary('a1', summary);
  await entered;
  let moved = false;
  const moving = f.conversations.saveCatalog(catalog).then(() => { moved = true; });
  try {
    await new Promise(resolve => setTimeout(resolve, 80));
    assert.equal(moved, false);
  } finally {
    release();
    await Promise.all([writing, moving]);
  }
  assert.deepEqual(await f.memory.repository.readSummary('a1'), summary);
  await assert.rejects(stat(join(f.root, 'Projects', 'p-a', 'Sessions', 'a1')), { code: 'ENOENT' });
});

test('memory list projectId and project-scope files use one ownership snapshot across concurrent chat moves', async t => {
  const f = await fixture(t);
  await f.memory.create('a1', { scope: 'project', content: '原项目记忆' });
  await f.memory.create('b1', { scope: 'project', content: '目标项目记忆' });
  const catalog = await f.conversations.catalog(), originalRead = f.memory.repository._read.bind(f.memory.repository);
  catalog.Projects[1].Chats.push(catalog.Projects[0].Chats.shift());
  let notifyEntered, release, pausedOnce = false;
  const entered = new Promise(resolve => { notifyEntered = resolve; });
  const paused = new Promise(resolve => { release = resolve; });
  t.mock.method(f.memory.repository, '_read', async location => {
    if (!pausedOnce && location.scope === 'chat' && location.scopeId === 'a1') {
      pausedOnce = true;
      notifyEntered();
      await paused;
    }
    return originalRead(location);
  });
  const listing = f.memory.listFor('a1');
  await entered;
  let moved = false;
  const moving = f.conversations.saveCatalog(catalog).then(() => { moved = true; });
  let snapshot;
  try {
    await new Promise(resolve => setTimeout(resolve, 80));
    assert.equal(moved, false, 'catalog move cannot change ownership between reading scope files');
  } finally {
    release();
    [snapshot] = await Promise.all([listing, moving]);
  }
  assert.equal(snapshot.projectId, 'p-a');
  const projectScope = snapshot.scopes.find(scope => scope.scope === 'project');
  assert.equal(projectScope.scopeId, snapshot.projectId);
  assert.equal(projectScope.entries[0].content, '原项目记忆');
  const after = await f.memory.listFor('a1');
  assert.equal(after.projectId, 'p-b');
  assert.equal(after.scopes.find(scope => scope.scope === 'project').entries[0].content, '目标项目记忆');
});

test('a failed scope read keeps the catalog guard until all other in-flight scope reads finish', async t => {
  const f = await fixture(t), catalog = await f.conversations.catalog(), originalRead = f.memory.repository._read.bind(f.memory.repository);
  catalog.Projects[1].Chats.push(catalog.Projects[0].Chats.shift());
  await writeFile(join(f.root, 'Projects', 'p-a', 'Memory', 'entries.json'), '{corrupt-project-memory');
  let notifyEntered, release, pausedOnce = false;
  const entered = new Promise(resolve => { notifyEntered = resolve; });
  const paused = new Promise(resolve => { release = resolve; });
  t.mock.method(f.memory.repository, '_read', async location => {
    if (!pausedOnce && location.scope === 'chat' && location.scopeId === 'a1') {
      pausedOnce = true;
      notifyEntered();
      await paused;
    }
    return originalRead(location);
  });
  let listingSettled = false;
  const listing = f.memory.listFor('a1').then(value => { listingSettled = true; return { value }; },
    error => { listingSettled = true; return { error }; });
  await entered;
  let moved = false;
  const moving = f.conversations.saveCatalog(catalog).then(() => { moved = true; });
  let outcome;
  try {
    await new Promise(resolve => setTimeout(resolve, 80));
    assert.equal(listingSettled, false, 'the early project-scope failure waits for the paused chat-scope IO');
    assert.equal(moved, false, 'the catalog guard remains held until all attempted scope IO has settled');
  } finally {
    release();
    [outcome] = await Promise.all([listing, moving]);
  }
  assert.equal(outcome.error?.code, 'CORRUPT_MEMORY', 'the original scope error remains visible after the guard can safely end');
  assert.equal(moved, true);
  await assert.rejects(stat(join(f.root, 'Projects', 'p-a', 'Sessions', 'a1')), { code: 'ENOENT' });
});

import assert from 'node:assert/strict';
import { appendFile, mkdir, mkdtemp, readFile, readdir, stat, unlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { test } from 'node:test';
import { ConversationStore } from '../data/conversations.mjs';

const user = (Id = 'u1', Content = '你好') => ({ Id, Role: 'user', Content, CreatedAt: '2026-01-01T00:00:00.000Z' });
const assistant = (Id = 'a1', Content = '回复', extra = {}) => ({ Id, Role: 'assistant', Content,
  Provider: 'provider-a', Model: 'model-a', Reasoning: '推理', Status: 'completed',
  CreatedAt: '2026-01-01T00:00:01.000Z', ...extra });
const chat = (Id = 'chat-a', Messages = [user()]) => ({ Id, Title: '聊天 A', Draft: '', Messages });
const project = (Id = 'project-a', Chats = [chat()]) => ({ Id, Name: '项目 A', FolderPath: 'D:\\work\\project', Chats });
const reopen = dataHome => new ConversationStore({ dataHome, legacyDesktopDirectory: join(dirname(dataHome), 'Desktop') });

async function fixture() {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-conversations-test-'));
  const dataHome = join(root, 'Models');
  await mkdir(dataHome);
  return { root, dataHome, store: reopen(dataHome) };
}

async function save(store, scopes) {
  return store.saveCatalog({ Revision: (await store.catalog()).Revision, ...scopes });
}

test('migration preserves desktop content, exact backups, metadata and all legacy model logs', async () => {
  const { root, dataHome, store } = await fixture();
  const desktop = join(root, 'Desktop');
  await mkdir(desktop);
  await mkdir(join(dataHome, 'sessions'));
  const projects = [project('project-a', [chat('chat-a', [user(), assistant('answer', '正文', {
    Reasoning: '思考过程', ReasoningDurationMs: 1200, Status: 'streaming', RequestHash: 'abc', ReplyTo: 'u1'
  })])])];
  projects[0].IsPinned = true;
  const plain = chat('normal', ['旧版文本']);
  plain.IsArchived = true;
  const projectSource = JSON.stringify(projects, null, 3) + '\n';
  const chatSource = JSON.stringify([plain, chat('empty', [])]);
  await writeFile(join(desktop, 'projects.json'), projectSource);
  await writeFile(join(desktop, 'chats.json'), chatSource);
  await writeFile(join(dataHome, 'sessions', 'hash-a.json'), '[{"role":"assistant","content":"legacy A"}]');
  await writeFile(join(dataHome, 'sessions', 'hash-b.json'), '[{"role":"assistant","content":"legacy B"}]');
  const catalog = await store.catalog();
  assert.equal(catalog.Projects[0].IsPinned, true);
  assert.equal(catalog.Projects[0].FolderPath, projects[0].FolderPath);
  assert.equal(catalog.Projects[0].Chats[0].Messages[1].Content, '正文');
  assert.equal(catalog.Projects[0].Chats[0].Messages[1].Reasoning, '思考过程');
  assert.equal(catalog.Projects[0].Chats[0].Messages[1].ReasoningDurationMs, 1200);
  assert.equal(catalog.Projects[0].Chats[0].Messages[1].Status, 'interrupted');
  assert.equal(catalog.Projects[0].Chats[0].Messages[1].RequestHash, 'abc');
  assert.equal(catalog.Chats.length, 1);
  assert.equal(catalog.Chats[0].IsArchived, true);
  assert.equal(catalog.Chats[0].Messages[0].Role, 'user');
  assert.equal(catalog.Chats[0].Messages[0].Content, '旧版文本');
  assert.match(catalog.Chats[0].Messages[0].Id, /^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$/);
  const backup = join(root, 'Backups', 'conversations-v1');
  assert.equal(await readFile(join(backup, 'Desktop', 'projects.json'), 'utf8'), projectSource);
  assert.equal(await readFile(join(backup, 'Desktop', 'chats.json'), 'utf8'), chatSource);
  assert.deepEqual((await readdir(join(backup, 'Models', 'sessions'))).sort(), ['hash-a.json', 'hash-b.json']);
  assert.equal(await readFile(join(desktop, 'projects.json'), 'utf8'), projectSource);
  assert.equal(await readFile(join(desktop, 'chats.json'), 'utf8'), chatSource);
  assert.equal(catalog.Projects[0].Chats[0].Messages.length, 2, 'ambiguous legacy model logs are not merged');
  const stored = JSON.parse(await readFile(join(root, 'catalog.json'), 'utf8'));
  assert.equal(stored.Projects[0].Chats[0].Messages, undefined);
  assert.equal(stored.Chats[0].Messages, undefined);
  await stat(join(root, 'Projects', 'project-a', 'Sessions', 'chat-a', 'events.jsonl'));
  const reopened = reopen(dataHome);
  assert.deepEqual(await reopened.catalog(), catalog, 'migration and restart do not duplicate messages');
  await reopened.initialize();
  assert.deepEqual(await reopened.catalog(), catalog);
});

test('migration validates the entire catalog before committing or creating a backup', async () => {
  const { root, store } = await fixture();
  await mkdir(join(root, 'Desktop'));
  const invalid = [project('project-a', [chat('same')]), project('project-b', [chat('same')])];
  const source = JSON.stringify(invalid);
  await writeFile(join(root, 'Desktop', 'projects.json'), source);
  await assert.rejects(store.initialize(), /重复聊天 ID/);
  await assert.rejects(stat(join(root, 'catalog.json')), { code: 'ENOENT' });
  await assert.rejects(stat(join(root, '.conversations-v1.json')), { code: 'ENOENT' });
  await assert.rejects(stat(join(root, 'Backups')), { code: 'ENOENT' });
  assert.equal(await readFile(join(root, 'Desktop', 'projects.json'), 'utf8'), source);
  await writeFile(join(root, 'Desktop', 'projects.json'), JSON.stringify([invalid[0]]));
  assert.equal((await store.catalog()).Projects.length, 1, 'failed initialization can be retried');
});

test('catalog edits cannot overwrite or insert assistant replies and keep runtime request metadata', async () => {
  const { store, root } = await fixture();
  const stale = await save(store, { Chats: [chat()] });
  await store.upsertMessage('chat-a', assistant('a1', '真实完整回复', { RequestHash: 'hash', ReplyTo: 'u1' }));
  stale.Chats[0].Title = '改名';
  stale.Chats[0].Draft = '草稿';
  stale.Chats[0].Messages.push(assistant('a1', '过期片段'), assistant('fake', 'UI 无权插入'));
  stale.Chats[0].Messages[0].Content = 'UI 无权改历史用户消息';
  const updated = await store.saveCatalog(stale);
  assert.equal(updated.Chats[0].Title, '改名');
  assert.equal(updated.Chats[0].Draft, '草稿');
  assert.deepEqual(updated.Chats[0].Messages.map(value => value.Content), ['你好', '真实完整回复']);
  assert.equal(updated.Chats[0].Messages[1].RequestHash, 'hash');
  assert.equal(updated.Chats[0].Messages[1].ReplyTo, 'u1');
  const source = await readFile(join(root, 'catalog.json'), 'utf8');
  assert.equal(source.includes('真实完整回复'), false);
  assert.equal(source.includes('Messages'), false);
});

test('different projects and standalone chats have independent paths; each model shares the same transcript', async () => {
  const { root, store } = await fixture();
  await save(store, { Projects: [project('p1', [chat('c1')]), project('p2', [chat('c2')])], Chats: [chat('c3')] });
  await store.upsertMessage('c1', assistant('a1', '模型 A'));
  await store.upsertMessage('c1', user('u2', '继续'));
  await store.upsertMessage('c1', assistant('a2', '模型 B', { Provider: 'provider-b', Model: 'model-b' }));
  assert.deepEqual((await store.readMessages('c1')).map(value => value.Content), ['你好', '模型 A', '继续', '模型 B']);
  assert.equal((await store.readMessages('c2')).length, 1);
  assert.equal((await store.readMessages('c3')).length, 1);
  await stat(join(root, 'Projects', 'p1', 'Sessions', 'c1', 'events.jsonl'));
  await stat(join(root, 'Projects', 'p2', 'Sessions', 'c2', 'events.jsonl'));
  await stat(join(root, 'Chats', 'c3', 'events.jsonl'));
  assert.deepEqual((await readdir(join(root, 'Projects', 'p1', 'Sessions', 'c1'))).sort(), ['Memory', 'attachments', 'events.jsonl']);
});

test('deleting a chat tombstones runtime writes and undo restores the complete canonical conversation', async () => {
  const { root, store, dataHome } = await fixture();
  const original = await save(store, { Projects: [project()] });
  await store.upsertMessage('chat-a', assistant('a1', '保留回复'));
  const deleted = await save(store, { Projects: [project('project-a', [])] });
  await stat(join(root, 'Trash', 'chat-a', 'events.jsonl'));
  await assert.rejects(stat(join(root, 'Projects', 'project-a', 'Sessions', 'chat-a')), { code: 'ENOENT' });
  await assert.rejects(store.upsertMessage('chat-a', assistant('a1', '晚到回复')), { code: 'CONVERSATION_DELETED' });
  await assert.rejects(store.ensureConversation('chat-a'), { code: 'CONVERSATION_DELETED' });
  const restarted = reopen(dataHome);
  await assert.rejects(restarted.ensureConversation('chat-a'), { code: 'CONVERSATION_DELETED' });
  original.Revision = deleted.Revision;
  const restored = await restarted.saveCatalog(original);
  assert.deepEqual(restored.Projects[0].Chats[0].Messages.map(value => value.Content), ['你好', '保留回复']);
  await assert.rejects(stat(join(root, 'Trash', 'chat-a')), { code: 'ENOENT' });
});

test('removing a project moves its conversations to trash without touching workspace files', async () => {
  const { store, root } = await fixture();
  const workspace = join(root, 'external-work');
  await mkdir(workspace);
  await writeFile(join(workspace, 'keep.txt'), 'workspace files');
  const value = project(); value.FolderPath = workspace;
  await save(store, { Projects: [value] });
  await save(store, { Projects: [] });
  assert.equal(await readFile(join(workspace, 'keep.txt'), 'utf8'), 'workspace files');
  await stat(join(root, 'Trash', 'chat-a', 'events.jsonl'));
});

test('optimistic revisions reject stale metadata while independent stream writes preserve the revision', async () => {
  const { store } = await fixture();
  const first = await save(store, { Chats: [chat()] });
  await store.upsertMessage('chat-a', assistant());
  assert.equal((await store.catalog()).Revision, first.Revision);
  await store.saveCatalog({ Revision: first.Revision, Chats: [{ ...first.Chats[0], Title: '新标题' }] });
  await assert.rejects(store.saveCatalog(first), { statusCode: 409, code: 'CATALOG_CONFLICT' });
  assert.equal((await store.catalog()).Chats[0].Title, '新标题');
});

test('empty drafts are filtered while sample entries and conversations with user input are retained', async () => {
  const { store } = await fixture();
  const empty = chat('empty', []); empty.Draft = '未发送的草稿';
  const fake = chat('fake', [assistant()]);
  const sample = { ...chat('sample', []), IsSample: true };
  const result = await save(store, { Chats: [empty, fake, sample, chat('actual')] });
  assert.deepEqual(result.Chats.map(value => value.Id), ['sample', 'actual']);
});

test('streaming checkpoints upsert stable IDs, preserve partial content and recover interrupted only on restart', async () => {
  const { store, dataHome } = await fixture();
  await store.ensureConversation('api-chat');
  await store.upsertMessage('api-chat', user());
  await store.upsertMessage('api-chat', assistant('a1', '半', { Status: 'streaming', RequestHash: 'hash' }));
  await store.upsertMessage('api-chat', { Id: 'a1', Content: '半句回复', ReasoningDurationMs: 99 });
  assert.equal((await store.readMessages('api-chat'))[1].Status, 'streaming');
  await store.catalog();
  await store.initialize();
  assert.equal((await store.readMessages('api-chat'))[1].Status, 'streaming');
  const reopened = reopen(dataHome);
  const messages = await reopened.readMessages('api-chat');
  assert.equal(messages.length, 2);
  assert.equal(messages[1].Status, 'interrupted');
  assert.equal(messages[1].Content, '半句回复');
  assert.equal(messages[1].ReasoningDurationMs, 99);
  assert.equal(messages[1].RequestHash, 'hash');
});

test('partial last append is recoverable, completed unterminated JSON is kept, corrupt interior fails closed', async () => {
  const { store, root, dataHome } = await fixture();
  await save(store, { Chats: [chat()] });
  const filename = join(root, 'Chats', 'chat-a', 'events.jsonl');
  const original = await readFile(filename, 'utf8');
  await appendFile(filename, '{"version":1,"message":');
  const reopened = reopen(dataHome);
  assert.equal((await reopened.readMessages('chat-a')).length, 1);
  assert.equal(await readFile(filename, 'utf8'), original);
  assert.equal((await readdir(join(root, 'Chats', 'chat-a'))).some(value => value.includes('.recovered-tail-')), true);
  await writeFile(filename, original.trimEnd());
  const complete = reopen(dataHome);
  assert.equal((await complete.readMessages('chat-a')).length, 1);
  assert.equal(await readFile(filename, 'utf8'), original);
  const corrupt = original + '{broken\n' + original;
  await writeFile(filename, corrupt);
  const broken = reopen(dataHome);
  await assert.rejects(broken.initialize(), { code: 'CORRUPT_CONVERSATION' });
  assert.equal(await readFile(filename, 'utf8'), corrupt);
});

test('a pending catalog transaction is replayed after a crash and migration marker is written last', async () => {
  const { root, store, dataHome } = await fixture();
  await save(store, { Chats: [chat()] });
  const next = JSON.parse(await readFile(join(root, 'catalog.json'), 'utf8'));
  next.Revision++;
  next.Chats = [];
  next.Tombstones.push({ Id: 'chat-a', ProjectId: null, DeletedAt: '2026-01-01T00:00:00.000Z' });
  const transaction = { Version: 1, NextCatalog: next,
    Moves: [{ From: { Id: 'chat-a', ProjectId: null }, To: { Id: 'chat-a', Trash: true } }], Writes: [] };
  await writeFile(join(root, '.catalog-transaction.json'), JSON.stringify(transaction));
  await unlink(join(root, '.conversations-v1.json'));
  const reopened = reopen(dataHome);
  assert.equal((await reopened.catalog()).Chats.length, 0);
  await stat(join(root, 'Trash', 'chat-a', 'events.jsonl'));
  await stat(join(root, '.conversations-v1.json'));
  await assert.rejects(stat(join(root, '.catalog-transaction.json')), { code: 'ENOENT' });
});

test('explicit roots, legacy model homes, path validation and case-insensitive duplicate identities', async () => {
  const { root } = await fixture();
  const custom = new ConversationStore({ dataHome: join(root, 'legacy-model-home'), legacyDesktopDirectory: null });
  assert.equal(custom.root, join(root, 'legacy-model-home', 'Conversations'));
  const override = new ConversationStore({ dataHome: join(root, 'legacy-model-home'), root: join(root, 'override'), legacyDesktopDirectory: null });
  assert.equal(override.root, join(root, 'override'));
  for (const value of ['../escape', 'a/b', 'a\\b', '.', '', 'bad:id', 'CON', 'nul', 'LPT1']) {
    await assert.rejects(custom.ensureConversation(value), /ID 无效/);
    await assert.rejects(custom.readMessages(value), /ID 无效/);
  }
  await assert.rejects(save(custom, { Projects: [project('p', [chat('ABC')])], Chats: [chat('abc')] }), /重复聊天 ID/);
  await custom.ensureConversation('Direct_API-123');
  await custom.upsertMessage('direct_api-123', user());
  assert.equal((await custom.readMessages('DIRECT_API-123')).length, 1);
});

test('compact legacy GUIDs round-trip through desktop dashed GUIDs without creating duplicate identities', async () => {
  const { root, store } = await fixture();
  const compactChat = '00112233445566778899aabbccddeeff';
  const compactUser = '112233445566778899aabbccddeeff00';
  const dashedChat = '00112233-4455-6677-8899-aabbccddeeff';
  const dashedUser = '11223344-5566-7788-99aa-bbccddeeff00';
  await mkdir(join(root, 'Desktop'));
  await writeFile(join(root, 'Desktop', 'chats.json'), JSON.stringify([chat(compactChat, [user(compactUser)])]));
  const initial = await store.catalog();
  assert.equal(initial.Chats[0].Id, dashedChat);
  assert.equal(initial.Chats[0].Messages[0].Id, dashedUser);
  await store.ensureConversation(dashedChat);
  await store.saveCatalog({ Revision: initial.Revision, Chats: [chat(dashedChat, [user(dashedUser)])] });
  assert.equal((await store.catalog()).Chats.length, 1);
  assert.equal((await store.readMessages(compactChat)).length, 1);
});

test('returned catalog and transcript objects cannot mutate the materialized message cache', async () => {
  const { store } = await fixture();
  await save(store, { Chats: [chat()] });
  const messages = await store.readMessages('chat-a');
  messages[0].Content = 'mutated';
  const catalog = await store.catalog();
  assert.equal(catalog.Chats[0].Messages[0].Content, '你好');
  catalog.Chats[0].Messages[0].Content = 'another mutation';
  assert.equal((await store.readMessages('chat-a'))[0].Content, '你好');
});

test('concurrent append operations are serialized without dropping messages', async () => {
  const { store } = await fixture();
  await store.ensureConversation('concurrent');
  await Promise.all(Array.from({ length: 20 }, (_, index) => store.upsertMessage('concurrent', user(`u${index}`, String(index)))));
  assert.deepEqual((await store.readMessages('concurrent')).map(value => value.Content), Array.from({ length: 20 }, (_, i) => String(i)));
});

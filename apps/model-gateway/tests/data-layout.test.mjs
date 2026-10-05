import assert from 'node:assert/strict';
import { mkdtemp, readFile, writeFile, stat, rm, rmdir } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { test } from 'node:test';
import { ConversationStore } from '../data/conversations.mjs';

async function fixture() {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-layout-'));
  const options = { root, dataHome: join(root, 'Models'), legacyDesktopDirectory: null };
  return { root, options, store: new ConversationStore(options) };
}
const readJson = async path => JSON.parse(await readFile(path, 'utf8'));

test('first startup creates a versioned complete root and repeat startup preserves identity/settings', async () => {
  const { root, options, store } = await fixture();
  await store.initialize();
  for (const folder of ['Projects', 'Chats', 'Memory', 'Index', 'Trash', 'Backups', 'Agent', 'Skills'])
    assert.equal((await stat(join(root, folder))).isDirectory(), true);
  const settings = await readJson(join(root, 'settings.json'));
  assert.equal(settings.Storage.LayoutVersion, 1);
  assert.ok(settings.Storage.StoreId);
  assert.equal((await readFile(join(root, 'Index', 'search.sqlite'))).subarray(0, 16).toString(), 'SQLite format 3\0');
  settings.Theme = 'user-choice';
  await writeFile(join(root, 'settings.json'), JSON.stringify(settings));
  await new ConversationStore(options).initialize();
  assert.deepEqual(await readJson(join(root, 'settings.json')), settings);
});

test('project creation, real chat, rename and relinking update scaffolding without moving the transcript', async () => {
  const { root, options, store } = await fixture();
  let catalog = await store.catalog();
  const project = { Id: 'project-a', Name: '中文项目', FolderPath: 'D:\\workspace-one', Chats: [] };
  catalog = await store.saveCatalog({ Revision: catalog.Revision, Projects: [project] });
  const projectRoot = join(root, 'Projects', project.Id);
  for (const folder of ['Sessions', 'Memory']) assert.equal((await stat(join(projectRoot, folder))).isDirectory(), true);
  const first = await readJson(join(projectRoot, 'project.json'));
  assert.equal(first.Name, project.Name);
  assert.equal(first.FolderPath, project.FolderPath);
  assert.equal(first.Source, '../../catalog.json');
  project.Chats.push({ Id: 'chat-a', Title: '持久聊天', Messages: [{ Id: 'user-a', Role: 'user', Content: 'hello' }] });
  project.Chats.push({ Id: 'draft-a', Title: '空草稿', Messages: [] });
  catalog = await store.saveCatalog({ Revision: catalog.Revision, Projects: [project] });
  const session = join(projectRoot, 'Sessions', 'chat-a');
  assert.equal((await stat(join(session, 'attachments'))).isDirectory(), true);
  await assert.rejects(stat(join(projectRoot, 'Sessions', 'draft-a')), { code: 'ENOENT' });
  const log = await readFile(join(session, 'events.jsonl'));
  await writeFile(join(projectRoot, 'Memory', 'user-note.md'), 'remember this');
  await writeFile(join(session, 'attachments', 'file.txt'), 'attachment');
  project.Name = '改名'; project.FolderPath = 'E:\\new-workspace';
  await store.saveCatalog({ Revision: catalog.Revision, Projects: [project] });
  const changed = await readJson(join(projectRoot, 'project.json'));
  assert.equal(changed.Name, '改名'); assert.equal(changed.FolderPath, project.FolderPath);
  assert.ok(log.equals(await readFile(join(session, 'events.jsonl'))));
  // Missing scaffold/index is reconstructable; user files and conversation text are not rewritten.
  // 缺失的目录骨架和索引可以重建；不改写用户文件或聊天正文。
  await rm(join(projectRoot, 'project.json'));
  await rm(join(root, 'Index', 'search.sqlite'));
  await new ConversationStore(options).initialize();
  assert.equal((await readJson(join(projectRoot, 'project.json'))).Name, '改名');
  assert.equal(await readFile(join(projectRoot, 'Memory', 'user-note.md'), 'utf8'), 'remember this');
  assert.equal(await readFile(join(session, 'attachments', 'file.txt'), 'utf8'), 'attachment');
  assert.ok(log.equals(await readFile(join(session, 'events.jsonl'))));
});

test('ordinary chat attachments follow deletion and undo with their original contents', async () => {
  const { root, store } = await fixture();
  await store.ensureConversation('chat');
  await store.upsertMessage('chat', { Id: 'user', Role: 'user', Content: 'one' });
  const before = await store.catalog();
  await writeFile(join(root, 'Chats', 'chat', 'attachments', 'upload.txt'), 'user file');
  const deleted = await store.saveCatalog({ Revision: before.Revision, Chats: [] });
  assert.equal(await readFile(join(root, 'Trash', 'chat', 'attachments', 'upload.txt'), 'utf8'), 'user file');
  await store.saveCatalog({ Revision: deleted.Revision, Chats: before.Chats });
  assert.equal(await readFile(join(root, 'Chats', 'chat', 'attachments', 'upload.txt'), 'utf8'), 'user file');
});

test('invalid or future layout versions are rejected before canonical files change', async () => {
  for (const value of ['invalid-json', 'null', JSON.stringify({ Storage: { LayoutVersion: 999 } })]) {
    const { root, options, store } = await fixture();
    await store.catalog();
    const before = await readFile(join(root, 'catalog.json'));
    await writeFile(join(root, 'settings.json'), value);
    await assert.rejects(new ConversationStore(options).initialize(), /版本|格式/);
    assert.ok(before.equals(await readFile(join(root, 'catalog.json'))));
    assert.equal(await readFile(join(root, 'settings.json'), 'utf8'), value);
  }
});

test('legacy root gets scaffolding without rewriting committed conversation events', async () => {
  const { root, options, store } = await fixture();
  await store.ensureConversation('old-chat');
  await store.upsertMessage('old-chat', { Id: 'user', Role: 'user', Content: 'legacy' });
  const path = join(root, 'Chats', 'old-chat', 'events.jsonl');
  const before = await readFile(path);
  await rm(join(root, 'settings.json'));
  await rmdir(join(root, 'Chats', 'old-chat', 'attachments'));
  await new ConversationStore(options).initialize();
  assert.ok(before.equals(await readFile(path)));
  assert.equal((await stat(join(root, 'Chats', 'old-chat', 'attachments'))).isDirectory(), true);
});

test('future derived index is rejected before a metadata transaction can change the catalog', async () => {
  const { root, store } = await fixture();
  const catalog = await store.catalog();
  const before = await readFile(join(root, 'catalog.json'));
  const { DatabaseSync } = await import('node:sqlite');
  const database = new DatabaseSync(join(root, 'Index', 'search.sqlite'));
  database.exec('PRAGMA user_version=999');
  database.close();
  await assert.rejects(store.saveCatalog({ Revision: catalog.Revision, Projects: [
    { Id: 'project', Name: 'must not commit', Chats: [] }
  ] }), error => error.code === 'UNSUPPORTED_INDEX_VERSION');
  assert.ok(before.equals(await readFile(join(root, 'catalog.json'))));
  await assert.rejects(stat(join(root, '.catalog-transaction.json')), { code: 'ENOENT' });
});

import assert from 'node:assert/strict';
import { test } from 'node:test';
import { mkdtemp, mkdir, writeFile, readFile, access, readdir } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { appendFileSync } from 'node:fs';
import { migrateStorage } from '../migrate-storage.mjs';

async function fixture(custom = false) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-storage-move-'));
  const desktopSource = join(root, 'old', 'Desktop'), modelSource = join(root, 'old', custom ? 'custom-model-home' : 'Models');
  const conversations = custom ? join(modelSource, 'Conversations') : dirname(modelSource);
  const target = join(root, 'new'), pointer = join(root, 'profile', 'storage.json');
  const managed = join(desktopSource, 'Projects', 'one'), external = join(root, 'external-work');
  const catalog = { Version: 1, Revision: 2,
    Projects: [{ Id: 'one', Name: 'One', FolderPath: managed, Chats: [{ Id: 'chat-one', Title: 'Project chat' }] },
      { Id: 'two', Name: 'Two', FolderPath: external, Chats: [] }],
    Chats: [{ Id: 'chat-two', Title: 'Standalone chat' }],
    Tombstones: [{ Id: 'chat-three', ProjectId: null, DeletedAt: '2026-01-01T00:00:00.000Z' }] };
  const event = (id, content) => JSON.stringify({ version: 1, type: 'message.upsert',
    message: { Id: id, Role: 'assistant', Content: content, Status: 'completed' } }) + '\n';
  const files = new Map([
    [join(desktopSource, 'projects.json'), JSON.stringify(catalog.Projects)],
    [join(desktopSource, 'chats.json'), '[]'],
    [join(managed, 'work.txt'), 'project file'],
    [join(external, 'untouched.txt'), 'external file'],
    [join(modelSource, 'connections.json'), '{"providers":[]}'],
    [join(modelSource, 'local-server.json'), JSON.stringify({ modelPath: join(modelSource, 'weights', 'test.gguf'), serverPath: join(external, 'llama-server.exe') })],
    [join(modelSource, 'sessions', 'old.json'), '[{"content":"old"}]'],
    [join(conversations, 'catalog.json'), JSON.stringify(catalog)],
    [join(conversations, '.catalog-transaction.json'), JSON.stringify({ Version: 1, NextCatalog: catalog, Moves: [], Writes: [] })],
    [join(conversations, '.conversations-v1.json'), '{"version":1}'],
    [join(conversations, 'settings.json'), JSON.stringify({ Appearance: { Theme: 'system' }, Storage: { LayoutVersion: 1, StoreId: 'stable-store', CreatedAt: '2026-01-01T00:00:00.000Z' } })],
    [join(conversations, 'Projects', 'one', 'project.json'), JSON.stringify({ Version: 1, Id: 'one', FolderPath: managed, Source: '../../catalog.json' })],
    [join(conversations, 'Projects', 'two', 'project.json'), JSON.stringify({ Version: 1, Id: 'two', FolderPath: external, Source: '../../catalog.json' })],
    [join(conversations, 'Projects', 'one', 'Sessions', 'chat-one', 'events.jsonl'), event('message-one', 'project reply')],
    [join(conversations, 'Projects', 'one', 'Sessions', 'chat-one', 'attachments', 'drawing.txt'), 'project attachment'],
    [join(conversations, 'Projects', 'one', 'Memory', 'preferences.md'), 'project preferences'],
    [join(conversations, 'Chats', 'chat-two', 'events.jsonl'), event('message-two', 'standalone reply')],
    [join(conversations, 'Chats', 'chat-two', 'attachments', 'notes.txt'), 'chat attachment'],
    [join(conversations, 'Memory', 'preferences.md'), 'user preferences'],
    [join(conversations, 'Index', 'search.sqlite'), 'opaque index fixture'],
    [join(conversations, 'Trash', 'chat-three', 'events.jsonl'), event('message-three', 'deleted reply')],
    [join(conversations, 'Backups', 'conversations-v1', 'Desktop', 'projects.json'), JSON.stringify(catalog.Projects)],
    [pointer, JSON.stringify({ version: 1, dataRoot: dirname(modelSource) })]
  ]);
  for (const [path, content] of files) { await mkdir(dirname(path), { recursive: true }); await writeFile(path, content); }
  await mkdir(join(managed, 'empty'));
  return { root, desktopSource, modelSource, conversations, target, pointer, external, files };
}

for (const custom of [false, true]) test(`storage move preserves canonical conversations and remaps managed folders (${custom ? 'custom' : 'standard'} root)`, async () => {
  const f = await fixture(custom);
  await mkdir(f.target); // Folder picker can return an empty existing directory.
  // 文件夹选择器可能返回已存在的空目录。
  const result = await migrateStorage(f);
  assert.equal(result.verifiedFiles, 21);
  const catalog = JSON.parse(await readFile(join(f.target, 'catalog.json'), 'utf8'));
  assert.equal(catalog.Projects[0].FolderPath, join(f.target, 'Desktop', 'Projects', 'one'));
  assert.equal(catalog.Projects[1].FolderPath, f.external);
  // Target initialization applies and removes the copied transaction before activation.
  // 目标初始化在激活之前应用并清除复制的事务。
  await assert.rejects(access(join(f.target, '.catalog-transaction.json')), { code: 'ENOENT' });
  const managedManifest = JSON.parse(await readFile(join(f.target, 'Projects', 'one', 'project.json'), 'utf8'));
  const externalManifest = JSON.parse(await readFile(join(f.target, 'Projects', 'two', 'project.json'), 'utf8'));
  assert.equal(managedManifest.FolderPath, catalog.Projects[0].FolderPath);
  assert.equal(externalManifest.FolderPath, f.external);
  assert.equal(managedManifest.Source, '../../catalog.json');
  for (const relative of ['Projects/one/Sessions/chat-one/events.jsonl', 'Chats/chat-two/events.jsonl', 'Trash/chat-three/events.jsonl', 'Backups/conversations-v1/Desktop/projects.json',
    'settings.json', 'Projects/one/Sessions/chat-one/attachments/drawing.txt', 'Projects/one/Memory/preferences.md',
    'Chats/chat-two/attachments/notes.txt', 'Memory/preferences.md'])
    assert.deepEqual(await readFile(join(f.target, relative)), await readFile(join(f.conversations, relative)));
  assert.equal((await readFile(join(f.target, 'Index', 'search.sqlite'))).subarray(0, 16).toString(), 'SQLite format 3\0');
  const recoveredIndex = (await readdir(join(f.target, 'Index'))).find(name => name.startsWith('search.sqlite.recovered-'));
  assert.ok(recoveredIndex);
  assert.deepEqual(await readFile(join(f.target, 'Index', recoveredIndex)), await readFile(join(f.conversations, 'Index', 'search.sqlite')));
  await access(join(f.target, 'Projects', 'two', 'Memory'));
  await access(join(f.target, 'Projects', 'two', 'Sessions'));
  await access(join(f.target, 'Desktop', 'Projects', 'one', 'empty'));
  if (custom) await assert.rejects(access(join(f.target, 'Models', 'Conversations')), { code: 'ENOENT' });
  for (const [path, source] of f.files) if (path !== f.pointer) assert.equal(await readFile(path, 'utf8'), source);
  assert.equal(JSON.parse(await readFile(f.pointer, 'utf8')).dataRoot, f.target);
  const local = JSON.parse(await readFile(join(f.target, 'Models', 'local-server.json'), 'utf8'));
  assert.equal(local.modelPath, join(f.target, 'Models', 'weights', 'test.gguf'));
  assert.equal(local.serverPath, join(f.external, 'llama-server.exe'));
});

test('storage move rejects canonical destination overlap and concurrent source changes without updating pointer', async () => {
  const f = await fixture(), pointerBefore = await readFile(f.pointer, 'utf8');
  await assert.rejects(migrateStorage({ ...f, target: join(f.conversations, 'Chats', 'nested') }), /overlap/);
  await assert.rejects(migrateStorage({ ...f, progress: () => {
    // The callback runs immediately before the final source verification.
    // Use a synchronous fixture mutation to deterministically simulate another writer.
    // 回调紧邻最终源数据校验之前执行；同步修改测试夹具，以确定地模拟另一个写入者。
    writeFixtureChange(join(f.conversations, 'catalog.json'));
  } }), /Source files changed/);
  assert.equal(await readFile(f.pointer, 'utf8'), pointerBefore);
});

function writeFixtureChange(path) { appendFileSync(path, ' '); }

test('storage migration preserves imported knowledge, retrieval overrides and revocation identities byte for byte', async () => {
  const f = await fixture();
  const payloads = new Map([
    ['Knowledge/catalog.json', JSON.stringify({ schemaVersion: 1, revision: 1, sources: [] })],
    ['Knowledge/imported/source/document.txt', '用户显式导入的资料快照，不是可丢弃缓存。'],
    ['Retrieval/settings.json', JSON.stringify({ schemaVersion: 1, revision: 3, local: { enabled: true }, web: { mode: 'off' } })],
    ['Retrieval/source-identities.json', JSON.stringify({ schemaVersion: 1, revision: 2, sources: { synthetic: { tombstone: true } } })],
    ['Retrieval/jobs.json', JSON.stringify({ schemaVersion: 1, jobs: [{ jobId: 'synthetic', status: 'cancelled' }] })]
  ]);
  for (const [path, content] of payloads) {
    await mkdir(dirname(join(f.conversations, path)), { recursive: true });
    await writeFile(join(f.conversations, path), content, 'utf8');
  }
  await migrateStorage(f);
  for (const [path, content] of payloads) {
    assert.equal(await readFile(join(f.target, path), 'utf8'), content);
    assert.equal(await readFile(join(f.conversations, path), 'utf8'), content);
  }
});

test('storage move refuses invalid or future layout settings and keeps the active pointer unchanged', async () => {
  for (const settings of ['{', 'null', '[]', '{"Storage":null}', '{"Storage":{"LayoutVersion":2}}', '{"Storage":{"LayoutVersion":"1"}}']) {
    const f = await fixture(), pointerBefore = await readFile(f.pointer, 'utf8');
    await writeFile(join(f.conversations, 'settings.json'), settings);
    await assert.rejects(migrateStorage(f), error => error.code === 'INVALID_DATA_LAYOUT');
    assert.equal(await readFile(f.pointer, 'utf8'), pointerBefore);
    assert.equal(await readFile(join(f.conversations, 'settings.json'), 'utf8'), settings);
    await assert.rejects(access(f.target), { code: 'ENOENT' });
  }
});

test('storage move validates copied layout again before activating the destination', async () => {
  const f = await fixture(), pointerBefore = await readFile(f.pointer, 'utf8');
  await assert.rejects(migrateStorage({ ...f, progress: () => {
    appendFileSync(join(f.target, 'settings.json'), 'invalid');
  } }), error => error.code === 'INVALID_DATA_LAYOUT');
  assert.equal(await readFile(f.pointer, 'utf8'), pointerBefore);
});

test('storage move keeps the old pointer when target conversation initialization fails', async () => {
  const f = await fixture(), pointerBefore = await readFile(f.pointer, 'utf8');
  const sourceLog = join(f.conversations, 'Projects', 'one', 'Sessions', 'chat-one', 'events.jsonl');
  await writeFile(sourceLog, '{"version":99}\n');
  await assert.rejects(migrateStorage(f), error => error.code === 'CORRUPT_CONVERSATION');
  assert.equal(await readFile(f.pointer, 'utf8'), pointerBefore);
  assert.equal(await readFile(sourceLog, 'utf8'), '{"version":99}\n');
});

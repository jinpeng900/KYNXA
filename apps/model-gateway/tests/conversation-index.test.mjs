import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readFile, readdir, rename, rm, stat, symlink, unlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { DatabaseSync } from 'node:sqlite';
import { inspectConversationIndex, rebuildConversationIndex } from '../data/conversation-index.mjs';

function document() {
  return { Version: 1, Revision: 3,
    Projects: [{ Id: 'project-a', Name: '实验项目 α', FolderPath: 'D:\\研究文件\\物理', IsPinned: true,
      Chats: [{ Id: 'chat-a', Title: '公式与 Unicode 🧪', IsArchived: true,
        Draft: '不进入索引的草稿', Messages: [{ Content: '不进入索引的正文', ApiKey: 'private-fixture-key' }] }] },
    { Id: 'project-b', Name: "O'Reilly; DROP TABLE projects;", FolderPath: null, IsArchived: true, Chats: [] }],
    Chats: [{ Id: 'chat-b', Title: '普通聊天', IsPinned: true }, { Id: 'chat-c', Title: '稍后处理' }] };
}

async function fixture(t) {
  const container = await mkdtemp(join(tmpdir(), 'kynxa-index-test-'));
  t.after(() => rm(container, { recursive: true, force: true }));
  const root = join(container, 'Data');
  return { root, container, filename: join(root, 'Index', 'search.sqlite') };
}

function query(filename, sql) {
  const database = new DatabaseSync(filename, { readOnly: true });
  try { return database.prepare(sql).all().map(row => ({ ...row })); }
  finally { database.close(); }
}

test('initializes an actual SQLite metadata index with Unicode and parameterized values, excluding messages and secrets', async t => {
  const { root, filename } = await fixture(t);
  const result = await rebuildConversationIndex(root, document());
  assert.deepEqual(result, { path: filename, rebuilt: true });
  const bytes = await readFile(filename);
  assert.equal(bytes.subarray(0, 16).toString(), 'SQLite format 3\0');
  for (const excluded of ['不进入索引的正文', '不进入索引的草稿', 'private-fixture-key'])
    assert.equal(bytes.includes(Buffer.from(excluded)), false);
  assert.deepEqual(query(filename, 'PRAGMA user_version'), [{ user_version: 1 }]);
  assert.deepEqual(query(filename, 'PRAGMA quick_check'), [{ quick_check: 'ok' }]);
  assert.deepEqual(query(filename, 'SELECT * FROM projects ORDER BY sort_order'), [
    { id: 'project-a', name: '实验项目 α', workspace_path: 'D:\\研究文件\\物理', sort_order: 0, pinned: 1, archived: 0 },
    { id: 'project-b', name: "O'Reilly; DROP TABLE projects;", workspace_path: null, sort_order: 1, pinned: 0, archived: 1 }
  ]);
  assert.deepEqual(query(filename, "SELECT * FROM sessions WHERE id = 'chat-a'"), [
    { id: 'chat-a', project_id: 'project-a', title: '公式与 Unicode 🧪', sort_order: 0, pinned: 0, archived: 1,
      workspace_path: 'D:\\研究文件\\物理' }
  ]);
  assert.deepEqual(query(filename, 'SELECT id, project_id, sort_order FROM sessions WHERE project_id IS NULL ORDER BY sort_order'), [
    { id: 'chat-b', project_id: null, sort_order: 0 }, { id: 'chat-c', project_id: null, sort_order: 1 }
  ]);
  const metadata = Object.fromEntries(query(filename, 'SELECT * FROM metadata').map(row => [row.key, row.value]));
  assert.equal(metadata.catalog_revision, '3');
  assert.match(metadata.fingerprint, /^[a-f0-9]{64}$/);
  assert.deepEqual(await readdir(join(root, 'Index')), ['search.sqlite']);
});

test('unchanged metadata skips disk replacement; drafts and messages cannot affect the fingerprint', async t => {
  const { root, filename } = await fixture(t), source = document();
  await rebuildConversationIndex(root, source);
  const before = await stat(filename), bytes = await readFile(filename);
  source.Projects[0].Chats[0].Messages = [{ Content: '新正文' }];
  source.Projects[0].Chats[0].Draft = '更新草稿';
  assert.equal((await rebuildConversationIndex(root, source)).rebuilt, false);
  assert.equal((await stat(filename)).mtimeMs, before.mtimeMs);
  assert.deepEqual(await readFile(filename), bytes);
});

test('renames, workspace relocation, ordering and deletes rebuild exactly from the canonical catalog', async t => {
  const { root, filename } = await fixture(t), source = document();
  await rebuildConversationIndex(root, source);
  source.Revision++;
  source.Projects[0].Name = '项目改名';
  source.Projects[0].FolderPath = 'E:\\迁移后的文件夹';
  source.Projects[0].Chats[0].Title = '新标题';
  source.Projects[0].Chats[0].IsPinned = true;
  source.Chats.reverse();
  assert.equal((await rebuildConversationIndex(root, source)).rebuilt, true);
  assert.equal(query(filename, "SELECT workspace_path FROM sessions WHERE id = 'chat-a'")[0].workspace_path, 'E:\\迁移后的文件夹');
  assert.equal(query(filename, "SELECT title FROM sessions WHERE id = 'chat-a'")[0].title, '新标题');
  assert.deepEqual(query(filename, 'SELECT id FROM sessions WHERE project_id IS NULL ORDER BY sort_order'), [{ id: 'chat-c' }, { id: 'chat-b' }]);
  source.Revision++;
  source.Chats = [source.Projects[0].Chats[0]];
  source.Projects = [];
  await rebuildConversationIndex(root, source);
  assert.deepEqual(query(filename, 'SELECT * FROM projects'), []);
  assert.deepEqual(query(filename, 'SELECT id, project_id, workspace_path FROM sessions'), [
    { id: 'chat-a', project_id: null, workspace_path: null }
  ]);
  assert.deepEqual(await readdir(join(root, 'Index')), ['search.sqlite']);
});

test('a deleted index is recreated and every call closes SQLite handles so the whole Data directory can move', async t => {
  const { root, filename, container } = await fixture(t);
  await rebuildConversationIndex(root, document());
  await unlink(filename);
  assert.equal((await rebuildConversationIndex(root, document())).rebuilt, true);
  const destination = join(container, 'MovedData');
  await rename(root, destination);
  assert.equal((await rebuildConversationIndex(destination, document())).rebuilt, false);
  assert.equal(query(join(destination, 'Index', 'search.sqlite'), 'SELECT count(*) AS count FROM sessions')[0].count, 3);
});

test('corrupt cache is preserved byte for byte before an atomic rebuild', async t => {
  const { root, filename } = await fixture(t);
  await mkdir(join(root, 'Index'), { recursive: true });
  const broken = Buffer.from('damaged SQLite fixture\0\xff');
  await writeFile(filename, broken);
  const result = await rebuildConversationIndex(root, document());
  assert.equal(result.rebuilt, true);
  assert.match(result.recoveredPath, /search\.sqlite\.recovered-/);
  assert.deepEqual(await readFile(result.recoveredPath), broken);
  assert.equal(query(filename, 'SELECT count(*) AS count FROM projects')[0].count, 2);
  assert.equal((await readdir(join(root, 'Index'))).length, 2);
});

test('a valid SQLite file with an incomplete schema is disposable and retained for recovery', async t => {
  const { root, filename } = await fixture(t);
  await mkdir(join(root, 'Index'), { recursive: true });
  const database = new DatabaseSync(filename);
  database.exec('PRAGMA user_version = 1; CREATE TABLE unexpected (value TEXT);');
  database.close();
  const old = await readFile(filename);
  const result = await rebuildConversationIndex(root, document());
  assert.deepEqual(await readFile(result.recoveredPath), old);
  assert.deepEqual(query(filename, 'PRAGMA quick_check'), [{ quick_check: 'ok' }]);
});

test('newer schema refuses all writes, even when the remainder of the database is damaged', async t => {
  const { root, filename } = await fixture(t);
  await rebuildConversationIndex(root, document());
  const database = new DatabaseSync(filename);
  database.exec('PRAGMA user_version = 9;');
  database.close();
  const bytes = (await readFile(filename)).subarray(0, 100);
  await writeFile(filename, bytes);
  await assert.rejects(inspectConversationIndex(root), { code: 'UNSUPPORTED_INDEX_VERSION', statusCode: 409 });
  await assert.rejects(rebuildConversationIndex(root, document()), { code: 'UNSUPPORTED_INDEX_VERSION', statusCode: 409 });
  assert.deepEqual(await readFile(filename), bytes);
  assert.deepEqual(await readdir(join(root, 'Index')), ['search.sqlite']);
});

test('preflight creates no files and lets corrupt disposable metadata be repaired later', async t => {
  const { root, filename } = await fixture(t);
  assert.deepEqual(await inspectConversationIndex(root), { path: filename, exists: false });
  await assert.rejects(stat(root), { code: 'ENOENT' });
  await mkdir(join(root, 'Index'), { recursive: true });
  await writeFile(filename, 'broken cache');
  assert.deepEqual(await inspectConversationIndex(root), { path: filename, exists: true });
  assert.equal(await readFile(filename, 'utf8'), 'broken cache');
  assert.deepEqual(await readdir(join(root, 'Index')), ['search.sqlite']);
});

test('Index junctions/symlinks cannot redirect writes outside the configured root', async t => {
  const { root, container } = await fixture(t);
  const outside = join(container, 'Outside');
  await mkdir(root);
  await mkdir(outside);
  await symlink(outside, join(root, 'Index'), process.platform === 'win32' ? 'junction' : 'dir');
  await assert.rejects(rebuildConversationIndex(root, document()), { code: 'UNSAFE_INDEX_PATH' });
  assert.deepEqual(await readdir(outside), []);
});

test('database file and sidecar symlinks cannot redirect index reads or writes', async t => {
  const { root, container, filename } = await fixture(t);
  await mkdir(join(root, 'Index'), { recursive: true });
  const outside = join(container, 'outside.sqlite');
  await writeFile(outside, 'external fixture');
  try { await symlink(outside, filename, 'file'); }
  catch (error) {
    if (['EPERM', 'EACCES'].includes(error.code)) { t.skip('OS denies creating test file symlinks'); return; }
    throw error;
  }
  await assert.rejects(rebuildConversationIndex(root, document()), { code: 'UNSAFE_INDEX_PATH' });
  assert.equal(await readFile(outside, 'utf8'), 'external fixture');
  await unlink(filename);
  await symlink(outside, filename + '-wal', 'file');
  await assert.rejects(rebuildConversationIndex(root, document()), { code: 'UNSAFE_INDEX_PATH' });
  assert.equal(await readFile(outside, 'utf8'), 'external fixture');
});

test('external SQLite sidecars block replacement instead of discarding a possible active database', async t => {
  const { root, filename } = await fixture(t);
  await rebuildConversationIndex(root, document());
  const before = await readFile(filename);
  await writeFile(filename + '-wal', 'external writer fixture');
  await assert.rejects(rebuildConversationIndex(root, document()), { code: 'CONVERSATION_INDEX_BUSY' });
  assert.deepEqual(await readFile(filename), before);
  assert.equal(await readFile(filename + '-wal', 'utf8'), 'external writer fixture');
});

test('parallel refreshes serialize per root and leave the latest catalog metadata', async t => {
  const { root, filename } = await fixture(t), first = document(), second = document();
  second.Revision++;
  second.Chats[0].Title = '最后提交的标题';
  await Promise.all([rebuildConversationIndex(root, first), rebuildConversationIndex(root, second)]);
  assert.equal(query(filename, "SELECT value FROM metadata WHERE key = 'catalog_revision'")[0].value, '4');
  assert.equal(query(filename, "SELECT title FROM sessions WHERE id = 'chat-b'")[0].title, '最后提交的标题');
  assert.deepEqual(await readdir(join(root, 'Index')), ['search.sqlite']);
});

test('invalid metadata is rejected before creating or replacing any index', async t => {
  const { root, filename } = await fixture(t), source = document();
  source.Chats.push({ Id: 'CHAT-A', Title: '重复 ID' });
  await assert.rejects(rebuildConversationIndex(root, source), { code: 'INVALID_INDEX_DOCUMENT' });
  await assert.rejects(stat(filename), { code: 'ENOENT' });
  await rebuildConversationIndex(root, document());
  const before = await readFile(filename);
  source.Revision = -1;
  await assert.rejects(rebuildConversationIndex(root, source), { code: 'INVALID_INDEX_DOCUMENT' });
  assert.deepEqual(await readFile(filename), before);
});

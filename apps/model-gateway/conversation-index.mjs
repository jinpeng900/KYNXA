import { createHash, randomUUID } from 'node:crypto';
import { constants } from 'node:fs';
import { copyFile, lstat, mkdir, open, rename, unlink } from 'node:fs/promises';
import { join, resolve } from 'node:path';

const schemaVersion = 1;
const queues = new Map();
let sqlite;

function failure(message, code, statusCode = 500) {
  return Object.assign(new Error(message), { code, statusCode });
}

function invalid() {
  return failure('无法从无效的会话目录构建索引。', 'INVALID_INDEX_DOCUMENT');
}

function requiredText(value) {
  if (typeof value !== 'string' || !value.trim()) throw invalid();
  return value;
}

// Index only sidebar metadata. Transcripts, drafts, credentials and memory never enter SQLite.
function projection(document) {
  if (!document || document.Version !== 1 || !Number.isSafeInteger(document.Revision) || document.Revision < 0 ||
      !Array.isArray(document.Projects) || !Array.isArray(document.Chats)) throw invalid();
  const projects = [], sessions = [], projectIds = new Set(), sessionIds = new Set();
  const addSession = (chat, order, project = null) => {
    const id = requiredText(chat?.Id), key = id.toLowerCase();
    if (sessionIds.has(key)) throw invalid();
    sessionIds.add(key);
    sessions.push({ id, project_id: project?.id ?? null, title: requiredText(chat.Title), sort_order: order,
      pinned: Number(Boolean(chat.IsPinned)), archived: Number(Boolean(chat.IsArchived)),
      workspace_path: project?.workspace_path ?? null });
  };
  for (const [order, source] of document.Projects.entries()) {
    const id = requiredText(source?.Id), key = id.toLowerCase();
    if (projectIds.has(key) || !Array.isArray(source.Chats) ||
        (source.FolderPath != null && typeof source.FolderPath !== 'string')) throw invalid();
    projectIds.add(key);
    const project = { id, name: requiredText(source.Name), workspace_path: source.FolderPath ?? null,
      sort_order: order, pinned: Number(Boolean(source.IsPinned)), archived: Number(Boolean(source.IsArchived)) };
    projects.push(project);
    source.Chats.forEach((chat, index) => addSession(chat, index, project));
  }
  document.Chats.forEach((chat, index) => addSession(chat, index));
  const content = { schemaVersion, catalogVersion: document.Version, revision: document.Revision, projects, sessions };
  return { ...content, fingerprint: createHash('sha256').update(JSON.stringify(content)).digest('hex') };
}

async function inspect(path, directory = false) {
  let info;
  try { info = await lstat(path); }
  catch (error) { if (error.code === 'ENOENT') return null; throw error; }
  if (info.isSymbolicLink() || !(directory ? info.isDirectory() : info.isFile()))
    throw failure('索引位置包含链接或不兼容的文件类型，已保留原位置。', 'UNSAFE_INDEX_PATH');
  return info;
}

async function inspectLocation(root, directory, filename) {
  await inspect(root, true);
  await inspect(directory, true);
  const info = await inspect(filename);
  // We never keep connections open or use WAL. A sidecar here can belong to an external
  // process; replacing its database could invalidate that process, so leave both intact.
  for (const suffix of ['-wal', '-shm', '-journal']) {
    if (await inspect(filename + suffix))
      throw failure('索引正在被其他程序使用，请关闭该程序后重试。', 'CONVERSATION_INDEX_BUSY');
  }
  return info;
}

async function headerVersion(filename) {
  const file = await open(filename, 'r');
  try {
    const buffer = Buffer.alloc(100);
    const { bytesRead } = await file.read(buffer, 0, buffer.length, 0);
    return bytesRead >= 64 && buffer.subarray(0, 16).equals(Buffer.from('SQLite format 3\0'))
      ? buffer.readUInt32BE(60) : 0;
  } finally { await file.close(); }
}

function checkVersion(version) {
  if (version > schemaVersion)
    throw failure('索引由更新版本的软件创建，请使用新版软件打开。', 'UNSUPPORTED_INDEX_VERSION', 409);
}

function inspectDatabase(DatabaseSync, filename, data) {
  let database;
  try {
    database = new DatabaseSync(filename, { readOnly: true });
    const version = database.prepare('PRAGMA user_version').get().user_version;
    checkVersion(version);
    if (version !== schemaVersion || database.prepare('PRAGMA quick_check').all().some(row => row.quick_check !== 'ok'))
      return { corrupt: true, unchanged: false };
    // Check required columns even if the cached fingerprint happens to match.
    database.prepare('SELECT id, name, workspace_path, sort_order, pinned, archived FROM projects LIMIT 0').all();
    database.prepare('SELECT id, project_id, title, sort_order, pinned, archived, workspace_path FROM sessions LIMIT 0').all();
    const metadata = Object.fromEntries(database.prepare('SELECT key, value FROM metadata').all().map(row => [row.key, row.value]));
    const projectCount = database.prepare('SELECT count(*) AS count FROM projects').get().count;
    const sessionCount = database.prepare('SELECT count(*) AS count FROM sessions').get().count;
    return { corrupt: false, unchanged: metadata.catalog_revision === String(data.revision) &&
      metadata.fingerprint === data.fingerprint && projectCount === data.projects.length && sessionCount === data.sessions.length };
  } catch (error) {
    if (error.code === 'UNSUPPORTED_INDEX_VERSION') throw error;
    // Opening or reading a disposable, invalid SQLite file never affects canonical JSONL.
    if (error.code === 'ERR_SQLITE_ERROR') return { corrupt: true, unchanged: false };
    throw error;
  } finally { database?.close(); }
}

function writeDatabase(DatabaseSync, filename, data) {
  const database = new DatabaseSync(filename);
  try {
    database.exec(`
      PRAGMA journal_mode = DELETE;
      PRAGMA synchronous = FULL;
      PRAGMA foreign_keys = ON;
      BEGIN IMMEDIATE;
      CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
      CREATE TABLE projects (
        id TEXT PRIMARY KEY COLLATE NOCASE, name TEXT NOT NULL, workspace_path TEXT,
        sort_order INTEGER NOT NULL, pinned INTEGER NOT NULL, archived INTEGER NOT NULL
      );
      CREATE TABLE sessions (
        id TEXT PRIMARY KEY COLLATE NOCASE, project_id TEXT REFERENCES projects(id), title TEXT NOT NULL,
        sort_order INTEGER NOT NULL, pinned INTEGER NOT NULL, archived INTEGER NOT NULL, workspace_path TEXT
      );
      CREATE INDEX sessions_project_order ON sessions (project_id, sort_order);
      PRAGMA user_version = 1;
    `);
    const insertProject = database.prepare('INSERT INTO projects VALUES (?, ?, ?, ?, ?, ?)');
    for (const row of data.projects)
      insertProject.run(row.id, row.name, row.workspace_path, row.sort_order, row.pinned, row.archived);
    const insertSession = database.prepare('INSERT INTO sessions VALUES (?, ?, ?, ?, ?, ?, ?)');
    for (const row of data.sessions)
      insertSession.run(row.id, row.project_id, row.title, row.sort_order, row.pinned, row.archived, row.workspace_path);
    const insertMetadata = database.prepare('INSERT INTO metadata VALUES (?, ?)');
    insertMetadata.run('catalog_revision', String(data.revision));
    insertMetadata.run('fingerprint', data.fingerprint);
    database.exec('COMMIT;');
  } finally { database.close(); }
}

function sameFile(before, after) {
  return !before ? !after : after && before.dev === after.dev && before.ino === after.ino &&
    before.size === after.size && before.mtimeMs === after.mtimeMs && before.ctimeMs === after.ctimeMs;
}

/** Read-only preflight for callers to run BEFORE committing canonical catalog changes. */
export async function inspectConversationIndex(root) {
  if (typeof root !== 'string' || !root.trim()) throw invalid();
  root = resolve(root);
  const directory = join(root, 'Index'), filename = join(directory, 'search.sqlite');
  const info = await inspectLocation(root, directory, filename);
  if (info) checkVersion(await headerVersion(filename));
  return { path: filename, exists: Boolean(info) };
}

async function rebuild(root, data) {
  const directory = join(root, 'Index'), filename = join(directory, 'search.sqlite');
  await inspect(root, true);
  await inspect(directory, true);
  await mkdir(directory, { recursive: true, mode: 0o700 });
  const original = await inspectLocation(root, directory, filename);
  // Read the header first so even a newer file with damaged tables is never replaced.
  if (original) checkVersion(await headerVersion(filename));
  sqlite ??= import('node:sqlite');
  const { DatabaseSync } = await sqlite;
  const state = original ? inspectDatabase(DatabaseSync, filename, data) : { corrupt: false, unchanged: false };
  if (state.unchanged) return { path: filename, rebuilt: false };

  const temporary = join(directory, `.search-${randomUUID()}.tmp`);
  let recoveredPath;
  try {
    // Reserve exclusively before SQLite opens the file, avoiding an existing link/file.
    const reservation = await open(temporary, 'wx', 0o600);
    await reservation.close();
    writeDatabase(DatabaseSync, temporary, data);
    const current = await inspectLocation(root, directory, filename);
    if (!sameFile(original, current))
      throw failure('索引在更新期间发生变化，已保留现有索引，请重试。', 'CONVERSATION_INDEX_CHANGED', 409);
    await inspect(temporary);
    if (state.corrupt) {
      recoveredPath = join(directory, `search.sqlite.recovered-${randomUUID()}`);
      await copyFile(filename, recoveredPath, constants.COPYFILE_EXCL);
    }
    await rename(temporary, filename);
    return { path: filename, rebuilt: true, ...(recoveredPath ? { recoveredPath } : {}) };
  } finally {
    // SQLite only ever writes the uniquely reserved temporary database. All handles are
    // closed before replacement, keeping the whole Data directory movable on Windows.
    for (const path of [temporary, temporary + '-journal']) {
      const info = await inspect(path);
      if (info) await unlink(path);
    }
  }
}

/** Rebuildable metadata cache. Canonical catalog/events remain authoritative. */
export function rebuildConversationIndex(root, document) {
  if (typeof root !== 'string' || !root.trim()) return Promise.reject(invalid());
  root = resolve(root);
  let data;
  try { data = projection(document); } catch (error) { return Promise.reject(error); }
  const queueKey = process.platform === 'win32' ? root.toLowerCase() : root;
  const operation = (queues.get(queueKey) ?? Promise.resolve()).then(() => rebuild(root, data));
  const settled = operation.catch(() => {});
  queues.set(queueKey, settled);
  settled.finally(() => { if (queues.get(queueKey) === settled) queues.delete(queueKey); });
  return operation;
}

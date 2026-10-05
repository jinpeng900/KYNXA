import { createHash, randomUUID } from 'node:crypto';
import { readFile, writeFile } from 'node:fs/promises';
import { basename, join } from 'node:path';
import { atomicJson } from '../../platform/atomic-json.mjs';
import { validateId } from '../../platform/conversation-id.mjs';
import { ensureLocalDirectory, inspectLocalPath, toolFailure } from '../../platform/tool-paths.mjs';
import { retrievalScopeKeys } from './retrieval-contracts.mjs';

const hash = value => createHash('sha256').update(value).digest('hex');
const scopeKey = entry => entry.scope === 'user' ? 'user' : `project:${entry.projectId}`;

/**
 * Explicitly imported text is authoritative; indexes never own or resurrect this registry.
 * 显式导入的文本与登记是权威资料；索引不能拥有或恢复已撤销的来源。
 */
export class SourceLibrary {
  constructor({ root, conversationStore }) {
    this.root = root; this.conversations = conversationStore;
    this.folder = join(root, 'Knowledge'); this.file = join(this.folder, 'catalog.json');
    this.queue = Promise.resolve(); this.errors = new Map();
  }

  _enqueue(operation) {
    const pending = this.queue.catch(() => {}).then(operation);
    this.queue = pending; return pending;
  }

  async _read() {
    await ensureLocalDirectory(this.folder);
    if (!await inspectLocalPath(this.file, { allowMissing: true })) return { schemaVersion: 1, revision: 0, sources: [] };
    const value = JSON.parse(await readFile(this.file, 'utf8'));
    if (value.schemaVersion !== 1 || !Number.isSafeInteger(value.revision) || !Array.isArray(value.sources))
      throw toolFailure('资料登记版本无效，原文件已保留。', 'UNSUPPORTED_RETRIEVAL_SCHEMA', 409);
    for (const source of value.sources) {
      validateId(source.id);
      if (!['user', 'project'].includes(source.scope) || typeof source.title !== 'string' ||
          !['ready', 'deleted'].includes(source.status) || !Number.isSafeInteger(source.revision))
        throw toolFailure('资料登记损坏，原文件已保留。', 'CORRUPT_RETRIEVAL_LIBRARY', 500);
      if (source.scope === 'project') validateId(source.projectId);
    }
    return value;
  }

  async _target(scope, projectId) {
    if (scope === 'user') return { scope: 'user', projectId: null };
    if (scope !== 'project') throw toolFailure('资料范围无效。', 'INVALID_RETRIEVAL_SCOPE', 400);
    projectId = validateId(projectId).toLowerCase();
    const relationship = await this.conversations.describeProject(projectId);
    if (relationship.isFolderlessWorkspace || relationship.isArchived)
      throw toolFailure('此工作不能添加共享资料。', 'RETRIEVAL_SOURCE_UNAVAILABLE', 409);
    return { scope, projectId };
  }

  list(projectId = null) {
    return this._enqueue(async () => {
      if (projectId) await this._target('project', projectId);
      const document = await this._read();
      return { revision: document.revision, sources: document.sources.filter(entry => entry.status !== 'deleted' &&
        (entry.scope === 'user' || projectId && entry.projectId === projectId.toLowerCase())).map(entry => ({
        id: entry.id, title: entry.title, scope: entry.scope, projectId: entry.projectId,
        path: entry.originalPath, revision: entry.revision, status: this.errors.has(entry.id) ? 'error' : entry.status,
        ...(this.errors.has(entry.id) ? { error: this.errors.get(entry.id) } : {})
      })) };
    });
  }

  add(files, { scope = 'user', projectId = null } = {}) {
    return this._enqueue(async () => {
      const target = await this._target(scope, projectId), document = await this._read();
      if (!Array.isArray(files) || !files.length || files.length > 512)
        throw toolFailure('没有可导入的文本资料或数量过大。', 'INVALID_RETRIEVAL_SOURCE', 400);
      const added = [];
      let scopeBytes = document.sources.filter(entry => entry.status !== 'deleted' && entry.scope === target.scope &&
        entry.projectId === target.projectId).reduce((total, entry) => total + (entry.storedBytes ?? 0), 0);
      let scopeCount = document.sources.filter(entry => entry.status !== 'deleted' && entry.scope === target.scope && entry.projectId === target.projectId).length;
      for (const file of files) {
        if (typeof file.text !== 'string' || Buffer.byteLength(file.text) > 2 * 1024 * 1024)
          throw toolFailure('资料文本过大。', 'INVALID_RETRIEVAL_SOURCE', 400);
        const existing = document.sources.find(entry => entry.status !== 'deleted' && entry.scope === target.scope &&
          entry.projectId === target.projectId && entry.originalPath === file.path && entry.contentHash === hash(file.text));
        if (existing) { added.push(existing); continue; }
        const storedBytes = Buffer.byteLength(file.text);
        if (++scopeCount > 1024 || (scopeBytes += storedBytes) > 32 * 1024 * 1024)
          throw toolFailure('此资料范围超过初版容量，请分拆资料或移除不需要的来源。', 'RETRIEVAL_LIBRARY_LIMIT', 413);
        const id = randomUUID(), folder = join(this.folder, id, 'source');
        await ensureLocalDirectory(folder);
        // Store a normalized text snapshot; original files are not modified or deleted.
        // 保存规范化文本快照，不修改或删除用户原文件。
        await writeFile(join(folder, 'document.txt'), file.text, { flag: 'wx', encoding: 'utf8' });
        const entry = { id, ...target, title: file.title || basename(file.path), originalPath: file.path,
          revision: 1, storedBytes, contentHash: hash(file.text), status: 'ready', createdAt: new Date().toISOString() };
        document.sources.push(entry); added.push(entry);
      }
      await this._target(scope, projectId);
      document.revision++;
      await atomicJson(this.file, document);
      return { revision: document.revision, sources: added.map(entry => ({ ...entry })) };
    });
  }

  async _readEntry(entry) {
    const file = join(this.folder, validateId(entry.id), 'source', 'document.txt');
    const info = await inspectLocalPath(file);
    if (!info.isFile() || info.size > 2 * 1024 * 1024) throw toolFailure('导入资料异常。', 'INVALID_RETRIEVAL_SOURCE', 409);
    const text = await readFile(file, 'utf8');
    if (hash(text) !== entry.contentHash) throw toolFailure('导入资料已被意外更改。', 'STALE_RETRIEVAL_SOURCE', 409);
    return { sourceId: entry.id, scopeKey: scopeKey(entry), sourceType: 'knowledge', title: entry.title,
      locator: { knowledgeId: entry.id, name: entry.title }, text, contentHash: entry.contentHash, sourceRevision: entry.revision };
  }

  /** Read one authorized snapshot from the registry and disk, including its actual content hash.
   * 从正式登记和磁盘回读单份已授权快照，核实版本及实际正文哈希。 */
  readSource(sourceId, { scopeKeys, sourceRevision } = {}) {
    sourceId = validateId(sourceId).toLowerCase();
    const scopes = retrievalScopeKeys(scopeKeys);
    return this._enqueue(async () => {
      const entry = (await this._read()).sources.find(value => value.id === sourceId);
      if (!entry || entry.status === 'deleted' || !scopes.includes(scopeKey(entry)) ||
          sourceRevision !== undefined && entry.revision !== sourceRevision) return null;
      if (entry.scope === 'project') {
        try { await this._target(entry.scope, entry.projectId); } catch { return null; }
      }
      try {
        const source = await this._readEntry(entry);
        this.errors.delete(entry.id);
        return source;
      } catch (error) {
        this.errors.set(entry.id, typeof error.code === 'string' ? error.code : 'RETRIEVAL_SOURCE_UNAVAILABLE');
        return null;
      }
    });
  }

  readAll(scopeKeys) {
    return this._enqueue(async () => {
      const document = await this._read(), sources = [];
      for (const entry of document.sources) {
        if (entry.status === 'deleted' || !scopeKeys.includes(scopeKey(entry))) continue;
        if (entry.scope === 'project') {
          try { await this._target(entry.scope, entry.projectId); } catch { continue; }
        }
        try {
          sources.push(await this._readEntry(entry));
          this.errors.delete(entry.id);
        } catch (error) {
          // One unavailable snapshot must not block other authorized sources; originals stay untouched.
          // 一份快照异常不能阻断其他已授权资料；保留原文件，显示来源错误供用户修复。
          this.errors.set(entry.id, typeof error.code === 'string' ? error.code : 'RETRIEVAL_SOURCE_UNAVAILABLE');
        }
      }
      return sources;
    });
  }

  isActive(sourceId, sourceRevision) {
    return this._enqueue(async () => (await this._read()).sources.some(entry =>
      entry.id === sourceId && entry.status !== 'deleted' && entry.revision === sourceRevision));
  }

  remove(sourceId, { expectedRevision } = {}) {
    return this._enqueue(async () => {
      sourceId = validateId(sourceId).toLowerCase();
      const document = await this._read(), entry = document.sources.find(source => source.id === sourceId);
      if (!entry) throw toolFailure('资料不存在。', 'RETRIEVAL_SOURCE_UNAVAILABLE', 404);
      if (expectedRevision !== undefined && expectedRevision !== entry.revision)
        throw toolFailure('资料已变化，请重新读取。', 'RETRIEVAL_CONFIG_CONFLICT', 409);
      if (entry.status !== 'deleted') {
        entry.status = 'deleted'; entry.revision++; entry.deletedAt = new Date().toISOString();
        document.revision++; await atomicJson(this.file, document);
      }
      return { ...entry, scopeKey: scopeKey(entry) };
    });
  }
}

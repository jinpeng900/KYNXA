import { createHash, randomUUID } from 'node:crypto';
import { open, readFile, unlink } from 'node:fs/promises';
import { basename, join } from 'node:path';
import { atomicJson } from '../../platform/atomic-json.mjs';
import { validateId } from '../../platform/conversation-id.mjs';
import { ensureLocalDirectory, inspectLocalPath, toolFailure } from '../../platform/tool-paths.mjs';
import { retrievalScopeKeys, validateSourceExtraction, validateSourceFileWindow } from './retrieval-contracts.mjs';
import { validateIndexingLimits } from './settings.mjs';

const hash = value => createHash('sha256').update(value).digest('hex');
const scopeKey = entry => entry.scope === 'user' ? 'user' : `project:${entry.projectId}`;
const MAX_REGISTRY_BYTES = 32 * 1024 * 1024;

/** Decoder input limits apply when importing bytes; stored snapshots retain output and page limits on every read.
 * 解码输入额度在导入原始字节时执行；已存正文快照每次读取仍受正文和页数额度约束。
 */
function documentLimitViolation(entry, limits, textBytes = entry.storedBytes ?? 0) {
  if (!entry.extraction) return null;
  if (textBytes > limits.maximumDocumentOutputBytes)
    return { dimension: 'maximumDocumentOutputBytes', limit: limits.maximumDocumentOutputBytes, observed: textBytes };
  const selectedPages = entry.extraction.pages?.length ?? entry.extraction.pageCount ?? 0;
  if (selectedPages > limits.maximumPdfPages)
    return { dimension: 'maximumPdfPages', limit: limits.maximumPdfPages, observed: selectedPages };
  return null;
}

/**
 * Explicitly imported text is authoritative; indexes never own or resurrect this registry.
 * 显式导入的文本与登记是权威资料；索引不能拥有或恢复已撤销的来源。
 */
export class SourceLibrary {
  constructor({ root, conversationStore }) {
    this.root = root; this.conversations = conversationStore;
    this.folder = join(root, 'Knowledge'); this.file = join(this.folder, 'catalog.json');
    this.queue = Promise.resolve(); this.errors = new Map();
    this.registryCache = null; this.sourceLookup = new WeakMap();
  }

  _enqueue(operation) {
    const pending = this.queue.catch(() => {}).then(operation);
    this.queue = pending; return pending;
  }

  async _read({ mutable = false } = {}) {
    await ensureLocalDirectory(this.folder);
    const info = await inspectLocalPath(this.file, { allowMissing: true });
    if (!info) { this.registryCache = null; return { schemaVersion: 1, revision: 0, sources: [] }; }
    const identity = JSON.stringify([info.dev, info.ino, info.size, info.mtimeMs, info.ctimeMs]);
    // Reuse validated registry metadata, not snapshot bodies; mutations receive an isolated copy.
    // 只复用已校验的登记元信息，不缓存资料正文；写入取得独立副本，失败事务不污染读取缓存。
    if (this.registryCache?.identity === identity)
      return mutable ? structuredClone(this.registryCache.document) : this.registryCache.document;
    if (info.size > MAX_REGISTRY_BYTES)
      throw toolFailure('资料登记文件超过资源预算。', 'RETRIEVAL_LIBRARY_LIMIT', 413);
    const value = JSON.parse(await readFile(this.file, 'utf8'));
    if (value.schemaVersion !== 1 || !Number.isSafeInteger(value.revision) || !Array.isArray(value.sources))
      throw toolFailure('资料登记版本无效，原文件已保留。', 'UNSUPPORTED_RETRIEVAL_SCHEMA', 409);
    for (const source of value.sources) {
      validateId(source.id);
      if (!['user', 'project'].includes(source.scope) || typeof source.title !== 'string' ||
          !['ready', 'deleted'].includes(source.status) || !Number.isSafeInteger(source.revision))
        throw toolFailure('资料登记损坏，原文件已保留。', 'CORRUPT_RETRIEVAL_LIBRARY', 500);
      if (source.scope === 'project') validateId(source.projectId);
      if (source.extraction !== undefined) validateSourceExtraction(source.extraction);
      if (source.fileWindow !== undefined) validateSourceFileWindow(source.fileWindow);
    }
    const after = await inspectLocalPath(this.file);
    if (identity !== JSON.stringify([after.dev, after.ino, after.size, after.mtimeMs, after.ctimeMs]))
      throw toolFailure('读取期间资料登记已变化。', 'STALE_RETRIEVAL_SOURCE', 409);
    this.registryCache = { identity, document: value };
    return mutable ? structuredClone(value) : value;
  }

  _entry(document, sourceId) {
    let entries = this.sourceLookup.get(document);
    if (!entries) { entries = new Map(document.sources.map(entry => [entry.id, entry])); this.sourceLookup.set(document, entries); }
    return entries.get(sourceId);
  }

  async _write(document, signal) {
    // Registry growth must not commit a file that the bounded reader would subsequently reject.
    // 登记增长不能提交读取器下一次无法打开的文件；容量失败保留已有登记并回滚新快照。
    if (Buffer.byteLength(`${JSON.stringify(document, null, 2)}\n`) > MAX_REGISTRY_BYTES)
      throw toolFailure('资料登记超过存储预算，已有来源已保留。', 'RETRIEVAL_LIBRARY_LIMIT', 413);
    await atomicJson(this.file, document, { signal });
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

  list(projectId = null, { signal } = {}) {
    return this._enqueue(async () => {
      signal?.throwIfAborted();
      if (projectId) await this._target('project', projectId);
      const document = await this._read();
      signal?.throwIfAborted();
      return { revision: document.revision, sources: document.sources.filter(entry => entry.status !== 'deleted' &&
        (entry.scope === 'user' || projectId && entry.projectId === projectId.toLowerCase())).map(entry => ({
        id: entry.id, title: entry.title, scope: entry.scope, projectId: entry.projectId,
        path: entry.originalPath, revision: entry.revision, status: this.errors.has(entry.id) ? 'error' : entry.status,
        ...(this.errors.has(entry.id) ? { error: this.errors.get(entry.id) } : {})
      })) };
    });
  }

  add(files, { scope = 'user', projectId = null, signal, limits } = {}) {
    return this._enqueue(async () => {
      signal?.throwIfAborted();
      const resourceLimits = validateIndexingLimits(limits);
      const target = await this._target(scope, projectId), document = await this._read({ mutable: true });
      const isStream = typeof files?.[Symbol.asyncIterator] === 'function';
      if (!isStream && (!Array.isArray(files) || !files.length || files.length > 512))
        throw toolFailure('没有可导入的文本资料或数量过大。', 'INVALID_RETRIEVAL_SOURCE', 400);
      const added = [];
      const ownedFiles = [];
      let committed = false;
      try {
        let scopeBytes = document.sources.filter(entry => entry.status !== 'deleted' && entry.scope === target.scope &&
          entry.projectId === target.projectId).reduce((total, entry) => total + (entry.storedBytes ?? 0), 0);
        let scopeCount = document.sources.filter(entry => entry.status !== 'deleted' && entry.scope === target.scope && entry.projectId === target.projectId).length;
        for await (const file of files) {
          signal?.throwIfAborted();
          if (typeof file.text !== 'string' || Buffer.byteLength(file.text) > Math.min(resourceLimits.maximumSourceBytes, 2 * 1024 * 1024))
            throw toolFailure('资料文本过大。', 'INVALID_RETRIEVAL_SOURCE', 400);
          const extraction = file.extraction === undefined ? undefined : validateSourceExtraction(file.extraction, file.text.length);
          if (documentLimitViolation({ extraction }, resourceLimits, Buffer.byteLength(file.text)))
            throw toolFailure('资料提取正文或页数超过当前文档额度。', 'RETRIEVAL_LIBRARY_LIMIT', 413);
          const fileWindow = file.fileWindow === undefined ? undefined : validateSourceFileWindow(file.fileWindow, file.text.length);
          const existing = document.sources.find(entry => entry.status !== 'deleted' && entry.scope === target.scope &&
            entry.projectId === target.projectId && entry.originalPath === file.path && entry.contentHash === hash(file.text) &&
            JSON.stringify(entry.extraction) === JSON.stringify(extraction) &&
            JSON.stringify(entry.fileWindow) === JSON.stringify(fileWindow));
          if (existing) { added.push(existing); continue; }
          const storedBytes = Buffer.byteLength(file.text);
          if (++scopeCount > resourceLimits.maximumFiles || (scopeBytes += storedBytes) > resourceLimits.maximumTotalBytes)
            throw toolFailure('此资料范围超过配置容量，请调整预算或移除不需要的来源。', 'RETRIEVAL_LIBRARY_LIMIT', 413);
          const id = randomUUID(), folder = join(this.folder, id, 'source');
          await ensureLocalDirectory(folder);
          // Store a normalized text snapshot; original files are not modified or deleted.
          // 保存规范化文本快照，不修改或删除用户原文件。
          const snapshotPath = join(folder, 'document.txt');
          const handle = await open(snapshotPath, 'wx');
          ownedFiles.push(snapshotPath);
          try { await handle.writeFile(file.text, { encoding: 'utf8', signal }); }
          finally { await handle.close(); }
          const entry = { id, ...target, title: file.title || basename(file.path), originalPath: file.path,
            revision: 1, storedBytes, contentHash: hash(file.text), status: 'ready', createdAt: new Date().toISOString(),
            ...(extraction === undefined ? {} : { extraction }), ...(fileWindow === undefined ? {} : { fileWindow }) };
          document.sources.push(entry); added.push(entry);
        }
        if (!added.length) throw toolFailure('没有成功读取的可导入资料。', 'RETRIEVAL_IMPORT_EMPTY', 400);
        await this._target(scope, projectId);
        signal?.throwIfAborted();
        document.revision++;
        await this._write(document, signal);
        committed = true;
        return { revision: document.revision, sources: added.map(entry => ({ ...entry })) };
      } finally {
        // Failed imports clean only snapshots created by this operation, never committed sources.
        // 失败导入只清理本操作创建且尚未提交的快照，不删除已经正式登记的资料。
        if (!committed) await Promise.all(ownedFiles.map(path => unlink(path).catch(error => {
          if (error.code !== 'ENOENT') throw error;
        })));
      }
    });
  }

  async _readEntry(entry, signal, limits) {
    signal?.throwIfAborted();
    const file = join(this.folder, validateId(entry.id), 'source', 'document.txt');
    const info = await inspectLocalPath(file);
    if (!info.isFile() || info.size > 2 * 1024 * 1024) throw toolFailure('导入资料异常。', 'INVALID_RETRIEVAL_SOURCE', 409);
    if (limits && (info.size > limits.maximumSourceBytes || documentLimitViolation(entry, limits, info.size)))
      throw toolFailure('已登记资料超过当前文档或来源额度，原文已保留。', 'RETRIEVAL_LIBRARY_LIMIT', 413);
    const text = await readFile(file, { encoding: 'utf8', signal });
    signal?.throwIfAborted();
    if (hash(text) !== entry.contentHash) throw toolFailure('导入资料已被意外更改。', 'STALE_RETRIEVAL_SOURCE', 409);
    return { sourceId: entry.id, scopeKey: scopeKey(entry), sourceType: 'knowledge', title: entry.title,
      locator: { knowledgeId: entry.id, name: entry.title, relativePath: basename(entry.originalPath),
        ...(entry.extraction === undefined ? {} : { extraction: validateSourceExtraction(entry.extraction, text.length) }),
        ...(entry.fileWindow === undefined ? {} : { fileWindow: validateSourceFileWindow(entry.fileWindow, text.length) }) },
      text, contentHash: entry.contentHash, sourceRevision: entry.revision };
  }

  /** Read one authorized snapshot from the registry and disk, including its actual content hash.
   * 从正式登记和磁盘回读单份已授权快照，核实版本及实际正文哈希。 */
  readSource(sourceId, { scopeKeys, sourceRevision, signal, limits } = {}) {
    sourceId = validateId(sourceId).toLowerCase();
    const scopes = retrievalScopeKeys(scopeKeys);
    const resourceLimits = limits === undefined ? undefined : validateIndexingLimits(limits);
    return this._enqueue(async () => {
      signal?.throwIfAborted();
      const entry = this._entry(await this._read(), sourceId);
      if (!entry || entry.status === 'deleted' || !scopes.includes(scopeKey(entry)) ||
          sourceRevision !== undefined && entry.revision !== sourceRevision) return null;
      if (entry.scope === 'project') {
        try { await this._target(entry.scope, entry.projectId); } catch { return null; }
      }
      try {
        const source = await this._readEntry(entry, signal, resourceLimits);
        this.errors.delete(entry.id);
        return source;
      } catch (error) {
        if (signal?.aborted) throw error;
        this.errors.set(entry.id, typeof error.code === 'string' ? error.code : 'RETRIEVAL_SOURCE_UNAVAILABLE');
        return null;
      }
    });
  }

  readAll(scopeKeys, { signal } = {}) {
    return this._enqueue(async () => {
      signal?.throwIfAborted();
      const document = await this._read(), sources = [];
      for (const entry of document.sources) {
        signal?.throwIfAborted();
        if (entry.status === 'deleted' || !scopeKeys.includes(scopeKey(entry))) continue;
        if (entry.scope === 'project') {
          try { await this._target(entry.scope, entry.projectId); } catch { continue; }
        }
        try {
          sources.push(await this._readEntry(entry, signal));
          this.errors.delete(entry.id);
        } catch (error) {
          if (signal?.aborted) throw error;
          // One unavailable snapshot must not block other authorized sources; originals stay untouched.
          // 一份快照异常不能阻断其他已授权资料；保留原文件，显示来源错误供用户修复。
          this.errors.set(entry.id, typeof error.code === 'string' ? error.code : 'RETRIEVAL_SOURCE_UNAVAILABLE');
        }
      }
      signal?.throwIfAborted();
      return sources;
    });
  }

  /** Describe authorized sources without reading or retaining the corpus text.
   * 只列出已授权来源元信息，不读取或驻留整个语料的正文。 */
  describeSources(scopeKeys, { signal, limits, allowPartial = false } = {}) {
    const scopes = retrievalScopeKeys(scopeKeys);
    const resourceLimits = validateIndexingLimits(limits);
    return this._enqueue(async () => {
      signal?.throwIfAborted();
      const document = await this._read(), sources = [], projects = new Map(), usage = new Map();
      const coverage = { complete: true, discovered: 0, admitted: 0, skipped: 0, limits: [] };
      for (const entry of document.sources) {
        signal?.throwIfAborted();
        if (entry.status === 'deleted' || !scopes.includes(scopeKey(entry))) continue;
        coverage.discovered++;
        if (entry.scope === 'project') {
          if (!projects.has(entry.projectId)) {
            try { await this._target(entry.scope, entry.projectId); projects.set(entry.projectId, true); }
            catch { projects.set(entry.projectId, false); }
          }
          if (!projects.get(entry.projectId)) continue;
        }
        const scope = scopeKey(entry), current = usage.get(scope) ?? { files: 0, bytes: 0 };
        const proposed = { files: current.files + 1, bytes: current.bytes + (entry.storedBytes ?? 0) };
        const documentLimit = documentLimitViolation(entry, resourceLimits);
        const dimension = documentLimit?.dimension ?? ((entry.storedBytes ?? 0) > resourceLimits.maximumSourceBytes ? 'maximumSourceBytes' :
          proposed.files > resourceLimits.maximumFiles ? 'maximumFiles' : proposed.bytes > resourceLimits.maximumTotalBytes ? 'maximumTotalBytes' : null);
        if (dimension) {
          if (!allowPartial) throw toolFailure('已登记资料超过当前索引预算，原文已保留。', 'RETRIEVAL_LIBRARY_LIMIT', 413);
          coverage.complete = false; coverage.skipped++;
          if (coverage.limits.length < 8 && !coverage.limits.some(item => item.dimension === dimension && item.scopeKey === scope))
            coverage.limits.push({ dimension, limit: resourceLimits[dimension], observed: documentLimit?.observed ?? (dimension === 'maximumSourceBytes' ?
              entry.storedBytes : dimension === 'maximumFiles' ? proposed.files : proposed.bytes), scopeKey: scope });
          continue;
        }
        usage.set(scope, proposed); coverage.admitted++;
        sources.push({ sourceId: entry.id, scopeKey: scopeKey(entry), sourceType: 'knowledge', title: entry.title,
          locator: { knowledgeId: entry.id, name: entry.title, relativePath: basename(entry.originalPath),
            ...(entry.extraction === undefined ? {} : { extraction: validateSourceExtraction(entry.extraction) }),
            ...(entry.fileWindow === undefined ? {} : { fileWindow: validateSourceFileWindow(entry.fileWindow) }) },
          contentHash: entry.contentHash, sourceRevision: entry.revision, storedBytes: entry.storedBytes ?? 0 });
      }
      return { revision: document.revision, sources, coverage };
    });
  }

  isActive(sourceId, sourceRevision) {
    return this._enqueue(async () => {
      const entry = this._entry(await this._read(), sourceId);
      return Boolean(entry && entry.status !== 'deleted' && entry.revision === sourceRevision);
    });
  }

  remove(sourceId, { expectedRevision, signal } = {}) {
    return this._enqueue(async () => {
      signal?.throwIfAborted();
      sourceId = validateId(sourceId).toLowerCase();
      const document = await this._read({ mutable: true }), entry = this._entry(document, sourceId);
      if (!entry) throw toolFailure('资料不存在。', 'RETRIEVAL_SOURCE_UNAVAILABLE', 404);
      if (expectedRevision !== undefined && expectedRevision !== entry.revision)
        throw toolFailure('资料已变化，请重新读取。', 'RETRIEVAL_CONFIG_CONFLICT', 409);
      if (entry.status !== 'deleted') {
        entry.status = 'deleted'; entry.revision++; entry.deletedAt = new Date().toISOString();
        document.revision++;
        signal?.throwIfAborted();
        await this._write(document, signal);
      }
      return { ...entry, scopeKey: scopeKey(entry) };
    });
  }
}

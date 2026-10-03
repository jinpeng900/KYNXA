import { randomUUID } from 'node:crypto';
import { lstat, mkdir, readFile, unlink, writeFile } from 'node:fs/promises';
import { dirname, join, relative, resolve, sep } from 'node:path';
import { atomicJson } from './store.mjs';
import { MEMORY_SCHEMA_VERSION, MAX_MEMORY_ENTRIES, memoryFailure, memoryId, memoryScope,
  memoryContent, memoryKind, expectedMemoryRevision, validateMemoryDocument, validateMemorySource } from './memory-contracts.mjs';

// All gateway instances in this process share a file queue. The gateway remains the sole writer of Data.
const queues = new Map();
const MAX_MEMORY_FILE_BYTES = 8 * 1024 * 1024;

export class MemoryRepository {
  constructor({ conversationStore }) {
    if (!conversationStore?.root) throw memoryFailure('缺少会话存储。');
    this.conversations = conversationStore;
    this.root = resolve(conversationStore.root);
  }

  async relationship(conversationId) {
    return this.conversations.describeConversation(memoryId(conversationId));
  }

  _location(relationship, scope) {
    scope = memoryScope(scope);
    let scopeId, folder;
    if (scope === 'user') { scopeId = 'user'; folder = join(this.root, 'Memory'); }
    else if (scope === 'project') {
      if (!relationship.projectId || relationship.isFolderlessWorkspace || relationship.projectArchived)
        throw memoryFailure('此聊天未关联可共享记忆的项目。');
      scopeId = memoryId(relationship.projectId);
      folder = join(this.root, 'Projects', scopeId, 'Memory');
    } else {
      scopeId = memoryId(relationship.conversationId);
      folder = join(relationship.sessionDirectory, 'Memory');
    }
    return { scope, scopeId, folder, file: join(folder, 'entries.json') };
  }

  async _safe(path, { create = false, file = false } = {}) {
    path = resolve(path);
    const suffix = relative(this.root, path);
    if (suffix === '..' || suffix.startsWith(`..${sep}`) || resolve(this.root, suffix) !== path)
      throw memoryFailure('记忆路径无效。');
    const parts = suffix ? suffix.split(sep) : [];
    let current = this.root;
    for (let index = -1; index < parts.length; index++) {
      if (index >= 0) current = join(current, parts[index]);
      const isFile = file && index === parts.length - 1;
      try {
        const info = await lstat(current);
        if (info.isSymbolicLink() || (isFile ? !info.isFile() : !info.isDirectory()))
          throw memoryFailure('记忆目录或文件包含链接或无效结构，请检查存储位置。', 'INVALID_MEMORY_PATH', 409);
        if (isFile && info.size > MAX_MEMORY_FILE_BYTES)
          throw memoryFailure('记忆文件过大，原文件已保留。', 'CORRUPT_MEMORY', 500);
      } catch (error) {
        if (error.code !== 'ENOENT') throw error;
        if (create && !isFile) await mkdir(current, { mode: 0o700 });
        else return false;
      }
    }
    return true;
  }

  async _read(location) {
    if (!await this._safe(location.file, { file: true }))
      return { schemaVersion: MEMORY_SCHEMA_VERSION, scope: location.scope, scopeId: location.scopeId, revision: 0, entries: [], dismissedSources: [] };
    let value;
    try { value = JSON.parse((await readFile(location.file, 'utf8')).replace(/^\uFEFF/, '')); }
    catch (error) {
      if (error instanceof SyntaxError)
        throw memoryFailure('记忆文件格式无效，原文件已保留。', 'CORRUPT_MEMORY', 500);
      throw error;
    }
    return validateMemoryDocument(value, location);
  }

  async _run(file, operation) {
    const previous = queues.get(file) ?? Promise.resolve();
    const result = previous.catch(() => {}).then(operation);
    queues.set(file, result);
    try { return await result; }
    finally { if (queues.get(file) === result) queues.delete(file); }
  }

  async read(conversationId, scope) {
    return this._withScope(conversationId, scope, location => this._read(location));
  }

  readScope(scope, scopeId) {
    return this._withManagedScope(scope, scopeId, location => this._read(location));
  }

  readFor(conversationId) {
    return this.conversations.withConversationStorage(memoryId(conversationId), async relationship => {
      const scopeNames = ['chat', ...(!relationship.isFolderlessWorkspace && !relationship.projectArchived && relationship.projectId ? ['project'] : []), 'user'];
      const reads = await Promise.allSettled(scopeNames.map(async scope => {
        const location = this._location(relationship, scope);
        return this._run(location.file, () => this._read(location));
      }));
      // A failed scope must not release the catalog guard while another scope still performs session IO.
      const failed = reads.find(result => result.status === 'rejected');
      if (failed) throw failed.reason;
      const scopes = reads.map(result => result.value);
      return { conversationId: relationship.conversationId, projectId: relationship.projectId,
        isFolderlessWorkspace: Boolean(relationship.isFolderlessWorkspace), scopes };
    });
  }

  _withScope(conversationId, scope, operation) {
    // Always acquire the conversation queue before the file queue. Resolve paths only while catalog moves are excluded.
    return this.conversations.withConversationStorage(memoryId(conversationId), relationship => {
      const location = this._location(relationship, scope);
      return this._run(location.file, () => operation(location));
    });
  }

  _withManagedScope(scope, scopeId, operation) {
    scope = memoryScope(scope);
    const run = relationship => {
      if (scope === 'project' && relationship.isFolderlessWorkspace)
        throw memoryFailure('无文件夹工作不提供共享记忆。');
      // Archived real work remains manageable; only conversation context suppresses its injection.
      const location = this._location(scope === 'project' ? { projectId: relationship.projectId } : {}, scope);
      return this._run(location.file, () => operation(location));
    };
    // Preserve catalog/conversation queue -> memory file queue ordering for every management operation.
    if (scope === 'user') return this.conversations.withCatalogStorage(() => run({}));
    if (scope === 'project') return this.conversations.withProjectStorage(memoryId(scopeId), run);
    throw memoryFailure('独立记忆管理仅支持工作和用户作用域。');
  }

  async mutate(conversationId, scope, expectedRevision, operation) {
    return this._withScope(conversationId, scope, location => this._mutate(location, expectedRevision, operation));
  }

  mutateScope(scope, scopeId, expectedRevision, operation) {
    return this._withManagedScope(scope, scopeId, location => this._mutate(location, expectedRevision, operation));
  }

  async _mutate(location, expectedRevision, operation) {
    const document = await this._read(location);
    if (expectedRevision !== undefined && document.revision !== expectedMemoryRevision(expectedRevision))
      throw memoryFailure('记忆已更新，请重新读取后重试。', 'MEMORY_CONFLICT', 409);
    const changed = await operation(document);
    if (changed === false) return structuredClone(document);
    document.revision++;
    const validated = validateMemoryDocument(document, location);
    // atomicJson writes indented JSON; enforce the size of those exact bytes so a successful write always remains readable.
    if (Buffer.byteLength(JSON.stringify(validated, null, 2)) > MAX_MEMORY_FILE_BYTES)
      throw memoryFailure('此作用域记忆文件已达容量上限，无法追加；已有记忆已保留。', 'MEMORY_CAPACITY_EXCEEDED', 409);
    await this._safe(location.folder, { create: true });
    await this._safe(location.file, { file: true });
    await atomicJson(location.file, validated);
    return structuredClone(validated);
  }

  create(conversationId, input) {
    return this._create(input, (scope, expected, operation) => this.mutate(conversationId, scope, expected, operation));
  }

  createScope(scope, scopeId, input) {
    return this._create({ ...input, scope }, (_, expected, operation) => this.mutateScope(scope, scopeId, expected, operation));
  }

  _create(input, mutate) {
    const scope = memoryScope(input.scope), content = memoryContent(input.content), kind = memoryKind(input.kind, scope);
    const source = validateMemorySource(input.source);
    return mutate(scope, expectedMemoryRevision(input.expectedRevision), document => {
      // A failed/retried model turn must not create the same explicit memory twice.
      if (source.type === 'user-message' && document.dismissedSources.some(item =>
          item.conversationId === source.conversationId && item.messageId === source.messageId)) return false;
      if (source.type === 'user-message' && document.entries.some(entry => entry.source.type === 'user-message' &&
          entry.source.conversationId === source.conversationId && entry.source.messageId === source.messageId)) return false;
      if (document.entries.length >= MAX_MEMORY_ENTRIES) throw memoryFailure('此作用域的记忆已达上限，请先整理或删除。');
      const now = new Date().toISOString();
      document.entries.push({ id: randomUUID(), scope, scopeId: document.scopeId, content, kind, status: 'confirmed',
        source, revision: 1, createdAt: now, updatedAt: now });
    });
  }

  update(conversationId, memoryIdValue, input) {
    return this._update(memoryIdValue, input, (scope, expected, operation) => this.mutate(conversationId, scope, expected, operation));
  }

  updateScope(scope, scopeId, memoryIdValue, input) {
    return this._update(memoryIdValue, { ...input, scope }, (_, expected, operation) => this.mutateScope(scope, scopeId, expected, operation));
  }

  _update(memoryIdValue, input, mutate) {
    const id = memoryId(memoryIdValue), scope = memoryScope(input.scope);
    const expected = expectedMemoryRevision(input.expectedRevision, true);
    const content = input.content === undefined ? undefined : memoryContent(input.content);
    const kind = input.kind === undefined ? undefined : memoryKind(input.kind, scope);
    if (content === undefined && kind === undefined) throw memoryFailure('请提供要更新的记忆内容或类型。');
    return mutate(scope, expected, document => {
      const entry = document.entries.find(item => item.id === id);
      if (!entry) throw memoryFailure('记忆不存在。', 'MEMORY_NOT_FOUND', 404);
      if (content !== undefined) entry.content = content;
      if (kind !== undefined) entry.kind = kind;
      entry.revision++; entry.updatedAt = new Date().toISOString();
    });
  }

  delete(conversationId, memoryIdValue, input) {
    return this._delete(memoryIdValue, input, (scope, expected, operation) => this.mutate(conversationId, scope, expected, operation));
  }

  deleteScope(scope, scopeId, memoryIdValue, input) {
    return this._delete(memoryIdValue, { ...input, scope }, (_, expected, operation) => this.mutateScope(scope, scopeId, expected, operation));
  }

  _delete(memoryIdValue, input, mutate) {
    const id = memoryId(memoryIdValue), scope = memoryScope(input.scope);
    return mutate(scope, expectedMemoryRevision(input.expectedRevision, true), document => {
      const index = document.entries.findIndex(item => item.id === id);
      if (index < 0) throw memoryFailure('记忆不存在。', 'MEMORY_NOT_FOUND', 404);
      const entry = document.entries[index];
      if (entry.source.type === 'user-message') document.dismissedSources.push({
        conversationId: entry.source.conversationId, messageId: entry.source.messageId, deletedAt: new Date().toISOString()
      });
      document.entries.splice(index, 1);
    });
  }

  _withSummary(conversationId, operation) {
    return this.conversations.withConversationStorage(memoryId(conversationId), relationship => {
      const file = join(relationship.sessionDirectory, 'context.json');
      return this._run(file, () => operation(file));
    });
  }

  async readSummary(conversationId) {
    return this._withSummary(conversationId, async file => {
      if (!await this._safe(file, { file: true })) return null;
      const original = await readFile(file);
      let value;
      try { value = JSON.parse(original.toString('utf8').replace(/^\uFEFF/, '')); }
      catch (error) {
        if (error instanceof SyntaxError) {
          // Summaries are rebuildable projections. Preserve the exact corrupt bytes before clearing the canonical file.
          await this._preserveCorruptSummary(file, original);
          return null;
        }
        throw error;
      }
      if (!value || value.schemaVersion !== 1)
        throw memoryFailure('聊天摘要版本不受当前程序支持，原文件已保留。', 'UNSUPPORTED_SUMMARY_VERSION', 409);
      return value;
    });
  }

  async _preserveCorruptSummary(file, original) {
    const backup = join(dirname(file), `context.corrupt-${randomUUID()}.json`);
    await this._safe(file, { file: true });
    await this._safe(backup, { file: true });
    await writeFile(backup, original, { flag: 'wx', mode: 0o600 });
    // A failed backup or removal must propagate; the caller must not rebuild over unpreserved data.
    await unlink(file);
  }

  async writeSummary(conversationId, summary) {
    if (!summary || summary.schemaVersion !== 1 || memoryId(summary.conversationId) !== memoryId(conversationId))
      throw memoryFailure('聊天摘要版本或归属无效。');
    if (Buffer.byteLength(JSON.stringify(summary, null, 2)) > MAX_MEMORY_FILE_BYTES) throw memoryFailure('聊天摘要过大。');
    return this._withSummary(conversationId, async file => {
      // An unsupported future document must never be replaced by an older application.
      if (await this._safe(file, { file: true })) {
        let previous;
        try { previous = JSON.parse((await readFile(file, 'utf8')).replace(/^\uFEFF/, '')); }
        catch (error) {
          if (error instanceof SyntaxError) throw memoryFailure('聊天摘要格式无效，原文件已保留。', 'CORRUPT_SUMMARY', 500);
          throw error;
        }
        if (previous?.schemaVersion !== 1)
          throw memoryFailure('聊天摘要版本不受当前程序支持，原文件已保留。', 'UNSUPPORTED_SUMMARY_VERSION', 409);
      }
      await this._safe(dirname(file), { create: true });
      await atomicJson(file, summary);
      return structuredClone(summary);
    });
  }
}

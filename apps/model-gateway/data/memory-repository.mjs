import { randomUUID } from 'node:crypto';
import { lstat, mkdir, readFile, unlink, writeFile } from 'node:fs/promises';
import { dirname, join, relative, resolve, sep } from 'node:path';
import { atomicJson } from '../platform/atomic-json.mjs';
import { MEMORY_SCHEMA_VERSION, MAX_MEMORY_ENTRIES, memoryFailure, memoryId, memoryScope,
  memoryContent, memoryKind, memoryStatus, memorySourceId, memoryCandidateFingerprint, expectedMemoryRevision,
  memoryEntryIdentity, memoryProposalFingerprint, validateMemoryDocument, validateMemorySource,
  validateMemoryCandidate, validateMemoryProposal, validateMemoryUpdateInput, requireMemoryProposalConfirmation } from './memory-contracts.mjs';

// All gateway instances in this process share a file queue. The gateway remains the sole writer of Data.
// 同一进程中的所有网关实例共用文件队列，网关仍是 Data 的唯一正式写入者。
const queues = new Map();
const MAX_MEMORY_FILE_BYTES = 8 * 1024 * 1024;

export class MemoryRepository {
  constructor({ conversationStore }) {
    if (!conversationStore?.root) throw memoryFailure('缺少会话存储。');
    this.conversations = conversationStore;
    this.root = resolve(conversationStore.root);
    this.listeners = new Set();
    this.pendingChanges = new WeakMap();
  }

  onChange(listener) {
    if (typeof listener !== 'function') throw new TypeError('Memory change listener is required.');
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  async _publish(document) {
    const changes = this.pendingChanges.get(document) ?? [];
    this.pendingChanges.delete(document);
    // Subscribers run only after catalog/file guards release; incremental indexing may itself read the catalog.
    // 订阅通知仅在目录和文件保护释放后执行，增量索引可安全地再次读取目录。
    for (const change of changes) for (const listener of this.listeners) {
      try { await listener({ ...change }); }
      catch (error) { this.lastChangeError = { code: typeof error?.code === 'string' ? error.code : 'MEMORY_CHANGE_NOTIFICATION_FAILED' }; }
    }
    return document;
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

  readFor(conversationId, selectedScopes) {
    if (selectedScopes !== undefined && (!Array.isArray(selectedScopes) || !selectedScopes.length ||
        selectedScopes.length > 3 || new Set(selectedScopes).size !== selectedScopes.length))
      throw memoryFailure('请选择有效且不重复的记忆范围。');
    selectedScopes?.forEach(memoryScope);
    return this.conversations.withConversationStorage(memoryId(conversationId), async relationship => {
      const visibleScopes = ['chat', ...(!relationship.isFolderlessWorkspace && !relationship.projectArchived && relationship.projectId ? ['project'] : []), 'user'];
      if (selectedScopes?.some(scope => !visibleScopes.includes(scope)))
        throw memoryFailure('当前聊天不可读取指定记忆范围。', 'MEMORY_SCOPE_CHANGED', 409);
      const scopeNames = selectedScopes ?? visibleScopes;
      const reads = await Promise.allSettled(scopeNames.map(async scope => {
        const location = this._location(relationship, scope);
        return this._run(location.file, () => this._read(location));
      }));
      // A failed scope must not release the catalog guard while another scope still performs session IO.
      // 某个范围失败时，不能在另一个范围仍进行会话 I/O 时释放目录保护。
      const failed = reads.find(result => result.status === 'rejected');
      if (failed) throw failed.reason;
      const scopes = reads.map(result => result.value);
      return { conversationId: relationship.conversationId, projectId: relationship.projectId,
        isFolderlessWorkspace: Boolean(relationship.isFolderlessWorkspace), scopes };
    });
  }

  _withScope(conversationId, scope, operation) {
    // Always acquire the conversation queue before the file queue. Resolve paths only while catalog moves are excluded.
    // 始终先取得会话队列，再取得文件队列；仅在目录移动被排除期间解析路径。
    return this.conversations.withConversationStorage(memoryId(conversationId), (relationship, storage) => {
      const location = this._location(relationship, scope);
      return this._run(location.file, () => operation(location, storage));
    });
  }

  _withManagedScope(scope, scopeId, operation) {
    scope = memoryScope(scope);
    const run = (relationship, storage) => {
      if (scope === 'project' && relationship.isFolderlessWorkspace)
        throw memoryFailure('无文件夹工作不提供共享记忆。');
      // Archived real work remains manageable; only conversation context suppresses its injection.
      // 已归档的真实工作仍可管理，只在聊天上下文中停止注入其记忆。
      const location = this._location(scope === 'project' ? { projectId: relationship.projectId } : {}, scope);
      return this._run(location.file, () => operation(location, storage));
    };
    // Preserve catalog/conversation queue -> memory file queue ordering for every management operation.
    // 所有管理操作都保持目录或会话队列到记忆文件队列的锁定顺序。
    if (scope === 'user') return this.conversations.withCatalogStorage(storage => run({}, storage));
    if (scope === 'project') return this.conversations.withProjectStorage(memoryId(scopeId), run);
    throw memoryFailure('独立记忆管理仅支持工作和用户作用域。');
  }

  async mutate(conversationId, scope, expectedRevision, operation, { signal } = {}) {
    signal?.throwIfAborted();
    return this._publish(await this._withScope(conversationId, scope,
      (location, storage) => this._mutate(location, expectedRevision, operation, storage, { signal })));
  }

  async mutateScope(scope, scopeId, expectedRevision, operation) {
    return this._publish(await this._withManagedScope(scope, scopeId, (location, storage) => this._mutate(location, expectedRevision, operation, storage)));
  }

  async _mutate(location, expectedRevision, operation, storage, { signal } = {}) {
    signal?.throwIfAborted();
    const document = await this._read(location);
    signal?.throwIfAborted();
    if (expectedRevision !== undefined && document.revision !== expectedMemoryRevision(expectedRevision))
      throw memoryFailure('记忆已更新，请重新读取后重试。', 'MEMORY_CONFLICT', 409);
    const previousEntries = new Map(document.entries.map(entry => [entry.id, structuredClone(entry)]));
    const changed = await operation(document, storage);
    signal?.throwIfAborted();
    if (changed === false) return structuredClone(document);
    document.revision++;
    const validated = validateMemoryDocument(document, location);
    // atomicJson writes indented JSON; enforce the size of those exact bytes so a successful write always remains readable.
    // atomicJson 写入带缩进 JSON，按实际写出字节检查大小，确保写入成功后仍可读取。
    if (Buffer.byteLength(JSON.stringify(validated, null, 2)) > MAX_MEMORY_FILE_BYTES)
      throw memoryFailure('此作用域记忆文件已达容量上限，无法追加；已有记忆已保留。', 'MEMORY_CAPACITY_EXCEEDED', 409);
    await this._safe(location.folder, { create: true });
    await this._safe(location.file, { file: true });
    // Cancellation may prevent dispatch of the atomic commit; a completed commit keeps its actual receipt.
    // 取消可阻止原子提交发起，提交完成后仍返回真实回执，不能再把已保存草稿报告成未执行。
    signal?.throwIfAborted();
    await atomicJson(location.file, validated, { signal });
    const result = structuredClone(validated), entries = new Map(result.entries.map(entry => [entry.id, entry]));
    const changes = [];
    for (const id of new Set([...previousEntries.keys(), ...entries.keys()])) {
      const previous = previousEntries.get(id), entry = entries.get(id);
      if (JSON.stringify(previous) === JSON.stringify(entry)) continue;
      const record = entry ?? previous;
      changes.push({ scope: result.scope, scopeId: result.scopeId,
        scopeKey: result.scope === 'user' ? 'user' : `${result.scope}:${result.scopeId}`,
        sourceId: memorySourceId(result.scope, result.scopeId, id), memoryId: id,
        previousStatus: previous?.status ?? null, status: entry?.status ?? null,
        previousEntryRevision: previous?.revision ?? null, entryRevision: entry?.revision ?? null, revision: result.revision,
        operation: !previous ? 'create' : !entry ? 'delete' : previous.status === 'draft' && entry.status === 'confirmed' ? 'confirm' : 'update',
        ...(record.source.conversationId ? { conversationId: record.source.conversationId } : {}) });
    }
    this.pendingChanges.set(result, changes);
    return result;
  }

  async createCandidate(conversationId, input) {
    const scope = memoryScope(input.scope), content = memoryContent(input.content), kind = memoryKind(input.kind, scope);
    const source = validateMemorySource(input.source), candidate = validateMemoryCandidate(input.candidate, source);
    if (candidate.fingerprint !== memoryCandidateFingerprint(scope, content)) throw memoryFailure('候选内容身份不一致。');
    const candidateId = randomUUID();
    const document = await this.mutate(conversationId, scope, undefined, document => {
      if (document.scopeId !== input.scopeId) throw memoryFailure('候选来源范围已改变。', 'MEMORY_SCOPE_CHANGED', 409);
      const sourceIds = new Set(candidate.quotes.map(quote => quote.messageId));
      if (document.dismissedSources.some(item => item.candidateFingerprint === candidate.fingerprint ||
          item.conversationId === source.conversationId && sourceIds.has(item.messageId)) ||
          document.entries.some(entry => memoryCandidateFingerprint(scope, entry.content) === candidate.fingerprint ||
            entry.source.type === 'user-message' && entry.source.conversationId === source.conversationId &&
            (sourceIds.has(entry.source.messageId) || entry.candidate?.quotes.some(quote => sourceIds.has(quote.messageId))))) return false;
      if (document.entries.length >= MAX_MEMORY_ENTRIES) throw memoryFailure('此作用域的记忆已达上限，请先整理或删除。');
      const now = new Date().toISOString();
      document.entries.push({ id: candidateId, scope, scopeId: document.scopeId, content, kind, status: 'draft',
        source, candidate, revision: 1, createdAt: now, updatedAt: now });
    });
    const entry = document.entries.find(entry => entry.id === candidateId);
    return { document, created: Boolean(entry), entry };
  }

  async createProposal(conversationId, input, validateSource, { signal } = {}) {
    signal?.throwIfAborted();
    const scope = memoryScope(input.scope), content = memoryContent(input.content), kind = memoryKind(input.kind, scope);
    const source = validateMemorySource(input.source), proposal = validateMemoryProposal(input.proposal, source);
    if (proposal.fingerprint !== memoryProposalFingerprint(scope, input.scopeId, content, kind, proposal))
      throw memoryFailure('记忆建议内容身份不一致。');
    const proposalId = randomUUID();
    let reason = 'created', existingId;
    const document = await this.mutate(conversationId, scope, undefined, async (document, storage) => {
      signal?.throwIfAborted();
      if (document.scopeId !== input.scopeId) throw memoryFailure('建议来源范围已改变。', 'MEMORY_SCOPE_CHANGED', 409);
      await validateSource({ scope, scopeId: document.scopeId, source, proposal, status: 'draft' }, storage);
      signal?.throwIfAborted();
      const sourceIds = new Set(proposal.quotes.map(quote => quote.messageId));
      if (document.dismissedSources.some(item => item.proposalFingerprint === proposal.fingerprint ||
          item.conversationId === source.conversationId && sourceIds.has(item.messageId))) {
        reason = 'dismissed'; return false;
      }
      const existing = document.entries.find(entry => entry.proposal?.fingerprint === proposal.fingerprint);
      if (existing) { reason = 'deduplicated'; existingId = existing.id; return false; }
      if (proposal.target) {
        if (document.revision !== proposal.target.expectedRevision)
          throw memoryFailure('目标范围版本已变化，请重新读取。', 'MEMORY_CONFLICT', 409);
        this._proposalTarget(document, proposal.target);
      }
      const exactMatches = document.entries.filter(entry => entry.status === 'confirmed' &&
        entry.id !== proposal.target?.id && memoryCandidateFingerprint(scope, entry.content) === memoryCandidateFingerprint(scope, content));
      proposal.conflicts = [...new Set([...exactMatches.map(entry => `exact-content:${entry.id}`), ...proposal.conflicts])].slice(0, 8);
      if (document.entries.length >= MAX_MEMORY_ENTRIES) throw memoryFailure('此作用域的记忆已达上限，请先整理或删除。');
      const now = new Date().toISOString();
      document.entries.push({ id: proposalId, scope, scopeId: document.scopeId, content, kind, status: 'draft',
        source, proposal, revision: 1, createdAt: now, updatedAt: now });
    }, { signal });
    const entry = document.entries.find(entry => entry.id === proposalId || entry.id === existingId);
    return { document, created: reason === 'created', reason, entry };
  }

  _proposalTarget(document, target) {
    if (target.scope !== document.scope || target.scopeId !== document.scopeId)
      throw memoryFailure('目标不能跨越记忆作用域。', 'MEMORY_SCOPE_CHANGED', 409);
    const entry = document.entries.find(entry => entry.id === target.id);
    if (!entry || entry.status !== 'confirmed' || entry.revision !== target.entryRevision || memoryEntryIdentity(entry) !== target.identity)
      throw memoryFailure('目标记忆的内容、来源或版本已变化，请重新读取。', 'MEMORY_TARGET_CHANGED', 409);
    return entry;
  }

  create(conversationId, input) {
    return this._create(input, (scope, expected, operation) => this.mutate(conversationId, scope, expected, operation));
  }

  createScope(scope, scopeId, input) {
    return this._create({ ...input, scope }, (_, expected, operation) => this.mutateScope(scope, scopeId, expected, operation));
  }

  _create(input, mutate) {
    if (input.candidate !== undefined || input.proposal !== undefined || input.status !== undefined && input.status !== 'confirmed')
      throw memoryFailure('自动候选不能通过手动创建入口伪造。');
    const scope = memoryScope(input.scope), content = memoryContent(input.content), kind = memoryKind(input.kind, scope);
    const source = validateMemorySource(input.source);
    return mutate(scope, expectedMemoryRevision(input.expectedRevision), document => {
      // A failed/retried model turn must not create the same explicit memory twice.
      // 模型轮次失败或重试时，不得重复创建同一条显式记忆。
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

  update(conversationId, memoryIdValue, input, validateSource) {
    return this._update(memoryIdValue, input, (scope, expected, operation) => this.mutate(conversationId, scope, expected, operation), validateSource);
  }

  updateScope(scope, scopeId, memoryIdValue, input, validateSource) {
    return this._update(memoryIdValue, { ...input, scope }, (_, expected, operation) => this.mutateScope(scope, scopeId, expected, operation), validateSource);
  }

  _update(memoryIdValue, input, mutate, validateSource) {
    validateMemoryUpdateInput(input);
    const id = memoryId(memoryIdValue), scope = memoryScope(input.scope);
    const expected = expectedMemoryRevision(input.expectedRevision, true);
    const content = input.content === undefined ? undefined : memoryContent(input.content);
    const kind = input.kind === undefined ? undefined : memoryKind(input.kind, scope);
    const status = input.status === undefined ? undefined : memoryStatus(input.status);
    if (content === undefined && kind === undefined && status === undefined) throw memoryFailure('请提供要更新的记忆内容、类型或确认状态。');
    return mutate(scope, expected, async (document, storage) => {
      const entry = document.entries.find(item => item.id === id);
      if (!entry) throw memoryFailure('记忆不存在。', 'MEMORY_NOT_FOUND', 404);
      if (status === 'draft' && entry.status === 'confirmed') throw memoryFailure('已确认记忆不能退回自动草稿。');
      if (status === 'confirmed' && entry.status === 'draft') {
        requireMemoryProposalConfirmation(entry, input.proposalAction);
        if (typeof validateSource !== 'function') throw memoryFailure('确认须经记忆服务核验来源。');
        await validateSource(entry, storage);
        if (entry.proposal?.target) {
          const target = this._proposalTarget(document, entry.proposal.target);
          this._recordDismissal(document, entry);
          this._recordDismissal(document, target);
          if (entry.proposal.action === 'update') {
            target.content = content ?? entry.content; target.kind = kind ?? entry.kind;
            target.source = structuredClone(entry.source);
            delete target.candidate;
            target.proposal = { ...structuredClone(entry.proposal), appliedAt: new Date().toISOString() };
            target.revision++; target.updatedAt = target.proposal.appliedAt;
          } else {
            document.entries.splice(document.entries.indexOf(target), 1);
          }
          document.entries.splice(document.entries.indexOf(entry), 1);
          return;
        }
      }
      if (content === undefined && kind === undefined && status === entry.status) return false;
      if (content !== undefined) entry.content = content;
      if (kind !== undefined) entry.kind = kind;
      if (status !== undefined) entry.status = status;
      if (entry.proposal && entry.status === 'draft')
        entry.proposal.fingerprint = memoryProposalFingerprint(scope, document.scopeId, entry.content, entry.kind, entry.proposal);
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
      this._recordDismissal(document, entry);
      document.entries.splice(index, 1);
    });
  }

  _recordDismissal(document, entry) {
    if (entry.source.type !== 'user-message') return;
    const deletedAt = new Date().toISOString();
    const quotes = entry.candidate?.quotes ?? entry.proposal?.quotes ?? [];
    for (const messageId of new Set([entry.source.messageId, ...quotes.map(quote => quote.messageId)])) {
      if (document.dismissedSources.some(source => source.conversationId === entry.source.conversationId &&
          source.messageId === messageId && source.candidateFingerprint === entry.candidate?.fingerprint &&
          source.proposalFingerprint === entry.proposal?.fingerprint)) continue;
      document.dismissedSources.push({ conversationId: entry.source.conversationId, messageId, deletedAt,
        ...(entry.candidate ? { candidateFingerprint: entry.candidate.fingerprint } : {}),
        ...(entry.proposal ? { proposalFingerprint: entry.proposal.fingerprint } : {}),
        ...(entry.candidate?.batchThroughMessageId ? { candidateBatchThroughMessageId: entry.candidate.batchThroughMessageId } : {}) });
    }
  }

  _withSummary(conversationId, operation) {
    return this.conversations.withConversationStorage(memoryId(conversationId), (relationship, storage) => {
      const file = join(relationship.sessionDirectory, 'context.json');
      return this._run(file, () => operation(file, storage));
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
          // 摘要是可重建视图；清除正式文件前先保留损坏内容的精确原始字节。
          await this._preserveCorruptSummary(file, original);
          return null;
        }
        throw error;
      }
      // v1 was request-independent first/last excerpts. Leave it intact until a v2
      // projection is rebuilt from authoritative history; never use its cached text.
      // v1 摘录与请求无关；从正式历史重建 v2 之前保留旧文件，但不使用其缓存文本。
      if (value?.schemaVersion === 1) return null;
      if (!value || ![2, 3].includes(value.schemaVersion))
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
    // 备份或移除失败必须向调用方传播，不得在未保存原数据的情况下重建。
    await unlink(file);
  }

  async writeSummary(conversationId, summary, { validateCurrent, signal } = {}) {
    signal?.throwIfAborted();
    if (!summary || ![2, 3].includes(summary.schemaVersion) || memoryId(summary.conversationId) !== memoryId(conversationId))
      throw memoryFailure('聊天摘要版本或归属无效。');
    if (summary.schemaVersion === 3 && typeof validateCurrent !== 'function')
      throw memoryFailure('语义摘要须在正式历史锁内复验。', 'SUMMARY_VALIDATION_REQUIRED', 409);
    if (Buffer.byteLength(JSON.stringify(summary, null, 2)) > MAX_MEMORY_FILE_BYTES) throw memoryFailure('聊天摘要过大。');
    return this._withSummary(conversationId, async (file, storage) => {
      signal?.throwIfAborted();
      // An unsupported future document must never be replaced by an older application.
      // 旧应用不得替换尚不支持的未来版本文档。
      if (await this._safe(file, { file: true })) {
        let previous;
        try { previous = JSON.parse((await readFile(file, 'utf8')).replace(/^\uFEFF/, '')); }
        catch (error) {
          if (error instanceof SyntaxError) throw memoryFailure('聊天摘要格式无效，原文件已保留。', 'CORRUPT_SUMMARY', 500);
          throw error;
        }
        if (![1, 2, 3].includes(previous?.schemaVersion))
          throw memoryFailure('聊天摘要版本不受当前程序支持，原文件已保留。', 'UNSUPPORTED_SUMMARY_VERSION', 409);
      }
      // The protected read and replacement share the conversation lock; appended content is never covered by a stale plan.
      // 受保护读取与替换共用聊天锁，旧计划不会覆盖生成期间新增或修改的内容。
      if (validateCurrent && !await validateCurrent(await storage.readModelMessages(conversationId)))
        throw memoryFailure('摘要来源已变化，保留已有摘要及原文。', 'SUMMARY_SOURCE_CHANGED', 409);
      await this._safe(dirname(file), { create: true });
      await atomicJson(file, summary, { signal });
      return structuredClone(summary);
    });
  }
}

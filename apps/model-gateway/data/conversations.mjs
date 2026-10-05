import { validateId } from '../platform/conversation-id.mjs';
import { createHash, randomUUID } from 'node:crypto';
import { appendFile, copyFile, mkdir, readFile, readdir, rename, stat, truncate, unlink, writeFile } from 'node:fs/promises';
import { basename, dirname, join, resolve } from 'node:path';
import { atomicJson, readJson } from '../platform/atomic-json.mjs';
import { ensureDataLayout, inspectDataLayout } from './data-layout.mjs';
import { conversationDataRoot } from './storage.mjs';
import { validateAssistantSegments } from '../platform/assistant-segments.mjs';
import { validateModelTranscript, publicConversationMessage } from '../platform/model-transcript.mjs';
import { storedReplyDurationMs } from '../platform/reply-timing.mjs';

const clone = value => structuredClone(value);
const key = id => validateId(id).toLowerCase();
const has = (value, property) => Object.prototype.hasOwnProperty.call(value, property);

function failure(message, code = 'INVALID_CONVERSATION_DATA', statusCode = 400) {
  return Object.assign(new Error(message), { code, statusCode });
}

export { validateId } from '../platform/conversation-id.mjs';

function record(value, label) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw failure(`${label}格式无效，原文件已保留。`);
  return value;
}

function text(value, fallback = '') {
  if (value === undefined || value === null) return fallback;
  if (typeof value !== 'string') throw failure('会话文本格式无效，原文件已保留。');
  return value;
}

function message(value, { legacy = false, seed = '' } = {}) {
  if (legacy && typeof value === 'string') value = { Role: 'user', Content: value };
  record(value, '消息');
  const id = validateId(value.Id ?? (legacy ? createHash('sha256').update(seed).digest('hex').slice(0, 32) : undefined));
  if (!['user', 'assistant', 'system', 'tool'].includes(value.Role)) throw failure('消息角色格式无效，原文件已保留。');
  if (value.ModelTranscript !== undefined && value.Role !== 'assistant') throw failure('模型转录只能由助手请求记录。', 'INVALID_MODEL_TRANSCRIPT');
  const createdAt = value.CreatedAt ?? new Date(0).toISOString();
  if (typeof createdAt !== 'string' || !Number.isFinite(Date.parse(createdAt))) throw failure('消息时间格式无效。');
  const duration = value.ReasoningDurationMs ?? 0;
  if (!Number.isFinite(duration) || duration < 0) throw failure('思考耗时格式无效。');
  return { ...value, Id: id, Role: value.Role, Content: text(value.Content), Reasoning: text(value.Reasoning),
    Status: text(value.Status, 'completed'), Error: text(value.Error), Provider: text(value.Provider),
    Model: text(value.Model), ReasoningDurationMs: duration, DurationMs: storedReplyDurationMs(value), CreatedAt: createdAt,
    ...(value.ModelTranscript === undefined ? {} : { ModelTranscript: validateModelTranscript(value.ModelTranscript) }),
    ...(value.AssistantSegments === undefined ? {} : { AssistantSegments: validateAssistantSegments(value.AssistantSegments) }) };
}

function chat(value, options = {}) {
  record(value, '聊天'); validateId(value.Id);
  if (!text(value.Title).trim()) throw failure('聊天标题不能为空。');
  if (value.Messages !== undefined && !Array.isArray(value.Messages)) throw failure('聊天消息列表格式无效。');
  const messages = (value.Messages ?? []).map((item, index) => message(item, { ...options, seed: `${value.Id}:${index}` }));
  if (new Set(messages.map(item => key(item.Id))).size !== messages.length) throw failure('聊天包含重复消息 ID。');
  return { ...value, Id: validateId(value.Id), Title: value.Title, Draft: text(value.Draft), IsSample: Boolean(value.IsSample),
    IsPinned: Boolean(value.IsPinned), IsArchived: Boolean(value.IsArchived), Messages: messages };
}

function project(value, options = {}) {
  record(value, '项目'); validateId(value.Id);
  if (!text(value.Name).trim() || !Array.isArray(value.Chats)) throw failure('项目列表格式无效。');
  return { ...value, Id: validateId(value.Id), Name: value.Name, FolderPath: value.FolderPath == null ? null : text(value.FolderPath),
    IsPinned: Boolean(value.IsPinned), IsArchived: Boolean(value.IsArchived),
    IsFolderlessWorkspace: Boolean(value.IsFolderlessWorkspace), Chats: value.Chats.map(item => chat(item, options)) };
}

function metadata(chatValue) {
  const { Messages, ...other } = chatValue;
  return other;
}

function locations(document) {
  return [...document.Chats.map(Chat => ({ Id: Chat.Id, ProjectId: null, Chat })),
    ...document.Projects.flatMap(Project => Project.Chats.map(Chat => ({ Id: Chat.Id, ProjectId: Project.Id, Chat })))];
}

function validateUnique(document) {
  if (new Set(document.Projects.map(item => key(item.Id))).size !== document.Projects.length)
    throw failure('项目包含重复 ID。');
  const chats = locations(document);
  if (new Set(chats.map(item => key(item.Id))).size !== chats.length) throw failure('项目和普通聊天之间存在重复聊天 ID。');
}

function validateDocument(input) {
  record(input, '会话目录');
  if (input.Version !== 1 || !Number.isSafeInteger(input.Revision) || input.Revision < 0 ||
      !Array.isArray(input.Projects) || !Array.isArray(input.Chats) || !Array.isArray(input.Tombstones))
    throw failure('会话目录格式无效，原文件已保留。');
  const document = { ...input, Projects: input.Projects.map(item => project(item)), Chats: input.Chats.map(item => chat(item)) };
  for (const item of document.Tombstones) { record(item, '已删除聊天'); validateId(item.Id); }
  validateUnique(document);
  const active = new Set(locations(document).map(item => key(item.Id)));
  if (document.Tombstones.some(item => active.has(key(item.Id)))) throw failure('聊天删除状态冲突。');
  // Transcript bodies never belong in the catalog, including after restart.
  // 聊天正文不进入目录元数据，重启后也维持这一约定。
  document.Projects = document.Projects.map(item => ({ ...item, Chats: item.Chats.map(metadata) }));
  document.Chats = document.Chats.map(metadata);
  return document;
}

async function exists(path) {
  try { await stat(path); return true; } catch (error) { if (error.code === 'ENOENT') return false; throw error; }
}

/**
 * One owner for both sidebar metadata and model context; model names never determine paths.
 * 侧栏元数据和模型上下文共享同一正式存储所有者，模型名称不决定存储路径。
 */
export class ConversationStore {
  constructor({ dataHome, root, legacyDesktopDirectory = process.env.KYNXA_LEGACY_DESKTOP_HOME } = {}) {
    if (!dataHome) throw failure('缺少模型数据目录。');
    this.dataHome = resolve(dataHome);
    const standardLayout = basename(this.dataHome).toLowerCase() === 'models';
    this.root = resolve(root ?? conversationDataRoot(this.dataHome));
    this.legacyDesktopDirectory = legacyDesktopDirectory === null ? null :
      (legacyDesktopDirectory ? resolve(legacyDesktopDirectory) : standardLayout ? join(this.root, 'Desktop') : null);
    this.catalogPath = join(this.root, 'catalog.json');
    this.transactionPath = join(this.root, '.catalog-transaction.json');
    this.markerPath = join(this.root, '.conversations-v1.json');
    this.queue = Promise.resolve();
    this.initializing = null;
    this.pendingTransaction = false;
    this.logs = new Map();
  }

  initialize() {
    if (!this.initializing) this.initializing = this._initialize().catch(error => { this.initializing = null; throw error; });
    return this.initializing;
  }

  async _initialize() {
    await inspectDataLayout(this.root);
    await mkdir(this.root, { recursive: true, mode: 0o700 });
    const transaction = await readJson(this.transactionPath, null);
    if (transaction) await this._apply(transaction);
    const current = await readJson(this.catalogPath, null);
    if (current) this.document = validateDocument(current);
    else await this._migrate();
    await inspectDataLayout(this.root, this.document);
    // Recover only when this process opens the store, never during an active stream.
    // 只在当前进程打开存储时恢复记录，不在正在进行的流中恢复。
    for (const location of locations(this.document)) {
      const messages = await this._readLog(location);
      for (const item of messages) if (item.Status === 'streaming')
        await this._append(location, { ...item, Status: 'interrupted' });
    }
    await ensureDataLayout(this.root, this.document);
    if (!await exists(this.markerPath)) await atomicJson(this.markerPath,
      { Version: 1, CompletedAt: new Date().toISOString(), LegacyHistory: 'Preserved in Backups; desktop transcripts are authoritative.' });
    return this;
  }

  async _run(operation) {
    const result = this.queue.then(async () => {
      await this.initialize();
      if (this.pendingTransaction) await this._apply(await readJson(this.transactionPath, null));
      return operation();
    });
    this.queue = result.catch(() => {});
    return result;
  }

  _directory(location) {
    const id = key(location.Id);
    if (location.Trash) return join(this.root, 'Trash', id);
    return location.ProjectId == null ? join(this.root, 'Chats', id) :
      join(this.root, 'Projects', key(location.ProjectId), 'Sessions', id);
  }

  _find(id) {
    const identifier = key(id);
    return locations(this.document).find(item => key(item.Id) === identifier);
  }

  _notDeleted(id) {
    if (this.document.Tombstones.some(item => key(item.Id) === key(id)))
      throw failure('聊天已删除，不能继续写入。', 'CONVERSATION_DELETED', 410);
  }

  async _readLog(location) {
    const filename = join(this._directory(location), 'events.jsonl');
    let bytes, info;
    try {
      info = await stat(filename);
      const cached = this.logs.get(filename);
      if (cached && cached.size === info.size && cached.mtimeMs === info.mtimeMs && cached.ctimeMs === info.ctimeMs)
        return cached.messages;
      bytes = await readFile(filename);
    } catch (error) { if (error.code === 'ENOENT') { this.logs.delete(filename); return []; } throw error; }
    const source = bytes.toString('utf8');
    const endsWithNewline = source.endsWith('\n');
    const lines = source.split('\n');
    const messages = new Map();
    let validLength = 0;
    for (let index = 0; index < lines.length; index++) {
      const line = lines[index];
      if (index === lines.length - 1 && line === '') break;
      try {
        if (!line.trim()) throw failure('会话日志中有空白记录。');
        const event = JSON.parse(line);
        if (event.version !== 1 || event.type !== 'message.upsert') throw failure('会话事件格式无效。');
        const value = message(event.message);
        messages.set(key(value.Id), value);
        validLength += Buffer.byteLength(line + (index < lines.length - 1 ? '\n' : ''));
      } catch (error) {
        if (index !== lines.length - 1 || endsWithNewline)
          throw failure(`会话日志中间记录损坏，已停止读取并保留原文件：${location.Id}`, 'CORRUPT_CONVERSATION', 500);
        // A crash can leave one incomplete final append. Keep its exact bytes for diagnosis.
        // 崩溃可能留下未完成的最后一次追加；保留其原始字节供诊断。
        await writeFile(`${filename}.recovered-tail-${randomUUID()}`, bytes.subarray(validLength), { mode: 0o600, flag: 'wx' });
        await truncate(filename, validLength);
        return this._cacheLog(filename, [...messages.values()]);
      }
    }
    if (source && !endsWithNewline) await appendFile(filename, '\n', { mode: 0o600 });
    return this._cacheLog(filename, [...messages.values()]);
  }

  async _cacheLog(filename, messages) {
    const info = await stat(filename);
    this.logs.set(filename, { size: info.size, mtimeMs: info.mtimeMs, ctimeMs: info.ctimeMs, messages });
    return messages;
  }

  async _append(location, value) {
    const current = await this._readLog(location);
    const previous = current.find(item => key(item.Id) === key(value.Id));
    const normalized = message({ ...previous, ...value });
    if (previous?.Role && previous.Role !== normalized.Role) throw failure('消息 ID 已用于其他角色。');
    if (previous && JSON.stringify(previous) === JSON.stringify(normalized)) return;
    const folder = this._directory(location);
    await mkdir(folder, { recursive: true, mode: 0o700 });
    await appendFile(join(folder, 'events.jsonl'), JSON.stringify({ version: 1, type: 'message.upsert', message: normalized }) + '\n',
      { encoding: 'utf8', mode: 0o600 });
    const updated = [...current], index = updated.findIndex(item => key(item.Id) === key(normalized.Id));
    if (index === -1) updated.push(normalized); else updated[index] = normalized;
    await this._cacheLog(join(folder, 'events.jsonl'), updated);
  }

  async _fullCatalog() {
    const result = { Revision: this.document.Revision, Projects: clone(this.document.Projects), Chats: clone(this.document.Chats) };
    for (const location of locations(result)) location.Chat.Messages = clone((await this._readLog(location)).map(publicConversationMessage));
    return result;
  }

  catalog() { return this._run(() => this._fullCatalog()); }

  readMessages(conversationId) {
    return this._readMessages(conversationId, false);
  }

  /**
   * Gateway-only model projection source. Public APIs always use readMessages/catalog.
   * 仅供网关生成模型视图，公开接口始终使用 readMessages 或目录数据。
   */
  readModelMessages(conversationId) {
    return this._readMessages(conversationId, true);
  }

  _readMessages(conversationId, includeModelTranscript) {
    return this._run(async () => {
      this._notDeleted(conversationId);
      const location = this._find(conversationId);
      if (!location) return [];
      const messages = await this._readLog(location);
      return clone(includeModelTranscript ? messages : messages.map(publicConversationMessage));
    });
  }

  // Memory ownership always follows the canonical catalog, never a caller's path.
  // 记忆归属始终依据正式目录，不能依赖调用方传入的路径。
  describeConversation(conversationId) {
    return this._run(() => {
      this._notDeleted(conversationId);
      const location = this._find(conversationId);
      if (!location) throw failure('聊天不存在。', 'CONVERSATION_NOT_FOUND', 404);
      const owner = location.ProjectId == null ? null :
        this.document.Projects.find(project => key(project.Id) === key(location.ProjectId));
      return { conversationId: location.Id, projectId: owner?.Id ?? null,
        projectName: owner?.Name ?? null, isFolderlessWorkspace: Boolean(owner?.IsFolderlessWorkspace),
        isArchived: Boolean(location.Chat.IsArchived), projectArchived: Boolean(owner?.IsArchived) };
    });
  }

  _projectRelationship(projectId) {
    const project = this.document.Projects.find(value => key(value.Id) === key(projectId));
    if (!project) throw failure('工作不存在。', 'PROJECT_NOT_FOUND', 404);
    return { projectId: project.Id, name: project.Name,
      isFolderlessWorkspace: Boolean(project.IsFolderlessWorkspace), isArchived: Boolean(project.IsArchived) };
  }

  describeProject(projectId) { return this._run(() => this._projectRelationship(projectId)); }

  /**
   * Scope IO holds the same catalog guard as session IO, including work removal and global memory initialization.
   * 范围 I/O 与会话 I/O 使用同一目录保护，包括工作删除和全局记忆初始化。
   */
  withCatalogStorage(operation) { return this._run(operation); }

  withProjectStorage(projectId, operation) {
    return this._run(() => operation(this._projectRelationship(projectId)));
  }

  resolveSessionDirectory(conversationId) {
    return this._run(async () => {
      this._notDeleted(conversationId);
      const location = this._find(conversationId);
      if (!location) throw failure('聊天不存在。', 'CONVERSATION_NOT_FOUND', 404);
      await inspectDataLayout(this.root, this.document);
      return this._directory(location);
    });
  }

  /**
   * Keep current ownership and dependent session IO atomic with catalog moves/deletions.
   * 保持当前归属及其依赖的会话 I/O 与目录移动、删除互斥。
   */
  withConversationStorage(conversationId, operation) {
    return this._run(() => {
      this._notDeleted(conversationId);
      const location = this._find(conversationId);
      if (!location) throw failure('聊天不存在。', 'CONVERSATION_NOT_FOUND', 404);
      const owner = location.ProjectId == null ? null :
        this.document.Projects.find(project => key(project.Id) === key(location.ProjectId));
      // The callback uses these app-owned paths under this queue, and must not reenter queued store methods.
      // 回调只在当前队列保护下使用应用拥有的路径，不得重入已排队的存储方法。
      return operation({ conversationId: location.Id, projectId: owner?.Id ?? null,
        projectName: owner?.Name ?? null, isFolderlessWorkspace: Boolean(owner?.IsFolderlessWorkspace),
        isArchived: Boolean(location.Chat.IsArchived), projectArchived: Boolean(owner?.IsArchived),
        sessionDirectory: this._directory(location) });
    });
  }

  relationships(conversationId) {
    return this._run(() => {
      this._notDeleted(conversationId);
      const location = this._find(conversationId);
      if (!location) throw failure('聊天不存在。', 'CONVERSATION_NOT_FOUND', 404);
      const owner = location.ProjectId == null ? null :
        this.document.Projects.find(project => key(project.Id) === key(location.ProjectId));
      const sharedWork = owner && !owner.IsFolderlessWorkspace && !owner.IsArchived;
      return { conversationId: location.Id, projectId: owner?.Id ?? null,
        projectName: owner?.Name ?? null, isFolderlessWorkspace: Boolean(owner?.IsFolderlessWorkspace),
        relatedConversations: sharedWork ? owner.Chats.filter(chat => key(chat.Id) !== key(location.Id))
          .map(chat => ({ id: chat.Id, title: chat.Title, isArchived: Boolean(chat.IsArchived) })) : [],
        memoryScopes: sharedWork ? ['chat', 'project', 'user'] : ['chat', 'user'] };
    });
  }

  ensureConversation(conversationId, { title = '新聊天' } = {}) {
    return this._run(async () => {
      validateId(conversationId); this._notDeleted(conversationId);
      if (this._find(conversationId)) return;
      const next = clone(this.document);
      next.Chats.push(metadata(chat({ Id: conversationId, Title: title, Messages: [] })));
      next.Revision++;
      await this._commit(next);
    });
  }

  upsertMessage(conversationId, input) {
    return this._run(async () => {
      this._notDeleted(conversationId);
      const location = this._find(conversationId);
      if (!location) throw failure('聊天不存在，请先创建聊天。', 'CONVERSATION_NOT_FOUND', 404);
      await this._append(location, input);
    });
  }

  saveCatalog(input) {
    return this._run(async () => {
      record(input, '会话目录');
      if (input.Revision !== this.document.Revision)
        throw failure('聊天目录已更新，请重新读取后重试。', 'CATALOG_CONFLICT', 409);
      const next = clone(this.document);
      if (has(input, 'Projects')) {
        if (!Array.isArray(input.Projects)) throw failure('项目列表格式无效。');
        next.Projects = input.Projects.map(value => project(value));
      }
      if (has(input, 'Chats')) {
        if (!Array.isArray(input.Chats)) throw failure('聊天列表格式无效。');
        next.Chats = input.Chats.map(value => chat(value));
      }
      validateUnique(next);
      const previous = new Map(locations(this.document).map(location => [key(location.Id), location]));
      const moves = [], writes = [], retained = new Set();
      for (const location of locations(next)) {
        const identifier = key(location.Id), old = previous.get(identifier);
        const deleted = this.document.Tombstones.find(item => key(item.Id) === identifier);
        const source = old ?? (deleted ? { Id: location.Id, Trash: true } : location);
        const messages = old || deleted ? await this._readLog(source) : [];
        const newMessages = (location.Chat.Messages ?? []).filter(item => item.Role === 'user' &&
          !messages.some(saved => key(saved.Id) === key(item.Id)));
        if (!location.Chat.IsSample && !messages.length && !newMessages.length) continue;
        retained.add(identifier);
        if ((old || deleted) && this._directory(source) !== this._directory(location)) moves.push({ From: source, To: location });
        for (const item of newMessages) writes.push({ Location: location, Message: item });
      }
      next.Chats = next.Chats.filter(item => retained.has(key(item.Id))).map(metadata);
      next.Projects = next.Projects.map(item => ({ ...item,
        Chats: item.Chats.filter(value => retained.has(key(value.Id))).map(metadata) }));
      next.Tombstones = next.Tombstones.filter(item => !retained.has(key(item.Id)));
      for (const [identifier, location] of previous) if (!retained.has(identifier)) {
        moves.push({ From: location, To: { Id: location.Id, Trash: true } });
        next.Tombstones.push({ Id: location.Id, ProjectId: location.ProjectId, DeletedAt: new Date().toISOString() });
      }
      next.Revision++;
      await this._commit(next, moves, writes);
      return this._fullCatalog();
    });
  }

  async _commit(next, moves = [], writes = []) {
    await inspectDataLayout(this.root, next);
    // Persist intent before any cross-folder move, so a process exit can finish the same transaction.
    // 跨目录移动前先持久化事务意图，进程退出后才能继续完成同一事务。
    const slim = location => ({ Id: location.Id, ...(location.Trash ? { Trash: true } : { ProjectId: location.ProjectId ?? null }) });
    const transaction = { Version: 1, NextCatalog: validateDocument(next),
      Moves: moves.map(item => ({ From: slim(item.From), To: slim(item.To) })),
      Writes: writes.map(item => ({ Location: slim(item.Location), Message: message(item.Message) })) };
    await atomicJson(this.transactionPath, transaction);
    this.pendingTransaction = true;
    await this._apply(transaction);
  }

  async _apply(transaction) {
    if (transaction.Version !== 1 || !Array.isArray(transaction.Moves) || !Array.isArray(transaction.Writes))
      throw failure('会话存储恢复文件格式无效。');
    const next = validateDocument(transaction.NextCatalog);
    await inspectDataLayout(this.root, next);
    // Validate all transaction paths before touching files.
    // 实际操作文件前，验证所有事务路径。
    for (const item of transaction.Moves) { this._directory(item.From); this._directory(item.To); }
    for (const item of transaction.Writes) { this._directory(item.Location); message(item.Message); }
    for (const move of transaction.Moves) {
      const from = this._directory(move.From), to = this._directory(move.To);
      if (from === to || !await exists(from)) continue;
      if (await exists(to)) throw failure('聊天记录目标目录已存在，已保留两处数据。', 'STORAGE_CONFLICT', 500);
      await mkdir(dirname(to), { recursive: true, mode: 0o700 });
      await rename(from, to);
      this.logs.delete(join(from, 'events.jsonl'));
      this.logs.delete(join(to, 'events.jsonl'));
    }
    for (const item of transaction.Writes) await this._append(item.Location, item.Message);
    await atomicJson(this.catalogPath, next);
    this.document = next;
    await ensureDataLayout(this.root, next);
    await unlink(this.transactionPath).catch(error => { if (error.code !== 'ENOENT') throw error; });
    this.pendingTransaction = false;
  }

  async _migrate() {
    const legacyProjects = this.legacyDesktopDirectory ? await readJson(join(this.legacyDesktopDirectory, 'projects.json'), []) : [];
    const legacyChats = this.legacyDesktopDirectory ? await readJson(join(this.legacyDesktopDirectory, 'chats.json'), []) : [];
    if (!Array.isArray(legacyProjects) || !Array.isArray(legacyChats)) throw failure('旧版聊天目录格式无效，迁移未开始。');
    const imported = { Version: 1, Revision: 0, Projects: legacyProjects.map(value => project(value, { legacy: true })),
      Chats: legacyChats.map(value => chat(value, { legacy: true })), Tombstones: [] };
    validateUnique(imported);
    const keep = value => value.IsSample || value.Messages.length;
    imported.Chats = imported.Chats.filter(keep);
    imported.Projects = imported.Projects.map(value => ({ ...value, Chats: value.Chats.filter(keep) }));
    const writes = locations(imported).flatMap(location => location.Chat.Messages.map(value => ({ Location: location, Message: value })));
    // Backup exact original catalogs and ALL legacy model logs before committing the new store.
    // 提交新存储前，备份原目录及全部旧模型日志的精确原始内容。
    const backup = join(this.root, 'Backups', 'conversations-v1');
    if (this.legacyDesktopDirectory) for (const filename of ['projects.json', 'chats.json'])
      await this._backup(join(this.legacyDesktopDirectory, filename), join(backup, 'Desktop', filename));
    await this._backup(join(this.dataHome, 'sessions'), join(backup, 'Models', 'sessions'));
    imported.Projects = imported.Projects.map(value => ({ ...value, Chats: value.Chats.map(metadata) }));
    imported.Chats = imported.Chats.map(metadata);
    await this._commit(imported, [], writes);
  }

  async _backup(source, target) {
    let info;
    try { info = await stat(source); } catch (error) { if (error.code === 'ENOENT') return; throw error; }
    if (info.isDirectory()) {
      await mkdir(target, { recursive: true, mode: 0o700 });
      for (const entry of await readdir(source, { withFileTypes: true })) {
        if (entry.isSymbolicLink()) throw failure('旧版存储包含符号链接，请先检查后再迁移。');
        await this._backup(join(source, entry.name), join(target, entry.name));
      }
    } else if (info.isFile()) {
      await mkdir(dirname(target), { recursive: true, mode: 0o700 });
      if (await exists(target)) {
        if (!(await readFile(source)).equals(await readFile(target)))
          throw failure('旧版记录与迁移备份不一致，请保留两份数据后检查。', 'MIGRATION_CONFLICT', 500);
      } else {
        // An interrupted backup must not leave a partial file at the final backup path.
        // 备份中断不能在最终备份路径留下半份文件。
        const temporary = `${target}.${randomUUID()}.tmp`, fallback = `${target}.${randomUUID()}.tmp`;
        let completed = temporary;
        try {
          try { await copyFile(source, temporary); }
          catch (error) {
            // Some Windows filesystems reject copyFile across drives although ordinary reads work.
            // 某些 Windows 文件系统会拒绝跨盘 copyFile，即使普通读取可用。
            if (!['UNKNOWN', 'EXDEV', 'ENOSYS'].includes(error.code)) throw error;
            await writeFile(fallback, await readFile(source), { mode: 0o600, flag: 'wx' });
            completed = fallback;
          }
          if (!(await readFile(source)).equals(await readFile(completed)))
            throw failure('旧版记录备份校验失败，迁移未提交。', 'MIGRATION_CONFLICT', 500);
          await rename(completed, target);
        } finally {
          for (const filename of [temporary, fallback]) await unlink(filename).catch(error => { if (error.code !== 'ENOENT') throw error; });
        }
      }
    } else throw failure('旧版存储包含不支持的文件类型。');
  }
}

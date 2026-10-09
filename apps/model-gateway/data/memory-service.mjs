import { createHash } from 'node:crypto';
import { MemoryRepository } from './memory-repository.mjs';
import { explicitMemoryInstruction, memoryFailure, memoryId, memoryScope, validateMemorySource } from './memory-contracts.mjs';

const entryIdentity = entry => createHash('sha256').update(JSON.stringify([
  entry.id, entry.scope, entry.scopeId, entry.revision, entry.kind, entry.content, entry.source
])).digest('hex');

/**
 * Long-term memory is user-confirmed data, separate from transcripts and derived context summaries.
 * 长期记忆是用户确认的数据，与聊天日志和派生上下文摘要分开管理。
 */
export class MemoryService {
  constructor({ conversationStore, repository }) {
    this.conversations = conversationStore;
    this.repository = repository ?? new MemoryRepository({ conversationStore });
  }

  async listFor(conversationId) {
    const snapshot = await this.repository.readFor(conversationId);
    const scopes = snapshot.scopes;
    const sources = new Map();
    for (const scope of scopes) await this._withSourceStatus(scope, sources);
    return snapshot;
  }

  async _withSourceStatus(document, sources = new Map()) {
    document.entries = await Promise.all(document.entries.map(async entry =>
      ({ ...entry, ...await this._sourceStatus(entry, sources) })));
    return document;
  }

  listScope(scope, scopeId) {
    return this.repository.readScope(scope, scopeId).then(document => this._withSourceStatus(document));
  }

  _scopeInput(scope, input) {
    if (!input || typeof input !== 'object' || Array.isArray(input)) throw memoryFailure('记忆输入无效。');
    memoryScope(scope);
    if (scope === 'chat') throw memoryFailure('独立记忆管理仅支持工作和用户作用域。');
    if (input.scope !== undefined && input.scope !== scope) throw memoryFailure('记忆作用域须与接口一致。');
    return { ...input, scope };
  }

  async createScope(scope, scopeId, input) {
    input = this._scopeInput(scope, input);
    const source = validateMemorySource(input.source ?? { type: 'manual', role: 'user' });
    // Saved-message sources remain on the conversation endpoint, which verifies the canonical user message.
    // 已保存消息的来源仍由会话端点验证，确保引用正式用户消息。
    if (source.type !== 'manual') throw memoryFailure('独立记忆管理只能创建手动确认记忆。');
    return this._withSourceStatus(await this.repository.createScope(scope, scopeId, { ...input, source }));
  }

  async updateScope(scope, scopeId, memoryIdValue, input) {
    input = this._scopeInput(scope, input);
    return this._withSourceStatus(await this.repository.updateScope(scope, scopeId, memoryIdValue, input));
  }

  async deleteScope(scope, scopeId, memoryIdValue, input) {
    input = this._scopeInput(scope, input);
    return this._withSourceStatus(await this.repository.deleteScope(scope, scopeId, memoryIdValue, input));
  }

  async _sourceStatus(entry, sources) {
    if (entry.source.type === 'manual') return { active: true, sourceAvailable: true, sourceArchived: false };
    if (!sources.has(entry.source.conversationId)) {
      sources.set(entry.source.conversationId, (async () => {
        try {
          const relationship = await this.conversations.describeConversation(entry.source.conversationId);
          const messages = await this.conversations.readMessages(relationship.conversationId);
          return { ...relationship, userMessageIds: new Set(messages.filter(item => item.Role === 'user').map(item => memoryId(item.Id))) };
        } catch (error) {
          if ([404, 410].includes(error.statusCode) || ['CONVERSATION_NOT_FOUND', 'CONVERSATION_DELETED'].includes(error.code)) return null;
          throw error;
        }
      })());
    }
    const source = await sources.get(entry.source.conversationId);
    if (!source) return { active: false, sourceAvailable: false, sourceArchived: false };
    const sourceAvailable = source.userMessageIds.has(entry.source.messageId);
    const sameScope = entry.scope === 'project' ? Boolean(source.projectId && !source.isFolderlessWorkspace && memoryId(source.projectId) === entry.scopeId) :
      entry.scope === 'chat' ? memoryId(source.conversationId) === entry.scopeId : true;
    return { active: sourceAvailable && sameScope, sourceAvailable, sourceArchived: Boolean(source.isArchived) };
  }

  async contextFor(conversationId) {
    const visible = await this.listFor(conversationId);
    const entries = [];
    for (const scope of visible.scopes) for (const entry of scope.entries)
      if (entry.status === 'confirmed' && entry.active) entries.push(entry);
    return { conversationId: visible.conversationId, projectId: visible.projectId,
      isFolderlessWorkspace: visible.isFolderlessWorkspace, entries };
  }

  // Snapshot only projected records; an unrelated memory edit must not invalidate this answer.
  // 只对已投影条目取快照，无关记忆编辑不应使本次回答失效；原文不进入诊断。
  snapshotFor(context, includedIds = [], projection = []) {
    const selected = new Set(includedIds);
    return { conversationId: context.conversationId, projectId: context.projectId,
      isFolderlessWorkspace: context.isFolderlessWorkspace,
      entries: context.entries.filter(entry => selected.has(entry.id)).map(entry => ({ memoryId: entry.id,
        scope: entry.scope, scopeId: entry.scopeId, identity: entryIdentity(entry) })),
      projection: projection.filter(item => selected.has(item.memoryId)).map(item => ({ ...item })) };
  }

  async validateSnapshot(conversationId, snapshot) {
    if (!snapshot?.entries.length) return { current: true, checked: 0, invalidSources: [] };
    const current = await this.repository.readFor(conversationId);
    const selected = new Set(snapshot.entries.map(record => record.memoryId)), sources = new Map();
    current.entries = [];
    // Recheck the projected subset, not every historical source conversation on each model round.
    // 每轮只核对已投影条目的来源，不为未使用记忆重新读取全部历史聊天。
    for (const scope of current.scopes) {
      scope.entries = scope.entries.filter(entry => selected.has(entry.id));
      await this._withSourceStatus(scope, sources);
      current.entries.push(...scope.entries.filter(entry => entry.status === 'confirmed' && entry.active));
    }
    const wrongConversation = memoryId(conversationId) !== memoryId(snapshot.conversationId);
    const scopeChanged = (current.projectId ?? null) !== (snapshot.projectId ?? null) ||
      Boolean(current.isFolderlessWorkspace) !== Boolean(snapshot.isFolderlessWorkspace);
    const entries = new Map(current.entries.map(entry => [entry.id, entry]));
    const invalidSources = snapshot.entries.flatMap(record => {
      const entry = entries.get(record.memoryId);
      if (!wrongConversation && (record.scope !== 'project' || !scopeChanged) && entry && entryIdentity(entry) === record.identity) return [];
      return [{ memoryId: record.memoryId, sourceType: 'memory', scope: record.scope,
        code: wrongConversation || scopeChanged && record.scope === 'project' ? 'MEMORY_SCOPE_CHANGED' : entry ? 'MEMORY_SOURCE_CHANGED' : 'MEMORY_SOURCE_UNAVAILABLE',
        next: 'read-current-confirmed-memory-before-relying-on-this-record' }];
    });
    return { current: invalidSources.length === 0, checked: snapshot.entries.length, invalidSources,
      freshnessOnly: true, correctnessCertified: false };
  }

  async _sourceFor(conversationId, input) {
    const id = memoryId(conversationId);
    const source = validateMemorySource(input.source ?? { type: 'manual', role: 'user', conversationId: id });
    if (source.type === 'manual' && source.conversationId === undefined) source.conversationId = id;
    if (source.conversationId !== id) throw memoryFailure('记忆来源须属于当前聊天。');
    if (source.type === 'user-message') {
      const messages = await this.conversations.readMessages(id);
      if (!messages.some(item => item.Role === 'user' && memoryId(item.Id) === source.messageId))
        throw memoryFailure('记忆来源须为当前聊天中已保存的真实用户消息。');
    }
    return source;
  }

  async create(conversationId, input) {
    if (!input || typeof input !== 'object') throw memoryFailure('记忆输入无效。');
    memoryScope(input.scope);
    const source = await this._sourceFor(conversationId, input);
    return this.repository.create(conversationId, { ...input, source });
  }

  async update(conversationId, memoryIdValue, input) {
    if (!input || typeof input !== 'object') throw memoryFailure('记忆输入无效。');
    return this.repository.update(conversationId, memoryIdValue, input);
  }

  async delete(conversationId, memoryIdValue, input) {
    if (!input || typeof input !== 'object') throw memoryFailure('记忆输入无效。');
    return this.repository.delete(conversationId, memoryIdValue, input);
  }

  async captureExplicit(conversationId, userMessageId, message) {
    const relationship = await this.repository.relationship(conversationId);
    const instruction = explicitMemoryInstruction(message, Boolean(relationship.projectId && !relationship.isFolderlessWorkspace && !relationship.projectArchived));
    if (!instruction) return null;
    const id = memoryId(userMessageId), messages = await this.conversations.readMessages(conversationId);
    if (!messages.some(item => item.Role === 'user' && memoryId(item.Id) === id && item.Content === message))
      throw memoryFailure('只能从已保存的真实用户消息提取记忆。');
    const document = await this.create(conversationId, { ...instruction,
      source: { type: 'user-message', role: 'user', conversationId, messageId: id } });
    return document.entries.find(entry => entry.source.type === 'user-message' && entry.source.messageId === id &&
      entry.source.conversationId === memoryId(conversationId)) ?? null;
  }
}

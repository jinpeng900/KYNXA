import { MemoryRepository } from './memory-repository.mjs';
import { explicitMemoryInstruction, memoryFailure, memoryId, memoryScope, validateMemorySource } from './memory-contracts.mjs';

/** Long-term memory is user-confirmed data, separate from transcripts and derived context summaries. */
export class MemoryService {
  constructor({ conversationStore, repository }) {
    this.conversations = conversationStore;
    this.repository = repository ?? new MemoryRepository({ conversationStore });
  }

  async listFor(conversationId) {
    const snapshot = await this.repository.readFor(conversationId);
    const scopes = snapshot.scopes;
    const sources = new Map();
    for (const scope of scopes) scope.entries = await Promise.all(scope.entries.map(async entry =>
      ({ ...entry, ...await this._sourceStatus(entry, sources) })));
    return snapshot;
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

  async _sourceFor(conversationId, input) {
    const id = memoryId(conversationId);
    const source = validateMemorySource(input.source ?? { type: 'manual', role: 'user', conversationId: id });
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

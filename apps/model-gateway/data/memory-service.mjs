import { createHash } from 'node:crypto';
import { MemoryRepository } from './memory-repository.mjs';
import { explicitMemoryInstruction, memoryFailure, memoryId, memoryScope, memoryCandidateFingerprint, expectedMemoryRevision, validateMemorySource } from './memory-contracts.mjs';
import { memoryCandidateSettings, extractMemoryCandidates } from './memory-candidates.mjs';

const entryIdentity = entry => createHash('sha256').update(JSON.stringify([
  entry.id, entry.scope, entry.scopeId, entry.revision, entry.kind, entry.content, entry.source
])).digest('hex');

/**
 * Long-term memory is user-confirmed data, separate from transcripts and derived context summaries.
 * 长期记忆是用户确认的数据，与聊天日志和派生上下文摘要分开管理。
 */
export class MemoryService {
  constructor({ conversationStore, repository, candidateSettings }) {
    this.conversations = conversationStore;
    this.repository = repository ?? new MemoryRepository({ conversationStore });
    this.candidateSettings = memoryCandidateSettings(candidateSettings);
    this.candidateSettingsOverride = candidateSettings !== undefined;
    this.candidateSettingsRevision = 0;
    this.candidateTasks = new Map();
    this.lastCandidateResult = null;
  }

  onChange(listener) { return this.repository.onChange(listener); }

  configureCandidates(settings) {
    this.candidateSettings = memoryCandidateSettings(settings, this.candidateSettings);
    this.candidateSettingsOverride = true;
    return this.candidateStatus();
  }

  initializeCandidates() {
    if (!this.candidateInitialization) this.candidateInitialization = this.repository.readScope('user', 'user').then(document => {
      if (!this.candidateSettingsOverride && document.candidateSettings) this.candidateSettings = memoryCandidateSettings(document.candidateSettings);
      this.candidateSettingsRevision = document.candidateSettingsRevision ?? 0;
    }).catch(error => { this.candidateInitialization = null; throw error; });
    return this.candidateInitialization;
  }

  async updateCandidateSettings(input) {
    if (!input || typeof input !== 'object' || Array.isArray(input) || !Object.hasOwn(input, 'patch') || Object.keys(input).some(key => !['patch', 'expectedRevision'].includes(key)))
      throw memoryFailure('自动候选配置请求无效。');
    const expected = expectedMemoryRevision(input.expectedRevision, true);
    await this.initializeCandidates();
    const document = await this.repository.mutateScope('user', 'user', undefined, document => {
      if ((document.candidateSettingsRevision ?? 0) !== expected) throw memoryFailure('候选配置已更新，请重新读取。', 'MEMORY_CONFLICT', 409);
      document.candidateSettings = memoryCandidateSettings(input.patch, document.candidateSettings ?? this.candidateSettings);
      document.candidateSettingsRevision = expected + 1;
    });
    this.candidateSettings = document.candidateSettings;
    this.candidateSettingsRevision = document.candidateSettingsRevision;
    this.candidateSettingsOverride = false;
    return this.candidateStatus();
  }

  candidateStatus() {
    return { settings: { ...this.candidateSettings }, settingsRevision: this.candidateSettingsRevision, pendingTasks: this.candidateTasks.size,
      algorithm: 'conservative-extractive-v1', lastResult: this.lastCandidateResult ? { ...this.lastCandidateResult } : null,
      ...(this.repository.lastChangeError ? { changeNotificationError: { ...this.repository.lastChangeError } } : {}) };
  }

  /** Queue cheap extraction after formal persistence; draft candidates never enter the model context or RAG.
   * 正式消息保存后排队进行轻量提取；候选草稿不会进入模型上下文或 RAG。 */
  scheduleCandidates(conversationId, options = {}) {
    const id = memoryId(conversationId);
    if (!this.candidateSettings.enabled) return Promise.resolve({ created: 0, reason: 'disabled' });
    if (!this.candidateTasks.has(id) && this.candidateTasks.size >= 128)
      return Promise.resolve({ created: 0, reason: 'queue-capacity' });
    const previous = this.candidateTasks.get(id) ?? Promise.resolve();
    const task = previous.catch(() => {}).then(() => this._captureCandidates(id, options));
    this.candidateTasks.set(id, task);
    task.then(result => { this.lastCandidateResult = result; }, error => {
      this.lastCandidateResult = { created: 0, reason: 'failed', code: error.code ?? 'MEMORY_CANDIDATE_FAILED' };
    }).finally(() => { if (this.candidateTasks.get(id) === task) this.candidateTasks.delete(id); });
    return task;
  }

  async flushCandidates(conversationId) {
    const tasks = conversationId ? [this.candidateTasks.get(memoryId(conversationId))].filter(Boolean) : [...this.candidateTasks.values()];
    return Promise.allSettled(tasks);
  }

  async _captureCandidates(conversationId, options) {
    await this.initializeCandidates();
    const settings = { ...this.candidateSettings };
    if (!settings.enabled) return { created: 0, reason: 'disabled' };
    const snapshot = await this.repository.readFor(conversationId);
    const consumedMessageIds = new Set();
    for (const document of snapshot.scopes) {
      for (const entry of document.entries) if (entry.source.conversationId === conversationId) {
        if (entry.source.messageId) consumedMessageIds.add(entry.source.messageId);
        for (const quote of entry.candidate?.quotes ?? []) consumedMessageIds.add(quote.messageId);
      }
      for (const source of document.dismissedSources) if (source.conversationId === conversationId) consumedMessageIds.add(source.messageId);
    }
    const messages = await this.conversations.readMessages(conversationId);
    const checkpointIds = new Set(snapshot.scopes.filter(document => document.scope === 'chat').flatMap(document => [
      ...document.entries.map(entry => entry.candidate?.batchThroughMessageId),
      ...document.dismissedSources.map(source => source.candidateBatchThroughMessageId)].filter(Boolean)));
    const afterMessageId = messages.findLast(message => message.Role === 'user' && checkpointIds.has(memoryId(message.Id)))?.Id.toLowerCase();
    const confirmedContents = new Set(snapshot.scopes.flatMap(document => document.entries.filter(entry => entry.status === 'confirmed')
      .map(entry => memoryCandidateFingerprint('chat', entry.content))));
    const extracted = extractMemoryCandidates(messages, { ...options, settings, consumedMessageIds, afterMessageId,
      hasProject: Boolean(snapshot.projectId && !snapshot.isFolderlessWorkspace && snapshot.scopes.some(document => document.scope === 'project')) });
    let created = 0, deduplicated = 0;
    for (const input of extracted.candidates) {
      if (!this.candidateSettings.enabled) break;
      if (confirmedContents.has(memoryCandidateFingerprint('chat', input.content))) { deduplicated++; continue; }
      const scope = snapshot.scopes.find(document => document.scope === input.scope);
      if (!scope) continue;
      const source = await this._sourceFor(conversationId, { source: { type: 'user-message', role: 'user', conversationId,
        messageId: input.candidate.quotes.at(-1).messageId } });
      this._validateCandidateQuotes(messages, input.candidate);
      if (!this.candidateSettings.enabled) break;
      const result = await this.repository.createCandidate(conversationId, { ...input, scopeId: scope.scopeId, source });
      if (result.created) { created++; scope.entries.push(result.entry); }
      else deduplicated++;
    }
    return { created, deduplicated, reason: extracted.reason, completeTurns: extracted.completeTurns, estimatedTokens: extracted.estimatedTokens };
  }

  _validateCandidateQuotes(messages, candidate) {
    const users = new Map(messages.filter(message => message.Role === 'user').map(message => [memoryId(message.Id), message.Content]));
    if (candidate.quotes.some(quote => users.get(quote.messageId) !== quote.text))
      throw memoryFailure('候选只能引用正式保存的完整用户原话。');
    if (candidate.batchThroughMessageId && !users.has(candidate.batchThroughMessageId)) throw memoryFailure('完整轮次游标缺少真实用户消息。');
  }

  async _validateConfirmation(read, input, id) {
    if (input.status !== 'confirmed') return;
    const document = await read(), entry = document.entries.find(entry => entry.id === memoryId(id));
    if (entry?.status === 'draft' && !(await this._sourceStatus({ ...entry, status: 'confirmed' }, new Map())).active)
      throw memoryFailure('候选原话已变化、删除或移出此范围，不能确认。', 'MEMORY_SOURCE_UNAVAILABLE', 409);
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
    await this._validateConfirmation(() => this.repository.readScope(scope, scopeId), input, memoryIdValue);
    return this._withSourceStatus(await this.repository.updateScope(scope, scopeId, memoryIdValue, input));
  }

  async deleteScope(scope, scopeId, memoryIdValue, input) {
    input = this._scopeInput(scope, input);
    return this._withSourceStatus(await this.repository.deleteScope(scope, scopeId, memoryIdValue, input));
  }

  async _sourceStatus(entry, sources) {
    if (entry.source.type === 'manual') return { active: entry.status === 'confirmed', sourceAvailable: true, sourceArchived: false };
    if (!sources.has(entry.source.conversationId)) {
      sources.set(entry.source.conversationId, (async () => {
        try {
          const relationship = await this.conversations.describeConversation(entry.source.conversationId);
          const messages = await this.conversations.readMessages(relationship.conversationId);
          return { ...relationship, userMessages: new Map(messages.filter(item => item.Role === 'user').map(item => [memoryId(item.Id), item.Content])) };
        } catch (error) {
          if ([404, 410].includes(error.statusCode) || ['CONVERSATION_NOT_FOUND', 'CONVERSATION_DELETED'].includes(error.code)) return null;
          throw error;
        }
      })());
    }
    const source = await sources.get(entry.source.conversationId);
    if (!source) return { active: false, sourceAvailable: false, sourceArchived: false };
    const sourceAvailable = source.userMessages.has(entry.source.messageId) &&
      (entry.candidate?.quotes.every(quote => source.userMessages.get(quote.messageId) === quote.text) ?? true);
    const sameScope = entry.scope === 'project' ? Boolean(source.projectId && !source.isFolderlessWorkspace && memoryId(source.projectId) === entry.scopeId) :
      entry.scope === 'chat' ? memoryId(source.conversationId) === entry.scopeId : true;
    return { active: entry.status === 'confirmed' && sourceAvailable && sameScope, sourceAvailable, sourceArchived: Boolean(source.isArchived) };
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
    if (input.candidate !== undefined || input.status !== undefined && input.status !== 'confirmed')
      throw memoryFailure('自动候选由正式用户消息提取；手动创建只接受用户确认记忆。');
    const source = await this._sourceFor(conversationId, input);
    return this.repository.create(conversationId, { ...input, source });
  }

  async update(conversationId, memoryIdValue, input) {
    if (!input || typeof input !== 'object') throw memoryFailure('记忆输入无效。');
    await this._validateConfirmation(() => this.repository.read(conversationId, input.scope), input, memoryIdValue);
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

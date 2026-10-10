import { MemoryRepository } from './memory-repository.mjs';
import { explicitMemoryInstruction, memoryFailure, memoryId, memoryScope, memoryContent, memoryKind,
  memoryCandidateFingerprint, memoryEntryIdentity, memoryProposalFingerprint, MEMORY_PROPOSAL_ALGORITHM,
  MEMORY_PROPOSAL_ACTIONS, MAX_MEMORY_CONTENT, MAX_MEMORY_ENTRIES, MEMORY_SCOPES,
  expectedMemoryRevision, validateMemorySource, validateMemoryProposal,
  validateMemoryProposalTarget, validateMemoryUpdateInput, requireMemoryProposalConfirmation } from './memory-contracts.mjs';
import { memoryCandidateSettings, extractMemoryCandidates } from './memory-candidates.mjs';

const MAX_MODEL_MEMORY_OFFSET = MAX_MEMORY_ENTRIES * MEMORY_SCOPES.length;

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
        for (const quote of entry.candidate?.quotes ?? entry.proposal?.quotes ?? []) consumedMessageIds.add(quote.messageId);
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
    if (entry?.status === 'draft') requireMemoryProposalConfirmation(entry, input.proposalAction);
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

  async _withSourceStatus(document, sources = new Map(), { signal } = {}) {
    signal?.throwIfAborted();
    document.entries = await Promise.all(document.entries.map(async entry =>
      ({ ...entry, ...await this._sourceStatus(entry, sources, { signal }),
        ...(entry.status === 'draft' && entry.proposal?.target ? {
          proposalTargetCurrent: document.entries.some(target => target.status === 'confirmed' && target.id === entry.proposal.target.id &&
            target.revision === entry.proposal.target.entryRevision && memoryEntryIdentity(target) === entry.proposal.target.identity)
        } : {}) })));
    signal?.throwIfAborted();
    return document;
  }

  /** Select only scopes visible to this conversation; similarity provides candidates, never permission to merge.
   * 只读取当前聊天可见范围，相似度只能提供候选，不能代替合并授权与目标核验。 */
  async readForModel(conversationId, input = {}, { signal } = {}) {
    signal?.throwIfAborted();
    const currentConversationId = memoryId(conversationId);
    const canExposeEntryProvenance = entry => entry.source.conversationId === undefined ?
      entry.scope === 'chat' && entry.scopeId === currentConversationId : entry.source.conversationId === currentConversationId;
    if (!input || typeof input !== 'object' || Array.isArray(input) ||
        Object.keys(input).some(key => !['scopes', 'query', 'limit', 'offset'].includes(key))) throw memoryFailure('模型记忆查询无效。');
    const selectedScopes = input.scopes === undefined ? null : input.scopes;
    if (selectedScopes !== null && (!Array.isArray(selectedScopes) || !selectedScopes.length || selectedScopes.length > 3 ||
        new Set(selectedScopes).size !== selectedScopes.length)) throw memoryFailure('请选择有效且不重复的记忆范围。');
    selectedScopes?.forEach(memoryScope);
    const limit = input.limit ?? 20;
    if (!Number.isSafeInteger(limit) || limit < 1 || limit > 50) throw memoryFailure('模型记忆查询最多返回 50 条。');
    const offset = input.offset === undefined ? 0 : input.offset;
    if (!Number.isSafeInteger(offset) || offset < 0 || offset > MAX_MODEL_MEMORY_OFFSET)
      throw memoryFailure(`模型记忆查询 offset 须为 0–${MAX_MODEL_MEMORY_OFFSET} 的整数。`);
    const queryValue = input.query === undefined ? '' : input.query;
    if (typeof queryValue !== 'string' || queryValue.length > MAX_MEMORY_CONTENT || queryValue.includes('\0'))
      throw memoryFailure(`记忆查询须为最多 ${MAX_MEMORY_CONTENT} 个字符的字符串。`);
    const query = queryValue.normalize('NFKC').trim().toLowerCase();
    const visible = await this.repository.readFor(conversationId, selectedScopes ?? undefined), sources = new Map();
    signal?.throwIfAborted();
    for (const document of visible.scopes) {
      // A shared scope grants confirmed content, never another conversation's unconfirmed draft or original quotations.
      // 共享范围只授权已确认内容，不授权其他聊天的未确认草稿或原始引用。
      document.entries = document.entries.filter(entry => entry.status === 'confirmed' || canExposeEntryProvenance(entry));
      await this._withSourceStatus(document, sources, { signal });
    }
    signal?.throwIfAborted();
    const scopes = visible.scopes.filter(document => !selectedScopes || selectedScopes.includes(document.scope));
    const terms = query.match(/[\p{L}\p{N}_]+/gu) ?? [];
    const ranked = scopes.flatMap(document => document.entries.map(entry => {
      const content = entry.content.normalize('NFKC').toLowerCase();
      const score = !query ? 1 : content.includes(query) ? 2 : terms.filter(term => content.includes(term)).length / Math.max(terms.length, 1);
      return { entry, document, score };
    })).filter(item => item.score > 0).sort((left, right) => right.score - left.score || left.entry.id.localeCompare(right.entry.id) ||
      left.document.scope.localeCompare(right.document.scope) || left.document.scopeId.localeCompare(right.document.scopeId));
    const entries = ranked.slice(offset, offset + limit).map(({ entry, document }) => {
      const target = { id: entry.id, scope: document.scope, scopeId: document.scopeId,
        expectedRevision: document.revision, entryRevision: entry.revision, identity: memoryEntryIdentity(entry) };
      if (canExposeEntryProvenance(entry)) return { ...entry, target };
      const { source, candidate, proposal, ...sharedEntry } = entry;
      return { ...sharedEntry, ...(proposal ? { isInference: proposal.isInference } : {}), target };
    });
    const hasMore = offset + entries.length < ranked.length;
    return { conversationId: visible.conversationId, projectId: visible.projectId, isFolderlessWorkspace: visible.isFolderlessWorkspace,
      scopes: scopes.map(document => ({ scope: document.scope, scopeId: document.scopeId, revision: document.revision })),
      entries, offset, nextOffset: hasMore ? offset + entries.length : null, hasMore,
      matched: ranked.length, truncated: ranked.length > entries.length, correctnessCertified: false,
      candidateSelection: 'lexical-only-no-automatic-merge' };
  }

  /** This tool stores suggestions only; confirmation, correction and deletion remain revisioned user operations.
   * 工具只保存建议，确认、更改和删除仍由用户通过版本保护的操作执行。 */
  async propose(conversationId, input, { signal } = {}) {
    signal?.throwIfAborted();
    if (!input || typeof input !== 'object' || Array.isArray(input) ||
        Object.keys(input).some(key => !['action', 'scope', 'scopeId', 'content', 'kind', 'quotes', 'reason', 'isInference', 'conflicts', 'target'].includes(key)) ||
        !MEMORY_PROPOSAL_ACTIONS.includes(input.action)) throw memoryFailure('模型记忆建议动作无效。');
    if (input.action === 'noop') {
      await this.repository.relationship(conversationId);
      signal?.throwIfAborted();
      return { action: 'noop', created: false, reason: 'no-change', requiresConfirmation: false, correctnessCertified: false };
    }
    await this.initializeCandidates();
    signal?.throwIfAborted();
    if (!this.candidateSettings.enabled) return { action: input.action, created: false, reason: 'disabled', requiresConfirmation: false };
    const scope = memoryScope(input.scope ?? input.target?.scope ?? 'chat');
    const snapshot = await this.repository.readFor(conversationId, [scope]), document = snapshot.scopes.find(document => document.scope === scope);
    signal?.throwIfAborted();
    if (!document || input.scopeId !== undefined && input.scopeId !== document.scopeId)
      throw memoryFailure('模型建议只能归属当前聊天可见的记忆范围。', 'MEMORY_SCOPE_CHANGED', 409);
    const target = input.target === undefined ? undefined : validateMemoryProposalTarget(input.target);
    if (target && (target.scope !== scope || target.scopeId !== document.scopeId))
      throw memoryFailure('目标不能跨越记忆作用域。', 'MEMORY_SCOPE_CHANGED', 409);
    const targetEntry = target ? document.entries.find(entry => entry.id === target.id) : undefined;
    const content = input.action === 'delete' && input.content === undefined ? memoryContent(targetEntry?.content) : memoryContent(input.content);
    const kind = memoryKind(input.kind ?? targetEntry?.kind, scope);
    const source = validateMemorySource({ type: 'user-message', role: 'user', conversationId,
      messageId: Array.isArray(input.quotes) ? input.quotes.at(-1)?.messageId : undefined });
    const partial = { algorithm: MEMORY_PROPOSAL_ALGORITHM, action: input.action, quotes: input.quotes,
      reason: input.reason, isInference: input.isInference, conflicts: input.conflicts ?? [], ...(target ? { target } : {}) };
    const proposal = validateMemoryProposal({ ...partial, fingerprint: '0'.repeat(64) }, source);
    proposal.fingerprint = memoryProposalFingerprint(scope, document.scopeId, content, kind, proposal);
    const result = await this.repository.createProposal(conversationId, { scope, scopeId: document.scopeId, content, kind, source, proposal },
      (entry, storage) => this._validateStoredSource(entry, storage, { signal }), { signal });
    return { action: input.action, created: result.created, reason: result.reason, scope, scopeId: result.document.scopeId,
      revision: result.document.revision, entry: result.entry ?? null,
      requiresConfirmation: result.entry?.status === 'draft', sourceVerified: true, correctnessCertified: false };
  }

  async _validateStoredSource(entry, storage, { signal } = {}) {
    signal?.throwIfAborted();
    if (entry.source.type === 'manual') return;
    if (!storage?.readConversation) throw memoryFailure('缺少受保护的记忆来源核验。');
    let source;
    try { source = await storage.readConversation(entry.source.conversationId); }
    catch (error) {
      signal?.throwIfAborted();
      if ([404, 410].includes(error.statusCode) || ['CONVERSATION_NOT_FOUND', 'CONVERSATION_DELETED'].includes(error.code))
        throw memoryFailure('候选原话已删除，不能保存或确认。', 'MEMORY_SOURCE_UNAVAILABLE', 409);
      throw error;
    }
    signal?.throwIfAborted();
    const userMessages = new Map(source.messages.filter(message => message.Role === 'user').map(message => [memoryId(message.Id), message.Content]));
    const quotes = entry.candidate?.quotes ?? entry.proposal?.quotes ?? [];
    const sameScope = entry.scope === 'project' ? Boolean(source.projectId && !source.isFolderlessWorkspace && memoryId(source.projectId) === entry.scopeId) :
      entry.scope === 'chat' ? memoryId(source.conversationId) === entry.scopeId : true;
    if (!sameScope || !userMessages.has(entry.source.messageId) || quotes.some(quote => userMessages.get(quote.messageId) !== quote.text))
      throw memoryFailure('候选原话已变化、删除或移出此范围，不能保存或确认。', 'MEMORY_SOURCE_UNAVAILABLE', 409);
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
    validateMemoryUpdateInput(input);
    await this._validateConfirmation(() => this.repository.readScope(scope, scopeId), input, memoryIdValue);
    return this._withSourceStatus(await this.repository.updateScope(scope, scopeId, memoryIdValue, input,
      (entry, storage) => this._validateStoredSource(entry, storage)));
  }

  async deleteScope(scope, scopeId, memoryIdValue, input) {
    input = this._scopeInput(scope, input);
    return this._withSourceStatus(await this.repository.deleteScope(scope, scopeId, memoryIdValue, input));
  }

  async _sourceStatus(entry, sources, { signal } = {}) {
    signal?.throwIfAborted();
    if (entry.source.type === 'manual') return { active: entry.status === 'confirmed', sourceAvailable: true, sourceArchived: false };
    if (!sources.has(entry.source.conversationId)) {
      sources.set(entry.source.conversationId, (async () => {
        try {
          const relationship = await this.conversations.describeConversation(entry.source.conversationId);
          signal?.throwIfAborted();
          const messages = await this.conversations.readMessages(relationship.conversationId);
          signal?.throwIfAborted();
          return { ...relationship, userMessages: new Map(messages.filter(item => item.Role === 'user').map(item => [memoryId(item.Id), item.Content])) };
        } catch (error) {
          signal?.throwIfAborted();
          if ([404, 410].includes(error.statusCode) || ['CONVERSATION_NOT_FOUND', 'CONVERSATION_DELETED'].includes(error.code)) return null;
          throw error;
        }
      })());
    }
    const source = await sources.get(entry.source.conversationId);
    signal?.throwIfAborted();
    if (!source) return { active: false, sourceAvailable: false, sourceArchived: false };
    const sourceAvailable = source.userMessages.has(entry.source.messageId) &&
      ((entry.candidate?.quotes ?? entry.proposal?.quotes)?.every(quote => source.userMessages.get(quote.messageId) === quote.text) ?? true);
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
        scope: entry.scope, scopeId: entry.scopeId, identity: memoryEntryIdentity(entry) })),
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
      if (!wrongConversation && (record.scope !== 'project' || !scopeChanged) && entry && memoryEntryIdentity(entry) === record.identity) return [];
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
    if (input.candidate !== undefined || input.proposal !== undefined || input.status !== undefined && input.status !== 'confirmed')
      throw memoryFailure('自动候选由正式用户消息提取；手动创建只接受用户确认记忆。');
    const source = await this._sourceFor(conversationId, input);
    return this.repository.create(conversationId, { ...input, source });
  }

  async update(conversationId, memoryIdValue, input) {
    validateMemoryUpdateInput(input);
    await this._validateConfirmation(() => this.repository.read(conversationId, input.scope), input, memoryIdValue);
    return this.repository.update(conversationId, memoryIdValue, input, (entry, storage) => this._validateStoredSource(entry, storage));
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

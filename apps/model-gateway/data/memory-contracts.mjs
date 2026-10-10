import { createHash } from 'node:crypto';
import { validateId } from '../platform/conversation-id.mjs';

export const MEMORY_SCHEMA_VERSION = 1;
export const MEMORY_SCOPES = ['chat', 'project', 'user'];
export const MEMORY_KINDS = ['fact', 'preference', 'decision'];
export const MAX_MEMORY_CONTENT = 4000;
export const MAX_MEMORY_ENTRIES = 1000;
export const MEMORY_CANDIDATE_ALGORITHM = 'conservative-extractive-v1';
export const MEMORY_PROPOSAL_ALGORITHM = 'main-model-tool-v1';
export const MEMORY_PROPOSAL_ACTIONS = ['add', 'update', 'delete', 'noop'];
export const MEMORY_CANDIDATE_TRIGGERS = ['remember', 'correction', 'decision', 'task-complete', 'ordinary-batch'];
export const DEFAULT_MEMORY_CANDIDATE_SETTINGS = Object.freeze({ enabled: true, minTurns: 6, maxTurns: 12,
  minTokens: 2048, maxTokens: 4096, maxCandidates: 8 });

export function memoryFailure(message, code = 'INVALID_MEMORY', statusCode = 400) {
  return Object.assign(new Error(message), { code, statusCode });
}

export function memoryId(value) { return validateId(value).toLowerCase(); }

export function memoryScope(value) {
  if (!MEMORY_SCOPES.includes(value)) throw memoryFailure('记忆作用域无效。');
  return value;
}

export function memoryContent(value) {
  if (typeof value !== 'string' || !value.trim() || value.length > MAX_MEMORY_CONTENT || value.includes('\0'))
    throw memoryFailure(`记忆内容须为 1–${MAX_MEMORY_CONTENT} 个字符。`);
  return value.trim();
}

export function memoryKind(value, scope) {
  value ??= scope === 'user' ? 'preference' : 'fact';
  if (!MEMORY_KINDS.includes(value)) throw memoryFailure('记忆类型无效。');
  return value;
}

export function expectedMemoryRevision(value, required = false) {
  if (value === undefined && !required) return undefined;
  if (!Number.isSafeInteger(value) || value < 0) throw memoryFailure('请提供有效的记忆版本 expectedRevision。');
  return value;
}

export function memoryStatus(value = 'confirmed') {
  if (!['draft', 'confirmed'].includes(value)) throw memoryFailure('记忆状态无效。');
  return value;
}

export function memoryCandidateSettings(input = {}, previous = DEFAULT_MEMORY_CANDIDATE_SETTINGS) {
  if (!input || typeof input !== 'object' || Array.isArray(input) ||
      Object.keys(input).some(key => !Object.hasOwn(DEFAULT_MEMORY_CANDIDATE_SETTINGS, key)))
    throw memoryFailure('自动记忆候选设置无效。');
  const settings = { ...previous, ...input };
  if (typeof settings.enabled !== 'boolean' || !Number.isSafeInteger(settings.minTurns) || settings.minTurns < 6 ||
      !Number.isSafeInteger(settings.maxTurns) || settings.maxTurns > 12 || settings.maxTurns < settings.minTurns ||
      !Number.isSafeInteger(settings.minTokens) || settings.minTokens < 2048 ||
      !Number.isSafeInteger(settings.maxTokens) || settings.maxTokens > 4096 || settings.maxTokens < settings.minTokens ||
      !Number.isSafeInteger(settings.maxCandidates) || settings.maxCandidates < 1 || settings.maxCandidates > 8)
    throw memoryFailure('自动候选须使用 6–12 完整轮次、2K–4K 估算 token 和最多 8 个候选。');
  return settings;
}

export function memoryCandidateFingerprint(scope, content) {
  return createHash('sha256').update(JSON.stringify([scope, content.normalize('NFKC').trim().replace(/\s+/gu, ' ').toLowerCase()])).digest('hex');
}

/** Bind an edit to its complete stored identity, including source quotations and entry version.
 * 修改绑定完整存储身份，包含来源原话和条目版本，不能凭相似度替代身份校验。 */
export function memoryEntryIdentity(entry) {
  return createHash('sha256').update(JSON.stringify([
    entry.id, entry.scope, entry.scopeId, entry.revision, entry.kind, entry.content, entry.source,
    entry.candidate ?? null, entry.proposal ?? null
  ])).digest('hex');
}

export function memoryProposalFingerprint(scope, scopeId, content, kind, proposal) {
  return createHash('sha256').update(JSON.stringify([scope, scopeId, proposal.action, content, kind,
    proposal.quotes, proposal.target ?? null])).digest('hex');
}

export function validateMemoryProposalTarget(value) {
  if (!value || typeof value !== 'object' || Array.isArray(value) ||
      Object.keys(value).some(key => !['id', 'scope', 'scopeId', 'expectedRevision', 'entryRevision', 'identity'].includes(key)) ||
      !Number.isSafeInteger(value.entryRevision) || value.entryRevision < 1 || !/^[a-f0-9]{64}$/u.test(value.identity))
    throw memoryFailure('修改建议须绑定已读取的完整目标身份。');
  const scope = memoryScope(value.scope);
  if (scope === 'user' && value.scopeId !== 'user') throw memoryFailure('全局记忆范围身份无效。');
  return { id: memoryId(value.id), scope, scopeId: scope === 'user' ? 'user' : memoryId(value.scopeId),
    expectedRevision: expectedMemoryRevision(value.expectedRevision, true), entryRevision: value.entryRevision, identity: value.identity };
}

/** Model suggestions retain verified quotations and uncertainty; only the user can resolve their action.
 * 模型建议保留核验后的原话和不确定性，仅用户确认后才能执行对应动作。 */
export function validateMemoryProposal(value, source) {
  if (!value || typeof value !== 'object' || Array.isArray(value) ||
      Object.keys(value).some(key => !['algorithm', 'action', 'quotes', 'reason', 'isInference', 'conflicts', 'target', 'fingerprint', 'appliedAt'].includes(key)) ||
      value.algorithm !== MEMORY_PROPOSAL_ALGORITHM || !['add', 'update', 'delete'].includes(value.action) ||
      source.type !== 'user-message' || typeof value.isInference !== 'boolean' ||
      typeof value.reason !== 'string' || !value.reason.trim() || value.reason.length > 1000 || value.reason.includes('\0') ||
      !/^[a-f0-9]{64}$/u.test(value.fingerprint) || !Array.isArray(value.quotes) ||
      !value.quotes.length || value.quotes.length > 12 || !Array.isArray(value.conflicts) || value.conflicts.length > 8 ||
      value.conflicts.some(conflict => typeof conflict !== 'string' || !conflict.trim() || conflict.length > 500 || conflict.includes('\0')))
    throw memoryFailure('记忆动作建议的原话、理由或不确定性格式无效。');
  const ids = new Set();
  const quotes = value.quotes.map(quote => {
    if (!quote || typeof quote !== 'object' || Array.isArray(quote) ||
        Object.keys(quote).some(key => !['messageId', 'text'].includes(key)) || typeof quote.text !== 'string' ||
        !quote.text.trim() || quote.text.length > MAX_MEMORY_CONTENT || quote.text.includes('\0'))
      throw memoryFailure('记忆建议须引用完整、有界的用户原话。');
    const messageId = memoryId(quote.messageId);
    if (ids.has(messageId)) throw memoryFailure('记忆建议不能重复引用同一原话。');
    ids.add(messageId);
    return { messageId, text: quote.text };
  });
  if (!ids.has(source.messageId)) throw memoryFailure('建议主来源不属于原话记录。');
  const target = value.target === undefined ? undefined : validateMemoryProposalTarget(value.target);
  if ((value.action !== 'add') !== Boolean(target)) throw memoryFailure('更新或删除须指定目标；新增不能伪造目标。');
  if (value.appliedAt !== undefined && value.action !== 'update') throw memoryFailure('只有已确认更新保留执行时间。');
  return { algorithm: value.algorithm, action: value.action, quotes, reason: value.reason.trim(),
    isInference: value.isInference, conflicts: [...new Set(value.conflicts.map(conflict => conflict.trim()))],
    fingerprint: value.fingerprint, ...(target ? { target } : {}),
    ...(value.appliedAt === undefined ? {} : { appliedAt: timestamp(value.appliedAt) }) };
}

export function validateMemoryUpdateInput(input) {
  if (!input || typeof input !== 'object' || Array.isArray(input) ||
      Object.keys(input).some(key => !['scope', 'content', 'kind', 'status', 'expectedRevision', 'proposalAction'].includes(key)))
    throw memoryFailure('记忆更新输入无效。');
  return input;
}

/** Legacy content confirmation cannot authorize an undisplayed destructive proposal action.
 * 旧界面的正文确认不能授权尚未呈现的覆盖或删除动作，须明确提交提案动作。 */
export function requireMemoryProposalConfirmation(entry, proposalAction) {
  if (['update', 'delete'].includes(entry?.proposal?.action) && proposalAction !== entry.proposal.action)
    throw memoryFailure('此建议需要明确确认更新或删除动作，请使用支持动作展示的确认入口。',
      'MEMORY_PROPOSAL_CONFIRMATION_REQUIRED', 409);
}

export function memorySourceId(scope, scopeId, id) {
  const scopeKey = scope === 'user' ? 'user' : `${scope}:${scopeId}`;
  return createHash('sha256').update(JSON.stringify(['memory', scopeKey, id])).digest('hex');
}

/** Candidate provenance retains exact user quotations; it cannot grant confirmation or a different scope.
 * 候选来源保留用户原话，但不能自行授予确认状态或其他范围的权限。 */
export function validateMemoryCandidate(value, source) {
  object(value, '记忆候选');
  if (Object.keys(value).some(key => !['algorithm', 'trigger', 'fingerprint', 'quotes', 'batchThroughMessageId'].includes(key)) ||
      value.algorithm !== MEMORY_CANDIDATE_ALGORITHM || !MEMORY_CANDIDATE_TRIGGERS.includes(value.trigger) ||
      !/^[a-f0-9]{64}$/u.test(value.fingerprint) || source.type !== 'user-message' ||
      !Array.isArray(value.quotes) || !value.quotes.length || value.quotes.length > 12)
    throw memoryFailure('记忆候选来源格式无效。');
  const ids = new Set();
  const quotes = value.quotes.map(quote => {
    object(quote, '候选原话');
    const messageId = memoryId(quote.messageId);
    if (Object.keys(quote).some(key => !['messageId', 'text'].includes(key)) || ids.has(messageId) ||
        typeof quote.text !== 'string' || !quote.text.trim() || quote.text.length > MAX_MEMORY_CONTENT || quote.text.includes('\0'))
      throw memoryFailure('候选原话须引用完整、有界的用户消息。');
    ids.add(messageId);
    return { messageId, text: quote.text };
  });
  if (!ids.has(source.messageId)) throw memoryFailure('候选主来源不属于原话记录。');
  if (value.batchThroughMessageId !== undefined && value.trigger !== 'ordinary-batch') throw memoryFailure('批次游标仅属于完整轮次候选。');
  return { algorithm: value.algorithm, trigger: value.trigger, fingerprint: value.fingerprint, quotes,
    ...(value.batchThroughMessageId === undefined ? {} : { batchThroughMessageId: memoryId(value.batchThroughMessageId) }) };
}

function object(value, label) {
  if (!value || typeof value !== 'object' || Array.isArray(value))
    throw memoryFailure(`${label}格式无效，原文件已保留。`, 'CORRUPT_MEMORY', 500);
  return value;
}

function timestamp(value) {
  if (typeof value !== 'string' || !Number.isFinite(Date.parse(value)))
    throw memoryFailure('记忆时间格式无效，原文件已保留。', 'CORRUPT_MEMORY', 500);
  return value;
}

export function validateMemorySource(value) {
  object(value, '记忆来源');
  const type = value.type ?? (value.messageId != null ? 'user-message' : 'manual');
  const role = value.role ?? 'user';
  if (!['user-message', 'manual'].includes(type) || role !== 'user')
    throw memoryFailure('记忆须由用户明确确认，来源格式无效。');
  const source = { type, role: 'user' };
  // Manual confirmation can belong directly to a work/global scope without creating a conversation.
  // 人工确认可直接归属工作或全局范围，不必创建聊天。
  if (type === 'user-message' || value.conversationId != null) source.conversationId = memoryId(value.conversationId);
  if (type === 'user-message') source.messageId = memoryId(value.messageId);
  else if (value.messageId != null) throw memoryFailure('手动记忆不能伪造消息来源。');
  return source;
}

export function validateMemoryDocument(value, { scope, scopeId }) {
  object(value, '记忆文件');
  if (value.schemaVersion !== MEMORY_SCHEMA_VERSION)
    throw memoryFailure('此记忆文件版本不受当前程序支持，原文件已保留。', 'UNSUPPORTED_MEMORY_VERSION', 409);
  if (value.scope !== scope || value.scopeId !== scopeId || !Number.isSafeInteger(value.revision) || value.revision < 0 ||
      !Array.isArray(value.entries) || value.entries.length > MAX_MEMORY_ENTRIES)
    throw memoryFailure('记忆文件归属或版本格式无效，原文件已保留。', 'CORRUPT_MEMORY', 500);
  const ids = new Set();
  const entries = value.entries.map(entry => {
    object(entry, '记忆条目');
    const id = memoryId(entry.id);
    if (ids.has(id) || entry.scope !== scope || entry.scopeId !== scopeId || !['draft', 'confirmed'].includes(entry.status) ||
        !Number.isSafeInteger(entry.revision) || entry.revision < 1)
      throw memoryFailure('记忆条目归属、状态或版本格式无效，原文件已保留。', 'CORRUPT_MEMORY', 500);
    ids.add(id);
    const source = validateMemorySource(entry.source);
    const candidate = entry.candidate === undefined ? undefined : validateMemoryCandidate(entry.candidate, source);
    const proposal = entry.proposal === undefined ? undefined : validateMemoryProposal(entry.proposal, source);
    if (candidate && proposal || entry.status === 'draft' && !candidate && !proposal)
      throw memoryFailure('草稿来源缺失或候选类型冲突。', 'CORRUPT_MEMORY', 500);
    if (proposal && (proposal.target && (proposal.target.scope !== scope || proposal.target.scopeId !== scopeId) ||
        entry.status === 'draft' && proposal.appliedAt || entry.status === 'confirmed' &&
        (proposal.action === 'delete' || proposal.action === 'update' && !proposal.appliedAt)))
      throw memoryFailure('记忆建议范围或确认动作无效。', 'CORRUPT_MEMORY', 500);
    if (proposal && entry.status === 'draft' &&
        proposal.fingerprint !== memoryProposalFingerprint(scope, scopeId, memoryContent(entry.content), memoryKind(entry.kind, scope), proposal))
      throw memoryFailure('记忆建议身份不一致。', 'CORRUPT_MEMORY', 500);
    if (candidate && candidate.fingerprint !== memoryCandidateFingerprint(scope, candidate.quotes.map(quote => quote.text.trim()).join('\n\n')))
      throw memoryFailure('候选原话身份不一致。', 'CORRUPT_MEMORY', 500);
    return { id, scope, scopeId, content: memoryContent(entry.content), kind: memoryKind(entry.kind, scope),
      status: entry.status, source, ...(candidate ? { candidate } : {}), ...(proposal ? { proposal } : {}), revision: entry.revision,
      createdAt: timestamp(entry.createdAt), updatedAt: timestamp(entry.updatedAt) };
  });
  if (value.dismissedSources !== undefined && !Array.isArray(value.dismissedSources))
    throw memoryFailure('记忆撤销来源格式无效，原文件已保留。', 'CORRUPT_MEMORY', 500);
  const dismissedSources = (value.dismissedSources ?? []).map(source => {
    object(source, '记忆撤销来源');
    if (source.candidateFingerprint !== undefined && !/^[a-f0-9]{64}$/u.test(source.candidateFingerprint))
      throw memoryFailure('候选撤销身份格式无效。', 'CORRUPT_MEMORY', 500);
    if (source.proposalFingerprint !== undefined && !/^[a-f0-9]{64}$/u.test(source.proposalFingerprint))
      throw memoryFailure('记忆建议撤销身份格式无效。', 'CORRUPT_MEMORY', 500);
    return { conversationId: memoryId(source.conversationId), messageId: memoryId(source.messageId), deletedAt: timestamp(source.deletedAt),
      ...(source.candidateFingerprint === undefined ? {} : { candidateFingerprint: source.candidateFingerprint }),
      ...(source.proposalFingerprint === undefined ? {} : { proposalFingerprint: source.proposalFingerprint }),
      ...(source.candidateBatchThroughMessageId === undefined ? {} : { candidateBatchThroughMessageId: memoryId(source.candidateBatchThroughMessageId) }) };
  });
  let candidateConfiguration;
  if (value.candidateSettings !== undefined || value.candidateSettingsRevision !== undefined) {
    if (scope !== 'user' || value.candidateSettings === undefined || !Number.isSafeInteger(value.candidateSettingsRevision) || value.candidateSettingsRevision < 0)
      throw memoryFailure('自动候选配置归属或版本无效。', 'CORRUPT_MEMORY', 500);
    candidateConfiguration = { candidateSettings: memoryCandidateSettings(value.candidateSettings), candidateSettingsRevision: value.candidateSettingsRevision };
  }
  return { schemaVersion: MEMORY_SCHEMA_VERSION, scope, scopeId, revision: value.revision, entries, dismissedSources, ...candidateConfiguration };
}

/**
 * Commands are explicit whole-message prefixes; assistant text and quoted code are never scanned.
 * 命令必须是整条用户消息的明确前缀，不扫描助手正文或引用代码。
 */
export function explicitMemoryInstruction(message, hasProject) {
  if (typeof message !== 'string') return null;
  const match = /^\s*(全局记住|聊天记住|项目记住|工作记住|记住这个|记住)\s*[:：]\s*([\s\S]+?)\s*$/.exec(message);
  if (!match) return null;
  let scope = match[1] === '全局记住' ? 'user' : match[1] === '聊天记住' ? 'chat' :
    ['项目记住', '工作记住'].includes(match[1]) ? 'project' : hasProject ? 'project' : 'chat';
  if (scope === 'project' && !hasProject)
    throw memoryFailure('此聊天未关联项目，请使用“聊天记住：”或先选择一个项目。');
  return { scope, content: memoryContent(match[2]), kind: memoryKind(undefined, scope) };
}

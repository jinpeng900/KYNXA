import { randomUUID } from 'node:crypto';
import { validateId } from '../../platform/conversation-id.mjs';
import { parseSourceReference, retrievalFailure, retrievalScopeKeys } from './retrieval-contracts.mjs';

export const EVIDENCE_REFERENCE_VERSION = 'ev1';
export const MAX_EVIDENCE_REFERENCES = 60;
const UUID = /^[a-f0-9]{8}-[a-f0-9]{4}-4[a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}$/i;
const HASH = /^[a-f0-9]{64}$/;
const IDENTITY_FIELDS = new Set(['sourceId', 'contentHash', 'sourceRevision', 'chunkId', 'chunkHash',
  'indexSnapshotId', 'scopeSnapshots', 'modelSourceRef', 'canonicalSourceRef', 'evidenceArchiveId']);
const invalid = () => retrievalFailure('Invalid evidence reference. / 证据引用无效。', 'INVALID_EVIDENCE_REFERENCE');
const sameId = (left, right) => validateId(left).toLowerCase() === validateId(right).toLowerCase();

export function allocateEvidenceArchiveId() { return randomUUID(); }

/** Archive UUIDs are reversible, not capabilities; authorization still requires a formal receipt.
 * 归档 UUID 可逆编码，不是访问凭证；授权仍需正式聊天回执。 */
export function evidenceSourceRef(archiveId, referenceNumber) {
  if (typeof archiveId !== 'string' || !UUID.test(archiveId) || !Number.isSafeInteger(referenceNumber) ||
      referenceNumber < 1 || referenceNumber > MAX_EVIDENCE_REFERENCES) throw invalid();
  const compactId = Buffer.from(archiveId.replace(/-/g, ''), 'hex').toString('base64url');
  return `${EVIDENCE_REFERENCE_VERSION}:${compactId}:${referenceNumber.toString(36).padStart(2, '0')}`;
}

export function parseEvidenceSourceRef(value) {
  if (typeof value !== 'string' || !/^ev1:[A-Za-z0-9_-]{22}:[0-9a-z]{2}$/.test(value)) throw invalid();
  const [, compactId, number] = value.split(':'), bytes = Buffer.from(compactId, 'base64url');
  if (bytes.length !== 16 || bytes.toString('base64url') !== compactId) throw invalid();
  const hex = bytes.toString('hex');
  const archiveId = `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
  const referenceNumber = Number.parseInt(number, 36);
  if (evidenceSourceRef(archiveId, referenceNumber) !== value) throw invalid();
  return { archiveId, referenceNumber };
}

/** Only complete canonical tuples can be mapped; optional duplicate identity fields must agree.
 * 只映射完整 canonical 元组；条目中可选的重复身份字段必须一致。 */
export function validatedEvidenceReference(item) {
  if (!item || typeof item !== 'object' || Array.isArray(item)) throw invalid();
  let descriptor;
  try { descriptor = parseSourceReference(item.sourceRef); } catch { throw invalid(); }
  if (!((Number.isSafeInteger(descriptor.sourceRevision) && descriptor.sourceRevision >= 0) ||
      (typeof descriptor.sourceRevision === 'string' && descriptor.sourceRevision.length > 0 && descriptor.sourceRevision.length <= 256)) ||
      (Object.hasOwn(descriptor, 'chunkId') !== Object.hasOwn(descriptor, 'chunkHash')) ||
      (descriptor.chunkId !== undefined && (typeof descriptor.chunkId !== 'string' || !descriptor.chunkId ||
        descriptor.chunkId.length > 256 || /[\x00-\x1f]/.test(descriptor.chunkId) || !HASH.test(descriptor.chunkHash ?? '')))) throw invalid();
  const encoded = item.sourceRef.slice(5);
  if (!/^[A-Za-z0-9_-]+$/.test(encoded) || Buffer.from(encoded, 'base64url').toString('base64url') !== encoded) throw invalid();
  for (const key of ['sourceId', 'scopeKey', 'sourceRevision', 'contentHash', 'chunkId', 'chunkHash']) {
    if (Object.hasOwn(item, key) && JSON.stringify(item[key]) !== JSON.stringify(descriptor[key])) throw invalid();
  }
  return descriptor;
}

function projectPayload(payload, archiveId) {
  if (!payload || typeof payload !== 'object' || !Array.isArray(payload.items) || payload.items.length > MAX_EVIDENCE_REFERENCES) throw invalid();
  const projected = Object.fromEntries(Object.entries(payload).filter(([key]) => !IDENTITY_FIELDS.has(key)));
  projected.items = payload.items.map((item, index) => {
    validatedEvidenceReference(item);
    return { ...Object.fromEntries(Object.entries(item).filter(([key]) => !IDENTITY_FIELDS.has(key))),
      reference: index + 1, sourceRef: evidenceSourceRef(archiveId, index + 1) };
  });
  return projected;
}

/** Model projection is separate from the full durable archive and the local public-view API.
 * 模型视图与完整持久归档、本地公开查看接口分离，原对象不作修改。 */
export function projectEvidenceSearchResult(result, archiveId) {
  // Validate even an empty result, so draft/final projections share the same fixed-length namespace.
  // 即使为空结果也校验归档 ID，使草稿与最终视图使用同一固定长度命名空间。
  evidenceSourceRef(archiveId, 1);
  if (!result || typeof result !== 'object' || Array.isArray(result)) throw invalid();
  if (!Object.hasOwn(result, 'structuredContent')) return projectPayload(result, archiveId);
  const payload = result.structuredContent, projected = projectPayload(payload, archiveId);
  const references = payload.items.map((item, index) => [item.sourceRef, evidenceSourceRef(archiveId, index + 1)]);
  const content = result.content?.map(block => {
    if (block?.type !== 'text' || typeof block.text !== 'string') return structuredClone(block);
    let text = block.text;
    try {
      const parsed = JSON.parse(text);
      if (Array.isArray(parsed?.items) && parsed.items.length === payload.items.length &&
          parsed.items.every((item, index) => item.sourceRef === payload.items[index].sourceRef))
        return { ...block, text: JSON.stringify(projectPayload(parsed, archiveId)) };
    } catch { /* Non-JSON summaries retain their text. / 非 JSON 摘要保留文字。 */ }
    for (const [canonical, compact] of references) text = text.split(canonical).join(compact);
    return { ...block, text };
  });
  return { ...result, structuredContent: projected, ...(content ? { content } : {}) };
}

function currentScopes(relationship) {
  return ['user', `chat:${relationship.conversationId.toLowerCase()}`,
    ...(!relationship.isFolderlessWorkspace && !relationship.projectArchived && relationship.projectId
      ? [`project:${relationship.projectId.toLowerCase()}`] : [])];
}

/** Resolution never returns archived excerpts; current index/source freshness must be checked by the caller.
 * 解析不返回旧归档正文；调用方仍需校验当前索引、来源版本和撤销状态。 */
export class EvidenceReferenceStore {
  constructor({ conversationStore, resultStore } = {}) {
    if (typeof conversationStore?.readModelMessages !== 'function' || typeof conversationStore?.describeConversation !== 'function' ||
        typeof resultStore?.evidenceReference !== 'function') throw retrievalFailure('Evidence stores are required. / 必须提供正式证据存储。', 'INVALID_EVIDENCE_STORE');
    this.conversations = conversationStore;
    this.results = resultStore;
  }

  async _scopes(context, scopeKeys, signal) {
    signal?.throwIfAborted();
    const relationship = await this.conversations.describeConversation(context.conversationId);
    if (Object.hasOwn(context, 'projectId') && ((context.projectId == null) !== (relationship.projectId == null) ||
        (context.projectId != null && !sameId(context.projectId, relationship.projectId))))
      throw retrievalFailure('Workspace changed. / 聊天工作范围已变化。', 'WORKSPACE_CHANGED', 409);
    if (relationship.isArchived || relationship.projectArchived)
      throw retrievalFailure('Evidence is unavailable in archived chats. / 归档聊天不能回读证据。', 'TOOL_RESULT_ARCHIVED', 409);
    const allowed = currentScopes(relationship), scopes = retrievalScopeKeys(scopeKeys ?? allowed);
    if (scopes.some(scope => !allowed.includes(scope))) throw retrievalFailure('Evidence is outside the current scope. / 证据超出当前范围。', 'RETRIEVAL_SCOPE_REQUIRED', 403);
    signal?.throwIfAborted();
    return { scopes, context: { ...context, projectId: relationship.projectId } };
  }

  async _resolve(context, sourceRef, parsed, receipt, owner, options) {
    const current = await this._scopes(context, options.scopeKeys, options.signal);
    if (receipt?.id !== parsed.archiveId || owner?.toolName !== 'knowledge.search') throw invalid();
    const item = await this.results.evidenceReference(current.context, receipt, owner, parsed.referenceNumber, { signal: options.signal });
    // Revalidate the catalog after IO; an old receipt cannot authorize a newly moved scope.
    // IO 后复核目录，旧回执不能授权聊天移动后不再可见的范围。
    await this._scopes(current.context, current.scopes, options.signal);
    if (!current.scopes.includes(item.scopeKey)) throw retrievalFailure('Evidence source is outside the current scope. / 证据来源超出当前范围。', 'RETRIEVAL_SOURCE_NOT_FOUND', 404);
    return { ...item, canonicalSourceRef: item.sourceRef, modelSourceRef: sourceRef,
      resultRef: { ...receipt }, referenceNumber: parsed.referenceNumber };
  }

  async resolve(context, sourceRef, options = {}) {
    const parsed = parseEvidenceSourceRef(sourceRef);
    await this._scopes(context, options.scopeKeys, options.signal);
    // Formal log reads happen outside the archive's catalog lock to avoid nested queue deadlocks.
    // 正式日志读取位于归档目录锁之外，避免嵌套排队死锁。
    const messages = await this.conversations.readModelMessages(context.conversationId);
    for (const message of messages) {
      if (message.Role !== 'assistant') continue;
      if (message.RetrievalResultRef?.id === parsed.archiveId) return this._resolve(context, sourceRef, parsed,
        message.RetrievalResultRef, { requestId: message.Id, toolCallId: `retrieval:${message.Id}`, toolName: 'knowledge.search' }, options);
      for (const activity of message.ToolActivities ?? []) {
        if (activity.name === 'knowledge.search' && activity.resultRef?.id === parsed.archiveId) return this._resolve(context, sourceRef, parsed,
          activity.resultRef, { requestId: message.Id, toolCallId: activity.toolCallId, toolName: activity.name }, options);
      }
    }
    throw retrievalFailure('No formal evidence receipt in this chat. / 当前聊天没有此证据的正式回执。', 'EVIDENCE_REFERENCE_NOT_FOUND', 404);
  }

  // Internal publication may precede the formal log write; this bypass is limited to its current request.
  // 内部发布可先于正式日志写入；此入口只接受本次请求身份，不能暴露给模型工具参数。
  resolveTrusted(context, sourceRef, { receipt, owner, scopeKeys, signal } = {}) {
    if (!owner?.requestId || !sameId(owner.requestId, context.requestId)) throw invalid();
    return this._resolve(context, sourceRef, parseEvidenceSourceRef(sourceRef), receipt, owner, { scopeKeys, signal });
  }
}

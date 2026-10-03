import { validateId } from './conversations.mjs';

export const MEMORY_SCHEMA_VERSION = 1;
export const MEMORY_SCOPES = ['chat', 'project', 'user'];
export const MEMORY_KINDS = ['fact', 'preference', 'decision'];
export const MAX_MEMORY_CONTENT = 4000;
export const MAX_MEMORY_ENTRIES = 1000;

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
    if (ids.has(id) || entry.scope !== scope || entry.scopeId !== scopeId || entry.status !== 'confirmed' ||
        !Number.isSafeInteger(entry.revision) || entry.revision < 1)
      throw memoryFailure('记忆条目归属、状态或版本格式无效，原文件已保留。', 'CORRUPT_MEMORY', 500);
    ids.add(id);
    return { id, scope, scopeId, content: memoryContent(entry.content), kind: memoryKind(entry.kind, scope),
      status: 'confirmed', source: validateMemorySource(entry.source), revision: entry.revision,
      createdAt: timestamp(entry.createdAt), updatedAt: timestamp(entry.updatedAt) };
  });
  if (value.dismissedSources !== undefined && !Array.isArray(value.dismissedSources))
    throw memoryFailure('记忆撤销来源格式无效，原文件已保留。', 'CORRUPT_MEMORY', 500);
  const dismissedSources = (value.dismissedSources ?? []).map(source => {
    object(source, '记忆撤销来源');
    return { conversationId: memoryId(source.conversationId), messageId: memoryId(source.messageId), deletedAt: timestamp(source.deletedAt) };
  });
  return { schemaVersion: MEMORY_SCHEMA_VERSION, scope, scopeId, revision: value.revision, entries, dismissedSources };
}

/** Commands are explicit whole-message prefixes; assistant text and quoted code are never scanned. */
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

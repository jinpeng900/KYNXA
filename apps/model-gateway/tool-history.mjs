import { validateId } from './conversations.mjs';
import { boundedInteger, toolFailure } from './tool-paths.mjs';
import { publicModelHistoryText } from './model-history.mjs';

export const historyDescriptors = [
  { name: 'conversation.history.search', description: 'Search current-chat public messages. Set includeTools to recover saved model steps and untrusted tool observations; never private thinking or settings. Stable IDs support paging.',
    inputSchema: { type: 'object', properties: { query: { type: 'string', maxLength: 200 },
      includeTools: { type: 'boolean' }, offset: { type: 'integer', minimum: 0, maximum: 1000000 }, limit: { type: 'integer', minimum: 1, maximum: 20 } },
      additionalProperties: false }, source: 'builtin' },
  { name: 'conversation.history.read', description: 'Read a current-chat message by stable ID in text pages. includeTools also returns recorded model steps/tool observations and result references, not private thinking. No other/archived chat access.',
    inputSchema: { type: 'object', properties: { messageId: { type: 'string' }, offset: { type: 'integer', minimum: 0, maximum: 16000000 },
      includeTools: { type: 'boolean' }, limit: { type: 'integer', minimum: 1, maximum: 16000 } }, required: ['messageId'], additionalProperties: false }, source: 'builtin' }
];

const sameId = (left, right) => left == null && right == null || typeof left === 'string' && typeof right === 'string' && left.toLowerCase() === right.toLowerCase();
function matchOffset(text, query) {
  const foldedOffset = text.toLowerCase().indexOf(query);
  if (foldedOffset <= 0) return foldedOffset;
  // Unicode case folding may change UTF-16 length (for example İ → i + dot).
  let offset = 0, folded = 0;
  for (const character of text) {
    const next = folded + character.toLowerCase().length;
    if (foldedOffset < next) return offset;
    folded = next;
    offset += character.length;
  }
  return offset;
}
function checkOwnership(relationship, context) {
  if (!sameId(relationship.conversationId, context.conversationId) || !sameId(relationship.projectId, context.projectId))
    throw toolFailure('聊天工作范围已变化。', 'WORKSPACE_CHANGED', 409);
  if (relationship.isArchived || relationship.projectArchived)
    throw toolFailure('已归档聊天不能用于模型历史查阅。', 'CONVERSATION_HISTORY_ARCHIVED', 409);
}

async function publicHistory(conversations, context, signal, includeTools = false) {
  signal?.throwIfAborted();
  checkOwnership(await conversations.describeConversation(context.conversationId), context);
  const messages = includeTools ? await conversations.readModelMessages(context.conversationId)
    : await conversations.readMessages(context.conversationId);
  // Queued public reads are rechecked so a concurrent archive/move cannot expose an obsolete scope.
  checkOwnership(await conversations.describeConversation(context.conversationId), context);
  signal?.throwIfAborted();
  return messages.filter(item => ['user', 'assistant'].includes(item.Role) && item.Status !== 'streaming' && typeof item.Content === 'string')
    .map(item => ({ messageId: item.Id, role: item.Role, status: item.Status,
      text: includeTools && item.Role === 'assistant' ? publicModelHistoryText(item) : item.Content,
      ...(item.CreatedAt ? { createdAt: item.CreatedAt } : {}) }));
}

export async function executeHistoryTool(conversations, context, name, input, signal) {
  const messages = await publicHistory(conversations, context, signal, input.includeTools === true);
  if (name === 'conversation.history.search') {
    const query = (input.query ?? '').toLowerCase();
    if (query.includes('\0')) throw toolFailure('历史查询不能包含空字符。', 'INVALID_TOOL_ARGUMENTS');
    const matching = messages.map(message => ({ ...message, matchOffset: query ? matchOffset(message.text, query) : 0 }))
      .filter(message => message.matchOffset >= 0);
    const offset = boundedInteger(input.offset, 0, 0, 1000000), limit = boundedInteger(input.limit, 10, 1, 20);
    if (offset > matching.length) throw toolFailure('历史查询分页位置超出范围。', 'INVALID_TOOL_ARGUMENTS');
    const selected = [];
    for (const { text, ...message } of matching.slice(offset, offset + limit)) {
      let start = Math.max(0, message.matchOffset - 120), end = Math.min(text.length, start + 512);
      if (start > 0 && /[\uDC00-\uDFFF]/.test(text[start] ?? '') && /[\uD800-\uDBFF]/.test(text[start - 1])) start--;
      if (end < text.length && /[\uD800-\uDBFF]/.test(text[end - 1] ?? '')) end--;
      const value = { ...message, excerpt: text.slice(start, end), excerptOffset: start, totalCharacters: text.length };
      if (JSON.stringify([...selected, value]).length > 60000) break;
      selected.push(value);
    }
    return { conversationId: context.conversationId, messages: selected, offset, nextOffset: Math.min(matching.length, offset + selected.length),
      total: matching.length, hasMore: offset + selected.length < matching.length };
  }
  const id = validateId(input.messageId), message = messages.find(item => sameId(item.messageId, id));
  if (!message) throw toolFailure('当前聊天不存在此公开消息。', 'CONVERSATION_HISTORY_MESSAGE_NOT_FOUND', 404);
  let offset = boundedInteger(input.offset, 0, 0, 16000000);
  const limit = boundedInteger(input.limit, 4096, 1, 16000);
  if (offset > message.text.length) throw toolFailure('历史消息分页位置超出范围。', 'INVALID_TOOL_ARGUMENTS');
  if (offset > 0 && /[\uDC00-\uDFFF]/.test(message.text[offset] ?? '') && /[\uD800-\uDBFF]/.test(message.text[offset - 1])) offset--;
  let end = Math.min(message.text.length, offset + limit);
  if (end < message.text.length && /[\uD800-\uDBFF]/.test(message.text[end - 1] ?? '')) end--;
  if (end === offset && end < message.text.length) end = Math.min(message.text.length, end + 2);
  const { text, ...source } = message;
  return { conversationId: context.conversationId, ...source, text: text.slice(offset, end), offset, nextOffset: end,
    totalCharacters: text.length, truncated: end < text.length };
}

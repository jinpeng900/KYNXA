// Preserve canonical identities across model, tool and storage boundaries.
// 在模型、工具与存储边界间保留既有稳定身份的校验和规范化规则。
const idPattern = /^[a-zA-Z0-9_-]{1,128}$/;

function failure(message, code = 'INVALID_CONVERSATION_DATA', statusCode = 400) {
  return Object.assign(new Error(message), { code, statusCode });
}

export function validateId(id) {
  if (typeof id !== 'string' || !idPattern.test(id)) throw failure('会话或消息 ID 无效。');
  if (/^(con|prn|aux|nul|com[1-9]|lpt[1-9])$/i.test(id)) throw failure('会话或消息 ID 无效。');
  if (/^[a-fA-F0-9]{32}$/.test(id)) return `${id.slice(0, 8)}-${id.slice(8, 12)}-${id.slice(12, 16)}-${id.slice(16, 20)}-${id.slice(20)}`.toLowerCase();
  if (/^[a-fA-F0-9]{8}-[a-fA-F0-9]{4}-[a-fA-F0-9]{4}-[a-fA-F0-9]{4}-[a-fA-F0-9]{12}$/.test(id)) return id.toLowerCase();
  return id;
}

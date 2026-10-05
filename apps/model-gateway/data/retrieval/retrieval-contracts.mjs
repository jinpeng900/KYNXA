import { createHash } from 'node:crypto';

export const RETRIEVAL_SCHEMA_VERSION = 1;
export const MAX_SOURCE_CHARACTERS = 2 * 1024 * 1024;
export const MAX_QUERY_CHARACTERS = 4000;

export function retrievalFailure(message, code = 'INVALID_RETRIEVAL_INPUT', statusCode = 400) {
  return Object.assign(new Error(message), { code, statusCode });
}

export function hashText(text) {
  return createHash('sha256').update(text).digest('hex');
}

export function retrievalRecord(value, label = 'Retrieval value') {
  if (!value || typeof value !== 'object' || Array.isArray(value))
    throw retrievalFailure(`${label} must be an object. / 检索配置必须是对象。`);
  return value;
}

export function requireKeys(value, allowed, label) {
  retrievalRecord(value, label);
  for (const name of Object.keys(value)) {
    if (!allowed.includes(name)) throw retrievalFailure(`Unknown ${label} field: ${name}. / 存在不支持的配置字段。`);
  }
}

export function retrievalScopeKeys(value) {
  if (!Array.isArray(value) || value.length === 0 || value.length > 128)
    throw retrievalFailure('Authorized scopes are required. / 必须提供已授权的检索范围。', 'RETRIEVAL_SCOPE_REQUIRED', 403);
  const keys = [...new Set(value)];
  for (const key of keys) {
    if (typeof key !== 'string' || !/^(user|(project|chat):[a-zA-Z0-9_-]{1,128})$/.test(key))
      throw retrievalFailure('Invalid retrieval scope. / 检索范围格式无效。');
  }
  return keys;
}

export function retrievalSourceId(value) {
  if (typeof value !== 'string' || !value.trim() || value.length > 256 || /[\x00-\x1f]/.test(value))
    throw retrievalFailure('Invalid source identity. / 资料身份无效。');
  return value;
}

export function sourceReference(source, chunk = null) {
  const descriptor = { sourceId: source.sourceId, scopeKey: source.scopeKey,
    sourceRevision: source.sourceRevision, contentHash: source.contentHash,
    ...(chunk ? { chunkId: chunk.chunkId, chunkHash: chunk.chunkHash } : {}) };
  return `rag1:${Buffer.from(JSON.stringify(descriptor)).toString('base64url')}`;
}

export function parseSourceReference(reference) {
  if (typeof reference !== 'string' || !reference.startsWith('rag1:') || reference.length > 4096)
    throw retrievalFailure('Invalid source reference. / 资料引用无效。');
  let value;
  try { value = JSON.parse(Buffer.from(reference.slice(5), 'base64url').toString('utf8')); }
  catch { throw retrievalFailure('Invalid source reference. / 资料引用无效。'); }
  requireKeys(value, ['sourceId', 'scopeKey', 'sourceRevision', 'contentHash', 'chunkId', 'chunkHash'], 'source reference');
  retrievalSourceId(value.sourceId);
  retrievalScopeKeys([value.scopeKey]);
  if (typeof value.contentHash !== 'string' || !/^[a-f0-9]{64}$/.test(value.contentHash))
    throw retrievalFailure('Invalid source hash. / 资料哈希无效。');
  return value;
}

export function validateSource(source) {
  retrievalRecord(source, 'Source');
  retrievalSourceId(source.sourceId);
  retrievalScopeKeys([source.scopeKey]);
  if (typeof source.text !== 'string' || source.text.length > MAX_SOURCE_CHARACTERS)
    throw retrievalFailure('Source text is missing or too large. / 资料正文缺失或过大。', 'RETRIEVAL_SOURCE_TOO_LARGE');
  const contentHash = hashText(source.text);
  if (source.contentHash !== undefined && source.contentHash !== contentHash)
    throw retrievalFailure('Source content hash changed. / 资料内容与哈希不一致。', 'RETRIEVAL_SOURCE_CHANGED', 409);
  const sourceRevision = source.sourceRevision ?? contentHash;
  if (!((Number.isSafeInteger(sourceRevision) && sourceRevision >= 0) ||
      (typeof sourceRevision === 'string' && sourceRevision.length > 0 && sourceRevision.length <= 256)))
    throw retrievalFailure('Invalid source revision. / 资料版本无效。');
  if (source.bindingRevision !== undefined && (!Number.isSafeInteger(source.bindingRevision) || source.bindingRevision < 0))
    throw retrievalFailure('Invalid folder binding revision. / 文件夹绑定版本无效。');
  const locator = source.locator ?? {};
  retrievalRecord(locator, 'Source locator');
  if (JSON.stringify(locator).length > 16384) throw retrievalFailure('Source locator is too large. / 资料定位信息过大。');
  return { ...source, contentHash, sourceRevision, locator,
    title: typeof source.title === 'string' ? source.title.slice(0, 1000) : source.sourceId,
    sourceType: typeof source.sourceType === 'string' ? source.sourceType.slice(0, 100) : 'document' };
}

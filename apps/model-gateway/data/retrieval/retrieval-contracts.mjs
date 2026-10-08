import { createHash } from 'node:crypto';

export const RETRIEVAL_SCHEMA_VERSION = 1;
export const RETRIEVAL_INDEX_SCHEMA_VERSION = 5;
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

/** Extraction identity refers to original bytes; page ranges address the stored, real extracted text.
 * 提取身份绑定原始文件字节；页范围指向保存的实际抽取正文，不生成伪装成原文的页说明。 */
export function validateSourceExtraction(value, textLength) {
  retrievalRecord(value, 'Source extraction');
  requireKeys(value, ['format', 'version', 'rawContentHash', 'pageCount', 'pages'], 'Source extraction');
  if (!['pdf', 'docx'].includes(value.format) || typeof value.version !== 'string' || !value.version ||
      value.version.length > 256 || /[\x00-\x1f]/u.test(value.version) ||
      typeof value.rawContentHash !== 'string' || !/^[a-f0-9]{64}$/u.test(value.rawContentHash))
    throw retrievalFailure('Invalid document extraction identity. / 文档提取身份无效。', 'INVALID_RETRIEVAL_EXTRACTION');
  const result = { format: value.format, version: value.version, rawContentHash: value.rawContentHash };
  if (value.format === 'docx') {
    if (value.pageCount !== undefined || value.pages !== undefined)
      throw retrievalFailure('DOCX text extraction cannot claim rendered page positions. / DOCX 文本提取不能声称已得到渲染页码。', 'INVALID_RETRIEVAL_EXTRACTION');
    return result;
  }
  if (!Number.isSafeInteger(value.pageCount) || value.pageCount < 1 || value.pageCount > 100 ||
      !Array.isArray(value.pages) || value.pages.length !== value.pageCount)
    throw retrievalFailure('Invalid PDF page ranges. / PDF 页范围无效。', 'INVALID_RETRIEVAL_EXTRACTION');
  let previousEnd = 0;
  const pages = value.pages.map((page, index) => {
    requireKeys(page, ['page', 'startOffset', 'endOffset'], 'PDF page range');
    if (page.page !== index + 1 || !Number.isSafeInteger(page.startOffset) || !Number.isSafeInteger(page.endOffset) ||
        page.startOffset < previousEnd || page.endOffset < page.startOffset || page.endOffset > MAX_SOURCE_CHARACTERS ||
        textLength !== undefined && page.endOffset > textLength)
      throw retrievalFailure('PDF page range differs from extracted text. / PDF 页范围与提取正文不匹配。', 'INVALID_RETRIEVAL_EXTRACTION');
    previousEnd = page.endOffset;
    return { page: page.page, startOffset: page.startOffset, endOffset: page.endOffset };
  });
  return { ...result, pageCount: value.pageCount, pages };
}

export function sourceFileRevision(file) {
  if (file.fileWindow) {
    const window = validateSourceFileWindow(file.fileWindow);
    return hashText(JSON.stringify([window.rawContentHash, window.startOffset, window.endOffset, window.contentHash]));
  }
  if (file.extraction === undefined) return file.contentHash;
  const extraction = validateSourceExtraction(file.extraction);
  return hashText(JSON.stringify([file.contentHash, extraction.version, extraction.rawContentHash, extraction.pages ?? null]));
}

export function validateSourceFileWindow(value, textLength) {
  retrievalRecord(value, 'Source file window');
  requireKeys(value, ['version', 'encoding', 'offsetUnit', 'startOffset', 'endOffset', 'startLine', 'endLine',
    'startByte', 'endByte', 'contentHash', 'textBytes', 'rawContentHash', 'textContentHash', 'totalCharacters', 'metadata'], 'source file window');
  if (value.version !== 'text-file-window-v1' || !['utf-8', 'utf-16le'].includes(value.encoding) || value.offsetUnit !== 'utf16-code-units' ||
      ['contentHash', 'rawContentHash', 'textContentHash'].some(key => !/^[a-f0-9]{64}$/u.test(value[key] ?? '')) ||
      ['startOffset', 'endOffset', 'startByte', 'endByte', 'startLine', 'endLine', 'textBytes', 'totalCharacters']
        .some(key => !Number.isSafeInteger(value[key]) || value[key] < 0) || value.endOffset <= value.startOffset ||
      value.endByte <= value.startByte || value.endOffset - value.startOffset > 128 * 1024 || value.endByte - value.startByte > 512 * 1024 ||
      value.textBytes > 512 * 1024 || value.startLine < 1 || value.endLine < value.startLine ||
      value.endOffset > value.totalCharacters || textLength !== undefined && textLength !== value.endOffset - value.startOffset ||
      !value.metadata || ['sizeBytes', 'mtimeMs', 'ctimeMs', 'device', 'inode'].some(key => !Number.isFinite(value.metadata[key])) ||
      !Number.isSafeInteger(value.metadata.sizeBytes) || ['sizeBytes', 'device', 'inode'].some(key => value.metadata[key] < 0) ||
      value.endByte > value.metadata.sizeBytes)
    throw retrievalFailure('Invalid source file window. / 来源文件窗口无效。', 'INVALID_RETRIEVAL_WINDOW');
  return structuredClone(value);
}

/** Model evidence carries only the relevant document pages, not the complete internal extraction manifest.
 * 模型证据只携带命中片段的文档页码，不反复发送完整内部提取清单。 */
export function sourceEvidenceLocator(locator, { startOffset, endOffset } = {}) {
  if (locator.fileWindow) {
    const window = validateSourceFileWindow(locator.fileWindow), { fileWindow, ...result } = locator;
    return { ...result, fileWindow: window, offsetBasis: 'source-window',
      ...(Number.isSafeInteger(startOffset) ? { originalStartOffset: window.startOffset + startOffset } : {}),
      ...(Number.isSafeInteger(endOffset) ? { originalEndOffset: window.startOffset + endOffset } : {}) };
  }
  if (locator.extraction === undefined) return locator;
  const { extraction: original, ...result } = locator, extraction = validateSourceExtraction(original);
  if (extraction.format === 'pdf' && Number.isSafeInteger(startOffset) && Number.isSafeInteger(endOffset)) {
    const pages = extraction.pages.filter(page => page.startOffset < endOffset && page.endOffset > startOffset);
    if (pages.length) { result.startPage = pages[0].page; result.endPage = pages.at(-1).page; }
  }
  return { ...result, documentFormat: extraction.format,
    ...(extraction.pageCount === undefined ? {} : { pageCount: extraction.pageCount }) };
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

const STRUCTURE_STATUSES = ['parsed', 'partial', 'unavailable'];
const STRUCTURE_DOMAINS = ['knowledge', 'code'];
const validStructureText = (value, maximum = 1024) => typeof value === 'string' && value.length > 0 &&
  value.length <= maximum && !/[\x00-\x1f]/u.test(value);
const structureFailure = () => retrievalFailure('Invalid retrieval structure. / 检索结构元信息无效。', 'INVALID_RETRIEVAL_STRUCTURE');

function validateStructureCommon(value) {
  if (!STRUCTURE_DOMAINS.includes(value.domain) || !STRUCTURE_STATUSES.includes(value.parseStatus) ||
      !(value.language === null && value.parseStatus === 'unavailable' ||
        typeof value.language === 'string' && /^[a-z][a-z0-9+#.-]{0,63}$/u.test(value.language))) throw structureFailure();
}

/** Structure is derived metadata, never a new permission or a claim of complete language-server coverage.
 * 结构是派生元信息，不能扩大权限，也不代表完整语言服务覆盖。
 */
export function validateStructureDescriptor(value) {
  if (value === undefined) return undefined;
  requireKeys(value, ['domain', 'language', 'parserVersion', 'parseStatus', 'diagnosticCodes'], 'source structure');
  validateStructureCommon(value);
  if (!validStructureText(value.parserVersion, 512)) throw structureFailure();
  const diagnosticCodes = value.diagnosticCodes ?? [];
  if (!Array.isArray(diagnosticCodes) || diagnosticCodes.length > 32 ||
      diagnosticCodes.some(code => typeof code !== 'string' || !/^[A-Z0-9_]{1,128}$/u.test(code))) throw structureFailure();
  return { domain: value.domain, language: value.language, parserVersion: value.parserVersion,
    parseStatus: value.parseStatus, diagnosticCodes: [...diagnosticCodes] };
}

export function validateChunkStructure(value, source, chunk) {
  if (value === undefined) return undefined;
  requireKeys(value, ['domain', 'language', 'kind', 'symbolName', 'qualifiedName', 'parentSymbol',
    'sectionTitle', 'sectionPath', 'unitStartOffset', 'unitEndOffset', 'parseStatus'], 'chunk structure');
  validateStructureCommon(value);
  if (!validStructureText(value.kind, 64) || !/^[a-z][a-zA-Z0-9_-]*$/u.test(value.kind)) throw structureFailure();
  if (source.structure && (value.domain !== source.structure.domain || value.language !== source.structure.language ||
      source.structure.parseStatus === 'unavailable' && value.parseStatus !== 'unavailable')) throw structureFailure();
  const result = { domain: value.domain, language: value.language, kind: value.kind, parseStatus: value.parseStatus };
  for (const key of ['symbolName', 'qualifiedName', 'parentSymbol', 'sectionTitle']) {
    if (value[key] === undefined) continue;
    if (!validStructureText(value[key], key === 'sectionTitle' ? 256 : 1024)) throw structureFailure();
    result[key] = value[key];
  }
  if (value.sectionPath !== undefined) {
    if (!Array.isArray(value.sectionPath) || value.sectionPath.length > 16 ||
        value.sectionPath.some(title => !validStructureText(title, 256))) throw structureFailure();
    result.sectionPath = [...value.sectionPath];
  }
  const { unitStartOffset, unitEndOffset } = value;
  const splitsPair = offset => offset > 0 && offset < source.text.length &&
    /[\uD800-\uDBFF]/u.test(source.text[offset - 1]) && /[\uDC00-\uDFFF]/u.test(source.text[offset]);
  if (!Number.isSafeInteger(unitStartOffset) || !Number.isSafeInteger(unitEndOffset) || unitStartOffset < 0 ||
      unitEndOffset > source.text.length || unitEndOffset <= unitStartOffset ||
      unitStartOffset > chunk.startOffset || unitEndOffset < chunk.endOffset || splitsPair(unitStartOffset) || splitsPair(unitEndOffset)) throw structureFailure();
  return { ...result, unitStartOffset, unitEndOffset };
}

export function normalizeRetrievalPath(value) {
  return value.replace(/\\/gu, '/').replace(/^(?:\.\/)+/u, '').replace(/\/{2,}/gu, '/').toLowerCase();
}

export function validateRetrievalIntent(value) {
  if (value === undefined) return undefined;
  requireKeys(value, ['domain', 'symbol', 'path'], 'retrieval intent');
  const domain = value.domain ?? 'mixed';
  if (!['knowledge', 'code', 'mixed'].includes(domain)) throw retrievalFailure('Invalid retrieval domain. / 检索领域无效。');
  if (value.symbol !== undefined && !validStructureText(value.symbol, 1024) ||
      value.path !== undefined && !validStructureText(value.path, 4096)) throw retrievalFailure('Invalid exact retrieval target. / 精确检索目标无效。');
  return { domain, ...(value.symbol !== undefined ? { symbol: value.symbol } : {}),
    ...(value.path !== undefined ? { path: normalizeRetrievalPath(value.path) } : {}) };
}

export function sourceReference(source, chunk = null) {
  const descriptor = { sourceId: source.sourceId, scopeKey: source.scopeKey,
    sourceRevision: source.sourceRevision, contentHash: source.contentHash,
    ...(source.derivationSignature ? { derivationSignature: source.derivationSignature } : {}),
    ...(chunk ? { chunkId: chunk.chunkId, chunkHash: chunk.chunkHash } : {}) };
  return `rag1:${Buffer.from(JSON.stringify(descriptor)).toString('base64url')}`;
}

export function parseSourceReference(reference) {
  if (typeof reference !== 'string' || !reference.startsWith('rag1:') || reference.length > 4096)
    throw retrievalFailure('Invalid source reference. / 资料引用无效。');
  let value;
  try { value = JSON.parse(Buffer.from(reference.slice(5), 'base64url').toString('utf8')); }
  catch { throw retrievalFailure('Invalid source reference. / 资料引用无效。'); }
  requireKeys(value, ['sourceId', 'scopeKey', 'sourceRevision', 'contentHash', 'chunkId', 'chunkHash', 'derivationSignature'], 'source reference');
  retrievalSourceId(value.sourceId);
  retrievalScopeKeys([value.scopeKey]);
  if (typeof value.contentHash !== 'string' || !/^[a-f0-9]{64}$/.test(value.contentHash))
    throw retrievalFailure('Invalid source hash. / 资料哈希无效。');
  if (!((Number.isSafeInteger(value.sourceRevision) && value.sourceRevision >= 0) ||
      (typeof value.sourceRevision === 'string' && value.sourceRevision.length > 0 && value.sourceRevision.length <= 256)))
    throw retrievalFailure('Invalid source reference revision. / 资料引用版本无效。');
  if (Object.hasOwn(value, 'chunkId') !== Object.hasOwn(value, 'chunkHash') ||
      value.chunkId !== undefined && (typeof value.chunkId !== 'string' || !value.chunkId || value.chunkId.length > 256 ||
        /[\x00-\x1f]/u.test(value.chunkId) || typeof value.chunkHash !== 'string' || !/^[a-f0-9]{64}$/u.test(value.chunkHash)))
    throw retrievalFailure('Invalid chunk reference. / 分块引用无效。');
  if (value.derivationSignature !== undefined && (typeof value.derivationSignature !== 'string' || !/^[a-f0-9]{64}$/.test(value.derivationSignature)))
    throw retrievalFailure('Invalid derivation signature. / 派生索引签名无效。');
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

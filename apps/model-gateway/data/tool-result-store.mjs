import { createHash, randomUUID } from 'node:crypto';
import { constants } from 'node:fs';
import { open, rename, unlink } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { validateId } from '../platform/conversation-id.mjs';
import { boundedInteger, ensureLocalDirectory, inspectLocalPath, objectInput, toolFailure, within } from '../platform/tool-paths.mjs';
import { MAX_EVIDENCE_REFERENCES, projectEvidenceSearchResult, validatedEvidenceReference } from './retrieval/evidence-references.mjs';

export const MAX_TOOL_RESULT_BYTES = 8 * 1024 * 1024;
export const TOOL_RESULT_METADATA_BYTES = 8192;
const MAX_DOCUMENT_BYTES = MAX_TOOL_RESULT_BYTES + TOOL_RESULT_METADATA_BYTES;
const UUID = /^[a-f0-9]{8}-[a-f0-9]{4}-4[a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}$/i;
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');
const sameId = (left, right) => validateId(left).toLowerCase() === validateId(right).toLowerCase();

function resultId(value) {
  if (typeof value !== 'string' || !UUID.test(value)) throw toolFailure('工具结果引用无效。', 'INVALID_TOOL_RESULT_REFERENCE');
  return value.toLowerCase();
}

function jsonText(value) {
  try {
    const text = JSON.stringify(value);
    if (typeof text !== 'string') throw new Error('JSON value missing');
    return text;
  } catch { throw toolFailure('工具结果不是可保存的 JSON。', 'INVALID_TOOL_RESULT'); }
}

function checkDepth(value) {
  const pending = [{ value, depth: 0 }];
  while (pending.length) {
    const { value: current, depth } = pending.pop();
    if (!current || typeof current !== 'object') continue;
    if (depth > 64) throw toolFailure('工具结果嵌套过深。', 'INVALID_TOOL_RESULT');
    for (const child of Object.values(current)) pending.push({ value: child, depth: depth + 1 });
  }
}

/**
 * Public projections never expose MCP _meta. Binary data is returned only to the explicit local-view API.
 * 公开视图不暴露 MCP _meta，二进制数据仅返回给明确的本地查看接口。
 */
export function publicToolResult(canonical, { resultRef, includeMediaData = false } = {}) {
  checkDepth(canonical);
  const walk = (value, pointer, kind = 'value') => {
    if (Array.isArray(value)) return value.map((item, index) => walk(item, `${pointer}/${index}`, kind === 'content' ? 'block' : 'value'));
    if (!value || typeof value !== 'object') return value;
    const entries = [];
    const media = kind === 'block' && ['image', 'audio'].includes(value.type) && typeof value.data === 'string';
    const blob = kind === 'resource' && typeof value.blob === 'string';
    for (const [key, item] of Object.entries(value)) {
      if (key === '_meta') continue;
      if (!includeMediaData && ((media && key === 'data') || (blob && key === 'blob'))) continue;
      const childKind = pointer === '' && key === 'content' && Array.isArray(item) ? 'content' :
        kind === 'block' && value.type === 'resource' && key === 'resource' ? 'resource' : 'value';
      entries.push([key, walk(item, `${pointer}/${key.replace(/~/g, '~0').replace(/\//g, '~1')}`, childKind)]);
    }
    if (!includeMediaData && (media || blob)) {
      entries.push(['media', { encoding: 'base64', bytes: Buffer.byteLength(media ? value.data : value.blob, 'base64'),
        ...(resultRef ? { resultRef: { ...resultRef, pointer: `${pointer}/${media ? 'data' : 'blob'}` } } : {}),
        availableInLocalView: Boolean(resultRef) }]);
    }
    return Object.fromEntries(entries);
  };
  return walk(canonical, '');
}

/**
 * Preserve small JSON verbatim; size the preview inside a valid envelope, including its JSON escaping.
 * 小 JSON 保持原文，预览大小在合法外层封装中计算，并包含 JSON 转义开销。
 */
export function previewToolResult(value, { resultRef, status = 'completed', maximumCharacters = 65536 } = {}) {
  maximumCharacters = boundedInteger(maximumCharacters, 65536, 256, 65536);
  const text = jsonText(value);
  if (text.length <= maximumCharacters) return text;
  const envelope = preview => ({ status, preview, totalCharacters: text.length, truncated: true,
    ...(resultRef ? { resultRef } : {}) });
  if (jsonText(envelope('')).length > maximumCharacters)
    throw toolFailure('工具结果引用超过预览预算。', 'INVALID_TOOL_RESULT_REFERENCE');
  let low = 0, high = text.length;
  while (low < high) {
    const middle = Math.ceil((low + high) / 2);
    if (jsonText(envelope(text.slice(0, middle))).length <= maximumCharacters) low = middle;
    else high = middle - 1;
  }
  if (/[\uD800-\uDBFF]$/.test(text.slice(0, low))) low--;
  return jsonText(envelope(text.slice(0, low)));
}

/**
 * Result files share the conversation catalog guard and move/trash/restore with their owning session.
 * 结果文件共用会话目录保护，随所属会话移动、删除到回收站或恢复。
 */
export class ToolResultStore {
  constructor({ conversationStore } = {}) {
    if (!conversationStore?.root || typeof conversationStore.withConversationStorage !== 'function')
      throw toolFailure('缺少正式聊天存储。', 'INVALID_TOOL_RESULT_STORE');
    this.conversations = conversationStore;
    this.root = resolve(conversationStore.root);
  }

  _scope(context, operation, { checkProject = true, allowArchived = false } = {}) {
    objectInput(context);
    const conversationId = validateId(context.conversationId);
    return this.conversations.withConversationStorage(conversationId, relationship => {
      if (!within(this.root, relationship.sessionDirectory)) throw toolFailure('工具结果存储路径无效。', 'UNSAFE_TOOL_PATH', 403);
      if (checkProject && Object.hasOwn(context, 'projectId') &&
          ((context.projectId == null) !== (relationship.projectId == null) ||
          (context.projectId != null && !sameId(context.projectId, relationship.projectId))))
        throw toolFailure('聊天工作范围已变化。', 'WORKSPACE_CHANGED', 409);
      if (!allowArchived && (relationship.isArchived || relationship.projectArchived))
        throw toolFailure('已归档聊天的工具结果仅供本机历史查看。', 'TOOL_RESULT_ARCHIVED', 409);
      return operation(relationship, join(relationship.sessionDirectory, 'tool-results'));
    });
  }

  async save(context, call, canonical, options = {}) {
    objectInput(call); objectInput(canonical);
    const requestId = validateId(context.requestId);
    if (typeof call.id !== 'string' || !call.id || call.id.length > 200 || /[\0\r\n]/.test(call.id) ||
        typeof call.name !== 'string' || !call.name || call.name.length > 256)
      throw toolFailure('工具执行身份无效。', 'INVALID_TOOL_RESULT');
    const source = jsonText(canonical), bytes = Buffer.byteLength(source);
    if (bytes > MAX_TOOL_RESULT_BYTES) throw toolFailure('完整工具结果超过 8 MiB 保存上限。', 'TOOL_RESULT_TOO_LARGE', 413);
    const safeCanonical = JSON.parse(source);
    checkDepth(safeCanonical);
    objectInput(options);
    const id = options.id === undefined ? randomUUID() : resultId(options.id), hash = sha256(source);
    return this._scope(context, async (relationship, directory) => {
      await ensureLocalDirectory(directory);
      const document = { version: 1, id, conversationId: relationship.conversationId, requestId,
        toolCallId: call.id, toolName: call.name, originalProjectId: Object.hasOwn(context, 'projectId') ? context.projectId : relationship.projectId,
        savedProjectId: relationship.projectId, createdAt: new Date().toISOString(), bytes, sha256: hash, canonical: safeCanonical };
      const serialized = jsonText(document), target = join(directory, `${id}.json`), temporary = join(directory, `.${id}.${randomUUID()}.tmp`);
      if (Buffer.byteLength(serialized) > MAX_DOCUMENT_BYTES) throw toolFailure('工具结果元信息过大。', 'TOOL_RESULT_TOO_LARGE', 413);
      let handle;
      try {
        handle = await open(temporary, 'wx', 0o600);
        await handle.writeFile(serialized); await handle.sync(); await handle.close(); handle = null;
        await inspectLocalPath(directory);
        if (await inspectLocalPath(target, { allowMissing: true })) throw toolFailure('工具结果引用已存在。', 'TOOL_RESULT_CONFLICT', 409);
        await rename(temporary, target);
      } finally {
        await handle?.close();
        await unlink(temporary).catch(error => { if (error.code !== 'ENOENT') throw error; });
      }
      return { id, bytes, sha256: hash };
    }, { checkProject: false, allowArchived: true });
  }

  _load(context, id, operation, { allowArchived = false, maximumReadBytes = MAX_DOCUMENT_BYTES } = {}) {
    id = resultId(id);
    return this._scope(context, async (relationship, directory) => {
      const path = join(directory, `${id}.json`);
      let handle;
      try {
        const info = await inspectLocalPath(path);
        if (!info.isFile() || info.size > MAX_DOCUMENT_BYTES) throw toolFailure('工具结果文件大小或结构无效。', 'CORRUPT_TOOL_RESULT', 500);
        if (info.size > maximumReadBytes) throw toolFailure('工具结果文件与回执大小不匹配。', 'TOOL_RESULT_REFERENCE_MISMATCH', 409);
        handle = await open(path, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0));
        const opened = await handle.stat();
        if (!opened.isFile() || opened.nlink !== 1 || opened.size > MAX_DOCUMENT_BYTES || opened.ino !== info.ino || opened.dev !== info.dev)
          throw toolFailure('工具结果文件已变化或包含链接。', 'UNSAFE_TOOL_PATH', 403);
        if (opened.size > maximumReadBytes) throw toolFailure('工具结果文件与回执大小不匹配。', 'TOOL_RESULT_REFERENCE_MISMATCH', 409);
        const bytes = await handle.readFile();
        const current = await inspectLocalPath(path);
        if (bytes.length > MAX_DOCUMENT_BYTES || current.ino !== opened.ino || current.dev !== opened.dev)
          throw toolFailure('工具结果读取期间已变化。', 'CORRUPT_TOOL_RESULT', 500);
        let document;
        try { document = JSON.parse(bytes.toString('utf8')); } catch { throw toolFailure('工具结果 JSON 已损坏。', 'CORRUPT_TOOL_RESULT', 500); }
        if (document?.version !== 1 || document.id !== id || typeof document.conversationId !== 'string' ||
            !sameId(document.conversationId, relationship.conversationId) || typeof document.requestId !== 'string' ||
            typeof document.toolCallId !== 'string' || typeof document.toolName !== 'string' ||
            !Number.isSafeInteger(document.bytes) || document.bytes < 0 || document.bytes > MAX_TOOL_RESULT_BYTES ||
            !document.canonical || typeof document.canonical !== 'object' || Array.isArray(document.canonical))
          throw toolFailure('工具结果身份或结构无效。', 'CORRUPT_TOOL_RESULT', 500);
        const source = jsonText(document.canonical);
        if (Buffer.byteLength(source) !== document.bytes || sha256(source) !== document.sha256)
          throw toolFailure('工具结果完整性校验失败。', 'CORRUPT_TOOL_RESULT', 500);
        checkDepth(document.canonical);
        return operation(document, { id, bytes: document.bytes, sha256: document.sha256 });
      } catch (error) {
        if (error.code === 'ENOENT') throw toolFailure('当前聊天不存在此工具结果。', 'TOOL_RESULT_NOT_FOUND', 404);
        throw error;
      } finally { await handle?.close(); }
    }, { allowArchived });
  }

  async read(context, id, { offset = 0, limit = 16000, projection = 'public', allowArchived = false } = {}) {
    offset = boundedInteger(offset, 0, 0, MAX_DOCUMENT_BYTES);
    limit = boundedInteger(limit, 16000, 1, 16000);
    if (projection !== 'public') throw toolFailure('模型只能读取公开工具结果。', 'INVALID_TOOL_RESULT_PROJECTION', 403);
    return this._load(context, id, (document, resultRef) => {
      const source = jsonText(publicToolResult(document.canonical, { resultRef }));
      if (offset > source.length) throw toolFailure('工具结果分页位置超出范围。', 'INVALID_TOOL_RESULT_OFFSET');
      if (offset > 0 && /[\uDC00-\uDFFF]/.test(source[offset] ?? '') && /[\uD800-\uDBFF]/.test(source[offset - 1])) offset--;
      let end = Math.min(source.length, offset + limit);
      if (end < source.length && /[\uD800-\uDBFF]/.test(source[end - 1] ?? '')) end--;
      if (end === offset && end < source.length) end = Math.min(source.length, end + 2);
      return { id: resultRef.id, text: source.slice(offset, end), offset, nextOffset: end,
        totalCharacters: source.length, truncated: end < source.length, resultRef };
    }, { allowArchived: allowArchived === true });
  }

  async get(context, id) {
    return this._load(context, id, document => publicToolResult(document.canonical, { includeMediaData: true }), { allowArchived: true });
  }

  /**
   * Internal model-history projection: complete text, typed media references, no private metadata.
   * 内部模型历史视图保留完整文本和带类型媒体引用，不包含私有元信息。
   */
  async modelResult(context, reference, owner) {
    if (!reference || !Number.isSafeInteger(reference.bytes) || reference.bytes < 0 || reference.bytes > MAX_TOOL_RESULT_BYTES ||
        !/^[0-9a-f]{64}$/i.test(reference.sha256 ?? '')) throw toolFailure('工具历史回执无效。', 'INVALID_TOOL_RESULT_REFERENCE');
    return this._load(context, reference.id, (document, resultRef) => {
      if (document.sha256 !== reference.sha256 || document.bytes !== reference.bytes ||
          !sameId(document.requestId, owner.requestId) || document.toolCallId !== owner.toolCallId || document.toolName !== owner.toolName)
        throw toolFailure('工具历史回执与归档身份不匹配。', 'TOOL_RESULT_REFERENCE_MISMATCH', 409);
      const projected = publicToolResult(document.canonical, { resultRef });
      return document.toolName === 'knowledge.search' ? projectEvidenceSearchResult(projected, resultRef.id) : projected;
    }, { maximumReadBytes: reference.bytes + TOOL_RESULT_METADATA_BYTES });
  }

  /** Resolve one canonical tuple only after checking the caller's formal receipt and archive owner.
   * 核对正式回执与归档归属后，只返回一条 canonical 身份元组，不返回旧正文。 */
  async evidenceReference(context, reference, owner, referenceNumber, { signal } = {}) {
    if (!reference || !Number.isSafeInteger(reference.bytes) || reference.bytes < 0 || reference.bytes > MAX_TOOL_RESULT_BYTES ||
        !/^[0-9a-f]{64}$/i.test(reference.sha256 ?? '') || owner?.toolName !== 'knowledge.search' ||
        !Number.isSafeInteger(referenceNumber) || referenceNumber < 1 || referenceNumber > MAX_EVIDENCE_REFERENCES)
      throw toolFailure('证据回执无效。', 'INVALID_EVIDENCE_REFERENCE');
    signal?.throwIfAborted();
    const result = await this._load(context, reference.id, document => {
      if (document.sha256 !== reference.sha256 || document.bytes !== reference.bytes ||
          !sameId(document.requestId, owner.requestId) || document.toolCallId !== owner.toolCallId || document.toolName !== owner.toolName)
        throw toolFailure('证据回执与归档身份不匹配。', 'TOOL_RESULT_REFERENCE_MISMATCH', 409);
      const items = document.canonical.structuredContent?.items;
      if (!Array.isArray(items) || items.length > MAX_EVIDENCE_REFERENCES || referenceNumber > items.length)
        throw toolFailure('证据条目不存在。', 'EVIDENCE_REFERENCE_NOT_FOUND', 404);
      const item = items[referenceNumber - 1], descriptor = validatedEvidenceReference(item);
      return { ...descriptor, sourceRef: item.sourceRef,
        ...(typeof item.title === 'string' ? { title: item.title } : {}),
        ...(typeof item.sourceType === 'string' ? { sourceType: item.sourceType } : {}),
        ...(item.locator && typeof item.locator === 'object' && !Array.isArray(item.locator) ? { locator: structuredClone(item.locator) } : {}),
        ...(item.bindingRevision !== undefined ? { bindingRevision: item.bindingRevision } : {}) };
    }, { maximumReadBytes: reference.bytes + TOOL_RESULT_METADATA_BYTES });
    signal?.throwIfAborted();
    return result;
  }

  /**
   * Native model state shares the protected result archive, never an independently writable chat log.
   * 原生模型状态共用受保护结果存储，不能另建可独立写入的聊天日志。
   */
  async saveModelContinuation(context, { origin, prefixFingerprint, round, continuation }) {
    return this.save(context, { id: `model-round-${round}`, name: 'model.continuation' },
      { content: [], _meta: { modelContinuation: { version: 1, origin, prefixFingerprint, round, continuation } } });
  }

  async modelContinuation(context, reference, { origin, prefixFingerprint }) {
    return this._load(context, reference.id, document => {
      const value = document.canonical?._meta?.modelContinuation;
      if (document.toolName !== 'model.continuation' || document.sha256 !== reference.sha256 ||
          value?.version !== 1 || value.prefixFingerprint !== prefixFingerprint ||
          JSON.stringify(value.origin) !== JSON.stringify(origin)) return null;
      return structuredClone(value.continuation);
    });
  }
}

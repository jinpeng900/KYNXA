import { createHash, randomUUID } from 'node:crypto';
import { lstat, mkdir, open, readFile, readdir, rename, rmdir, unlink } from 'node:fs/promises';
import { dirname, join, parse, relative } from 'node:path';
import { boundedInteger, inspectLocalPath, revalidateLocalPathBinding, toolFailure } from '../platform/tool-paths.mjs';
import { searchFilesystem } from './filesystem-search.mjs';

export const MAX_TOOL_FILE_BYTES = 1024 * 1024;
const fileQueues = new Map();
const decoder = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true });
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');
const pathProperty = { type: 'string', description: 'Relative to the linked work folder, or an absolute path with an explicit reason.' };
const reasonProperty = { type: 'string', description: 'Required explanation for access outside the linked work folder.' };
const hashProperty = { type: ['string', 'null'], description: 'SHA-256 returned by read/stat; null only when creating a new file or deleting an empty directory.' };
const descriptor = (name, description, properties, required = []) => ({ name: `filesystem.${name}`, description,
  inputSchema: { type: 'object', properties: { path: pathProperty, reason: reasonProperty, ...properties }, required, additionalProperties: false }, source: 'builtin' });

export const filesystemDescriptors = [
  descriptor('list', 'List one directory without following links. Returns bounded entries.', { limit: { type: 'integer', minimum: 1, maximum: 500 } }),
  descriptor('read', 'Read UTF-8 text (file <=1 MiB) with full-file SHA-256. Optional startLine/endLine select inclusive 1-based lines. Continue nextOffset while hasMore, retaining the same line range; restart if hash changes.',
    { maxBytes: { type: 'integer', minimum: 1, maximum: MAX_TOOL_FILE_BYTES }, maxChars: { type: 'integer', minimum: 1, maximum: 64000 },
      offset: { type: 'integer', minimum: 0, maximum: MAX_TOOL_FILE_BYTES, description: 'Absolute UTF-16 position; use nextOffset to avoid splitting characters.' },
      startLine: { type: 'integer', minimum: 1, maximum: MAX_TOOL_FILE_BYTES + 1 },
      endLine: { type: 'integer', minimum: 1, maximum: MAX_TOOL_FILE_BYTES + 1 } }, ['path']),
  descriptor('stat', 'Inspect a file/directory. Small regular files include a SHA-256 for conflict-safe changes.', {}, ['path']),
  descriptor('search', 'Find literal UTF-8 content or paths without an index; path mode reads names only. Continue nextCursor while hasMore, even with no matches; retain query/options. Ignore files apply by default. Changed sources require a fresh search.',
    { query: { type: 'string', minLength: 1, maxLength: 200 }, maxMatches: { type: 'integer', minimum: 1, maximum: 200 },
      mode: { type: 'string', enum: ['content', 'path'], description: 'Default content; path matches a literal relative-path/name substring (case-insensitive on Windows).' },
      respectIgnoreFiles: { type: 'boolean', description: 'Default true: honor .gitignore and .ignore. False includes ignored files; protected paths and generated-directory exclusions remain.' },
      recursive: { type: 'boolean' }, cursor: { type: 'string', minLength: 32, maxLength: 32,
        description: 'Opaque nextCursor from the preceding page in this request; omit to start over.' } }, ['query']),
  descriptor('write', 'Atomically write UTF-8 text. Existing files require their exact expectedHash; new files require null. Parent must exist.',
    { content: { type: 'string' }, expectedHash: hashProperty }, ['path', 'content', 'expectedHash']),
  descriptor('edit', 'Replace exactly one literal occurrence in a UTF-8 file. Requires its exact expectedHash and preserves all other text.',
    { oldText: { type: 'string', minLength: 1 }, newText: { type: 'string' }, expectedHash: { type: 'string' } }, ['path', 'oldText', 'newText', 'expectedHash']),
  descriptor('delete', 'Delete one regular file with expectedHash, or one empty directory with null. Never deletes recursively or the work root.',
    { expectedHash: hashProperty }, ['path', 'expectedHash']),
  descriptor('mkdir', 'Create one directory. Its parent must exist; existing directories are kept.', {}, ['path'])
];

async function queued(path, operation) {
  const previous = fileQueues.get(path) ?? Promise.resolve(), result = previous.catch(() => {}).then(operation);
  fileQueues.set(path, result);
  try { return await result; }
  finally { if (fileQueues.get(path) === result) fileQueues.delete(path); }
}

function text(bytes) {
  try {
    const value = decoder.decode(bytes);
    if (value.includes('\0')) throw new Error('binary');
    return value;
  } catch { throw toolFailure('文件不是受支持的 UTF-8 文本。', 'NON_TEXT_FILE'); }
}

function encode(content) {
  if (typeof content !== 'string' || content.includes('\0') || !content.isWellFormed()) throw toolFailure('请提供有效的 UTF-8 文本。');
  const bytes = Buffer.from(content, 'utf8');
  if (bytes.length > MAX_TOOL_FILE_BYTES) throw toolFailure('文件内容超过工具的 1 MiB 上限。', 'TOOL_FILE_TOO_LARGE');
  return bytes;
}

async function readBounded(path, maxBytes = MAX_TOOL_FILE_BYTES) {
  const info = await inspectLocalPath(path, { allowHardLinks: true });
  if (!info.isFile()) throw toolFailure('路径不是普通文件。');
  if (info.size > maxBytes) throw toolFailure('文件超过本次读取的大小上限。', 'TOOL_FILE_TOO_LARGE');
  const bytes = await readFile(path);
  if (bytes.length > maxBytes) throw toolFailure('文件在读取期间超过大小上限。', 'TOOL_FILE_TOO_LARGE');
  return { info, bytes, hash: sha256(bytes) };
}

function textPage(full, offset, maxChars) {
  if (offset > full.length) throw toolFailure('offset 超出当前文件长度，请从 0 重新读取。');
  if (offset > 0 && offset < full.length && /[\uD800-\uDBFF]/.test(full[offset - 1]) && /[\uDC00-\uDFFF]/.test(full[offset]))
    throw toolFailure('offset 不能位于字符代理对中间，请使用上一页的 nextOffset。');
  let nextOffset = Math.min(full.length, offset + maxChars);
  if (nextOffset < full.length && /[\uD800-\uDBFF]/.test(full[nextOffset - 1]) && /[\uDC00-\uDFFF]/.test(full[nextOffset])) {
    // A one-unit request must still return one complete supplementary character and advance.
    // 即使请求一个计量单位，也应返回完整补充字符并推进游标。
    nextOffset += nextOffset - offset === 1 ? 1 : -1;
  }
  const hasMore = nextOffset < full.length;
  return { content: full.slice(offset, nextOffset), offset, nextOffset, totalCharacters: full.length,
    hasMore, truncated: offset > 0 || hasMore };
}

function lineRangePage(full, input, maxChars) {
  const requestedStartLine = boundedInteger(input.startLine, 1, 1, MAX_TOOL_FILE_BYTES + 1);
  const requestedEndLine = boundedInteger(input.endLine, MAX_TOOL_FILE_BYTES + 1, 1, MAX_TOOL_FILE_BYTES + 1);
  if (requestedEndLine < requestedStartLine) throw toolFailure('endLine 不能小于 startLine。');
  let totalLines = 1, rangeStartOffset = requestedStartLine === 1 ? 0 : null, rangeEndOffset = full.length;
  for (let index = full.indexOf('\n'); index >= 0; index = full.indexOf('\n', index + 1)) {
    totalLines++;
    if (totalLines === requestedStartLine) rangeStartOffset = index + 1;
    if (totalLines === requestedEndLine + 1) rangeEndOffset = index + 1;
  }
  if (rangeStartOffset === null) throw toolFailure('startLine 超出当前文件行数，请重新读取文件状态。');
  const offset = boundedInteger(input.offset, rangeStartOffset, 0, MAX_TOOL_FILE_BYTES);
  if (offset < rangeStartOffset || offset > rangeEndOffset) throw toolFailure('offset 不在指定行范围中，请使用该范围返回的 nextOffset。');
  // Character paging remains absolute, so a long individual line can continue without repeating its prefix.
  // 字符分页始终使用绝对位置；单行过长时也能续读，不会重复该行前半段。
  const page = textPage(full, offset, Math.min(maxChars, rangeEndOffset - offset));
  const hasMore = page.nextOffset < rangeEndOffset;
  let nextLine = requestedStartLine;
  for (let index = full.indexOf('\n', rangeStartOffset); index >= 0 && index < page.nextOffset; index = full.indexOf('\n', index + 1)) nextLine++;
  return { ...page, hasMore, lineRangeHasMore: hasMore, hasMoreInFile: page.nextOffset < full.length,
    nextLine: hasMore ? nextLine : null,
    lineRange: { startLine: requestedStartLine, endLine: Math.min(requestedEndLine, totalLines), totalLines,
      startOffset: rangeStartOffset, endOffset: rangeEndOffset } };
}

async function checkHash(path, expectedHash) {
  const info = await inspectLocalPath(path, { allowMissing: true });
  if (!info) {
    if (expectedHash !== null) throw toolFailure('文件已不存在或版本不匹配，请重新读取。', 'TOOL_FILE_CONFLICT', 409);
    return null;
  }
  if (!info.isFile()) throw toolFailure('路径不是普通文件。');
  const value = await readBounded(path);
  if (typeof expectedHash !== 'string' || !/^[a-f0-9]{64}$/.test(expectedHash) || value.hash !== expectedHash)
    throw toolFailure('文件已更新，请重新读取后再修改。', 'TOOL_FILE_CONFLICT', 409);
  return value;
}

async function atomicText(path, bytes, expectedHash, signal, pathBinding) {
  const original = await checkHash(path, expectedHash), parent = dirname(path);
  const parentInfo = await inspectLocalPath(parent);
  if (!parentInfo.isDirectory()) throw toolFailure('文件父目录不存在。');
  const temporary = join(parent, `.kynxa-write-${randomUUID()}.tmp`);
  let handle;
  try {
    handle = await open(temporary, 'wx', original ? original.info.mode & 0o777 : 0o600);
    await handle.writeFile(bytes);
    await handle.sync();
    await handle.close(); handle = null;
    // Recheck links and external edits immediately before the atomic replacement.
    // 原子替换前立即重新检查链接及外部修改。
    await inspectLocalPath(parent);
    await checkHash(path, expectedHash);
    if (pathBinding) await revalidateLocalPathBinding(pathBinding);
    signal?.throwIfAborted();
    await rename(temporary, path);
  } finally {
    await handle?.close();
    await unlink(temporary).catch(error => { if (error.code !== 'ENOENT') throw error; });
  }
  return { path, bytes: bytes.length, sha256: sha256(bytes) };
}

export async function executeFilesystem(name, context, input, path, signal, { protectedRoots = [], denyRead = () => false,
  pathBinding, searchOwner } = {}) {
  signal?.throwIfAborted();
  if (pathBinding) await revalidateLocalPathBinding(pathBinding);
  if (name === 'filesystem.list') {
    const info = await inspectLocalPath(path);
    if (!info.isDirectory()) throw toolFailure('路径不是目录。');
    const entries = await readdir(path, { withFileTypes: true }), limit = boundedInteger(input.limit, 200, 1, 500);
    entries.sort((left, right) => left.name.localeCompare(right.name));
    return { path, entries: entries.slice(0, limit).map(entry => ({ name: entry.name,
      type: entry.isSymbolicLink() ? 'link' : entry.isDirectory() ? 'directory' : entry.isFile() ? 'file' : 'other' })), truncated: entries.length > limit };
  }
  if (name === 'filesystem.read') {
    const value = await readBounded(path, boundedInteger(input.maxBytes, MAX_TOOL_FILE_BYTES, 1, MAX_TOOL_FILE_BYTES));
    const full = text(value.bytes), maxChars = boundedInteger(input.maxChars, 32000, 1, 64000);
    const page = input.startLine !== undefined || input.endLine !== undefined ? lineRangePage(full, input, maxChars)
      : textPage(full, boundedInteger(input.offset, 0, 0, MAX_TOOL_FILE_BYTES), maxChars);
    return { path, sha256: value.hash, bytes: value.bytes.length, ...page };
  }
  if (name === 'filesystem.stat') {
    const info = await inspectLocalPath(path, { allowHardLinks: true });
    const hash = info.isFile() && info.size <= MAX_TOOL_FILE_BYTES ? (await readBounded(path)).hash : null;
    return { path, type: info.isDirectory() ? 'directory' : info.isFile() ? 'file' : 'other', bytes: info.size,
      modifiedAt: info.mtime.toISOString(), sha256: hash };
  }
  if (name === 'filesystem.search') return searchFilesystem(context, path, input, signal,
    { protectedRoots, denyRead, searchOwner, workspaceRoot: pathBinding?.workspaceRoot ?? context.workspaceRoot });
  return queued(path, async () => {
    signal?.throwIfAborted();
    if (pathBinding) await revalidateLocalPathBinding(pathBinding);
    if (name === 'filesystem.write') return atomicText(path, encode(input.content), input.expectedHash, signal, pathBinding);
    if (name === 'filesystem.edit') {
      const original = await checkHash(path, input.expectedHash), full = text(original.bytes);
      if (typeof input.oldText !== 'string' || !input.oldText || typeof input.newText !== 'string') throw toolFailure('请提供有效的替换文本。');
      const index = full.indexOf(input.oldText);
      if (index < 0 || full.indexOf(input.oldText, index + 1) >= 0)
        throw toolFailure('替换文本必须在文件中恰好出现一次。', 'TOOL_EDIT_NOT_UNIQUE', 409);
      return atomicText(path, encode(full.slice(0, index) + input.newText + full.slice(index + input.oldText.length)), input.expectedHash, signal, pathBinding);
    }
    if (name === 'filesystem.delete') {
      // Deleting an alias must never be translated into deleting the shared real target.
      // 删除链接入口不能被转换为删除共享真实目标；链接入口管理留给明确的专用操作。
      if (pathBinding && (await lstat(pathBinding.requestedPath)).isSymbolicLink())
        throw toolFailure('此路径是链接入口，本次没有删除链接或其真实目标。', 'UNSAFE_TOOL_PATH', 403);
      const workspaceRoot = pathBinding?.workspaceRoot ?? context.workspaceRoot;
      if (path === parse(path).root || (workspaceRoot && relative(workspaceRoot, path) === ''))
        throw toolFailure('不能删除工作或文件系统根目录。', 'UNSAFE_TOOL_PATH', 403);
      const info = await inspectLocalPath(path);
      if (info.isDirectory()) {
        if (input.expectedHash !== null) throw toolFailure('删除空目录时 expectedHash 必须为 null。');
        await rmdir(path);
      } else { await checkHash(path, input.expectedHash); await unlink(path); }
      return { path, deleted: true };
    }
    if (name === 'filesystem.mkdir') {
      const info = await inspectLocalPath(path, { allowMissing: true });
      if (info) {
        if (!info.isDirectory()) throw toolFailure('此路径已存在其他文件。', 'TOOL_FILE_CONFLICT', 409);
        return { path, created: false };
      }
      const parent = await inspectLocalPath(dirname(path));
      if (!parent.isDirectory()) throw toolFailure('父目录不存在。');
      await mkdir(path, { mode: 0o700 });
      return { path, created: true };
    }
    throw toolFailure('未知文件工具。', 'TOOL_NOT_FOUND', 404);
  });
}

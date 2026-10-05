import { createHash, randomUUID } from 'node:crypto';
import { mkdir, open, readFile, readdir, rename, rmdir, unlink } from 'node:fs/promises';
import { basename, dirname, join, parse, relative } from 'node:path';
import { boundedInteger, inspectLocalPath, toolFailure, within } from '../platform/tool-paths.mjs';

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
  descriptor('read', 'Read a UTF-8 text page and full-file SHA-256. Continue with nextOffset while hasMore; restart if the hash changes.',
    { maxBytes: { type: 'integer', minimum: 1, maximum: MAX_TOOL_FILE_BYTES }, maxChars: { type: 'integer', minimum: 1, maximum: 64000 },
      offset: { type: 'integer', minimum: 0, maximum: MAX_TOOL_FILE_BYTES, description: 'UTF-16 position; use nextOffset to avoid splitting characters.' } }, ['path']),
  descriptor('stat', 'Inspect a file/directory. Small regular files include a SHA-256 for conflict-safe changes.', {}, ['path']),
  descriptor('search', 'Search UTF-8 files for literal text. Does not follow links; traversal, file size and matches are bounded.',
    { query: { type: 'string', minLength: 1, maxLength: 200 }, maxMatches: { type: 'integer', minimum: 1, maximum: 200 },
      recursive: { type: 'boolean' } }, ['query']),
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
  const info = await inspectLocalPath(path);
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

async function atomicText(path, bytes, expectedHash, signal) {
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
    signal?.throwIfAborted();
    await rename(temporary, path);
  } finally {
    await handle?.close();
    await unlink(temporary).catch(error => { if (error.code !== 'ENOENT') throw error; });
  }
  return { path, bytes: bytes.length, sha256: sha256(bytes) };
}

export async function executeFilesystem(name, context, input, path, signal, { protectedRoots = [], denyRead = () => false } = {}) {
  signal?.throwIfAborted();
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
    const offset = boundedInteger(input.offset, 0, 0, MAX_TOOL_FILE_BYTES);
    return { path, sha256: value.hash, bytes: value.bytes.length, ...textPage(full, offset, maxChars) };
  }
  if (name === 'filesystem.stat') {
    const info = await inspectLocalPath(path);
    const hash = info.isFile() && info.size <= MAX_TOOL_FILE_BYTES ? (await readBounded(path)).hash : null;
    return { path, type: info.isDirectory() ? 'directory' : info.isFile() ? 'file' : 'other', bytes: info.size,
      modifiedAt: info.mtime.toISOString(), sha256: hash };
  }
  if (name === 'filesystem.search') return searchFiles(path, input, signal, protectedRoots, denyRead);
  return queued(path, async () => {
    signal?.throwIfAborted();
    if (name === 'filesystem.write') return atomicText(path, encode(input.content), input.expectedHash, signal);
    if (name === 'filesystem.edit') {
      const original = await checkHash(path, input.expectedHash), full = text(original.bytes);
      if (typeof input.oldText !== 'string' || !input.oldText || typeof input.newText !== 'string') throw toolFailure('请提供有效的替换文本。');
      const index = full.indexOf(input.oldText);
      if (index < 0 || full.indexOf(input.oldText, index + 1) >= 0)
        throw toolFailure('替换文本必须在文件中恰好出现一次。', 'TOOL_EDIT_NOT_UNIQUE', 409);
      return atomicText(path, encode(full.slice(0, index) + input.newText + full.slice(index + input.oldText.length)), input.expectedHash, signal);
    }
    if (name === 'filesystem.delete') {
      if (path === parse(path).root || (context.workspaceRoot && relative(context.workspaceRoot, path) === ''))
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

async function searchFiles(path, input, signal, protectedRoots, denyRead) {
  if (typeof input.query !== 'string' || !input.query || input.query.length > 200) throw toolFailure('搜索文本须为 1–200 个字符。');
  const maxMatches = boundedInteger(input.maxMatches, 100, 1, 200), matches = [];
  let visited = 0, visitedEntries = 0, skipped = 0, truncated = false;
  const excluded = new Set(['.git', 'node_modules', 'bin', 'obj']);
  const walk = async (current, depth) => {
    signal?.throwIfAborted();
    if (protectedRoots.some(root => within(root, current)) || denyRead(current)) { skipped++; return; }
    if (matches.length >= maxMatches || visited >= 1000 || visitedEntries >= 2000 || depth > 8) { truncated = true; return; }
    visitedEntries++;
    let info;
    try { info = await inspectLocalPath(current); }
    catch (error) {
      if (['UNSAFE_TOOL_PATH', 'ENOENT', 'EACCES', 'EPERM'].includes(error.code)) { skipped++; return; }
      throw error;
    }
    if (info.isDirectory()) {
      const entries = await readdir(current, { withFileTypes: true });
      for (const entry of entries.sort((left, right) => left.name.localeCompare(right.name))) {
        if (entry.isSymbolicLink() || excluded.has(entry.name)) { skipped++; continue; }
        if (entry.isDirectory() && (input.recursive === false || depth >= 8)) continue;
        await walk(join(current, entry.name), depth + 1);
        if (matches.length >= maxMatches || visited >= 1000 || visitedEntries >= 2000) { truncated = true; break; }
      }
      return;
    }
    if (!info.isFile()) { skipped++; return; }
    visited++;
    if (info.size > MAX_TOOL_FILE_BYTES) { skipped++; return; }
    let value;
    try { value = text((await readBounded(current)).bytes); }
    catch (error) { if (error.code === 'NON_TEXT_FILE') { skipped++; return; } throw error; }
    const lines = value.split(/\r?\n/);
    for (let index = 0; index < lines.length && matches.length < maxMatches; index++)
      if (lines[index].includes(input.query)) matches.push({ path: relative(path, current) || basename(current), line: index + 1, text: lines[index].slice(0, 400) });
  };
  await walk(path, 0);
  return { path, query: input.query, matches, visitedFiles: visited, skipped, truncated };
}

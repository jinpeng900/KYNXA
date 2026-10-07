import { createHash } from 'node:crypto';
import { constants } from 'node:fs';
import { open, readdir } from 'node:fs/promises';
import { basename, dirname, extname, join, relative, resolve } from 'node:path';
import { inspectLocalPath, toolFailure, within } from '../../platform/tool-paths.mjs';
import { isSensitiveFilePath } from '../sensitive-files.mjs';
import ignore from 'ignore';

const TEXT_EXTENSIONS = new Set(['.txt', '.md', '.markdown', '.mjs', '.cjs', '.js', '.jsx', '.ts', '.mts', '.cts', '.tsx', '.cs', '.xaml', '.html',
  '.css', '.json', '.yaml', '.yml', '.xml', '.toml', '.csv', '.py', '.rs', '.go', '.java', '.cpp', '.c', '.h', '.sql', '.ps1']);
const IGNORED_DIRECTORIES = new Set(['.git', '.vs', '.idea', 'node_modules', 'bin', 'obj', 'target', '__pycache__', '.venv', 'venv', '.kynxa', '.sandbox-runtime', '.sandbox-temp']);
const hash = value => createHash('sha256').update(value).digest('hex');

async function readOrdinaryFile(path, maximumBytes, signal) {
  signal?.throwIfAborted();
  const before = await inspectLocalPath(path);
  signal?.throwIfAborted();
  if (!before.isFile() || before.size > maximumBytes) throw toolFailure('资料文件超过允许大小。', 'INVALID_RETRIEVAL_SOURCE', 400);
  const handle = await open(path, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0));
  try {
    const opened = await handle.stat();
    if (!opened.isFile() || opened.nlink !== 1 || opened.ino !== before.ino || opened.dev !== before.dev)
      throw toolFailure('资料路径已变化。', 'UNSAFE_TOOL_PATH', 403);
    if (opened.size > maximumBytes) throw toolFailure('资料文件超过允许大小。', 'INVALID_RETRIEVAL_SOURCE', 400);
    // Bound allocation even if another process grows the file while this read is in progress.
    // 即使其他进程在读取期间扩展文件，分配和读取也不能突破单文件预算。
    const buffer = Buffer.allocUnsafe(Math.min(opened.size + 1, maximumBytes + 1));
    let readBytes = 0;
    while (readBytes < buffer.length) {
      signal?.throwIfAborted();
      const read = await handle.read(buffer, readBytes, buffer.length - readBytes, readBytes);
      if (!read.bytesRead) break;
      readBytes += read.bytesRead;
    }
    const bytes = buffer.subarray(0, readBytes); signal?.throwIfAborted();
    const after = await inspectLocalPath(path);
    signal?.throwIfAborted();
    if (readBytes !== opened.size || after.size !== opened.size || after.mtimeMs !== opened.mtimeMs || after.ino !== opened.ino || after.dev !== opened.dev)
      throw toolFailure('读取期间资料发生变化。', 'STALE_RETRIEVAL_SOURCE', 409);
    return { bytes, metadata: fileMetadata(after) };
  } finally { await handle.close(); }
}

function fileMetadata(info) {
  return { sizeBytes: info.size, mtimeMs: info.mtimeMs, ctimeMs: info.ctimeMs, device: info.dev, inode: info.ino };
}

export function sameSourceMetadata(previous, current) {
  return previous && ['sizeBytes', 'mtimeMs', 'ctimeMs', 'device', 'inode'].every(key => previous[key] === current[key]);
}

/** Reads only bounded ordinary text; indexed content never bypasses the existing path policy.
 * 只读取有界普通文本，索引内容不能绕过现有路径策略。 */
export async function readSourceFile(path, { root = null, excludedRoots = [], maximumSourceBytes = 2 * 1024 * 1024, signal } = {}) {
  signal?.throwIfAborted(); path = resolve(path);
  if (root && !within(resolve(root), path) || isSensitiveFilePath(path) || excludedRoots.some(folder => within(folder, path)))
    throw toolFailure('此路径不允许加入检索资料。', 'PROTECTED_RETRIEVAL_SOURCE', 403);
  if (!TEXT_EXTENSIONS.has(extname(path).toLowerCase()))
    throw toolFailure('目前只支持文本与代码资料。', 'UNSUPPORTED_RETRIEVAL_SOURCE', 400);
  const { bytes, metadata } = await readOrdinaryFile(path, maximumSourceBytes, signal);
  let text;
  try {
    text = bytes[0] === 0xff && bytes[1] === 0xfe ? new TextDecoder('utf-16le', { fatal: true }).decode(bytes.subarray(2))
      : new TextDecoder('utf-8', { fatal: true }).decode(bytes).replace(/^\uFEFF/u, '');
  } catch { throw toolFailure('资料字符编码不受支持。', 'UNSUPPORTED_RETRIEVAL_SOURCE', 400); }
  if (text.includes('\0')) throw toolFailure('二进制文件不能作为文本资料。', 'UNSUPPORTED_RETRIEVAL_SOURCE', 400);
  signal?.throwIfAborted();
  return { path, title: root ? relative(root, path) : basename(path), text, contentHash: hash(text),
    textBytes: Buffer.byteLength(text), metadata };
}

/** Enumerates ordinary sources and reuses verified stat receipts, never cached bodies or path authority.
 * 枚举普通来源并复用已校验的文件状态回执，不把缓存正文或旧路径当作当前授权。 */
export async function* scanSourceTree(path, options = {}) {
  options.signal?.throwIfAborted();
  const root = resolve(path), info = await inspectLocalPath(root);
  options.signal?.throwIfAborted();
  if (info.isFile()) { yield await readSourceFile(root, options); return; }
  if (!info.isDirectory()) throw toolFailure('资料路径无效。', 'INVALID_RETRIEVAL_SOURCE', 400);
  const pending = [{ path: root, rules: [] }]; let visited = 0, bytes = 0, files = 0, cursor = 0;
  const maximumFiles = options.maximumFiles ?? 2048, maximumBytes = options.maximumBytes ?? 32 * 1024 * 1024;
  const maximumEntries = options.maximumEntries ?? 20000;
  const stats = options.stats ?? {};
  for (const key of ['fileReads', 'reusedFiles', 'scannedFiles', 'visitedEntries', 'bytesRead']) stats[key] ??= 0;
  while (cursor < pending.length) {
    options.signal?.throwIfAborted(); const { path: directory, rules: parentRules } = pending[cursor++];
    if (options.excludedRoots?.some(folder => within(folder, directory))) continue;
    await inspectLocalPath(directory);
    const rules = [...parentRules];
    try { rules.push({ root: directory, matcher: ignore().add((await readOrdinaryFile(join(directory, '.gitignore'), 128 * 1024, options.signal)).bytes.toString('utf8')) }); }
    catch (error) { if (!['ENOENT', 'INVALID_RETRIEVAL_SOURCE', 'UNSAFE_TOOL_PATH'].includes(error.code)) throw error; }
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      options.signal?.throwIfAborted();
      stats.visitedEntries++;
      if (++visited > maximumEntries) throw toolFailure('资料目录超过当前扫描预算。', 'RETRIEVAL_SCAN_LIMIT', 413);
      const candidate = join(directory, entry.name);
      if (entry.isSymbolicLink() || isSensitiveFilePath(candidate)) continue;
      if (rules.some(rule => rule.matcher.ignores(relative(rule.root, candidate).replaceAll('\\', '/') + (entry.isDirectory() ? '/' : '')))) continue;
      if (entry.isDirectory() && !IGNORED_DIRECTORIES.has(entry.name.toLowerCase())) pending.push({ path: candidate, rules });
      else if (entry.isFile() && TEXT_EXTENSIONS.has(extname(candidate).toLowerCase())) {
        try {
          const relativePath = relative(root, candidate), previous = options.previousFiles?.get(relativePath);
          let file, reused = false;
          if (previous && !options.changedPaths?.has(relativePath)) {
            const current = await inspectLocalPath(candidate);
            if (!current.isFile() || current.size > (options.maximumSourceBytes ?? 2 * 1024 * 1024)) continue;
            const metadata = fileMetadata(current);
            reused = sameSourceMetadata(previous.metadata, metadata);
            if (reused) file = { path: candidate, title: relativePath, contentHash: previous.contentHash,
              textBytes: previous.textBytes, metadata, reused: true };
          }
          file ??= await readSourceFile(candidate, { ...options, root });
          if (reused) stats.reusedFiles++;
          else { stats.fileReads++; stats.bytesRead += file.metadata.sizeBytes; }
          bytes += file.textBytes; stats.scannedFiles++;
          if (bytes > maximumBytes || files++ >= maximumFiles) throw toolFailure('资料目录超过当前来源或字节预算。', 'RETRIEVAL_SCAN_LIMIT', 413);
          yield file;
        }
        catch (error) { if (!['PROTECTED_RETRIEVAL_SOURCE', 'UNSUPPORTED_RETRIEVAL_SOURCE', 'INVALID_RETRIEVAL_SOURCE', 'ENOENT'].includes(error.code)) throw error; }
      }
    }
  }
  options.signal?.throwIfAborted();
}

export async function readSourceTree(path, options = {}) {
  const result = [];
  for await (const file of scanSourceTree(path, options)) {
    result.push(file.text === undefined ? await readSourceFile(file.path, { ...options, root: resolve(path) }) : file);
  }
  return result;
}

/** Rechecks explicit dirty files against ancestor ignore rules; directory changes request a full reconciliation.
 * 按祖先忽略规则复核明确变化的文件，目录变化交由完整核对处理。 */
export async function* scanSourcePaths(path, dirtyPaths, options = {}) {
  const root = resolve(path), rulesCache = new Map();
  const info = await inspectLocalPath(root);
  if (!info.isDirectory()) throw toolFailure('挂载来源根目录无效。', 'INVALID_RETRIEVAL_SOURCE', 400);
  const stats = options.stats ?? {};
  for (const key of ['fileReads', 'reusedFiles', 'scannedFiles', 'visitedEntries', 'bytesRead']) stats[key] ??= 0;
  for (const relativePath of dirtyPaths) {
    options.signal?.throwIfAborted();
    const candidate = resolve(root, relativePath);
    if (!within(root, candidate) || candidate === root)
      throw toolFailure('文件变化范围需要完整核对。', 'RETRIEVAL_FULL_SCAN_REQUIRED', 409);
    stats.visitedEntries++;
    const directories = [];
    for (let directory = dirname(candidate); within(root, directory); directory = dirname(directory)) {
      directories.unshift(directory);
      if (directory === root) break;
    }
    let protectedPath = isSensitiveFilePath(candidate) ||
      options.excludedRoots?.some(folder => within(folder, candidate));
    for (const directory of directories) {
      options.signal?.throwIfAborted();
      if (directory !== root && IGNORED_DIRECTORIES.has(basename(directory).toLowerCase())) protectedPath = true;
      if (!rulesCache.has(directory)) {
        let matcher;
        try {
          const { bytes } = await readOrdinaryFile(join(directory, '.gitignore'), 128 * 1024, options.signal);
          matcher = ignore().add(bytes.toString('utf8'));
        } catch (error) {
          if (!['ENOENT', 'INVALID_RETRIEVAL_SOURCE', 'UNSAFE_TOOL_PATH'].includes(error.code)) throw error;
        }
        rulesCache.set(directory, matcher);
      }
      if (rulesCache.get(directory)?.ignores(relative(directory, candidate).replaceAll('\\', '/'))) protectedPath = true;
    }
    if (protectedPath) { yield { path: candidate, title: relativePath, missing: true }; continue; }
    const current = await inspectLocalPath(candidate, { allowMissing: true });
    if (!current) { yield { path: candidate, title: relativePath, missing: true }; continue; }
    if (current.isDirectory()) throw toolFailure('目录变化需要完整核对。', 'RETRIEVAL_FULL_SCAN_REQUIRED', 409);
    if (!TEXT_EXTENSIONS.has(extname(candidate).toLowerCase())) {
      yield { path: candidate, title: relativePath, missing: true }; continue;
    }
    try {
      const file = await readSourceFile(candidate, { ...options, root });
      stats.fileReads++; stats.scannedFiles++; stats.bytesRead += file.metadata.sizeBytes;
      yield file;
    } catch (error) {
      if (['ENOENT', 'PROTECTED_RETRIEVAL_SOURCE', 'UNSUPPORTED_RETRIEVAL_SOURCE', 'INVALID_RETRIEVAL_SOURCE'].includes(error.code))
        yield { path: candidate, title: relativePath, missing: true };
      else throw error;
    }
  }
}

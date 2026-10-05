import { createHash } from 'node:crypto';
import { constants } from 'node:fs';
import { open, readdir } from 'node:fs/promises';
import { basename, extname, join, relative, resolve } from 'node:path';
import { inspectLocalPath, toolFailure, within } from '../../platform/tool-paths.mjs';
import { isSensitiveFilePath } from '../sensitive-files.mjs';
import ignore from 'ignore';

const TEXT_EXTENSIONS = new Set(['.txt', '.md', '.markdown', '.mjs', '.js', '.jsx', '.ts', '.tsx', '.cs', '.xaml', '.html',
  '.css', '.json', '.yaml', '.yml', '.xml', '.toml', '.csv', '.py', '.rs', '.go', '.java', '.cpp', '.c', '.h', '.sql', '.ps1']);
const IGNORED_DIRECTORIES = new Set(['.git', '.vs', '.idea', 'node_modules', 'bin', 'obj', 'target', '__pycache__', '.venv', 'venv', '.kynxa', '.sandbox-runtime', '.sandbox-temp']);
const hash = value => createHash('sha256').update(value).digest('hex');

async function readOrdinaryFile(path, maximumBytes, signal) {
  const before = await inspectLocalPath(path);
  if (!before.isFile() || before.size > maximumBytes) throw toolFailure('资料文件超过允许大小。', 'INVALID_RETRIEVAL_SOURCE', 400);
  const handle = await open(path, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0));
  try {
    const opened = await handle.stat();
    if (!opened.isFile() || opened.nlink !== 1 || opened.ino !== before.ino || opened.dev !== before.dev)
      throw toolFailure('资料路径已变化。', 'UNSAFE_TOOL_PATH', 403);
    const bytes = await handle.readFile(); signal?.throwIfAborted();
    const after = await inspectLocalPath(path);
    if (after.size !== opened.size || after.mtimeMs !== opened.mtimeMs || after.ino !== opened.ino || after.dev !== opened.dev)
      throw toolFailure('读取期间资料发生变化。', 'STALE_RETRIEVAL_SOURCE', 409);
    return bytes;
  } finally { await handle.close(); }
}

/** Reads only bounded ordinary text; indexed content never bypasses the existing path policy.
 * 只读取有界普通文本，索引内容不能绕过现有路径策略。 */
export async function readSourceFile(path, { root = null, excludedRoots = [], signal } = {}) {
  signal?.throwIfAborted(); path = resolve(path);
  if (root && !within(resolve(root), path) || isSensitiveFilePath(path) || excludedRoots.some(folder => within(folder, path)))
    throw toolFailure('此路径不允许加入检索资料。', 'PROTECTED_RETRIEVAL_SOURCE', 403);
  if (!TEXT_EXTENSIONS.has(extname(path).toLowerCase()))
    throw toolFailure('目前只支持文本与代码资料。', 'UNSUPPORTED_RETRIEVAL_SOURCE', 400);
  const bytes = await readOrdinaryFile(path, 2 * 1024 * 1024, signal);
  let text;
  try {
    text = bytes[0] === 0xff && bytes[1] === 0xfe ? new TextDecoder('utf-16le', { fatal: true }).decode(bytes.subarray(2))
      : new TextDecoder('utf-8', { fatal: true }).decode(bytes).replace(/^\uFEFF/u, '');
  } catch { throw toolFailure('资料字符编码不受支持。', 'UNSUPPORTED_RETRIEVAL_SOURCE', 400); }
  if (text.includes('\0')) throw toolFailure('二进制文件不能作为文本资料。', 'UNSUPPORTED_RETRIEVAL_SOURCE', 400);
  return { path, title: root ? relative(root, path) : basename(path), text, contentHash: hash(text) };
}

export async function readSourceTree(path, options = {}) {
  const root = resolve(path), info = await inspectLocalPath(root), result = [];
  if (info.isFile()) return [await readSourceFile(root, options)];
  if (!info.isDirectory()) throw toolFailure('资料路径无效。', 'INVALID_RETRIEVAL_SOURCE', 400);
  const pending = [{ path: root, rules: [] }]; let visited = 0, bytes = 0;
  const maximumFiles = options.maximumFiles ?? 2048, maximumBytes = options.maximumBytes ?? 32 * 1024 * 1024;
  while (pending.length) {
    options.signal?.throwIfAborted(); const { path: directory, rules: parentRules } = pending.shift();
    if (options.excludedRoots?.some(folder => within(folder, directory))) continue;
    await inspectLocalPath(directory);
    const rules = [...parentRules];
    try { rules.push({ root: directory, matcher: ignore().add((await readOrdinaryFile(join(directory, '.gitignore'), 128 * 1024, options.signal)).toString('utf8')) }); }
    catch (error) { if (!['ENOENT', 'INVALID_RETRIEVAL_SOURCE', 'UNSAFE_TOOL_PATH'].includes(error.code)) throw error; }
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      if (++visited > 20000) throw toolFailure('资料目录过大，请选择较小的子目录。', 'RETRIEVAL_SCAN_LIMIT', 413);
      const candidate = join(directory, entry.name);
      if (entry.isSymbolicLink() || isSensitiveFilePath(candidate)) continue;
      if (rules.some(rule => rule.matcher.ignores(relative(rule.root, candidate).replaceAll('\\', '/') + (entry.isDirectory() ? '/' : '')))) continue;
      if (entry.isDirectory() && !IGNORED_DIRECTORIES.has(entry.name.toLowerCase())) pending.push({ path: candidate, rules });
      else if (entry.isFile() && TEXT_EXTENSIONS.has(extname(candidate).toLowerCase())) {
        try {
          const file = await readSourceFile(candidate, { ...options, root });
          bytes += Buffer.byteLength(file.text);
          if (bytes > maximumBytes || result.length >= maximumFiles) throw toolFailure('资料批次过大，请选择较小的子目录。', 'RETRIEVAL_SCAN_LIMIT', 413);
          result.push(file);
        }
        catch (error) { if (!['PROTECTED_RETRIEVAL_SOURCE', 'UNSUPPORTED_RETRIEVAL_SOURCE', 'INVALID_RETRIEVAL_SOURCE', 'ENOENT'].includes(error.code)) throw error; }
      }
    }
  }
  return result;
}

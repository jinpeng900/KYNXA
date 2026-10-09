import { createHash } from 'node:crypto';
import { constants } from 'node:fs';
import { open, readdir } from 'node:fs/promises';
import { setTimeout as wait } from 'node:timers/promises';
import { basename, dirname, extname, join, relative, resolve } from 'node:path';
import { inspectLocalPath, toolFailure, within } from '../../platform/tool-paths.mjs';
import { isSensitiveFilePath } from '../sensitive-files.mjs';
import { extractDocumentBytes, documentExtractionCoverage } from './document-extraction.mjs';
import { DOCUMENT_EXTRACTION_LIMITS, DOCUMENT_EXTRACTION_VERSIONS, documentExtractionFailure,
  validateDocumentExtractionLimits } from './document-extraction-contracts.mjs';
import ignore from 'ignore';
import { describeTextFileWindows, readTextFileWindow } from './text-file-windows.mjs';
import { validateSourceFileWindow } from '../../data/retrieval/retrieval-contracts.mjs';

const TEXT_EXTENSIONS = new Set(['.txt', '.md', '.markdown', '.mjs', '.cjs', '.js', '.jsx', '.ts', '.mts', '.cts', '.tsx', '.cs', '.xaml', '.html',
  '.css', '.json', '.yaml', '.yml', '.xml', '.toml', '.csv', '.py', '.pyi', '.rs', '.go', '.java', '.cpp', '.c', '.h', '.sql', '.ps1']);
const DOCUMENT_FORMATS = new Map([['.pdf', 'pdf'], ['.docx', 'docx']]);
const IGNORED_DIRECTORIES = new Set(['.git', '.vs', '.idea', 'node_modules', 'bin', 'obj', 'target', '__pycache__', '.venv', 'venv', '.kynxa', '.sandbox-runtime', '.sandbox-temp']);
const hash = value => createHash('sha256').update(value).digest('hex');
const supportedSource = path => TEXT_EXTENSIONS.has(extname(path).toLowerCase()) || DOCUMENT_FORMATS.has(extname(path).toLowerCase());
const extractionLimitsKey = options => JSON.stringify([options.maximumSourceBytes ?? 32 * 1024 * 1024,
  options.maximumDocumentInputBytes ?? DOCUMENT_EXTRACTION_LIMITS.maximumInputBytes,
  options.maximumDocumentOutputBytes ?? DOCUMENT_EXTRACTION_LIMITS.maximumOutputBytes,
  options.maximumPdfPages ?? DOCUMENT_EXTRACTION_LIMITS.maximumPages, options.ocrBackend === null ? 'disabled' : 'available']);

/** Retry only transient observations; unsupported or malformed documents need a changed source or configuration.
 * 只重试暂时性观测错误；不支持或损坏的文档须等待来源或配置变化，不能无限重复解码。 */
export function classifySourceFailure(error) {
  const code = /^[A-Z][A-Z0-9_]{0,127}$/u.test(error?.code ?? '') ? error.code : 'RETRIEVAL_SOURCE_FAILED';
  if (error?.name === 'AbortError') return { errorCode: code, category: 'cancelled', retryable: false };
  if (['EACCES', 'EPERM', 'UNSAFE_TOOL_PATH', 'PROTECTED_RETRIEVAL_SOURCE'].includes(code))
    return { errorCode: code, category: 'permission', retryable: false };
  if (/^OCR_/u.test(code ?? '')) return { errorCode: code, category: 'ocr-unavailable', retryable: false };
  if (/STALE|SOURCE_CHANGED/u.test(code) || code === 'ENOENT')
    return { errorCode: code, category: 'source-changed', retryable: true };
  if (/RESOURCE|DECODER_BUSY/u.test(code)) return { errorCode: code, category: 'resource', retryable: true };
  if (['EBUSY', 'EAGAIN', 'EMFILE', 'ENFILE', 'ETIMEDOUT', 'DOCUMENT_DECODER_TIMEOUT'].includes(code))
    return { errorCode: code, category: 'temporary', retryable: true };
  if (/LIMIT|BUDGET/u.test(code) || code === 'INVALID_RETRIEVAL_SOURCE')
    return { errorCode: code, category: 'limit', retryable: false };
  if (/UNSUPPORTED/u.test(code)) return { errorCode: code, category: 'unsupported', retryable: false };
  return { errorCode: code, category: 'invalid-source', retryable: false };
}

/** A mounted scan may publish an honest partial view; explicit imports retain their fail-on-limit contract.
 * 挂载扫描可以发布明确的部分覆盖视图；显式导入仍保留超限报错约定。 */
function scanLimit(stats, options, dimension, limit, observed) {
  stats.incomplete = true;
  stats.limit = { dimension, limit, observed, unvisitedCountKnown: false };
  if (options.allowPartial) return false;
  throw Object.assign(toolFailure('资料目录超过当前扫描预算。', 'RETRIEVAL_SCAN_LIMIT', 413), { limit: stats.limit });
}

function recordSourceFailure(stats, root, path, error, directory = false) {
  stats.failedFiles = (stats.failedFiles ?? 0) + 1;
  stats.failures ??= [];
  if (stats.failures.length < 10000) stats.failures.push({ relativePath: relative(root, path),
    ...classifySourceFailure(error), attempts: error.sourceReadAttempts ?? 1,
    ...(error.details?.documentCoverage ? { documentCoverage: error.details.documentCoverage } : {}),
    ...(directory ? { directory: true } : {}) });
  else stats.failureReportTruncated = true;
}

/** Compare extraction rules independently of file stat receipts. / 提取规则独立于文件状态回执进行版本复核。 */
export function sourceExtractionVersion(path) {
  return DOCUMENT_EXTRACTION_VERSIONS[DOCUMENT_FORMATS.get(extname(path).toLowerCase())] ?? null;
}

async function readOrdinaryFile(path, maximumBytes, signal) {
  signal?.throwIfAborted();
  const before = await inspectLocalPath(path);
  signal?.throwIfAborted();
  if (!before.isFile() || before.size > maximumBytes) throw Object.assign(toolFailure('资料文件超过允许大小。', 'INVALID_RETRIEVAL_SOURCE', 400),
    { observedBytes: before.size });
  const handle = await open(path, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0));
  try {
    const opened = await handle.stat();
    if (!opened.isFile() || opened.nlink !== 1 || opened.ino !== before.ino || opened.dev !== before.dev)
      throw toolFailure('资料路径已变化。', 'UNSAFE_TOOL_PATH', 403);
    if (opened.size > maximumBytes) throw Object.assign(toolFailure('资料文件超过允许大小。', 'INVALID_RETRIEVAL_SOURCE', 400),
      { observedBytes: opened.size });
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

/** Reads authorized, bounded text or offline document bytes; extraction never bypasses path policy.
 * 只读取已授权的有界文本或离线文档字节，提取过程不能绕过现有路径策略。 */
async function readSourceFileOnce(path, { root = null, excludedRoots = [], maximumSourceBytes = 32 * 1024 * 1024,
  maximumDocumentInputBytes = DOCUMENT_EXTRACTION_LIMITS.maximumInputBytes,
  maximumDocumentOutputBytes = DOCUMENT_EXTRACTION_LIMITS.maximumOutputBytes,
  maximumPdfPages = DOCUMENT_EXTRACTION_LIMITS.maximumPages, signal, resourceService, pdfPageWindow, ocrBackend } = {}) {
  signal?.throwIfAborted(); path = resolve(path);
  if (root && !within(resolve(root), path) || isSensitiveFilePath(path) || excludedRoots.some(folder => within(folder, path)))
    throw toolFailure('此路径不允许加入检索资料。', 'PROTECTED_RETRIEVAL_SOURCE', 403);
  if (!supportedSource(path))
    throw toolFailure('目前支持文本、代码和基础 PDF/DOCX 文本提取。', 'UNSUPPORTED_RETRIEVAL_SOURCE', 400);
  const format = DOCUMENT_FORMATS.get(extname(path).toLowerCase());
  const documentLimits = format ? validateDocumentExtractionLimits({
    maximumInputBytes: Math.min(maximumSourceBytes, maximumDocumentInputBytes),
    maximumOutputBytes: Math.min(maximumSourceBytes, maximumDocumentOutputBytes), maximumPages: maximumPdfPages }) : null;
  if (!format && (await inspectLocalPath(path)).size > 2 * 1024 * 1024)
    return { path, title: root ? relative(root, path) : basename(path), ...await describeTextFileWindows(path, { maximumSourceBytes, signal }) };
  let bytes, metadata;
  try {
    ({ bytes, metadata } = await readOrdinaryFile(path,
      format ? documentLimits.maximumInputBytes : maximumSourceBytes, signal));
  } catch (error) {
    if (format && error.code === 'INVALID_RETRIEVAL_SOURCE') throw documentExtractionFailure('DOCUMENT_BYTES_LIMIT', {
      documentCoverage: documentExtractionCoverage(format, documentLimits, 'DOCUMENT_BYTES_LIMIT', undefined,
        { limit: { dimension: 'maximumInputBytes', limit: documentLimits.maximumInputBytes, observed: error.observedBytes ?? 0 } }) });
    throw error;
  }
  if (format) {
    const { text, extraction } = await extractDocumentBytes(bytes, format, { signal, resourceService,
      maximumInputBytes: documentLimits.maximumInputBytes, maximumOutputBytes: documentLimits.maximumOutputBytes,
      maximumPages: documentLimits.maximumPages, pageWindow: pdfPageWindow,
      ...(ocrBackend === undefined ? {} : { ocrBackend }) });
    signal?.throwIfAborted();
    if (!sameSourceMetadata(metadata, fileMetadata(await inspectLocalPath(path))))
      throw toolFailure('提取期间资料发生变化。', 'STALE_RETRIEVAL_SOURCE', 409);
    return { path, title: root ? relative(root, path) : basename(path), text, contentHash: hash(text),
      textBytes: Buffer.byteLength(text), metadata, extraction };
  }
  let text;
  try {
    text = bytes[0] === 0xff && bytes[1] === 0xfe ? new TextDecoder('utf-16le', { fatal: true }).decode(bytes.subarray(2))
      : new TextDecoder('utf-8', { fatal: true }).decode(bytes).replace(/^\uFEFF/u, '');
  } catch { throw toolFailure('资料字符编码不受支持。', 'UNSUPPORTED_RETRIEVAL_SOURCE', 400); }
  if (text.includes('\0')) throw toolFailure('二进制文件不能作为文本资料。', 'UNSUPPORTED_RETRIEVAL_SOURCE', 400);
  signal?.throwIfAborted();
  // Decoded UTF-16 may be larger as a UTF-8 snapshot than the original byte file.
  // UTF-16 解码后的 UTF-8 快照可能比原文件更大，同样通过窗口避免登记不可回读的大正文。
  if (Buffer.byteLength(text) > 2 * 1024 * 1024)
    return { path, title: root ? relative(root, path) : basename(path), ...await describeTextFileWindows(path, { maximumSourceBytes, signal }) };
  return { path, title: root ? relative(root, path) : basename(path), text, contentHash: hash(text),
    textBytes: Buffer.byteLength(text), metadata };
}

export async function readSourceFile(path, options = {}) {
  for (let attempt = 1; attempt <= 2; attempt++) {
    try {
      const file = await readSourceFileOnce(path, options);
      return attempt > 1 ? { ...file, readAttempts: attempt } : file;
    } catch (error) {
      error.sourceReadAttempts = attempt;
      if (options.signal?.aborted || attempt === 2 || !classifySourceFailure(error).retryable) throw error;
      await wait(25, undefined, { signal: options.signal });
    }
  }
}

/** Resume bounded PDF pages against one original revision; callers may publish each real window before requesting the next.
 * 在同一原文件版本上继续有界 PDF 页面；调用方可先发布每个真实窗口，再请求后续页面。
 */
export async function* readSourceFileDocumentWindows(path, options = {}) {
  if (extname(path).toLowerCase() !== '.pdf') { yield await readSourceFile(path, options); return; }
  let cursor = options.pdfPageWindow, metadata, rawContentHash;
  for (;;) {
    options.signal?.throwIfAborted();
    const file = await readSourceFile(path, { ...options, pdfPageWindow: cursor });
    if (rawContentHash && (file.extraction.rawContentHash !== rawContentHash || !sameSourceMetadata(metadata, file.metadata)))
      throw toolFailure('PDF 页面游标不再属于当前文件版本。', 'STALE_RETRIEVAL_SOURCE', 409);
    rawContentHash ??= file.extraction.rawContentHash; metadata ??= file.metadata;
    const window = file.extraction.pageWindow;
    yield { ...file, documentWindowKey: hash(JSON.stringify([rawContentHash, window.startPage, window.endPage,
      file.extraction.version, file.extraction.ocr ?? null])) };
    if (window.nextPage === null) return;
    cursor = { startPage: window.nextPage, rawContentHash };
  }
}

async function* continueDocumentWindows(file, options) {
  if (file.documentWindows) {
    for (const window of file.documentWindows) yield { ...file, ...window, documentWindows: undefined, text: undefined, reused: true };
    return;
  }
  yield file;
  if (file.extraction?.format !== 'pdf' || file.extraction.pageWindow.nextPage === null) return;
  yield* readSourceFileDocumentWindows(file.path, { ...options,
    pdfPageWindow: { startPage: file.extraction.pageWindow.nextPage, rawContentHash: file.extraction.rawContentHash } });
}

export async function readSourceFileWindow(path, window, options = {}) {
  const { root = null, excludedRoots = [], maximumSourceBytes = 32 * 1024 * 1024, signal } = options;
  path = resolve(path); signal?.throwIfAborted();
  if (root && !within(resolve(root), path) || isSensitiveFilePath(path) || excludedRoots.some(folder => within(folder, path)) || !TEXT_EXTENSIONS.has(extname(path).toLowerCase()))
    throw toolFailure('此路径不允许回读来源窗口。', 'PROTECTED_RETRIEVAL_SOURCE', 403);
  const fileWindow = validateSourceFileWindow(window);
  return { path, title: root ? relative(root, path) : basename(path),
    ...await readTextFileWindow(path, fileWindow, { maximumSourceBytes, signal }), fileWindow };
}

/** Import windows sequentially so formal snapshots need no complete large-file allocation.
 * 顺序导入窗口，使正式快照无需分配完整大文件正文。
 */
export async function* readSourceImportTree(path, options = {}) {
  for await (const file of scanSourceTree(path, options)) {
    if (file.failed || file.skipped) continue;
    try {
      if (!file.windows) {
        yield file.text === undefined ? await readSourceFile(file.path, { ...options,
          ...(file.extraction?.pageWindow ? { pdfPageWindow: { startPage: file.extraction.pageWindow.startPage,
            endPage: file.extraction.pageWindow.endPage, rawContentHash: file.extraction.rawContentHash } } : {}) }) : file;
        continue;
      }
      for (const window of file.windows) yield { ...file, windows: undefined,
        ...await readSourceFileWindow(file.path, window, options), title: `${file.title} [${window.startOffset}-${window.endOffset}]` };
    } catch (error) {
      if (options.signal?.aborted || error.name === 'AbortError') throw error;
      // A stale window affects its original file; later independent files can still be imported.
      // 窗口过期只影响所属原文件，后续独立文件仍可导入；已保存的窗口保持原版本快照。
      recordSourceFailure(options.stats ?? {}, resolve(path), file.path, error);
    }
  }
}

/** Enumerates ordinary sources and reuses verified stat receipts, never cached bodies or path authority.
 * 枚举普通来源并复用已校验的文件状态回执，不把缓存正文或旧路径当作当前授权。 */
export async function* scanSourceTree(path, options = {}) {
  options.signal?.throwIfAborted();
  const root = resolve(path), info = await inspectLocalPath(root);
  options.signal?.throwIfAborted();
  if (info.isFile()) {
    const stats = options.stats ?? {}, first = await readSourceFile(root, options);
    stats.discoveredFiles = (stats.discoveredFiles ?? 0) + 1; stats.scannedFiles = (stats.scannedFiles ?? 0) + 1;
    let windows = 0, bytes = 0;
    for await (const file of continueDocumentWindows(first, options)) {
      if (++windows > (options.maximumFiles ?? 2048) && !scanLimit(stats, options, 'files', options.maximumFiles ?? 2048, windows)) return;
      bytes += file.textBytes;
      if (bytes > (options.maximumBytes ?? 32 * 1024 * 1024) && !scanLimit(stats, options, 'bytes', options.maximumBytes ?? 32 * 1024 * 1024, bytes)) return;
      stats.fileReads = (stats.fileReads ?? 0) + 1; stats.bytesRead = (stats.bytesRead ?? 0) + file.metadata.sizeBytes;
      if (file.extraction?.format === 'pdf') stats.documentWindows = (stats.documentWindows ?? 0) + 1;
      yield file;
    }
    return;
  }
  if (!info.isDirectory()) throw toolFailure('资料路径无效。', 'INVALID_RETRIEVAL_SOURCE', 400);
  const pending = [{ path: root, rules: [] }]; let visited = 0, bytes = 0, files = 0, cursor = 0;
  const maximumFiles = options.maximumFiles ?? 2048, maximumBytes = options.maximumBytes ?? 32 * 1024 * 1024;
  const maximumEntries = options.maximumEntries ?? 20000;
  const stats = options.stats ?? {};
  for (const key of ['fileReads', 'reusedFiles', 'scannedFiles', 'visitedEntries', 'bytesRead']) stats[key] ??= 0;
  while (cursor < pending.length) {
    options.signal?.throwIfAborted(); const { path: directory, rules: parentRules } = pending[cursor++];
    if (options.excludedRoots?.some(folder => within(folder, directory))) continue;
    try { await inspectLocalPath(directory); }
    catch (error) {
      if (options.signal?.aborted || directory === root) throw error;
      recordSourceFailure(stats, root, directory, error, true); stats.incomplete = true; continue;
    }
    const rules = [...parentRules];
    try { rules.push({ root: directory, matcher: ignore().add((await readOrdinaryFile(join(directory, '.gitignore'), 128 * 1024, options.signal)).bytes.toString('utf8')) }); }
    catch (error) {
      if (options.signal?.aborted || error.name === 'AbortError') throw error;
      if (!['ENOENT', 'INVALID_RETRIEVAL_SOURCE', 'UNSAFE_TOOL_PATH'].includes(error.code)) {
        recordSourceFailure(stats, root, directory, error, true); stats.incomplete = true; continue;
      }
    }
    let entries;
    try { entries = await readdir(directory, { withFileTypes: true }); }
    catch (error) {
      if (options.signal?.aborted || directory === root) throw error;
      recordSourceFailure(stats, root, directory, error, true); stats.incomplete = true; continue;
    }
    for (const entry of entries) {
      options.signal?.throwIfAborted();
      stats.visitedEntries++;
      if (++visited > maximumEntries && !scanLimit(stats, options, 'entries', maximumEntries, visited)) return;
      const candidate = join(directory, entry.name);
      if (entry.isSymbolicLink() || isSensitiveFilePath(candidate)) continue;
      if (rules.some(rule => rule.matcher.ignores(relative(rule.root, candidate).replaceAll('\\', '/') + (entry.isDirectory() ? '/' : '')))) continue;
      if (entry.isDirectory() && !IGNORED_DIRECTORIES.has(entry.name.toLowerCase())) pending.push({ path: candidate, rules });
      else if (entry.isFile() && supportedSource(candidate)) {
        stats.discoveredFiles = (stats.discoveredFiles ?? 0) + 1;
        try {
          const relativePath = relative(root, candidate), previous = options.previousFiles?.get(relativePath);
          let file, reused = false;
          const failedSource = options.failureCache?.get(relativePath);
          if (failedSource && !options.changedPaths?.has(relativePath)) {
            const current = await inspectLocalPath(candidate);
            if (sameSourceMetadata(failedSource.metadata, fileMetadata(current)) &&
                (failedSource.error.category !== 'ocr-unavailable' || Date.now() - (failedSource.cachedAt ?? 0) < 5000) &&
                failedSource.extractionVersion === sourceExtractionVersion(candidate) && failedSource.limitsKey === extractionLimitsKey(options)) {
              recordSourceFailure(stats, root, candidate, failedSource.error);
              stats.reusedFailures = (stats.reusedFailures ?? 0) + 1;
              continue;
            }
            options.failureCache.delete(relativePath);
          }
          if (previous && !options.changedPaths?.has(relativePath)) {
            const current = await inspectLocalPath(candidate);
            if (!current.isFile()) continue;
            const version = sourceExtractionVersion(candidate);
            const maximumFileBytes = version ? Math.min(options.maximumSourceBytes ?? 32 * 1024 * 1024,
              DOCUMENT_EXTRACTION_LIMITS.maximumInputBytes) : options.maximumSourceBytes ?? 32 * 1024 * 1024;
            if (current.size > maximumFileBytes) {
              if (version) await readSourceFile(candidate, { ...options, root });
              throw toolFailure('资料文件超过配置预算。', 'INVALID_RETRIEVAL_SOURCE', 413);
            }
            const metadata = fileMetadata(current);
            reused = sameSourceMetadata(previous.metadata, metadata) && (previous.extraction?.version ?? null) === version &&
              (!version || previous.extraction?.format !== 'pdf' || previous.documentComplete === true || previous.extraction.complete === true) &&
              Number.isSafeInteger(previous.textBytes) && (previous.documentWindows || previous.textBytes <= maximumFileBytes) && (!version ||
              current.size <= (options.maximumDocumentInputBytes ?? DOCUMENT_EXTRACTION_LIMITS.maximumInputBytes) &&
              (previous.documentWindows || previous.textBytes <= (options.maximumDocumentOutputBytes ?? DOCUMENT_EXTRACTION_LIMITS.maximumOutputBytes)) &&
              (previous.extraction?.pages?.length ?? previous.extraction?.pageCount ?? 0) <= (options.maximumPdfPages ?? DOCUMENT_EXTRACTION_LIMITS.maximumPages));
            if (reused && previous.documentWindows?.some(window => window.extraction.pages.length >
                (options.maximumPdfPages ?? DOCUMENT_EXTRACTION_LIMITS.maximumPages) ||
                window.textBytes > (options.maximumDocumentOutputBytes ?? DOCUMENT_EXTRACTION_LIMITS.maximumOutputBytes))) reused = false;
            if (reused) file = { path: candidate, title: relativePath, contentHash: previous.contentHash,
              textBytes: previous.textBytes, metadata, ...(previous.extraction ? { extraction: previous.extraction } : {}),
              ...(previous.windows ? { windows: previous.windows } : {}),
              ...(previous.documentWindows ? { documentWindows: previous.documentWindows } : {}), reused: true };
          }
          file ??= await readSourceFile(candidate, { ...options, root });
          options.failureCache?.delete(relativePath);
          if (reused) stats.reusedFiles++;
          else { stats.fileReads++; stats.bytesRead += file.metadata.sizeBytes; }
          stats.scannedFiles++;
          for await (const window of continueDocumentWindows(file, { ...options, root })) {
            if (files >= maximumFiles && !scanLimit(stats, options, 'files', maximumFiles, files + 1)) return;
            if (bytes + window.textBytes > maximumBytes && !scanLimit(stats, options, 'bytes', maximumBytes, bytes + window.textBytes)) return;
            bytes += window.textBytes; files++;
            if (window !== file && !window.reused) { stats.fileReads++; stats.bytesRead += window.metadata.sizeBytes; }
            if (window.extraction?.format === 'pdf') stats.documentWindows = (stats.documentWindows ?? 0) + 1;
            yield window;
          }
        }
        catch (error) {
          if (options.signal?.aborted || error.name === 'AbortError' || error.code === 'RETRIEVAL_SCAN_LIMIT') throw error;
          recordSourceFailure(stats, root, candidate, error);
          const classification = classifySourceFailure(error);
          if (options.failureCache && !classification.retryable &&
              ['unsupported', 'invalid-source', 'ocr-unavailable', 'limit'].includes(classification.category)) {
            const current = await inspectLocalPath(candidate).catch(() => null);
            if (current?.isFile()) {
              const relativePath = relative(root, candidate);
              options.failureCache.set(relativePath, { metadata: fileMetadata(current),
                limitsKey: extractionLimitsKey(options),
                extractionVersion: sourceExtractionVersion(candidate), cachedAt: Date.now(), error: { ...classification, code: classification.errorCode,
                  ...(error.details?.documentCoverage ? { details: { documentCoverage: error.details.documentCoverage } } : {}) } });
              while (options.failureCache.size > 256) options.failureCache.delete(options.failureCache.keys().next().value);
            }
          }
        }
      }
    }
  }
  options.signal?.throwIfAborted();
}

export async function readSourceTree(path, options = {}) {
  const result = [];
  for await (const file of scanSourceTree(path, options)) {
    result.push(file.text === undefined && !file.windows ? await readSourceFile(file.path, { ...options, root: resolve(path),
      ...(file.extraction?.pageWindow ? { pdfPageWindow: { startPage: file.extraction.pageWindow.startPage,
        endPage: file.extraction.pageWindow.endPage, rawContentHash: file.extraction.rawContentHash } } : {}) }) : file);
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
  paths: for (const relativePath of dirtyPaths) {
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
          if (options.signal?.aborted || error.name === 'AbortError') throw error;
          if (!['ENOENT', 'INVALID_RETRIEVAL_SOURCE', 'UNSAFE_TOOL_PATH'].includes(error.code)) {
            recordSourceFailure(stats, root, candidate, error);
            yield { path: candidate, title: relativePath, failed: true, errorCode: error.code ?? 'RETRIEVAL_SOURCE_FAILED' };
            continue paths;
          }
        }
        rulesCache.set(directory, matcher);
      }
      if (rulesCache.get(directory)?.ignores(relative(directory, candidate).replaceAll('\\', '/'))) protectedPath = true;
    }
    if (protectedPath) { options.failureCache?.delete(relativePath); yield { path: candidate, title: relativePath, missing: true }; continue; }
    let current;
    try { current = await inspectLocalPath(candidate, { allowMissing: true }); }
    catch (error) {
      if (options.signal?.aborted || error.name === 'AbortError') throw error;
      recordSourceFailure(stats, root, candidate, error);
      yield { path: candidate, title: relativePath, failed: true, errorCode: error.code ?? 'RETRIEVAL_SOURCE_FAILED' };
      continue;
    }
    if (!current) { options.failureCache?.delete(relativePath); yield { path: candidate, title: relativePath, missing: true }; continue; }
    if (current.isDirectory()) throw toolFailure('目录变化需要完整核对。', 'RETRIEVAL_FULL_SCAN_REQUIRED', 409);
    if (!supportedSource(candidate)) {
      yield { path: candidate, title: relativePath, missing: true }; continue;
    }
    stats.discoveredFiles = (stats.discoveredFiles ?? 0) + 1;
    try {
      const file = await readSourceFile(candidate, { ...options, root });
      stats.fileReads++; stats.scannedFiles++; stats.bytesRead += file.metadata.sizeBytes;
      yield* continueDocumentWindows(file, { ...options, root });
    } catch (error) {
      if (['ENOENT', 'PROTECTED_RETRIEVAL_SOURCE'].includes(error.code))
        yield { path: candidate, title: relativePath, missing: true };
      else {
        if (options.signal?.aborted || error.name === 'AbortError') throw error;
        recordSourceFailure(stats, root, candidate, error);
        yield { path: candidate, title: relativePath, failed: true, errorCode: error.code ?? 'RETRIEVAL_SOURCE_FAILED' };
      }
    }
  }
}

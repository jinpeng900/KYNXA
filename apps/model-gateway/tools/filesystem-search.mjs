import { createHash, randomBytes } from 'node:crypto';
import { constants, watch } from 'node:fs';
import { open, opendir } from 'node:fs/promises';
import { basename, dirname, join, relative, resolve, sep } from 'node:path';
import { setImmediate as yieldTurn } from 'node:timers/promises';
import ignore from 'ignore';
import { boundedInteger, inspectLocalPath, toolFailure, within } from '../platform/tool-paths.mjs';

const MAX_FILE_BYTES = 1024 * 1024;
const MAX_PAGE_FILES = 1000;
const MAX_PAGE_ENTRIES = 2000;
const MAX_PAGE_BYTES = 16 * 1024 * 1024;
const MAX_PAGE_MATCH_CHARACTERS = 48000;
const MAX_PAGE_DURATION_MS = 1000;
const MAX_OPEN_DIRECTORIES = 256;
const MAX_SEARCH_SESSIONS = 16;
const MAX_OBSERVED_VERSIONS = 4096;
const MAX_PENDING_CHANGES = 512;
const MAX_PAGE_FAILURES = 50;
const MAX_PAGE_FAILURE_CHARACTERS = 8000;
const MAX_IGNORE_FILE_BYTES = 128 * 1024;
const MAX_ACTIVE_IGNORE_BYTES = 1024 * 1024;
const SEARCH_IDLE_TIMEOUT_MS = 10 * 60 * 1000;
// Keep generated-directory defaults aligned with mounted-source discovery; user ignore syntax shares its dependency.
// 生成目录默认项与挂载来源发现保持一致，用户忽略语法复用其已有依赖。
const EXCLUDED_DIRECTORIES = new Set(['.git', '.vs', '.idea', 'node_modules', 'bin', 'obj', 'target', '__pycache__',
  '.venv', 'venv', '.kynxa', '.sandbox-runtime', '.sandbox-temp']);
const sessions = new Map();
const releasedIdentities = new WeakSet();
const decoder = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true });
const hashBytes = bytes => createHash('sha256').update(bytes).digest('hex');
const fileVersion = info => `${info.dev}:${info.ino}:${info.size}:${info.mtimeMs}:${info.ctimeMs}`;
const directoryVersion = info => `${info.dev}:${info.ino}:${info.mtimeMs}:${info.ctimeMs}`;

function staleSearch() {
  return toolFailure('搜索期间目录或文件已改变；已返回内容是旧观察，请省略 cursor 从头搜索当前版本。', 'TOOL_SEARCH_CHANGED', 409);
}

function invalidCursor() {
  return toolFailure('搜索游标已使用、过期或不属于当前请求、路径和查询；请省略 cursor 重新搜索。', 'INVALID_TOOL_SEARCH_CURSOR', 409);
}

function bindingFor(context, root, input, protectedRoots, workspaceRoot) {
  return JSON.stringify([root, input.query, input.mode ?? 'content', input.respectIgnoreFiles !== false,
    input.recursive !== false, context.requestId, context.conversationId,
    context.projectId, context.workspaceRoot, workspaceRoot, context.permissionMode, protectedRoots.map(rootPath => resolve(rootPath)).sort()]);
}

async function closeSession(session) {
  if (session.closed) return;
  session.closed = true;
  sessions.delete(session.cursor);
  clearTimeout(session.expiry);
  session.watcher?.close();
  session.observedVersions.clear();
  session.changedPaths.clear();
  session.ignoreRules.length = 0;
  await Promise.all(session.directories.splice(0).map(frame => frame.handle.close().catch(() => {})));
}

/** Release only the owning request/service; cursors never keep abandoned directory handles indefinitely.
 * 仅释放所属请求或服务的扫描，游标不会让废弃目录句柄永久驻留。
 */
export async function closeFilesystemSearches(identity) {
  if (identity && typeof identity === 'object') releasedIdentities.add(identity);
  await Promise.all([...sessions.values()].filter(session => session.context === identity || session.owner === identity)
    .map(closeSession));
}

function assertSearchOwner(context, owner) {
  if (releasedIdentities.has(context) || releasedIdentities.has(owner)) throw invalidCursor();
}

function excludedPath(session, candidate) {
  return session.protectedRoots.some(root => within(root, candidate)) || session.denyRead(candidate);
}

async function createSession(context, root, input, options) {
  const info = await inspectLocalPath(root, { allowHardLinks: true });
  while (sessions.size >= MAX_SEARCH_SESSIONS) {
    const idle = [...sessions.values()].find(session => !session.busy);
    if (!idle) throw toolFailure('并行文件搜索已达到上限，请等待当前搜索页完成。', 'TOOL_SEARCH_BUSY', 409);
    await closeSession(idle);
  }
  // Disposal can occur before the first scan is registered; remember ownership without retaining the owner.
  // 首个扫描登记前也可能释放请求；弱引用身份检查避免漏关，同时不永久持有所有者。
  assertSearchOwner(context, options.searchOwner ?? context);
  const session = { context, owner: options.searchOwner ?? context, root, rootIsDirectory: info.isDirectory(),
    query: input.query, recursive: input.recursive !== false,
    mode: input.mode ?? 'content', respectIgnoreFiles: input.respectIgnoreFiles !== false,
    ignoreRoot: options.workspaceRoot && within(options.workspaceRoot, root) ? resolve(options.workspaceRoot) : info.isDirectory() ? root : dirname(root),
    ignoreRules: [], ignoreBytes: 0, ignoreRulesInitialized: false, incompleteIgnoreFiles: 0, ignoreFilesRead: 0,
    protectedRoots: options.protectedRoots, denyRead: options.denyRead,
    binding: bindingFor(context, root, input, options.protectedRoots, options.workspaceRoot), directories: [], nextPath: root, pendingFile: null,
    cursor: randomBytes(24).toString('base64url'), busy: true, closed: false, changedPaths: new Set(), observedVersions: new Map(),
    totalVisitedFiles: 0, totalVisitedEntries: 0, totalSkipped: 0, totalFailedEntries: 0,
    page: 0, monitorState: 'active', unverifiedChanges: false };
  rememberVersion(session, root, info);
  sessions.set(session.cursor, session);
  try {
    // Watch changes even in completed branches; active-branch stats and resumed-file hashes add independent checks.
    // 已走完的分支仍监控变更；当前分支属性及续读文件哈希再做独立校验。
    session.watcher = (options.watchDirectory ?? watch)(root, { recursive: info.isDirectory(), persistent: false }, (_event, name) => {
      const candidate = name && info.isDirectory() ? join(root, String(name)) : root;
      if (name && info.isDirectory()) {
        const parts = relative(root, candidate).split(sep);
        if (parts.some(part => EXCLUDED_DIRECTORIES.has(part.toLowerCase())) || excludedPath(session, candidate)) return;
      }
      if (session.changedPaths.size < MAX_PENDING_CHANGES) session.changedPaths.add(candidate);
      else session.unverifiedChanges = true;
    });
    session.watcher.on('error', () => { session.monitorState = 'unavailable'; session.watcher?.close(); });
  } catch {
    // Monitoring is an additional observation, never a prerequisite for reading authorized files.
    // 目录监控是额外观察手段，不能成为读取已授权文件的新前置条件。
    session.monitorState = 'unavailable';
  }
  return session;
}

async function checkTraversal(session) {
  if (session.closed) throw invalidCursor();
  for (const frame of session.directories) {
    const info = await inspectLocalPath(frame.path);
    if (!info.isDirectory() || directoryVersion(info) !== frame.version) throw staleSearch();
  }
  for (const rule of session.ignoreRules) {
    if (!rule.checked) continue;
    const info = await inspectLocalPath(rule.path, { allowMissing: true, allowHardLinks: true });
    if ((info ? fileVersion(info) : null) !== rule.version) throw staleSearch();
  }
  const changedPaths = [...session.changedPaths];
  session.changedPaths.clear();
  for (const candidate of changedPaths) {
    // Windows may deliver delayed metadata notifications without changing content or directory entries.
    // Windows 可能延迟发送元数据通知；先核验真实版本，不能仅凭通知就中止搜索。
    const observedPath = session.observedVersions.has(candidate) ? candidate : dirname(candidate);
    const previous = session.observedVersions.get(observedPath);
    if (!previous) { session.unverifiedChanges = true; continue; }
    const info = await inspectLocalPath(observedPath, { allowHardLinks: true });
    if (previous !== (info.isDirectory() ? directoryVersion(info) : fileVersion(info))) throw staleSearch();
  }
}

function rememberVersion(session, candidate, info) {
  session.observedVersions.delete(candidate);
  session.observedVersions.set(candidate, info.isDirectory() ? directoryVersion(info) : fileVersion(info));
  if (session.observedVersions.size > MAX_OBSERVED_VERSIONS)
    session.observedVersions.delete(session.observedVersions.keys().next().value);
}

function skip(page, reason) {
  page.skipped++;
  page.skippedByReason[reason] = (page.skippedByReason[reason] ?? 0) + 1;
}

function recordEntryFailure(session, page, candidate, code) {
  skip(page, code);
  page.failedEntries++;
  const failure = { path: relative(session.root, candidate) || basename(candidate), code };
  const characters = JSON.stringify(failure).length + 1;
  if (page.failures.length < MAX_PAGE_FAILURES && page.failureCharacters + characters <= MAX_PAGE_FAILURE_CHARACTERS &&
      page.failureCharacters + page.matchCharacters + characters <= MAX_PAGE_MATCH_CHARACTERS) {
    page.failures.push(failure);
    page.failureCharacters += characters;
  }
}

async function readSearchBytes(path, info, maximumBytes, signal) {
  const handle = await open(path, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0));
  try {
    const opened = await handle.stat();
    if (!opened.isFile() || fileVersion(info) !== fileVersion(opened)) throw staleSearch();
    // A concurrently growing file must not make a bounded search allocate its entire new contents.
    // 文件并发增长时也只分配原长度加一字节，不能因整文件读取而突破搜索内存边界。
    const buffer = Buffer.allocUnsafe(Math.min(maximumBytes + 1, opened.size + 1));
    let length = 0;
    while (length < buffer.length) {
      signal?.throwIfAborted();
      const result = await handle.read(buffer, length, buffer.length - length, length);
      if (!result.bytesRead) break;
      length += result.bytesRead;
    }
    const after = await inspectLocalPath(path, { allowHardLinks: true });
    if (length !== opened.size || length > maximumBytes || fileVersion(opened) !== fileVersion(await handle.stat()) ||
        fileVersion(opened) !== fileVersion(after)) throw staleSearch();
    return buffer.subarray(0, length);
  } finally { await handle.close(); }
}

async function loadIgnoreRules(session, directory, signal) {
  const rules = [];
  if (!session.respectIgnoreFiles) return rules;
  for (const name of ['.gitignore', '.ignore']) {
    signal?.throwIfAborted();
    const path = join(directory, name);
    if (excludedPath(session, path)) { session.incompleteIgnoreFiles++; continue; }
    try {
      const info = await inspectLocalPath(path, { allowMissing: true, allowHardLinks: true });
      if (!info) { rules.push({ path, version: null, checked: true, bytes: 0 }); continue; }
      if (!info.isFile() || info.size > MAX_IGNORE_FILE_BYTES || session.ignoreBytes + info.size > MAX_ACTIVE_IGNORE_BYTES) {
        session.incompleteIgnoreFiles++;
        continue;
      }
      const bytes = await readSearchBytes(path, info, MAX_IGNORE_FILE_BYTES, signal);
      const matcher = ignore().add(decoder.decode(bytes));
      rememberVersion(session, path, info);
      session.ignoreBytes += bytes.length;
      session.ignoreFilesRead++;
      rules.push({ root: directory, path, version: fileVersion(info), checked: true, matcher, bytes: bytes.length });
    } catch (error) {
      if (signal?.aborted || error.name === 'AbortError' || error.code === 'TOOL_SEARCH_CHANGED') throw error;
      session.incompleteIgnoreFiles++;
    }
  }
  session.ignoreRules.push(...rules);
  return rules;
}

async function initializeIgnoreRules(session, signal) {
  if (session.ignoreRulesInitialized) return;
  session.ignoreRulesInitialized = true;
  if (!session.respectIgnoreFiles || !session.rootIsDirectory || session.ignoreRoot === session.root) return;
  const ancestors = [];
  for (let directory = dirname(session.root); within(session.ignoreRoot, directory); directory = dirname(directory)) {
    ancestors.unshift(directory);
    if (directory === session.ignoreRoot) break;
  }
  for (const directory of ancestors) await loadIgnoreRules(session, directory, signal);
}

function ignoredByRules(session, candidate, isDirectory) {
  if (!session.respectIgnoreFiles || candidate === session.root) return false;
  let ignored = false;
  for (const rule of session.ignoreRules) {
    if (!rule.matcher || !within(rule.root, candidate)) continue;
    const path = relative(rule.root, candidate).replaceAll('\\', '/') + (isDirectory ? '/' : '');
    const decision = rule.matcher.test(path);
    if (decision.ignored) ignored = true;
    else if (decision.unignored) ignored = false;
  }
  return ignored;
}

function addMatch(page, match) {
  const matchCharacters = JSON.stringify(match).length + 1;
  if ((page.matches.length || page.failures.length) && page.failureCharacters + page.matchCharacters + matchCharacters > MAX_PAGE_MATCH_CHARACTERS) {
    page.resultFull = true;
    return false;
  }
  page.matches.push(match);
  page.matchCharacters += matchCharacters;
  return true;
}

async function nextCandidate(session, page) {
  if (session.nextPath) {
    const candidate = session.nextPath;
    session.nextPath = null;
    if (!session.nextPathAlreadyVisited) page.visitedEntries++;
    session.nextPathAlreadyVisited = false;
    return candidate;
  }
  while (session.directories.length && page.visitedEntries < MAX_PAGE_ENTRIES) {
    const frame = session.directories.at(-1);
    const entry = await frame.handle.read();
    if (!entry) {
      if (directoryVersion(await inspectLocalPath(frame.path)) !== frame.version) throw staleSearch();
      session.directories.pop();
      for (const rule of frame.ignoreRules) session.ignoreBytes -= rule.bytes;
      if (frame.ignoreRules.length) session.ignoreRules.splice(-frame.ignoreRules.length);
      await frame.handle.close();
      continue;
    }
    page.visitedEntries++;
    if (entry.isSymbolicLink()) { skip(page, 'link'); continue; }
    if (entry.isDirectory() && EXCLUDED_DIRECTORIES.has(entry.name.toLowerCase())) { skip(page, 'excludedDirectory'); continue; }
    if (entry.isDirectory() && !session.recursive) { skip(page, 'nonRecursiveDirectory'); continue; }
    return join(frame.path, entry.name);
  }
  return null;
}

async function scanFile(session, pendingFile, page, signal, maxMatches) {
  const candidate = pendingFile.path;
  if (excludedPath(session, candidate)) { skip(page, 'protected'); session.pendingFile = null; return; }
  const info = await inspectLocalPath(candidate, { allowHardLinks: true });
  if (!info.isFile()) throw staleSearch();
  if (info.size > MAX_FILE_BYTES) {
    if (pendingFile.sha256) throw staleSearch();
    skip(page, 'fileTooLarge'); session.pendingFile = null; return;
  }
  const bytes = await readSearchBytes(candidate, info, MAX_FILE_BYTES, signal);
  const sha256 = hashBytes(bytes);
  if (pendingFile.sha256 && pendingFile.sha256 !== sha256) throw staleSearch();
  rememberVersion(session, candidate, info);
  page.readBytes += bytes.length;
  let content;
  try { content = decoder.decode(bytes); if (content.includes('\0')) throw new Error('binary'); }
  catch { skip(page, 'nonText'); session.pendingFile = null; return; }
  let offset = pendingFile.offset, line = pendingFile.line;
  while (offset < content.length) {
    signal?.throwIfAborted();
    const newline = content.indexOf('\n', offset);
    const lineEnd = newline < 0 ? content.length : newline;
    const text = content.slice(offset, lineEnd).replace(/\r$/, '');
    if (text.includes(session.query)) {
      const match = { path: relative(session.root, candidate) || basename(candidate), line, text: text.slice(0, 400), sha256 };
      // Leave room for the cursor in the broker's JSON preview; resume before this unreturned line.
      // 为工具层 JSON 预览中的游标保留空间；未返回的这一行留到下一页继续。
      if (!addMatch(page, match)) break;
    }
    offset = newline < 0 ? content.length : newline + 1;
    line++;
    if (page.matches.length >= maxMatches || performance.now() - page.startedAt >= MAX_PAGE_DURATION_MS) break;
  }
  session.pendingFile = offset < content.length ? { path: candidate, offset, line, sha256 } : null;
}

async function scanPendingFile(session, page, signal, maxMatches) {
  const pending = session.pendingFile;
  try { await scanFile(session, pending, page, signal, maxMatches); }
  catch (error) {
    // One unreadable new file does not block unrelated results; an interrupted prior file must keep its version contract.
    // 新发现的单个文件不可读不阻塞其他结果；此前已分页返回的文件仍须遵守版本一致性。
    if (!pending.sha256 && ['ENOENT', 'EACCES', 'EPERM'].includes(error.code)) {
      recordEntryFailure(session, page, pending.path, error.code);
      session.pendingFile = null;
      return;
    }
    throw error;
  }
}

async function scanPage(session, maxMatches, signal) {
  const page = { matches: [], matchCharacters: 0, resultFull: false, visitedFiles: 0, visitedEntries: 0, skipped: 0,
    skippedByReason: {}, failures: [], failureCharacters: 0, failedEntries: 0, readBytes: 0, startedAt: performance.now() };
  await initializeIgnoreRules(session, signal);
  while (!page.resultFull && page.matches.length < maxMatches && page.visitedFiles < MAX_PAGE_FILES && page.visitedEntries < MAX_PAGE_ENTRIES &&
      page.readBytes < MAX_PAGE_BYTES && performance.now() - page.startedAt < MAX_PAGE_DURATION_MS) {
    signal?.throwIfAborted();
    if (session.closed) throw invalidCursor();
    if (session.pendingFile) {
      await scanPendingFile(session, page, signal, maxMatches);
      continue;
    }
    const candidate = await nextCandidate(session, page);
    if (!candidate) break;
    if (excludedPath(session, candidate)) { skip(page, 'protected'); continue; }
    let info;
    try { info = await inspectLocalPath(candidate, { allowHardLinks: true }); }
    catch (error) {
      if (candidate !== session.root && ['ENOENT', 'EACCES', 'EPERM'].includes(error.code)) {
        recordEntryFailure(session, page, candidate, error.code);
        continue;
      }
      if (error.code === 'UNSAFE_TOOL_PATH') { skip(page, error.code); continue; }
      if (error.code === 'ENOENT') throw staleSearch();
      throw error;
    }
    if (ignoredByRules(session, candidate, info.isDirectory())) { skip(page, 'ignoreRule'); continue; }
    rememberVersion(session, candidate, info);
    if (session.mode === 'path' && (info.isFile() || info.isDirectory())) {
      const relativePath = relative(session.root, candidate) || (info.isDirectory() ? '.' : basename(candidate));
      const candidatePath = (relative(session.root, candidate) || basename(candidate)).replaceAll('\\', '/');
      const queryPath = session.query.replaceAll('\\', '/');
      const matchesPath = process.platform === 'win32' ? candidatePath.toLowerCase().includes(queryPath.toLowerCase()) : candidatePath.includes(queryPath);
      if (matchesPath && !addMatch(page, { path: relativePath, type: info.isDirectory() ? 'directory' : 'file',
        bytes: info.size, modifiedAt: info.mtime.toISOString() })) {
        session.nextPath = candidate;
        session.nextPathAlreadyVisited = true;
        break;
      }
    }
    if (info.isDirectory()) {
      if (session.directories.length >= MAX_OPEN_DIRECTORIES)
        throw toolFailure('目录嵌套超过本次扫描句柄额度，请从更深的子目录继续独立搜索。', 'TOOL_SEARCH_DEPTH_LIMIT', 409);
      const ignoreRules = await loadIgnoreRules(session, candidate, signal);
      const handle = await opendir(candidate, { bufferSize: 32 });
      // Request disposal can race directory opening; never publish a handle after its owner has closed.
      // 请求释放可能与打开目录并发；所有者关闭后不能再挂入新句柄。
      if (session.closed || signal?.aborted) { await handle.close(); signal?.throwIfAborted(); throw invalidCursor(); }
      session.directories.push({ path: candidate, version: directoryVersion(info), ignoreRules, handle });
    } else if (info.isFile()) {
      page.visitedFiles++;
      if (session.mode === 'path') continue;
      session.pendingFile = { path: candidate, offset: 0, line: 1 };
      // Read the counted file in this page, even when it reaches the file-count boundary.
      // 已计数的文件本页必须读取，避免文件数边界使它永久跳过。
      await scanPendingFile(session, page, signal, maxMatches);
    } else skip(page, 'notRegularFile');
  }
  await yieldTurn(undefined, { signal });
  await checkTraversal(session);
  page.limitReasons = [page.resultFull ? 'resultCharacters' : null,
    page.matches.length >= maxMatches ? 'matches' : null,
    page.visitedFiles >= MAX_PAGE_FILES ? 'files' : null,
    page.visitedEntries >= MAX_PAGE_ENTRIES ? 'entries' : null,
    page.readBytes >= MAX_PAGE_BYTES ? 'readBytes' : null,
    performance.now() - page.startedAt >= MAX_PAGE_DURATION_MS ? 'duration' : null].filter(Boolean);
  return page;
}

/** A cursor resumes a bounded live walk, not an atomic snapshot or permission grant.
 * 游标续接有界实时遍历，不代表原子快照，也不授予新的读取权限。
 */
export async function searchFilesystem(context, path, input, signal, { protectedRoots = [], denyRead = () => false,
  searchOwner, watchDirectory, workspaceRoot = context.workspaceRoot } = {}) {
  signal?.throwIfAborted();
  if (typeof input.query !== 'string' || !input.query || input.query.length > 200) throw toolFailure('搜索文本须为 1–200 个字符。');
  if (input.mode !== undefined && !['content', 'path'].includes(input.mode)) throw toolFailure('搜索 mode 必须为 content 或 path。');
  if (input.respectIgnoreFiles !== undefined && typeof input.respectIgnoreFiles !== 'boolean') throw toolFailure('respectIgnoreFiles 必须为布尔值。');
  const maxMatches = boundedInteger(input.maxMatches, 100, 1, 200), root = resolve(path);
  assertSearchOwner(context, searchOwner ?? context);
  const binding = bindingFor(context, root, input, protectedRoots, workspaceRoot);
  let session;
  if (input.cursor !== undefined) {
    if (typeof input.cursor !== 'string' || !/^[A-Za-z0-9_-]{32}$/.test(input.cursor)) throw invalidCursor();
    session = sessions.get(input.cursor);
    if (!session || session.context !== context || session.owner !== (searchOwner ?? context) || session.binding !== binding) throw invalidCursor();
    if (session.busy) throw toolFailure('该搜索页仍在执行，请等待它完成后使用新的游标。', 'TOOL_SEARCH_BUSY', 409);
    session.denyRead = denyRead;
    session.busy = true;
    clearTimeout(session.expiry);
  } else session = await createSession(context, root, input, { protectedRoots, denyRead, searchOwner, watchDirectory, workspaceRoot });
  try {
    signal?.throwIfAborted();
    await checkTraversal(session);
    const page = await scanPage(session, maxMatches, signal);
    signal?.throwIfAborted();
    assertSearchOwner(context, searchOwner ?? context);
    if (session.closed) throw invalidCursor();
    const hasMore = Boolean(session.pendingFile || session.nextPath || session.directories.length);
    session.totalVisitedFiles += page.visitedFiles;
    session.totalVisitedEntries += page.visitedEntries;
    session.totalSkipped += page.skipped;
    session.totalFailedEntries += page.failedEntries;
    session.page++;
    sessions.delete(session.cursor);
    session.cursor = randomBytes(24).toString('base64url');
    if (hasMore) {
      sessions.set(session.cursor, session);
      session.busy = false;
      session.expiry = setTimeout(() => { void closeSession(session); }, SEARCH_IDLE_TIMEOUT_MS).unref();
    } else await closeSession(session);
    return { path: root, query: input.query, mode: session.mode, matches: page.matches, visitedFiles: page.visitedFiles, skipped: page.skipped,
      skippedByReason: page.skippedByReason, truncated: hasMore, hasMore, nextCursor: hasMore ? session.cursor : null,
      failures: page.failures, failureReportTruncated: page.failedEntries > page.failures.length,
      pageLimitReasons: page.limitReasons,
      page: session.page, totalVisitedFiles: session.totalVisitedFiles, totalVisitedEntries: session.totalVisitedEntries,
      totalSkipped: session.totalSkipped, consistency: { mode: session.monitorState === 'active' && !session.unverifiedChanges ? 'live-walk' : 'live-unverified',
        directoryMonitoring: session.monitorState, atomicSnapshot: false, changesDetected: false,
        unverifiedChangeNotifications: session.unverifiedChanges,
        restartOnChange: true, note: 'Directory events, active directory versions and resumed file hashes are checked; rerun after edits. Results carry file hashes. No atomic whole-repository snapshot.' },
      ...(session.monitorState !== 'active' ? { diagnostics: [{ code: 'TOOL_SEARCH_MONITOR_UNAVAILABLE',
        message: 'Active directory versions and resumed file hashes are checked; changes in completed branches may require a fresh search.' }] } : {}),
      coverage: { traversalComplete: !hasMore, fileSizeLimitBytes: MAX_FILE_BYTES, fileSizeLimitAppliesTo: 'content',
        respectIgnoreFiles: session.respectIgnoreFiles, ignoreFileNames: ['.gitignore', '.ignore'], ignoreFilesRead: session.ignoreFilesRead,
        failedEntries: session.totalFailedEntries,
        incompleteIgnoreFiles: session.incompleteIgnoreFiles, excludedDirectories: [...EXCLUDED_DIRECTORIES],
        unreadableOrUnsupportedEntriesSkipped: session.totalSkipped > 0 },
      ...(hasMore ? { cursorExpiresInMs: SEARCH_IDLE_TIMEOUT_MS } : {}) };
  } catch (error) {
    await closeSession(session);
    if (error.code === 'ENOENT' || error.code === 'ENOTDIR') throw staleSearch();
    throw error;
  }
}

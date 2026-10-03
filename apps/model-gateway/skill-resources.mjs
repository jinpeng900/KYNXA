import { createHash } from 'node:crypto';
import { constants } from 'node:fs';
import { open, opendir, realpath } from 'node:fs/promises';
import { isAbsolute, join, relative, resolve, sep, win32 } from 'node:path';
import { boundedInteger, inspectLocalPath, toolFailure, within } from './tool-paths.mjs';

export const SKILL_PACKAGE_LIMITS = Object.freeze({ files: 128, entries: 512, depth: 8,
  fileBytes: 2 * 1024 * 1024, totalBytes: 8 * 1024 * 1024, previewChars: 16000 });
const decoder = new TextDecoder('utf-8', { fatal: true });
const privateDirectories = new Set(['.git', '.ssh', '.aws', '.azure', '.gnupg', '.kube', '.codex', '.config', '.npm', '.docker']);
const dependencyDirectories = new Set(['node_modules', '.venv', 'venv', '__pycache__']);
const mimeTypes = { '.md': 'text/markdown', '.txt': 'text/plain', '.csv': 'text/csv', '.json': 'application/json',
  '.yaml': 'application/yaml', '.yml': 'application/yaml', '.js': 'text/javascript', '.mjs': 'text/javascript',
  '.cjs': 'text/javascript', '.py': 'text/x-python', '.sh': 'text/x-shellscript', '.ps1': 'text/plain',
  '.html': 'text/html', '.css': 'text/css', '.xml': 'application/xml', '.svg': 'image/svg+xml',
  '.png': 'image/png', '.jpg': 'image/jpeg', '.jpeg': 'image/jpeg', '.gif': 'image/gif', '.pdf': 'application/pdf' };

function cancelled(signal) { signal?.throwIfAborted(); }
const portable = value => value.split(sep).join('/');
const diagnostic = (code, message, path, severity = 'error') => ({ code, message, severity, ...(path ? { path } : {}) });

export function assertSkillResourceAllowed(path, denyResource) {
  if (denyResource?.(resolve(path)))
    throw toolFailure('Private application data cannot be exposed through skill resources.', 'PROTECTED_SKILL_RESOURCE', 403);
}

export function skillResourceMimeType(path) {
  const suffix = /\.[^.\/\\]+$/.exec(path.toLowerCase())?.[0];
  return mimeTypes[suffix] ?? 'application/octet-stream';
}

export function isPrivateSkillResource(path) {
  const parts = path.replace(/\\/g, '/').split('/');
  return parts.some(part => privateDirectories.has(part.toLowerCase())) || parts.some(part =>
    /^\.env(?:[._-]|$)/i.test(part) || /^(?:id_(?:rsa|dsa|ecdsa|ed25519)|authorized_keys|known_hosts)$/i.test(part) ||
    /\.(?:pem|key|pfx|p12|jks|keystore)$/i.test(part) ||
    /^(?:connections|credentials|secrets|tokens)(?:[._-][^.]+)*\.(?:json|ya?ml|toml|ini|conf|env)$/i.test(part));
}

/** Resource references always resolve from the skill root, never the work folder. */
export function resolveSkillResource(skillRoot, value) {
  if (typeof value !== 'string' || !value.trim() || value.length > 2048 || /[\0\r\n:]/.test(value) ||
      isAbsolute(value) || win32.isAbsolute(value))
    throw toolFailure('Skill resource requires a relative package path.', 'INVALID_SKILL_RESOURCE_PATH');
  const parts = value.replace(/\\/g, '/').split('/');
  if (parts.some(part => part === '..' || (/[. ]$/.test(part) && part !== '.') ||
      /^(?:con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)/i.test(part)))
    throw toolFailure('Skill resource path crosses a package boundary or uses an unsafe name.', 'UNSAFE_SKILL_RESOURCE_PATH', 403);
  const path = resolve(skillRoot, ...parts), relativePath = portable(relative(skillRoot, path));
  if (!within(skillRoot, path) || !relativePath)
    throw toolFailure('Skill resource must be a file inside this package.', 'UNSAFE_SKILL_RESOURCE_PATH', 403);
  if (isPrivateSkillResource(relativePath))
    throw toolFailure('Credential resources cannot be read or copied from skills.', 'PROTECTED_SKILL_RESOURCE', 403);
  return { path, relativePath };
}

/** Read and hash one regular file, refusing a changed path before returning any bytes. */
export async function readSkillFile(file, { maxBytes = SKILL_PACKAGE_LIMITS.fileBytes, signal, denyResource } = {}) {
  cancelled(signal);
  assertSkillResourceAllowed(file, denyResource);
  const original = await inspectLocalPath(file, { allowMissing: true });
  if (!original) throw toolFailure('Skill resource was not found.', 'SKILL_RESOURCE_NOT_FOUND', 404);
  if (!original.isFile() || original.size > maxBytes)
    throw toolFailure('Skill resource is not a regular file or exceeds its size limit.', 'SKILL_RESOURCE_LIMIT');
  assertSkillResourceAllowed(await realpath(file), denyResource);
  const handle = await open(file, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0));
  try {
    const first = await handle.stat();
    if (!first.isFile() || first.nlink > 1 || first.size > maxBytes || first.dev !== original.dev || first.ino !== original.ino)
      throw toolFailure('Skill resource changed while opening.', 'APP_SKILL_CHANGED', 409);
    assertSkillResourceAllowed(await realpath(file), denyResource);
    const bytes = Buffer.alloc(first.size + 1);
    let count = 0;
    while (count < bytes.length) {
      cancelled(signal);
      const result = await handle.read(bytes, count, bytes.length - count, count);
      if (!result.bytesRead) break;
      count += result.bytesRead;
    }
    const after = await handle.stat(), pathInfo = await inspectLocalPath(file);
    if (count !== first.size || after.size !== first.size || after.mtimeMs !== first.mtimeMs || after.ctimeMs !== first.ctimeMs ||
        pathInfo.dev !== first.dev || pathInfo.ino !== first.ino || pathInfo.nlink > 1 ||
        pathInfo.size !== first.size || pathInfo.mtimeMs !== first.mtimeMs || pathInfo.ctimeMs !== first.ctimeMs)
      throw toolFailure('Skill resource changed while reading.', 'APP_SKILL_CHANGED', 409);
    assertSkillResourceAllowed(file, denyResource);
    assertSkillResourceAllowed(await realpath(file), denyResource);
    cancelled(signal);
    const content = bytes.subarray(0, count);
    return { bytes: content, size: count, sha256: createHash('sha256').update(content).digest('hex') };
  } finally { await handle.close(); }
}

export async function readSkillResource(skillRoot, path, { offset, limit, signal, denyResource } = {}) {
  const target = resolveSkillResource(skillRoot, path);
  assertSkillResourceAllowed(target.path, denyResource);
  offset = boundedInteger(offset, 0, 0, SKILL_PACKAGE_LIMITS.fileBytes);
  limit = boundedInteger(limit, SKILL_PACKAGE_LIMITS.previewChars, 1, SKILL_PACKAGE_LIMITS.previewChars);
  await inspectLocalPath(target.path);
  const canonicalRoot = await realpath(skillRoot), canonicalFile = await realpath(target.path);
  assertSkillResourceAllowed(canonicalFile, denyResource);
  // Windows short-name aliases must not bypass credential-name or package-boundary checks.
  if (!within(canonicalRoot, canonicalFile))
    throw toolFailure('Skill resource crosses its canonical package boundary.', 'UNSAFE_SKILL_RESOURCE_PATH', 403);
  resolveSkillResource(canonicalRoot, portable(relative(canonicalRoot, canonicalFile)));
  cancelled(signal);
  const file = await readSkillFile(target.path, { signal, denyResource });
  const common = { path: target.relativePath, mimeType: skillResourceMimeType(target.relativePath),
    byteLength: file.size, sha256: file.sha256 };
  let content;
  try { content = decoder.decode(file.bytes); } catch { content = null; }
  if (content == null || content.includes('\0'))
    return { ...common, kind: 'binary', encoding: null, content: null, readable: false,
      message: 'Binary asset retained in the package; the text reader does not decode it or expose base64.' };
  // Code-point pages do not split surrogate pairs at the preview boundary.
  const chars = Array.from(content), nextOffset = Math.min(chars.length, offset + limit);
  return { ...common, kind: 'text', encoding: 'utf-8', content: chars.slice(offset, nextOffset).join(''),
    offset, nextOffset, totalChars: chars.length, hasMore: nextOffset < chars.length, readable: true };
}

/** Bounded inventory is also the whitelist for native read-only snapshot copying. */
export async function inspectSkillPackage(skillRoot, { signal, hashes = false, denyResource } = {}) {
  skillRoot = resolve(skillRoot);
  assertSkillResourceAllowed(skillRoot, denyResource);
  const rootInfo = await inspectLocalPath(skillRoot), files = [], diagnostics = [];
  assertSkillResourceAllowed(await realpath(skillRoot), denyResource);
  if (!rootInfo.isDirectory()) throw toolFailure('Skill package root is not a directory.', 'INVALID_APP_SKILL');
  let visited = 0, totalBytes = 0, truncated = false, stopped = false;
  const walk = async (directory, depth) => {
    cancelled(signal);
    if (stopped) return;
    assertSkillResourceAllowed(directory, denyResource);
    assertSkillResourceAllowed(await realpath(directory), denyResource);
    if (depth > SKILL_PACKAGE_LIMITS.depth) {
      truncated = true;
      diagnostics.push(diagnostic('SKILL_PACKAGE_DEPTH', 'Skill package nesting exceeds the safe inventory limit.', portable(relative(skillRoot, directory))));
      return;
    }
    const entries = [];
    for await (const entry of await opendir(directory)) {
      cancelled(signal);
      if (++visited > SKILL_PACKAGE_LIMITS.entries) { truncated = true; stopped = true; break; }
      entries.push(entry.name);
    }
    entries.sort();
    for (const name of entries) {
      if (stopped) break;
      cancelled(signal);
      const file = join(directory, name), path = portable(relative(skillRoot, file));
      if (isPrivateSkillResource(path)) {
        diagnostics.push(diagnostic('PROTECTED_SKILL_RESOURCE', 'Credential resources are excluded from skill access and snapshots.', path));
        continue;
      }
      if (dependencyDirectories.has(name.toLowerCase())) {
        diagnostics.push(diagnostic('SKILL_DEPENDENCIES_EXCLUDED', 'Installed dependencies are excluded; requirements must be checked without auto-installing.', path, 'warning'));
        continue;
      }
      try {
        const target = resolveSkillResource(skillRoot, path);
        assertSkillResourceAllowed(target.path, denyResource);
        const info = await inspectLocalPath(target.path);
        assertSkillResourceAllowed(await realpath(target.path), denyResource);
        if (info.isDirectory()) { await walk(file, depth + 1); continue; }
        if (!info.isFile()) {
          diagnostics.push(diagnostic('UNSAFE_SKILL_RESOURCE', 'Only regular skill resource files are supported.', path));
          continue;
        }
        if (files.length >= SKILL_PACKAGE_LIMITS.files || info.size > SKILL_PACKAGE_LIMITS.fileBytes ||
            totalBytes + info.size > SKILL_PACKAGE_LIMITS.totalBytes) {
          truncated = true; stopped = true;
          diagnostics.push(diagnostic('SKILL_PACKAGE_LIMIT', 'Skill package exceeds its file count or byte budget.', path));
          break;
        }
        const data = hashes ? await readSkillFile(file, { signal, denyResource }) : null;
        files.push({ path: target.path, relativePath: target.relativePath, size: data?.size ?? info.size,
          mimeType: skillResourceMimeType(path), ...(data ? { sha256: data.sha256 } : {}) });
        totalBytes += data?.size ?? info.size;
      } catch (error) {
        if (signal?.aborted || error.name === 'AbortError') throw error;
        diagnostics.push(diagnostic(error.code ?? 'SKILL_RESOURCE_UNAVAILABLE', 'Skill resource is unavailable or failed the package safety checks.', path));
      }
    }
  };
  await walk(skillRoot, 0);
  if (visited > SKILL_PACKAGE_LIMITS.entries)
    diagnostics.push(diagnostic('SKILL_PACKAGE_ENTRIES', 'Skill package exceeds its directory entry budget.'));
  files.sort((a, b) => a.relativePath < b.relativePath ? -1 : a.relativePath > b.relativePath ? 1 : 0);
  return { skillRoot, files, totalBytes, truncated, valid: !truncated && !diagnostics.some(item => item.severity === 'error'), diagnostics };
}

export function skillScriptRuntime(path) {
  if (/\.(?:mjs|cjs|js)$/i.test(path)) return 'node';
  if (/\.py$/i.test(path)) return 'python';
  if (/\.ps1$/i.test(path)) return 'powershell';
  if (/\.(?:sh|bash)$/i.test(path)) return 'bash';
  if (/\.(?:cmd|bat)$/i.test(path)) return 'cmd';
  return 'unknown';
}

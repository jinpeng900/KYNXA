import { lstat, mkdir } from 'node:fs/promises';
import { basename, dirname, isAbsolute, join, parse, relative, resolve, sep } from 'node:path';

export function toolFailure(message, code = 'INVALID_TOOL_ARGUMENTS', statusCode = 400) {
  return Object.assign(new Error(message), { code, statusCode });
}

export function within(root, path) {
  const suffix = relative(resolve(root), resolve(path));
  return suffix === '' || (suffix !== '..' && !suffix.startsWith(`..${sep}`) && !isAbsolute(suffix));
}

export function isModelCredentialPath(path, dataHome, appDataRoot) {
  const modelConnection = (within(dataHome, path) || (appDataRoot && within(join(appDataRoot, 'Backups'), path))) &&
    /^connections(?:[.\-_]|$)/i.test(basename(path));
  const agentConfig = appDataRoot && [join(appDataRoot, 'Agent'), join(appDataRoot, 'Backups')].some(root => within(root, path)) &&
    /^config(?:[.\-_]|$)/i.test(basename(path));
  return Boolean(modelConnection || agentConfig);
}

/** Reject links/reparse directories at every existing component, including parents outside the work root. */
export async function inspectLocalPath(path, { allowMissing = false } = {}) {
  path = resolve(path);
  const root = parse(path).root;
  const components = relative(root, path).split(sep).filter(Boolean);
  let current = root, info;
  for (let index = -1; index < components.length; index++) {
    if (index >= 0) current = resolve(current, components[index]);
    try { info = await lstat(current); }
    catch (error) {
      if (error.code === 'ENOENT' && allowMissing) return null;
      throw error;
    }
    if (info.isSymbolicLink() || (index < components.length - 1 && !info.isDirectory()) ||
        (info.isFile() && info.nlink > 1))
      throw toolFailure('工具路径包含链接或不安全结构，操作已拒绝。', 'UNSAFE_TOOL_PATH', 403);
  }
  return info;
}

export function resolveToolPath(context, input, protectedRoots = []) {
  const value = input.path ?? '.';
  if (typeof value !== 'string' || !value.trim() || value.length > 4096 || /[\0\r\n]/.test(value))
    throw toolFailure('请提供有效文件路径。');
  // Device paths, alternate data streams and network shares do not belong to local UTF-8 tools.
  if (process.platform === 'win32' && (/^\\\\/.test(value) || /^[a-z]:(?:$|[^\\/])/i.test(value) || value.replace(/^[a-z]:/i, '').includes(':') ||
      value.split(/[\\/]/).some(part => (/[. ]$/.test(part) && !['.', '..'].includes(part)) ||
        /^(?:con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)/i.test(part))))
    throw toolFailure('工具不支持设备、网络或歧义路径。', 'UNSAFE_TOOL_PATH', 403);
  if (!isAbsolute(value) && !context.workspaceRoot)
    throw toolFailure('当前聊天没有关联工作文件夹，不能使用相对路径。', 'WORKSPACE_REQUIRED');
  const path = isAbsolute(value) ? resolve(value) : resolve(context.workspaceRoot, value);
  if (protectedRoots.some(root => within(root, path)))
    throw toolFailure('正式应用数据由专用服务管理，文件工具不能访问或改写。', 'PROTECTED_APP_DATA', 403);
  const outsideWorkspace = !context.workspaceRoot || !within(context.workspaceRoot, path);
  if (outsideWorkspace && (typeof input.reason !== 'string' || !input.reason.trim()))
    throw toolFailure('访问工作范围以外的路径必须说明原因。', 'OUTSIDE_WORKSPACE_REASON_REQUIRED', 403);
  return { path, outsideWorkspace };
}

export async function ensureLocalDirectory(path) {
  if (await inspectLocalPath(path, { allowMissing: true })) return;
  const parent = dirname(path);
  if (parent === path) throw toolFailure('不能创建文件系统根目录。', 'UNSAFE_TOOL_PATH', 403);
  await ensureLocalDirectory(parent);
  try { await mkdir(path, { mode: 0o700 }); }
  catch (error) { if (error.code !== 'EEXIST') throw error; }
  const info = await inspectLocalPath(path);
  if (!info.isDirectory()) throw toolFailure('应用工具目录结构无效。', 'UNSAFE_TOOL_PATH', 403);
}

export function objectInput(value) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw toolFailure('工具输入必须为对象。');
  return value;
}

export function boundedInteger(value, fallback, min, max) {
  value ??= fallback;
  if (!Number.isSafeInteger(value) || value < min || value > max) throw toolFailure(`整数参数须在 ${min}–${max} 范围内。`);
  return value;
}

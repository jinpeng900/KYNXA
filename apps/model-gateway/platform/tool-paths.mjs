import { lstat, mkdir, realpath } from 'node:fs/promises';
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

/**
 * Keep managed/sandbox paths link-free; explicit read-only callers may inspect hard-linked files.
 * 应用管理及沙箱路径保持无链接；明确的只读调用方可检查硬链接文件，仍检查所有祖先目录。
 */
export async function inspectLocalPath(path, { allowMissing = false, allowHardLinks = false } = {}) {
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
        (info.isFile() && info.nlink > 1 && !allowHardLinks))
      throw toolFailure('工具路径包含链接或不安全结构，操作已拒绝。', 'UNSAFE_TOOL_PATH', 403);
  }
  return info;
}

async function canonicalLocalPath(path, allowMissing) {
  let ancestor = resolve(path);
  const missing = [];
  for (;;) {
    try { return resolve(await realpath(ancestor), ...missing); }
    catch (error) {
      if (error.code !== 'ENOENT' || !allowMissing || ancestor === dirname(ancestor)) throw error;
      // A dangling link is not a new file: never silently replace an unresolved alias.
      // 悬空链接不是待创建文件，不能把未解析的别名静默当成新路径。
      try {
        if ((await lstat(ancestor)).isSymbolicLink())
          throw toolFailure('路径链接的目标不存在。', 'UNSAFE_TOOL_PATH', 403);
      } catch (inspectionError) { if (inspectionError.code !== 'ENOENT') throw inspectionError; }
      missing.unshift(basename(ancestor));
      ancestor = dirname(ancestor);
    }
  }
}

/**
 * Bind an explicitly selected host path to its real target before deciding scope and approval.
 * 应用管理目录仍用严格检查；用户宿主路径先绑定真实目标，再依据真实范围决定审批。
 */
export async function bindLocalPath(path, { workspaceRoot, allowMissing = false, allowHardLinks = false,
  protectedRoots = [] } = {}) {
  const requestedPath = resolve(path), canonicalPath = await canonicalLocalPath(requestedPath, allowMissing);
  const canonicalWorkspace = workspaceRoot ? await canonicalLocalPath(workspaceRoot, false) : null;
  const canonicalProtectedRoots = [];
  for (const root of protectedRoots) {
    const protectedRoot = await canonicalLocalPath(root, true);
    canonicalProtectedRoots.push(protectedRoot);
    if (within(root, requestedPath) || within(protectedRoot, canonicalPath))
      throw toolFailure('正式应用数据由专用服务管理，文件工具不能访问或改写。', 'PROTECTED_APP_DATA', 403);
  }
  await inspectLocalPath(canonicalPath, { allowMissing, allowHardLinks });
  return Object.freeze({ requestedPath, path: canonicalPath, requestedWorkspaceRoot: workspaceRoot ? resolve(workspaceRoot) : null,
    workspaceRoot: canonicalWorkspace, outsideWorkspace: !canonicalWorkspace || !within(canonicalWorkspace, canonicalPath),
    allowMissing, allowHardLinks, protectedRoots: Object.freeze([...protectedRoots]),
    canonicalProtectedRoots: Object.freeze(canonicalProtectedRoots) });
}

/**
 * Approval authorizes the bound target, not a mutable junction or symlink name.
 * 批准仅授权已绑定的真实目标；审批后链接改指向时，拒绝执行并重新发现。
 */
export async function revalidateLocalPathBinding(binding) {
  if (!binding || typeof binding.requestedPath !== 'string' || typeof binding.path !== 'string')
    throw toolFailure('缺少已授权路径的真实目标。', 'TOOL_PATH_CHANGED', 409);
  const current = await bindLocalPath(binding.requestedPath, {
    workspaceRoot: binding.requestedWorkspaceRoot, allowMissing: binding.allowMissing,
    allowHardLinks: binding.allowHardLinks, protectedRoots: binding.protectedRoots
  });
  if (current.path !== binding.path || current.workspaceRoot !== binding.workspaceRoot ||
      current.canonicalProtectedRoots.some((root, index) => root !== binding.canonicalProtectedRoots[index]))
    throw toolFailure('路径或工作文件夹的真实目标已改变，请重新读取后重试。', 'TOOL_PATH_CHANGED', 409);
  return inspectLocalPath(binding.path, { allowMissing: binding.allowMissing, allowHardLinks: binding.allowHardLinks });
}

export function resolveToolPath(context, input, protectedRoots = [], { deferScopeCheck = false } = {}) {
  const value = input.path ?? '.';
  if (typeof value !== 'string' || !value.trim() || value.length > 4096 || /[\0\r\n]/.test(value))
    throw toolFailure('请提供有效文件路径。');
  // Device paths, alternate data streams and network shares do not belong to local UTF-8 tools.
  // 设备路径、替代数据流和网络共享不属于本地 UTF-8 文件工具支持范围。
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
  // A bound host path is authorized against its canonical scope, not an alternate lexical alias.
  // 宿主路径绑定后应按真实范围授权，不应仅因另一词法别名就提前要求范围外理由。
  if (!deferScopeCheck && outsideWorkspace && (typeof input.reason !== 'string' || !input.reason.trim()))
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

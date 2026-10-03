import { existsSync, lstatSync, readFileSync } from 'node:fs';
import { link, mkdir, unlink, writeFile } from 'node:fs/promises';
import { randomUUID } from 'node:crypto';
import { homedir } from 'node:os';
import { dirname, isAbsolute, join, parse, relative, resolve, sep } from 'node:path';
import { conversationDataRoot } from './storage.mjs';

export const EXTENSION_STORAGE_PROTOCOL = 1;
export const EXTENSION_LAYOUT_VERSION = 1;
export const EXTENSION_LAYOUT_DIRECTORIES = Object.freeze(['Agent', 'Skills', 'MCP', 'MCP/npm-cache',
  'MCP/browser-cache', 'MCP/uv-cache', 'MCP/uv-tools', 'MCP/bin', 'MCP/python', 'MCP/python-bin', 'MCP/runtimes',
  'Backups', 'Backups/Extensions', 'Backups/Extensions/Migrations']);

function invalid(message, code = 'INVALID_EXTENSION_STORAGE') {
  return Object.assign(new Error(message), { code, statusCode: 409 });
}

function absolute(value, label) {
  if (typeof value !== 'string' || !value.trim() || value.length > 4096 || /[\0\r\n]/.test(value) || !isAbsolute(value) ||
      value.startsWith('\\\\') || (process.platform === 'win32' && !/^[a-z]:[\\/]/i.test(value)))
    throw invalid(`${label}必须为有效绝对路径。`);
  const path = resolve(value);
  if (path === parse(path).root) throw invalid(`${label}不能使用磁盘根目录。`);
  inspectPath(path);
  return path;
}

function inspectPath(path) {
  const root = parse(path).root, components = relative(root, path).split(sep).filter(Boolean);
  let current = root;
  for (let index = -1; index < components.length; index++) {
    if (index >= 0) current = join(current, components[index]);
    let info;
    try { info = lstatSync(current); }
    catch (error) { if (error.code === 'ENOENT') return; throw invalid('扩展位置路径不可访问，原配置已保留。'); }
    if (info.isSymbolicLink() || (index < components.length - 1 && !info.isDirectory()) ||
        (info.isFile() && info.nlink > 1)) throw invalid('扩展位置包含链接或不安全结构，原配置已保留。');
  }
}

export function extensionPointerPath(env = process.env, userHome = homedir()) {
  return env.KYNXA_EXTENSION_POINTER ? absolute(env.KYNXA_EXTENSION_POINTER, '扩展位置配置') :
    join(userHome, '.kynxa', 'extensions.json');
}

/** Configuration and maintenance belong to native settings, never generic model file mutations. */
export function extensionControlPaths(pointer = extensionPointerPath(), userHome = homedir()) {
  const profile = join(userHome, '.kynxa');
  return [...new Set([resolve(pointer), join(profile, 'storage.json'), join(profile, 'extensions.json'),
    ...[profile, dirname(pointer)].flatMap(directory => ['storage-migration.lock', 'extensions-operation.lock',
      'extension-migration.lock'].map(name => join(directory, name)))])];
}

export function isExtensionControlPath(path, pointer = extensionPointerPath(), userHome = homedir()) {
  // Ancestors cannot be deleted/moved to remove a protected configuration file either.
  return extensionControlPaths(pointer, userHome).some(control => {
    const suffix = relative(resolve(path), control);
    return suffix === '' || (suffix !== '..' && !suffix.startsWith(`..${sep}`) && !isAbsolute(suffix));
  });
}

export function isExtensionManagedPath(path, extensionRoot) {
  const root = resolve(extensionRoot), target = resolve(path), suffix = relative(root, target);
  if (suffix === '') return true;
  if (suffix === '..' || suffix.startsWith(`..${sep}`) || isAbsolute(suffix)) return false;
  const first = suffix.split(sep)[0].toLowerCase();
  return ['agent', 'skills', 'mcp', 'backups'].includes(first) ||
    (suffix.split(sep).length === 1 && ['extension-migration-info.json', 'extensions-pointer.previous.json',
      'extension-layout.json'].includes(first));
}

function parsePointer(content) {
  const value = JSON.parse(content), keys = new Set();
  let depth = 0;
  // JSON.parse accepts duplicate keys. Match C#'s strict top-level pointer semantics.
  for (let index = 0; index < content.length; index++) {
    const char = content[index];
    if (char === '"') {
      const start = index++;
      while (index < content.length && content[index] !== '"') { if (content[index] === '\\') index++; index++; }
      if (depth === 1 && /^\s*:/.test(content.slice(index + 1))) {
        const name = JSON.parse(content.slice(start, index + 1));
        if (keys.has(name)) throw invalid('扩展位置配置含重复字段，原文件已保留。');
        keys.add(name);
      }
    } else if (char === '{' || char === '[') depth++;
    else if (char === '}' || char === ']') depth--;
  }
  return value;
}

/** dataHome is the model store directory; the default is its formal conversation root. */
export function extensionHome(dataHome, env = process.env, userHome = homedir()) {
  if (env.KYNXA_EXTENSION_HOME) return absolute(env.KYNXA_EXTENSION_HOME, '扩展目录');
  const explicitPointer = env.KYNXA_EXTENSION_POINTER;
  // An isolated Data/Model override must never accidentally import a real user's extension settings.
  if (!explicitPointer && (env.KYNXA_DATA_HOME || env.KYNXA_MODEL_HOME)) return conversationDataRoot(dataHome);
  const pointer = extensionPointerPath(env, userHome);
  inspectPath(pointer);
  if (!existsSync(pointer)) return conversationDataRoot(dataHome);
  const info = lstatSync(pointer);
  if (!info.isFile() || info.isSymbolicLink() || info.nlink > 1 || info.size > 16384)
    throw invalid('扩展位置配置结构无效，原文件已保留。');
  let value;
  try { value = parsePointer(new TextDecoder('utf-8', { fatal: true }).decode(readFileSync(pointer)).replace(/^\uFEFF/, '')); }
  catch { throw invalid('扩展位置配置无效，原文件已保留。'); }
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw invalid('扩展位置配置无效。');
  if (value.version !== EXTENSION_STORAGE_PROTOCOL)
    throw invalid('扩展位置配置版本不受支持。', 'UNSUPPORTED_EXTENSION_STORAGE');
  return absolute(value.extensionRoot, '扩展目录');
}

export function extensionCacheDirectories(root) {
  root = absolute(root, '扩展目录');
  return { npmCache: join(root, 'MCP', 'npm-cache'), browserCache: join(root, 'MCP', 'browser-cache'),
    uvCache: join(root, 'MCP', 'uv-cache'), uvTools: join(root, 'MCP', 'uv-tools'), python: join(root, 'MCP', 'python'),
    pythonBin: join(root, 'MCP', 'python-bin'), uvToolBin: join(root, 'MCP', 'bin') };
}

/** Inspection is read-only; invalid versions or occupied framework paths precede every mkdir. */
export function inspectExtensionLayout(root) {
  root = absolute(root, '扩展目录');
  for (const path of [root, ...EXTENSION_LAYOUT_DIRECTORIES.map(name => join(root, name))]) {
    inspectPath(path);
    if (existsSync(path) && !lstatSync(path).isDirectory()) throw invalid('扩展目录结构无效，原文件已保留。', 'INVALID_EXTENSION_LAYOUT');
  }
  for (const name of ['Agent/config.json', 'extension-layout.json', 'extension-migration-info.json', 'extensions-pointer.previous.json']) {
    const path = join(root, name);
    inspectPath(path);
    if (existsSync(path) && !lstatSync(path).isFile()) throw invalid('扩展目录结构无效，原文件已保留。', 'INVALID_EXTENSION_LAYOUT');
  }
  const metadata = join(root, 'extension-layout.json');
  if (!existsSync(metadata)) return null;
  if (lstatSync(metadata).size > 16384) throw invalid('扩展配置格式无效，原文件已保留。', 'INVALID_EXTENSION_LAYOUT');
  let document;
  try { document = parsePointer(new TextDecoder('utf-8', { fatal: true }).decode(readFileSync(metadata)).replace(/^\uFEFF/, '')); }
  catch { throw invalid('扩展配置格式无效，原文件已保留。', 'INVALID_EXTENSION_LAYOUT'); }
  if (!document || typeof document !== 'object' || Array.isArray(document))
    throw invalid('扩展配置格式无效，原文件已保留。', 'INVALID_EXTENSION_LAYOUT');
  if (document.version !== EXTENSION_LAYOUT_VERSION)
    throw invalid('扩展配置版本不受当前程序支持，原文件已保留。', 'UNSUPPORTED_EXTENSION_LAYOUT');
  return document;
}

/** Only gateway-owned startup/activation initializes storage; lookup and inspection never write. */
const initializations = new Map();
export function ensureExtensionLayout(root, options = {}) {
  root = absolute(root, '扩展目录');
  if (initializations.has(root)) return initializations.get(root);
  const pending = createExtensionLayout(root, options).finally(() => initializations.delete(root));
  initializations.set(root, pending);
  return pending;
}

async function createExtensionLayout(root, { maintenanceActive = () => false } = {}) {
  root = absolute(root, '扩展目录');
  const previous = inspectExtensionLayout(root);
  const writable = () => {
    if (maintenanceActive()) throw invalid('正在迁移数据，请完成后再试。', 'STORAGE_MAINTENANCE_ACTIVE');
  };
  for (const path of [root, ...EXTENSION_LAYOUT_DIRECTORIES.map(name => join(root, name))]) {
    writable(); inspectPath(path);
    if (!existsSync(path)) await mkdir(path, { recursive: true, mode: 0o700 });
    if (!lstatSync(path).isDirectory()) throw invalid('扩展目录结构无效，原文件已保留。', 'INVALID_EXTENSION_LAYOUT');
    inspectPath(path);
  }
  if (!previous) {
    writable(); inspectPath(join(root, 'extension-layout.json'));
    const temporary = join(root, '.extension-layout.' + randomUUID() + '.tmp');
    try {
      await writeFile(temporary, JSON.stringify({ version: EXTENSION_LAYOUT_VERSION }), { flag: 'wx', mode: 0o600 });
      writable(); inspectPath(join(root, 'extension-layout.json'));
      // link publishes a complete file atomically and refuses to replace a racing owner.
      try { await link(temporary, join(root, 'extension-layout.json')); }
      catch (error) { if (error.code !== 'EEXIST') throw error; }
    } finally {
      try { await unlink(temporary); } catch (error) { if (error.code !== 'ENOENT') throw error; }
    }
  }
  inspectExtensionLayout(root);
}

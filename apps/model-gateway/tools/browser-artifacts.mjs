import { constants } from 'node:fs';
import { open, realpath } from 'node:fs/promises';
import { extname, isAbsolute, relative, resolve } from 'node:path';
import { inspectLocalPath, within } from '../platform/tool-paths.mjs';

const maximumImageBytes = 4 * 1024 * 1024;
const maximumPixels = 16000000;
const screenshotTools = new Set(['browser_take_screenshot', 'take_screenshot']);
const endpointOptions = ['--cdp-endpoint', '--endpoint', '--browser-url', '--browserUrl', '--ws-endpoint', '--wsEndpoint'];
const endpointEnvironment = ['PLAYWRIGHT_MCP_CDP_ENDPOINT', 'PLAYWRIGHT_MCP_ENDPOINT'];

export function isBrowserScreenshotTool(descriptor) {
  return descriptor?.operation === 'tools/call' && screenshotTools.has(descriptor.toolName);
}

function option(args, names) {
  for (let index = 0; index < args.length; index++) {
    for (const name of names) {
      if (args[index] === name) return args[index + 1];
      if (args[index].startsWith(name + '=')) return args[index].slice(name.length + 1);
    }
  }
}

function remoteBrowser(server) {
  if ((server?.transport ?? 'stdio') !== 'stdio') return true;
  if (endpointEnvironment.some(name => Object.hasOwn(server.envRefs ?? {}, name))) return true;
  const endpoint = option(server.args ?? [], endpointOptions) ?? endpointEnvironment.map(name => server.env?.[name]).find(Boolean);
  if (!endpoint) return false;
  try {
    const host = new URL(endpoint).hostname;
    return host !== 'localhost' && host !== '[::1]' && !/^127(?:\.\d{1,3}){3}$/.test(host);
  } catch { return true; }
}

/**
 * Header/structure bounds precede native image decoding in the local viewer.
 * 本地查看器原生解码图片前，先检查文件头和结构边界。
 */
export function inspectScreenshotImage(bytes, declaredMime) {
  if (!Buffer.isBuffer(bytes) || bytes.length === 0 || bytes.length > maximumImageBytes) return null;
  let mimeType, width, height;
  if (bytes.length >= 45 && bytes.subarray(0, 8).equals(Buffer.from([137, 80, 78, 71, 13, 10, 26, 10])) &&
      bytes.readUInt32BE(8) === 13 && bytes.toString('ascii', 12, 16) === 'IHDR') {
    mimeType = 'image/png'; width = bytes.readUInt32BE(16); height = bytes.readUInt32BE(20);
    let offset = 8, data = false, ended = false;
    while (offset + 12 <= bytes.length) {
      const length = bytes.readUInt32BE(offset), end = offset + length + 12;
      if (end > bytes.length) return null;
      const chunk = bytes.toString('ascii', offset + 4, offset + 8);
      if (chunk === 'IDAT') data = true;
      if (chunk === 'IEND') { ended = length === 0 && end === bytes.length; break; }
      offset = end;
    }
    if (!data || !ended) return null;
  } else if (bytes.length >= 4 && bytes[0] === 255 && bytes[1] === 216 &&
      bytes[bytes.length - 2] === 255 && bytes[bytes.length - 1] === 217) {
    mimeType = 'image/jpeg';
    let offset = 2;
    while (offset + 4 <= bytes.length) {
      if (bytes[offset++] !== 255) return null;
      while (bytes[offset] === 255) offset++;
      const marker = bytes[offset++];
      if (marker === 218 || marker === 217) break;
      if (marker === 1 || (marker >= 208 && marker <= 215)) continue;
      if (offset + 2 > bytes.length) return null;
      const length = bytes.readUInt16BE(offset);
      if (length < 2 || offset + length > bytes.length) return null;
      if ([192, 193, 194].includes(marker)) {
        if (length < 8) return null;
        height = bytes.readUInt16BE(offset + 3); width = bytes.readUInt16BE(offset + 5); break;
      }
      offset += length;
    }
  }
  if (!mimeType || (declaredMime && declaredMime !== mimeType) || !width || !height || width * height > maximumPixels) return null;
  return { mimeType, width, height };
}

function decodedImage(encoded, mimeType) {
  if (typeof encoded !== 'string' || encoded.length > Math.ceil(maximumImageBytes / 3) * 4 ||
      !/^[A-Za-z0-9+/]+={0,2}$/.test(encoded)) return null;
  const bytes = Buffer.from(encoded, 'base64');
  if (bytes.toString('base64') !== encoded) return null;
  const dimensions = inspectScreenshotImage(bytes, mimeType);
  return dimensions ? { type: 'image', data: encoded, mimeType: dimensions.mimeType } : null;
}

function safePath(value, root) {
  if (typeof value !== 'string' || !value || value.length > 4096 || /[\0\r\n]/.test(value) || /^\\\\/.test(value) ||
      /^[a-z][a-z0-9+.-]*:/i.test(value.replace(/^[a-z]:[\\/]/i, '')) ||
      (process.platform === 'win32' && (value.replace(/^[a-z]:/i, '').includes(':') ||
        value.split(/[\\/]/).some(part => (/[. ]$/.test(part) && !['.', '..'].includes(part)) ||
          /^(?:con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)/i.test(part))))) return null;
  return resolve(root, value);
}

async function artifactPaths(server, context, args, result) {
  const cwd = server.cwd ?? context.workspaceRoot;
  if (!cwd || !isAbsolute(cwd)) return [];
  const roots = [resolve(cwd)];
  const output = option(server.args ?? [], ['--output-dir']) ?? server.env?.PLAYWRIGHT_MCP_OUTPUT_DIR;
  if (output) { const directory = safePath(output, cwd); if (directory) roots.push(directory); }
  const aliases = [];
  for (const root of roots) {
    try {
      if (!(await inspectLocalPath(root)).isDirectory()) continue;
      const canonical = await realpath(root);
      if (!(await inspectLocalPath(canonical)).isDirectory()) continue;
      aliases.push({ lexical: root, canonical });
    } catch { /* Invalid configured roots cannot grant access to artifact paths. 无效的配置根目录不能授予附件路径访问权限。 */ }
  }
  const normalize = value => {
    const path = safePath(value, cwd);
    if (!path) return null;
    for (const root of aliases) {
      // Win32 expands 8.3 paths in browser receipts. Only known roots gain aliases;
      // an arbitrary path is never resolved to discover a wider read boundary.
      // Win32 会在浏览器回执中展开 8.3 短路径；只为已知根目录建立别名，不解析任意路径来扩大读取范围。
      if (within(root.lexical, path)) return resolve(root.canonical, relative(root.lexical, path));
      if (within(root.canonical, path)) return path;
    }
    return null;
  };
  const supplied = args.filename ?? args.filePath;
  let candidates = supplied === undefined ? [] : [supplied];
  // Chrome normalizes .jpg to .jpeg (and other extensions to the capture format).
  // Accept only its explicit saved-file receipt with the same requested stem.
  // Chrome 会按截图格式规范化扩展名，例如将 .jpg 改为 .jpeg；只接受文件主名与请求一致的明确保存回执。
  if (args.filePath !== undefined) {
    const requested = normalize(args.filePath), reported = []; let hasReceipt = false;
    for (const block of result.content ?? []) {
      if (block.type !== 'text' || typeof block.text !== 'string' || block.text.length > 65536) continue;
      for (const match of block.text.matchAll(/^Saved screenshot to (.+)\.$/gm)) {
        hasReceipt = true;
        const path = normalize(match[1]);
        if (path && requested && path.slice(0, path.length - extname(path).length) === requested.slice(0, requested.length - extname(requested).length))
          reported.push(match[1]);
      }
    }
    if (hasReceipt) candidates = reported;
  }
  // Only the known screenshot tool's explicit Markdown output links are considered.
  // A configured output root is required for generated paths; arbitrary result text never widens scope.
  // 仅识别已知截图工具明确返回的 Markdown 文件链接；生成路径必须有配置的输出根目录，任意结果文本不能扩大范围。
  if (supplied === undefined && output) for (const block of result.content ?? []) {
    if (block.type !== 'text' || typeof block.text !== 'string' || block.text.length > 65536) continue;
    for (const match of block.text.matchAll(/\[(?:Screenshot[^\]\r\n]*|Page screenshot)\]\(([^)\r\n]+)\)/g)) candidates.push(match[1]);
  }
  return [...new Set(candidates.slice(0, 4).map(normalize).filter(Boolean))];
}

async function readImage(path, signal) {
  signal?.throwIfAborted();
  const original = await inspectLocalPath(path);
  if (!original.isFile() || original.size === 0 || original.size > maximumImageBytes) return null;
  let handle;
  try {
    handle = await open(path, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0));
    const opened = await handle.stat();
    if (!opened.isFile() || opened.nlink !== 1 || opened.size !== original.size || opened.ino !== original.ino || opened.dev !== original.dev) return null;
    // Allocate only the bounded length from the opened receipt; a growing file cannot allocate unbounded memory.
    // 仅按已打开回执的受限长度分配内存，防止文件持续增长导致无限分配。
    const bytes = Buffer.alloc(opened.size); let offset = 0;
    while (offset < bytes.length) {
      signal?.throwIfAborted();
      const read = await handle.read(bytes, offset, bytes.length - offset, offset);
      if (!read.bytesRead) return null;
      offset += read.bytesRead;
    }
    const current = await inspectLocalPath(path), final = await handle.stat();
    if (current.ino !== opened.ino || current.dev !== opened.dev || final.size !== opened.size || final.mtimeMs !== opened.mtimeMs) return null;
    const dimensions = inspectScreenshotImage(bytes);
    return dimensions ? { type: 'image', data: bytes.toString('base64'), mimeType: dimensions.mimeType } : null;
  } finally { await handle?.close(); }
}

/**
 * Preserve every upstream block/field; append verified media without rewriting third-party metadata.
 * 保留上游所有块和字段，只追加已验证媒体，不改写第三方元信息。
 */
export async function archiveBrowserScreenshot(result, { descriptor, server = {}, context = {}, args = {}, signal } = {}) {
  const canonical = structuredClone(result);
  if (!isBrowserScreenshotTool(descriptor) || result.isError) return { canonical };
  const existing = (result.content ?? []).filter(block => block.type === 'image' && decodedImage(block.data, block.mimeType));
  if (existing.length) return { canonical, screenshotStatus: 'available' };
  const embedded = (result.content ?? []).filter(block => block.type === 'resource')
    .slice(0, 20).map(block => decodedImage(block.resource?.blob, block.resource?.mimeType)).find(Boolean);
  if (embedded) return { canonical: { ...canonical, content: [...(canonical.content ?? []), embedded] }, screenshotStatus: 'available' };
  if (remoteBrowser(server)) return { canonical, screenshotStatus: 'unavailable', screenshotCode: 'BROWSER_REMOTE_IMAGE_REQUIRED' };
  if (signal?.aborted) return { canonical, screenshotStatus: 'unavailable', screenshotCode: 'BROWSER_SCREENSHOT_PREVIEW_CANCELLED' };
  for (const path of await artifactPaths(server, context, args, result)) {
    try {
      const image = await readImage(path, signal);
      if (image) return { canonical: { ...canonical, content: [...(canonical.content ?? []), image] }, screenshotStatus: 'available' };
    } catch (error) {
      if (signal?.aborted || error?.name === 'AbortError')
        return { canonical, screenshotStatus: 'unavailable', screenshotCode: 'BROWSER_SCREENSHOT_PREVIEW_CANCELLED' };
    }
  }
  return { canonical, screenshotStatus: 'unavailable', screenshotCode: 'BROWSER_SCREENSHOT_ARTIFACT_UNAVAILABLE' };
}

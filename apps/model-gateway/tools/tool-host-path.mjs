import { access, lstat } from 'node:fs/promises';
import { constants } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const directory = dirname(dirname(fileURLToPath(import.meta.url)));

/**
 * The installed helper is preferred. No PATH lookup or user-configured MCP impersonates it.
 * 优先使用已安装助手，不通过 PATH 搜索，也不允许用户配置的 MCP 冒充它。
 */
export async function findNativeToolHost(toolHostPath, unavailableCode) {
  if (process.platform !== 'win32') throw Object.assign(new Error('The native tool host requires Windows.'), { code: unavailableCode });
  const candidates = toolHostPath ? [resolve(toolHostPath)] : [join(directory, '..', 'ToolHost', 'KYNXA.ToolHost.exe'),
    join(directory, '..', 'tool-host', 'KYNXA.ToolHost.exe'),
    ...['Debug', 'Release'].flatMap(configuration => [`win-${process.arch}`, ''].map(runtime =>
      join(directory, '..', 'tool-host', 'bin', configuration, 'net10.0-windows', runtime, 'KYNXA.ToolHost.exe')))];
  for (const path of candidates) {
    try {
      const info = await lstat(path);
      if (!info.isFile() || info.isSymbolicLink()) continue;
      await access(path, constants.R_OK);
      return path;
    } catch (error) { if (!['ENOENT', 'ENOTDIR'].includes(error.code)) throw error; }
  }
  throw Object.assign(new Error('The native KYNXA.ToolHost executable has not been built or packaged.'), { code: unavailableCode });
}

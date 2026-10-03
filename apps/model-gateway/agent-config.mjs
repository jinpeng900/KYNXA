import { readFile } from 'node:fs/promises';
import { isAbsolute, join, resolve } from 'node:path';
import { atomicJson } from './store.mjs';
import { ensureLocalDirectory, inspectLocalPath, objectInput, toolFailure } from './tool-paths.mjs';

const MAX_CONFIG_BYTES = 256 * 1024;
const configQueues = new Map();
export const LEGACY_MCP_PROTOCOL = '2025-11-25';
export const MODERN_MCP_PROTOCOL = '2026-07-28';

function string(value, label, max = 4096) {
  if (typeof value !== 'string' || !value.trim() || value.length > max || /[\0\r\n]/.test(value))
    throw toolFailure(`${label}格式无效。`, 'INVALID_AGENT_CONFIG');
  return value.trim();
}

function validateConfig(value) {
  objectInput(value);
  if (value.version !== 1) throw toolFailure('工具配置版本不受支持，原文件已保留。', 'UNSUPPORTED_AGENT_CONFIG', 409);
  if (!Number.isSafeInteger(value.revision) || value.revision < 0 || !Array.isArray(value.mcpServers) ||
      value.mcpServers.length > 32 || !Array.isArray(value.skillDirectories) || value.skillDirectories.length > 32)
    throw toolFailure('工具配置格式无效。', 'INVALID_AGENT_CONFIG');
  const ids = new Set();
  const mcpServers = value.mcpServers.map(server => {
    objectInput(server);
    const id = string(server.id, 'MCP ID', 40);
    if (!/^[a-z][a-z0-9-]{1,39}$/.test(id) || ids.has(id)) throw toolFailure('MCP ID 无效或重复。', 'INVALID_AGENT_CONFIG');
    ids.add(id);
    const command = string(server.command, 'MCP command');
    if (!Array.isArray(server.args) || server.args.length > 64 || server.args.some(arg =>
        typeof arg !== 'string' || arg.length > 16384 || arg.includes('\0')))
      throw toolFailure('MCP args 须为有效字符串数组。', 'INVALID_AGENT_CONFIG');
    if (typeof server.enabled !== 'boolean') throw toolFailure('请明确是否启用 MCP 服务。', 'INVALID_AGENT_CONFIG');
    const protocolVersion = server.protocolVersion ?? LEGACY_MCP_PROTOCOL;
    if (![LEGACY_MCP_PROTOCOL, MODERN_MCP_PROTOCOL].includes(protocolVersion))
      throw toolFailure('MCP 协议须明确为 2025-11-25 兼容或 2026-07-28。', 'INVALID_AGENT_CONFIG');
    return { id, name: string(server.name, 'MCP name', 100), command, args: [...server.args], enabled: server.enabled, protocolVersion };
  });
  const skillDirectories = [...new Set(value.skillDirectories.map(path => {
    path = string(path, '技能目录');
    if (!isAbsolute(path)) throw toolFailure('技能目录须为绝对路径。', 'INVALID_AGENT_CONFIG');
    return resolve(path);
  }))];
  return { version: 1, revision: value.revision, mcpServers, skillDirectories };
}

export class AgentConfigRepository {
  constructor(root) { this.root = resolve(root); this.folder = join(this.root, 'Agent'); this.file = join(this.folder, 'config.json'); }

  async read() {
    const info = await inspectLocalPath(this.file, { allowMissing: true });
    if (!info) return { version: 1, revision: 0, mcpServers: [], skillDirectories: [] };
    if (!info.isFile() || info.size > MAX_CONFIG_BYTES) throw toolFailure('工具配置文件损坏，原文件已保留。', 'CORRUPT_AGENT_CONFIG', 500);
    let value;
    try { value = JSON.parse((await readFile(this.file, 'utf8')).replace(/^\uFEFF/, '')); }
    catch (error) {
      if (error instanceof SyntaxError) throw toolFailure('工具配置文件损坏，原文件已保留。', 'CORRUPT_AGENT_CONFIG', 500);
      throw error;
    }
    try { return validateConfig(value); }
    catch (error) {
      if (error.code === 'UNSUPPORTED_AGENT_CONFIG') throw error;
      throw toolFailure('工具配置文件损坏，原文件已保留。', 'CORRUPT_AGENT_CONFIG', 500);
    }
  }

  async update(input) {
    objectInput(input);
    if (!Number.isSafeInteger(input.expectedRevision) || input.expectedRevision < 0)
      throw toolFailure('请提供工具配置 expectedRevision。', 'INVALID_AGENT_CONFIG');
    const previous = configQueues.get(this.file) ?? Promise.resolve();
    const result = previous.catch(() => {}).then(async () => {
      const current = await this.read();
      if (current.revision !== input.expectedRevision)
        throw toolFailure('工具配置已更新，请重新读取。', 'AGENT_CONFIG_CONFLICT', 409);
      const next = validateConfig({ ...input, revision: current.revision + 1 });
      if (Buffer.byteLength(JSON.stringify(next, null, 2)) > MAX_CONFIG_BYTES) throw toolFailure('工具配置过大。', 'INVALID_AGENT_CONFIG');
      for (const directory of next.skillDirectories) {
        const info = await inspectLocalPath(directory, { allowMissing: true });
        if (info && !info.isDirectory()) throw toolFailure('技能路径不是目录。', 'INVALID_AGENT_CONFIG');
      }
      await ensureLocalDirectory(this.folder);
      await inspectLocalPath(this.file, { allowMissing: true });
      await atomicJson(this.file, next);
      return structuredClone(next);
    });
    configQueues.set(this.file, result);
    try { return await result; }
    finally { if (configQueues.get(this.file) === result) configQueues.delete(this.file); }
  }
}

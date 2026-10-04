import { readFile } from 'node:fs/promises';
import { isAbsolute, join, resolve } from 'node:path';
import { atomicJson } from './store.mjs';
import { ensureLocalDirectory, inspectLocalPath, objectInput, toolFailure } from './tool-paths.mjs';
import { mcpEndpointIdentity, normalizeMcpConnection } from './mcp-config.mjs';
import { mergeOfficialConfig, splitOfficialConfig } from './agent-config-layers.mjs';
export { mcpEndpointIdentity, sameMcpEndpoint } from './mcp-config.mjs';

const MAX_CONFIG_BYTES = 256 * 1024;
const configQueues = new Map();
const overrideFields = new Set(['name', 'command', 'args', 'enabled', 'protocolVersion', 'disabledTools', 'transport', 'cwd',
  'env', 'envRefs', 'url', 'headerEnv', 'auth', 'startupTimeoutMs']);
export const LEGACY_MCP_PROTOCOL = '2025-11-25';
export const MODERN_MCP_PROTOCOL = '2026-07-28';

function string(value, label, max = 4096) {
  if (typeof value !== 'string' || !value.trim() || value.length > max || /[\0\r\n]/.test(value))
    throw toolFailure(`${label}格式无效。`, 'INVALID_AGENT_CONFIG');
  return value.trim();
}

function normalizeServer(server) {
  objectInput(server);
  const id = string(server.id, 'MCP ID', 40);
  if (!/^[a-z][a-z0-9-]{1,39}$/.test(id)) throw toolFailure('MCP ID 无效或重复。', 'INVALID_AGENT_CONFIG');
  const connection = normalizeMcpConnection(server);
  if (typeof server.enabled !== 'boolean') throw toolFailure('请明确是否启用 MCP 服务。', 'INVALID_AGENT_CONFIG');
  const protocolVersion = server.protocolVersion ?? LEGACY_MCP_PROTOCOL;
  if (![LEGACY_MCP_PROTOCOL, MODERN_MCP_PROTOCOL].includes(protocolVersion))
    throw toolFailure('MCP 协议须明确为 2025-11-25 兼容或 2026-07-28。', 'INVALID_AGENT_CONFIG');
  const disabledTools = server.disabledTools ?? [];
  if (!Array.isArray(disabledTools) || disabledTools.length > 4096 || disabledTools.some(name =>
      typeof name !== 'string' || !/^[a-zA-Z0-9_.-]{1,128}$/.test(name)))
    throw toolFailure('MCP 禁用工具列表无效。', 'INVALID_AGENT_CONFIG');
  return { id, name: string(server.name, 'MCP name', 100), ...connection, enabled: server.enabled, protocolVersion,
    disabledTools: [...new Set(disabledTools)] };
}

function presetIds(values) {
  if (!Array.isArray(values) || values.length > 64 || values.some(id => typeof id !== 'string' || !/^[a-z][a-z0-9-]{1,39}$/.test(id)))
    throw toolFailure('官方工具启停列表无效。', 'INVALID_AGENT_CONFIG');
  return [...new Set(values)];
}

function validateConfig(value, { rejectDuplicateEndpoints = true, maxServers = 32 } = {}) {
  objectInput(value);
  if (value.version !== 1) throw toolFailure('工具配置版本不受支持，原文件已保留。', 'UNSUPPORTED_AGENT_CONFIG', 409);
  if (!Number.isSafeInteger(value.revision) || value.revision < 0 || !Array.isArray(value.mcpServers) ||
      value.mcpServers.length > maxServers || !Array.isArray(value.skillDirectories) || value.skillDirectories.length > 32)
    throw toolFailure('工具配置格式无效。', 'INVALID_AGENT_CONFIG');
  const ids = new Set();
  const endpoints = new Set();
  const mcpServers = value.mcpServers.map(server => {
    const normalized = normalizeServer(server);
    if (ids.has(normalized.id)) throw toolFailure('MCP ID 无效或重复。', 'INVALID_AGENT_CONFIG');
    ids.add(normalized.id);
    const identity = mcpEndpointIdentity(normalized);
    if (rejectDuplicateEndpoints && endpoints.has(identity)) throw toolFailure('同一个 MCP 连接已配置，请编辑原连接。', 'DUPLICATE_MCP_ENDPOINT', 409);
    endpoints.add(identity);
    return normalized;
  });
  const skillDirectories = [...new Set(value.skillDirectories.map(path => {
    path = string(path, '技能目录');
    if (!isAbsolute(path)) throw toolFailure('技能目录须为绝对路径。', 'INVALID_AGENT_CONFIG');
    return resolve(path);
  }))];
  const disabledSkills = value.disabledSkills ?? [];
  if (!Array.isArray(disabledSkills) || disabledSkills.length > 4096 || disabledSkills.some(id =>
      typeof id !== 'string' || !/^[a-f0-9]{24}$/.test(id)))
    throw toolFailure('禁用技能列表无效。', 'INVALID_AGENT_CONFIG');
  const result = { version: 1, revision: value.revision, mcpServers, skillDirectories, disabledSkills: [...new Set(disabledSkills)] };
  if (value.disabledOfficialMcpServers != null) result.disabledOfficialMcpServers = presetIds(value.disabledOfficialMcpServers);
  if (value.officialMcpOverrides != null) {
    if (!Array.isArray(value.officialMcpOverrides) || value.officialMcpOverrides.length > 64)
      throw toolFailure('官方工具自定义配置无效。', 'INVALID_AGENT_CONFIG');
    const seen = new Set();
    result.officialMcpOverrides = value.officialMcpOverrides.map(item => {
      objectInput(item);
      const [presetId] = presetIds([item.presetId]);
      if (seen.has(presetId)) throw toolFailure('官方工具自定义配置重复。', 'INVALID_AGENT_CONFIG');
      seen.add(presetId);
      if (item.server) return { presetId, server: normalizeServer(item.server) };
      const id = string(item.id, 'MCP ID', 40);
      if (!/^[a-z][a-z0-9-]{1,39}$/.test(id)) throw toolFailure('MCP ID 无效。', 'INVALID_AGENT_CONFIG');
      objectInput(item.changes);
      if (Object.keys(item.changes).some(key => !overrideFields.has(key)))
        throw toolFailure('官方工具自定义字段无效。', 'INVALID_AGENT_CONFIG');
      return { presetId, id, changes: structuredClone(item.changes) };
    });
  }
  return result;
}

export class AgentConfigRepository {
  constructor(root, { officialPresets = null, officialToolsRoot, officialPackageVersion, normalizeDisabledSkills = value => value } = {}) {
    this.root = resolve(root); this.folder = join(this.root, 'Agent'); this.file = join(this.folder, 'config.json');
    this.officialPresets = officialPresets === null ? null : structuredClone(officialPresets);
    this.metadata = { officialToolsRoot, userToolsRoot: this.root, officialPackageVersion };
    this.normalizeDisabledSkills = normalizeDisabledSkills;
  }

  async read() {
    return this._effective(await this._readUser());
  }

  async _effective(user) {
    if (!this.officialPresets) return user;
    const result = mergeOfficialConfig(user, this.officialPresets, normalizeServer, this.metadata);
    result.disabledSkills = await this.normalizeDisabledSkills(result.disabledSkills);
    return result;
  }

  async _readUser() {
    const info = await inspectLocalPath(this.file, { allowMissing: true });
    if (!info) return { version: 1, revision: 0, mcpServers: [], skillDirectories: [], disabledSkills: [] };
    if (!info.isFile() || info.size > MAX_CONFIG_BYTES) throw toolFailure('工具配置文件损坏，原文件已保留。', 'CORRUPT_AGENT_CONFIG', 500);
    let value;
    try { value = JSON.parse((await readFile(this.file, 'utf8')).replace(/^\uFEFF/, '')); }
    catch (error) {
      if (error instanceof SyntaxError) throw toolFailure('工具配置文件损坏，原文件已保留。', 'CORRUPT_AGENT_CONFIG', 500);
      throw error;
    }
    try { return validateConfig(value, { rejectDuplicateEndpoints: false }); }
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
      const previousUser = await this._readUser(), current = await this._effective(previousUser);
      if (previousUser.revision !== input.expectedRevision)
        throw toolFailure('工具配置已更新，请重新读取。', 'AGENT_CONFIG_CONFLICT', 409);
      const effective = validateConfig({ ...input, revision: current.revision + 1 },
        { maxServers: 32 + (this.officialPresets?.length ?? 0) });
      const next = this.officialPresets
        ? validateConfig(splitOfficialConfig(effective, previousUser, current, this.officialPresets, normalizeServer)) : effective;
      if (Buffer.byteLength(JSON.stringify(next, null, 2)) > MAX_CONFIG_BYTES) throw toolFailure('工具配置过大。', 'INVALID_AGENT_CONFIG');
      for (const directory of next.skillDirectories) {
        const info = await inspectLocalPath(directory, { allowMissing: true });
        if (info && !info.isDirectory()) throw toolFailure('技能路径不是目录。', 'INVALID_AGENT_CONFIG');
      }
      const nextEffective = await this._effective(next);
      await ensureLocalDirectory(this.folder);
      await inspectLocalPath(this.file, { allowMissing: true });
      await atomicJson(this.file, next);
      return structuredClone(nextEffective);
    });
    configQueues.set(this.file, result);
    try { return await result; }
    finally { if (configQueues.get(this.file) === result) configQueues.delete(this.file); }
  }
}

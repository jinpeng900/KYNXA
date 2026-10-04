import { isDeepStrictEqual } from 'node:util';
import { sameMcpEndpoint } from './mcp-config.mjs';

/**
 * Publisher matching only prevents duplicate defaults; it never grants permission or rewrites an existing ID.
 * 发布者匹配只用于去除重复默认项，不授予权限，也不改写已有 ID。
 */
export function configuredMcpPreset(preset, servers) {
  return servers.find(server => server.origin === 'official' && server.presetId === preset.id) ??
    servers.find(server => sameMcpEndpoint(server, preset.server)) ?? servers.find(server => {
    if (preset.server.transport === 'streamable-http') return server.transport === 'streamable-http' &&
      server.url?.replace(/\/$/, '') === preset.server.url.replace(/\/$/, '');
    if ((server.transport ?? 'stdio') !== 'stdio' || !preset.package) return false;
    const packageName = preset.package.name;
    const executable = server.command.replaceAll('\\', '/').split('/').at(-1).replace(/\.(?:exe|cmd|ps1)$/i, '');
    if (executable === preset.package.entryPoint) return true;
    return server.args?.some(argument => argument === packageName ||
      argument.startsWith(packageName + (preset.package.registry === 'npm' ? '@' : '==')) ||
      argument.replaceAll('\\', '/').includes('/node_modules/' + packageName + '/') ||
      (preset.package.registry === 'pypi' && argument === packageName.replaceAll('-', '_'))) ?? false;
  });
}

export function officialMcpServerId(presetId, occupied = new Set()) {
  const base = `official-${presetId}`;
  let id = base, suffix = 2;
  while (occupied.has(id)) id = `${base}-${suffix++}`;
  return id;
}

/**
 * Official files remain immutable. This is a disposable execution/settings projection over the user document.
 * 官方文件保持只读；此处只是基于用户配置生成可丢弃的执行和设置视图。
 */
export function mergeOfficialConfig(user, presets, normalizeServer, metadata = {}) {
  const mcpServers = user.mcpServers.map(server => ({ ...structuredClone(server), origin: 'user' }));
  const occupied = new Set(mcpServers.map(server => server.id));
  const hidden = new Set(user.disabledOfficialMcpServers ?? []);
  const overrides = new Map((user.officialMcpOverrides ?? []).map(item => [item.presetId, item]));
  for (const preset of presets) {
    if (hidden.has(preset.id)) continue;
    const override = overrides.get(preset.id);
    if (!override) {
      const existing = configuredMcpPreset(preset, mcpServers);
      if (existing) { existing.presetId ??= preset.id; continue; }
    }
    const id = override?.id ?? override?.server?.id ?? officialMcpServerId(preset.id, occupied);
    // An older/custom configuration owns its existing ID, even after a package update.
    // 旧配置或自定义配置始终保留自己的 ID，包升级也不改变其归属。
    if (occupied.has(id)) continue;
    const server = normalizeServer(override?.server ?? { ...preset.server, ...override?.changes, id });
    if (mcpServers.some(existing => sameMcpEndpoint(existing, server))) continue;
    occupied.add(id);
    mcpServers.push({ ...server, origin: 'official', presetId: preset.id, overridden: Boolean(override) });
  }
  return { version: user.version, revision: user.revision, mcpServers, skillDirectories: [...user.skillDirectories],
    disabledSkills: [...user.disabledSkills], disabledOfficialMcpServers: [...hidden], ...metadata };
}

/**
 * Save explicit user choices, never a copy of every official default. Removed official rows stay hidden.
 * 只保存用户明确选择，不复制整份官方默认配置；删除的官方项继续保持隐藏。
 */
export function splitOfficialConfig(effective, previousUser, current, presets, normalizeServer) {
  const byId = new Map(current.mcpServers.filter(server => server.origin === 'official').map(server => [server.id, server.presetId]));
  const byPreset = new Map(presets.map(preset => [preset.id, preset]));
  const previousUserIds = new Set(previousUser.mcpServers.map(server => server.id));
  const hidden = new Set(effective.disabledOfficialMcpServers ?? previousUser.disabledOfficialMcpServers ?? []);
  const selectedIds = new Set(effective.mcpServers.map(server => server.id));
  for (const [id, presetId] of byId) if (!selectedIds.has(id)) hidden.add(presetId);
  const visiblePresets = new Set(byId.values());
  const mcpServers = [], officialMcpOverrides = (previousUser.officialMcpOverrides ?? [])
    .filter(item => !visiblePresets.has(item.presetId)).map(item => structuredClone(item));
  for (const server of effective.mcpServers) {
    // Metadata supplied by a client is ignored; only the current ID and the reserved default ID identify a layer.
    // 忽略客户端传入的来源元信息，仅通过当前 ID 和保留的默认 ID 判断配置层。
    const presetId = byId.get(server.id) ?? (!previousUserIds.has(server.id)
      ? presets.find(preset => officialMcpServerId(preset.id, previousUserIds) === server.id)?.id : undefined);
    if (!presetId) { mcpServers.push(server); continue; }
    const preset = byPreset.get(presetId);
    const baseline = normalizeServer({ ...preset.server, id: server.id });
    hidden.delete(presetId);
    const previousOverrideIndex = officialMcpOverrides.findIndex(item => item.presetId === presetId);
    if (previousOverrideIndex >= 0) officialMcpOverrides.splice(previousOverrideIndex, 1);
    if (!isDeepStrictEqual(server, baseline)) {
      const changes = Object.fromEntries(Object.keys({ ...baseline, ...server }).filter(key => key !== 'id' &&
        !isDeepStrictEqual(server[key], baseline[key])).map(key => [key, server[key] ?? null]));
      officialMcpOverrides.push({ presetId, id: server.id, changes });
    }
  }
  return { version: effective.version, revision: effective.revision, mcpServers,
    skillDirectories: effective.skillDirectories, disabledSkills: effective.disabledSkills,
    officialMcpOverrides, disabledOfficialMcpServers: [...hidden] };
}

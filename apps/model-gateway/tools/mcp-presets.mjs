import { configuredMcpPreset as configuredPreset, officialMcpServerId } from './agent-config-layers.mjs';
import { objectInput, toolFailure } from '../platform/tool-paths.mjs';
import { curatedMcpPresets as presets } from './mcp-preset-catalog.mjs';

export function mcpPresetCatalog(config) {
  return { presets: presets.map(preset => {
    const existing = configuredPreset(preset, config.mcpServers);
    return { ...structuredClone(preset), alreadyConfigured: Boolean(existing), ...(existing ? { configuredServerId: existing.id } : {}) };
  }), reusedCapabilities: ['filesystem', 'chat-memory', 'work-memory', 'tool-results', 'sandbox-terminal',
    'host-terminal', 'desktop-control', 'public-web-fetch'] };
}

export async function addMcpPreset(service, id, input) {
  objectInput(input);
  const preset = presets.find(item => item.id === id);
  if (!preset) throw toolFailure('MCP 预设不存在。', 'MCP_PRESET_NOT_FOUND', 404);
  const config = await service.getConfig();
  if (input.expectedRevision !== config.revision) throw toolFailure('工具配置已变化，请刷新后重试。', 'AGENT_CONFIG_CONFLICT', 409);
  if (configuredPreset(preset, config.mcpServers)) return config;
  if (service.officialTools) {
    const serverId = officialMcpServerId(preset.id, new Set(config.mcpServers.map(server => server.id)));
    return service.updateConfig({ ...config, expectedRevision: config.revision,
      disabledOfficialMcpServers: (config.disabledOfficialMcpServers ?? []).filter(hidden => hidden !== preset.id),
      mcpServers: [...config.mcpServers, { ...structuredClone(preset.server), id: serverId }] });
  }
  let serverId = preset.id, suffix = 2;
  while (config.mcpServers.some(server => server.id === serverId)) serverId = `${preset.id}-${suffix++}`;
  return service.updateConfig({ ...config, expectedRevision: config.revision,
    mcpServers: [...config.mcpServers, { ...structuredClone(preset.server), id: serverId }] });
}

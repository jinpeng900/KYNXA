import { sameMcpEndpoint } from './agent-config.mjs';
import { objectInput, toolFailure } from './tool-paths.mjs';
import { curatedMcpPresets as presets } from './mcp-preset-catalog.mjs';

// Upstream servers are connected as MCP services; no second filesystem or memory implementation is installed.
function configuredPreset(preset, servers) {
  return servers.find(server => sameMcpEndpoint(server, preset.server)) ?? servers.find(server => {
    if (preset.server.transport === 'streamable-http') return server.transport === 'streamable-http' &&
      server.url?.replace(/\/$/, '') === preset.server.url.replace(/\/$/, '');
    // Adding a curated service must not replace an existing publisher installation,
    // even when its version, credentials or process flags were customized.
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

export function mcpPresetCatalog(config) {
  return { presets: presets.map(preset => {
    const existing = configuredPreset(preset, config.mcpServers);
    return { ...structuredClone(preset), alreadyConfigured: Boolean(existing), ...(existing ? { configuredServerId: existing.id } : {}) };
  }), reusedCapabilities: ['filesystem', 'chat-memory', 'work-memory', 'tool-results', 'sandbox-terminal'] };
}

export async function addMcpPreset(service, id, input) {
  objectInput(input);
  const preset = presets.find(item => item.id === id);
  if (!preset) throw toolFailure('MCP 预设不存在。', 'MCP_PRESET_NOT_FOUND', 404);
  const config = await service.getConfig();
  if (input.expectedRevision !== config.revision) throw toolFailure('工具配置已变化，请刷新后重试。', 'AGENT_CONFIG_CONFLICT', 409);
  if (configuredPreset(preset, config.mcpServers)) return config;
  let serverId = preset.id, suffix = 2;
  while (config.mcpServers.some(server => server.id === serverId)) serverId = `${preset.id}-${suffix++}`;
  return service.updateConfig({ ...config, expectedRevision: config.revision,
    mcpServers: [...config.mcpServers, { ...structuredClone(preset.server), id: serverId }] });
}

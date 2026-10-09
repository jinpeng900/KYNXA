import { isDeepStrictEqual } from 'node:util';

export function connectionConfiguration(server) {
  if (!server) return null;
  const { name, disabledTools, ...connection } = server;
  return connection;
}

/** Reconnect only changed execution endpoints; names and tool toggles do not replace processes.
 * 只重连执行端点变化的服务，名称和单工具启停不替换进程。
 */
export function changedMcpServerIds(previous, current) {
  const before = new Map((previous.mcpServers ?? []).map(server => [server.id, server]));
  const after = new Map((current.mcpServers ?? []).map(server => [server.id, server]));
  return [...new Set([...before.keys(), ...after.keys()])].filter(id =>
    !isDeepStrictEqual(connectionConfiguration(before.get(id)), connectionConfiguration(after.get(id))));
}

/** A configuration edit revokes only the capability whose authority or execution identity changed.
 * 配置修改仅撤销权限或执行身份确实变化的能力，不影响无关工具。
 */
export function hasToolConfigurationChanged(previous, current, descriptor, args = {}) {
  if (!previous || !current || !descriptor) return false;
  if (descriptor.source?.startsWith('mcp:')) {
    const before = previous.mcpServers.find(server => server.id === descriptor.serverId);
    const after = current.mcpServers.find(server => server.id === descriptor.serverId);
    return !after?.enabled || after.disabledTools?.includes(descriptor.toolName) ||
      !isDeepStrictEqual(connectionConfiguration(before), connectionConfiguration(after));
  }
  if (descriptor.name === 'skill.run') {
    return !isDeepStrictEqual(previous.skillDirectories, current.skillDirectories) ||
      current.disabledSkills?.includes(args.id);
  }
  return false;
}

export function isConfiguredToolEnabled(config, descriptor) {
  if (!descriptor.source?.startsWith('mcp:')) return descriptor.enabled !== false;
  const server = config?.mcpServers.find(item => item.id === descriptor.serverId);
  return server?.enabled === true && !server.disabledTools?.includes(descriptor.toolName);
}

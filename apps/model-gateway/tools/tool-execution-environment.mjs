import { browserConnection } from './browser-connections.mjs';
import { normalizeToolExecutionEnvironment } from '../platform/tool-execution-environment.mjs';

function network(requestOrigin = 'unknown') {
  return { requestOrigin, egress: 'unknown', proxy: 'unknown', sameEgressDoesNotProveSameMachine: true };
}

function environment(executorKind = 'unknown', executorLocation = 'unknown', operationLocation = 'unknown',
  requestOrigin = 'unknown', locationScope = 'unknown') {
  return { schemaVersion: 1, executorKind, executorLocation, operationLocation, locationScope,
    gatewayHostMeaning: 'machine-running-the-gateway', userDeviceRelationship: 'unverified',
    grantsPermission: false, network: network(requestOrigin) };
}

function configuredServer(descriptor, servers) {
  const serverId = descriptor?.serverId;
  if (typeof serverId !== 'string' || descriptor.source !== `mcp:${serverId}` ||
      !descriptor.name?.startsWith(`mcp.${serverId}.`)) return undefined;
  const values = servers instanceof Map ? [...servers.values()] : Array.isArray(servers) ? servers : [];
  if (values.length > 64) return undefined;
  return values.find(server => server?.id === serverId && server.enabled !== false);
}

function browserTarget(server, trustedBrowserTargets) {
  const verified = trustedBrowserTargets instanceof Map ? trustedBrowserTargets.get(server.id) : undefined;
  if (verified && ['gateway-host', 'remote-browser', 'unknown'].includes(verified.location)) {
    return { location: verified.location, evidence: 'application-verified-browser-target' };
  }
  const connection = browserConnection(server);
  if (!connection) return undefined;
  const local = ['existing-browser', 'independent-browser'].includes(connection.mode);
  // A configured endpoint may be a local CDP bridge. Do not equate a remote-style transport with a cloud machine.
  // 配置端点可能是本机 CDP 桥接，不能把远程形式的传输直接认作云端机器；自定义配置也保持未知。
  return { location: local ? 'gateway-host' : 'unknown', mode: connection.mode,
    evidence: 'trusted-browser-configuration', ...(connection.mode === 'independent-browser'
      ? { visibility: connection.headless ? 'headless' : 'visible' } : {}) };
}

/** Project only catalog ownership and trusted app configuration; tool arguments, descriptions and metadata are ignored.
 * 仅投影目录归属与可信应用配置，忽略工具参数、第三方说明及 metadata；调用方再注入正式描述符/回执。
 * Transport provenance is not network egress evidence, user-device identity or an authorization grant.
 * 传输来源不证明网络出口、用户设备身份，也不授予操作权限。 */
export function resolveToolExecutionEnvironment(descriptor, { servers = [], trustedBrowserTargets, context } = {}) {
  const name = typeof descriptor?.name === 'string' ? descriptor.name : '';
  if (descriptor?.source === 'builtin') {
    if (name === 'web.fetch') {
      return normalizeToolExecutionEnvironment(environment('builtin-http', 'gateway-host', 'gateway-host', 'gateway-host', 'builtin-execution-policy'));
    }
    if (name.startsWith('terminal.host.')) {
      return normalizeToolExecutionEnvironment(environment('host-terminal', 'gateway-host', 'gateway-host', 'tool-defined-unknown', 'builtin-execution-policy'));
    }
    if (name.startsWith('computer.')) {
      return normalizeToolExecutionEnvironment(environment('desktop-tool-host', 'gateway-host', 'gateway-host', 'unknown', 'builtin-execution-policy'));
    }
    if (name === 'terminal.run' || name === 'skill.run') {
      const sandbox = context?.sandboxCapabilities;
      const verified = sandbox?.available === true && sandbox.sandbox === 'appcontainer' &&
        sandbox.failClosed === true && sandbox.checksChildToken === true;
      return normalizeToolExecutionEnvironment({ ...environment('sandbox-terminal', 'gateway-host', 'gateway-host',
        verified && sandbox.network === false ? 'unavailable' : 'unknown', 'builtin-execution-policy'),
      isolation: 'windows-appcontainer', sandboxVerified: verified });
    }
    return normalizeToolExecutionEnvironment(environment('builtin-tool', 'gateway-host', 'gateway-host', 'unknown', 'builtin-execution-policy'));
  }
  const server = configuredServer(descriptor, servers);
  if (!server) return normalizeToolExecutionEnvironment(environment());
  const target = browserTarget(server, trustedBrowserTargets);
  if ((server.transport ?? 'stdio') === 'stdio') {
    // stdio proves only where the configured transport process starts; wrappers may call remote tools or proxies.
    // stdio 仅证明配置的传输进程在哪里启动，包装进程仍可能调用远端工具或代理，不能推断下游在本机。
    return normalizeToolExecutionEnvironment({ ...environment('mcp-stdio', 'gateway-host', 'unknown', 'tool-defined-unknown',
      'configured-transport-process'), ...(target ? { browserTarget: target } : {}) });
  }
  if (server.transport === 'streamable-http') {
    // An HTTP service endpoint does not prove the physical server or its downstream request origin.
    // HTTP 服务端点不能证明物理服务器位置或下游请求来源，配置 URL 与凭据完全不进入此投影。
    return normalizeToolExecutionEnvironment({ ...environment('mcp-http', 'remote-service', 'unknown', 'tool-defined-unknown',
      'configured-service-endpoint'), serviceMachineIdentity: 'unknown', ...(target ? { browserTarget: target } : {}) });
  }
  return normalizeToolExecutionEnvironment(environment());
}

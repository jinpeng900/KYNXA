import { createHash } from 'node:crypto';
import { isAbsolute, resolve } from 'node:path';
import { objectInput, toolFailure } from './tool-paths.mjs';

const ENV_NAME = /^[a-zA-Z_][a-zA-Z0-9_]{0,127}$/;
const HEADER_NAME = /^[!#$%&'*+.^_`|~a-zA-Z0-9-]{1,100}$/;
const OWNED_HEADERS = new Set(['host', 'content-length', 'content-type', 'accept', 'origin', 'mcp-session-id', 'mcp-protocol-version']);
const invalid = () => toolFailure('MCP 连接配置无效；凭据只能使用环境变量引用。', 'INVALID_AGENT_CONFIG');

export function mcpUrl(value, { issuer = false } = {}) {
  if (typeof value !== 'string' || value.length > 4096 || /[\0\r\n]/.test(value)) throw invalid();
  let url;
  try { url = new URL(value); } catch { throw invalid(); }
  const loopback = url.hostname === 'localhost' || url.hostname === '[::1]' || /^127(?:\.\d{1,3}){3}$/.test(url.hostname);
  if (url.username || url.password || url.search || url.hash ||
      (url.protocol !== 'https:' && !(url.protocol === 'http:' && loopback && !issuer))) throw invalid();
  return issuer ? value : url.href;
}

function map(value, kind) {
  if (value == null) return {};
  objectInput(value);
  const entries = Object.entries(value);
  if (entries.length > 64) throw invalid();
  const seen = new Set();
  for (const [key, item] of entries) {
    if (!(kind === 'header' ? HEADER_NAME : ENV_NAME).test(key) ||
        (kind === 'header' && (OWNED_HEADERS.has(key.toLowerCase()) || seen.has(key.toLowerCase()))) ||
        typeof item !== 'string' || /[\0\r\n]/.test(item) || item.length > 16384 ||
        (kind !== 'constant' && !ENV_NAME.test(item)) ||
        (kind === 'constant' && /(?:KEY|TOKEN|SECRET|PASSWORD|CREDENTIAL)/i.test(key))) throw invalid();
    seen.add(key.toLowerCase());
  }
  return Object.fromEntries(entries);
}

export function normalizeMcpConnection(server) {
  const transport = server.transport ?? 'stdio';
  if (!['stdio', 'streamable-http'].includes(transport)) throw invalid();
  const startupTimeoutMs = server.startupTimeoutMs ?? 15000;
  if (!Number.isInteger(startupTimeoutMs) || startupTimeoutMs < 1000 || startupTimeoutMs > 120000) throw invalid();
  if (transport === 'stdio') {
    if (typeof server.command !== 'string' || !server.command.trim() || server.command.length > 4096 || /[\0\r\n]/.test(server.command) ||
        !Array.isArray(server.args) || server.args.length > 64 || server.args.some(arg =>
          typeof arg !== 'string' || arg.length > 16384 || arg.includes('\0'))) throw invalid();
    const env = map(server.env, 'constant');
    const envRefs = map(server.envRefs, 'reference');
    if (Object.keys(env).some(key => Object.hasOwn(envRefs, key)) || server.url || server.auth || Object.keys(server.headerEnv ?? {}).length) throw invalid();
    let cwd;
    if (server.cwd !== undefined && server.cwd !== null && server.cwd !== '') {
      if (typeof server.cwd !== 'string' || server.cwd.length > 4096 || /[\0\r\n]/.test(server.cwd) || !isAbsolute(server.cwd)) throw invalid();
      cwd = resolve(server.cwd);
    }
    return { transport, command: server.command.trim(), args: [...server.args], ...(cwd ? { cwd } : {}), env, envRefs, startupTimeoutMs };
  }
  if (server.cwd || Object.keys(server.env ?? {}).length || Object.keys(server.envRefs ?? {}).length) throw invalid();
  const url = mcpUrl(server.url);
  const headerEnv = map(server.headerEnv, 'header');
  let auth;
  if (server.auth !== undefined && server.auth !== null) {
    objectInput(server.auth);
    if (server.auth.type === 'bearer-env' && ENV_NAME.test(server.auth.tokenEnv ?? '')) {
      auth = { type: 'bearer-env', tokenEnv: server.auth.tokenEnv };
    } else if (server.auth.type === 'oauth-client-credentials' && ENV_NAME.test(server.auth.clientSecretEnv ?? '') &&
        typeof server.auth.clientId === 'string' && server.auth.clientId.trim() && server.auth.clientId.length <= 256 &&
        !/[\0\r\n]/.test(server.auth.clientId) && (server.auth.scope == null ||
          (typeof server.auth.scope === 'string' && server.auth.scope.length <= 2000 && !/[\0\r\n]/.test(server.auth.scope)))) {
      auth = { type: 'oauth-client-credentials', clientId: server.auth.clientId.trim(), clientSecretEnv: server.auth.clientSecretEnv,
        issuer: mcpUrl(server.auth.issuer, { issuer: true }), ...(server.auth.scope ? { scope: server.auth.scope } : {}) };
    } else throw invalid();
    if (Object.keys(headerEnv).some(key => key.toLowerCase() === 'authorization')) throw invalid();
  }
  return { transport, command: '', args: [], url, headerEnv, ...(auth ? { auth } : {}), startupTimeoutMs };
}

function ordered(map) { return Object.entries(map ?? {}).sort(([a], [b]) => a.localeCompare(b)); }

/**
 * Configuration identity excludes display names and IDs, but preserves distinct process instances.
 * 配置身份不包含显示名称和 ID，但仍区分不同进程实例。
 */
export function mcpEndpointIdentity(server) {
  const value = normalizeMcpConnection(server);
  return createHash('sha256').update(JSON.stringify(value.transport === 'stdio'
    ? [value.transport, value.command, value.args, value.cwd ?? null, ordered(value.env), ordered(value.envRefs)]
    : [value.transport, value.url, ordered(value.headerEnv), value.auth ?? null])).digest('hex');
}

export function sameMcpEndpoint(left, right) { return mcpEndpointIdentity(left) === mcpEndpointIdentity(right); }

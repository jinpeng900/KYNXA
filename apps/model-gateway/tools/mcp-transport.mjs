import { auth, ClientCredentialsProvider, StreamableHTTPClientTransport, UnauthorizedError } from '@modelcontextprotocol/client';
import { getDefaultEnvironment, StdioClientTransport } from '@modelcontextprotocol/client/stdio';
import { execFile } from 'node:child_process';
import { join } from 'node:path';
import { promisify } from 'node:util';
import { MODERN_MCP_PROTOCOL } from './agent-config.mjs';
import { mcpUrl, normalizeMcpConnection } from './mcp-config.mjs';
import { ensureLocalDirectory, toolFailure } from '../platform/tool-paths.mjs';
import { extensionCacheDirectories } from '../data/extension-storage.mjs';

// Pinned modern stdio uses the SDK's documented in-place negotiation path,
// avoiding the disposable sibling-process probe used for the base class.
// 固定现代 stdio 协议使用 SDK 公开的原位协商路径，避免基类通过临时同级进程探测。
const executeFile = promisify(execFile);
class OwnedWindowsStdioTransport extends StdioClientTransport {
  async close() {
    if (this.ownedClose) return this.ownedClose;
    // Capture the SDK-owned root while it is still alive. Killing that root
    // first would orphan npx/npm's Node descendants and keep their pipes open.
    // 趁 SDK 拥有的根进程仍存活时记录其身份；先杀根进程会使 npx/npm 的 Node 子进程失去父进程并保持管道打开。
    const pid = this.pid;
    this.ownedClose = (async () => {
      let failure;
      if (Number.isSafeInteger(pid) && pid > 0) {
        const systemRoot = process.env.SystemRoot ?? process.env.WINDIR;
        try {
          if (!systemRoot) throw new Error('Windows system directory unavailable.');
          await executeFile(join(systemRoot, 'System32', 'taskkill.exe'), ['/PID', String(pid), '/T', '/F'],
            { windowsHide: true, shell: false, timeout: 5000, maxBuffer: 32768 });
        } catch (error) {
          // 128 means the SDK-owned root exited before teardown. Do not search
          // for or kill unrelated processes based on names or broad patterns.
          // 退出码 128 表示 SDK 根进程已在清理前退出；不按名称或宽泛模式查找和终止其他进程。
          if (error.code !== 128) failure = toolFailure('MCP 自有进程清理未完成。', 'MCP_PROCESS_CLEANUP_FAILED', 502);
        }
      }
      await super.close();
      if (failure) throw failure;
    })();
    return this.ownedClose;
  }
}
const StdioTransport = process.platform === 'win32' ? OwnedWindowsStdioTransport : StdioClientTransport;
class PinnedModernStdioTransport extends StdioTransport {}
const LOCAL_CODES = new Set(['MCP_ENV_MISSING', 'MCP_AUTH_ORIGIN_REJECTED', 'MCP_INVALID_CATALOG', 'MCP_PROTOCOL_MISMATCH',
  'MCP_INPUT_REQUIRED', 'MCP_CONNECTION_CAPACITY', 'MCP_NOT_CONNECTED', 'MCP_NOT_ENABLED', 'MCP_CATALOG_CHANGED', 'MCP_PROCESS_CLEANUP_FAILED',
  'MCP_CONFIG_REQUIRED', 'MCP_COMMAND_NOT_FOUND']);

function environment(name) {
  const value = process.env[name];
  if (typeof value !== 'string' || !value || value.length > 16384 || /[\0\r\n]/.test(value))
    throw toolFailure('MCP 所需的环境变量未配置或无效。', 'MCP_ENV_MISSING', 409);
  return value;
}

/**
 * Never propagate SDK messages: they can contain URLs, headers or process arguments.
 * 不直接传播 SDK 错误消息，因为其中可能包含 URL、请求头或进程参数。
 */
export function mcpFailure(error, fallback = 'MCP_CONNECTION_FAILED') {
  const code = error?.code ?? error?.cause?.code;
  if (LOCAL_CODES.has(error?.code))
    return toolFailure('MCP 连接或请求未完成。', error.code, error.statusCode ?? 409);
  if (error instanceof UnauthorizedError || ['CLIENT_HTTP_AUTHENTICATION', 'CLIENT_HTTP_AUTHORIZATION', 'invalid_client', 'invalid_token', 'insufficient_scope'].includes(code))
    return toolFailure('MCP 服务需要有效认证，请检查环境变量引用。', 'MCP_AUTH_REQUIRED', 401);
  if (['REQUEST_TIMEOUT', 'ETIMEDOUT', 'UND_ERR_CONNECT_TIMEOUT'].includes(code)) return toolFailure('MCP 请求超时；未自动重试。', 'MCP_TIMEOUT', 504);
  if (code === 'ENOENT') return toolFailure('MCP 启动程序或运行目录不存在，请检查依赖与配置。', 'MCP_COMMAND_NOT_FOUND', 409);
  if (['CONNECTION_CLOSED', 'NOT_CONNECTED', 'SEND_FAILED', 'ECONNREFUSED', 'ECONNRESET', 'EPIPE', 'UND_ERR_SOCKET', 'ENOTFOUND'].includes(code))
    return toolFailure('MCP 连接已中断，请手动重新连接；调用未自动重放。', 'MCP_CONNECTION_LOST', 409);
  return toolFailure('MCP 连接或请求失败；调用未自动重试。', fallback, 502);
}

export async function createMcpTransport(server, context, { fetch: fetchImpl = globalThis.fetch, extensionRoot = context?.extensionRoot } = {}) {
  const value = normalizeMcpConnection(server);
  if (value.transport === 'stdio') {
    // A shipped configuration template is enabled but cannot execute until its path is supplied.
    // 随包配置模板默认启用，但必填路径未填写前不能执行。
    if (value.args.some(argument => argument.includes('<absolute-path-to-')))
      throw toolFailure('MCP 配置模板中的文件路径尚未填写。', 'MCP_CONFIG_REQUIRED', 409);
    const env = { ...getDefaultEnvironment(), ...value.env };
    for (const [name, reference] of Object.entries(value.envRefs)) env[name] = environment(reference);
    if (extensionRoot) {
      const directories = extensionCacheDirectories(extensionRoot);
      const explicit = new Set([...Object.keys(value.env), ...Object.keys(value.envRefs)].map(name => name.toLowerCase()));
      for (const [name, directory] of [['npm_config_cache', directories.npmCache], ['PLAYWRIGHT_BROWSERS_PATH', directories.browserCache],
        ['UV_CACHE_DIR', directories.uvCache], ['UV_TOOL_DIR', directories.uvTools], ['UV_PYTHON_INSTALL_DIR', directories.python],
        ['UV_PYTHON_BIN_DIR', directories.pythonBin], ['UV_TOOL_BIN_DIR', directories.uvToolBin]]) {
        if (explicit.has(name.toLowerCase())) continue;
        await ensureLocalDirectory(directory);
        env[name] = directory;
      }
      // Copied package files can move with Extensions without cross-root hard links.
      // 复制的包文件可随 Extensions 移动，不依赖跨根目录硬链接。
      if (!explicit.has('uv_link_mode')) env.UV_LINK_MODE = 'copy';
    }
    const Transport = server.protocolVersion === MODERN_MCP_PROTOCOL ? PinnedModernStdioTransport : StdioTransport;
    return new Transport({ command: value.command, args: value.args, env,
      ...(value.cwd || context?.workspaceRoot ? { cwd: value.cwd ?? context.workspaceRoot } : {}),
      stderr: 'ignore', maxBufferSize: 2 * 1024 * 1024 });
  }
  const headers = new Headers();
  for (const [name, reference] of Object.entries(value.headerEnv)) headers.set(name, environment(reference));
  const resourceOrigin = new URL(value.url).origin;
  const issuerOrigin = value.auth?.issuer ? new URL(value.auth.issuer).origin : undefined;
  // Auth discovery may follow metadata links. Credentials and requests remain
  // confined to the configured resource / issuer origins; redirects are refused.
  // 认证发现可能跟随元数据链接；凭据和请求仅限配置的资源或签发者来源，并拒绝重定向。
  const safeFetch = async (input, init = {}) => {
    const url = new URL(input instanceof Request ? input.url : input);
    mcpUrl(url.href);
    if (url.origin !== resourceOrigin && url.origin !== issuerOrigin)
      throw toolFailure('MCP 认证元数据指向未配置的服务。', 'MCP_AUTH_ORIGIN_REJECTED', 403);
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), value.startupTimeoutMs); timer.unref();
    try {
      return await fetchImpl(input, { ...init, redirect: 'error',
        signal: init.signal ? AbortSignal.any([init.signal, controller.signal]) : controller.signal });
    } finally { clearTimeout(timer); }
  };
  let authProvider;
  if (value.auth?.type === 'bearer-env') authProvider = { token: async () => environment(value.auth.tokenEnv) };
  else if (value.auth?.type === 'oauth-client-credentials') {
    const provider = new ClientCredentialsProvider({ clientId: value.auth.clientId,
      clientSecret: environment(value.auth.clientSecretEnv), expectedIssuer: value.auth.issuer,
      ...(value.auth.scope ? { scope: value.auth.scope } : {}) });
    await auth(provider, { serverUrl: value.url, scope: value.auth.scope, fetchFn: safeFetch });
    // Authentication happens during explicit connection only. Expose token()
    // without onUnauthorized so a completed tool POST is never retried for auth.
    // 仅在显式连接时认证；只提供 token()，不接入 onUnauthorized，防止已完成的工具 POST 因认证重试。
    authProvider = { token: async () => provider.tokens()?.access_token };
  }
  return new StreamableHTTPClientTransport(new URL(value.url), { requestInit: { headers, redirect: 'error' },
    fetch: safeFetch, ...(authProvider ? { authProvider } : {}), onInsufficientScope: 'throw',
    reconnectionOptions: { maxReconnectionDelay: 1000, initialReconnectionDelay: 1000, reconnectionDelayGrowFactor: 1, maxRetries: 0 } });
}

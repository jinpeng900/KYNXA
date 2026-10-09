import { createHash } from 'node:crypto';
import { Client, isInputRequiredResult } from '@modelcontextprotocol/client';
import { LEGACY_MCP_PROTOCOL, MODERN_MCP_PROTOCOL } from './agent-config.mjs';
import { createMcpTransport, mcpFailure } from './mcp-transport.mjs';
import { objectInput, toolFailure } from '../platform/tool-paths.mjs';
import { archiveBrowserScreenshot } from './browser-artifacts.mjs';
import { BrowserSessionRegistry, browserOperation } from './browser-sessions.mjs';
import { connectionConfiguration } from './tool-configuration.mjs';

const executionNotDispatched = Symbol('mcp-execution-not-dispatched');

/**
 * Only the adapter's pre-RPC path can create this marker; server fields cannot.
 * 只有适配器在 RPC 之前的路径能创建此标记，服务端字段不能伪造。
 */
export const isMcpExecutionNotDispatched = error => error?.[executionNotDispatched] === true;

function markExecutionNotDispatched(error) {
  // Abort reasons may be shared by already-dispatched calls. Never mark that
  // shared object; only this queue's owned wrapper carries execution metadata.
  // 取消理由可能被已派发的调用共享；不修改共享对象，仅在当前队列拥有的包装错误中记录执行元信息。
  const failure = new Error(typeof error?.message === 'string' ? error.message : 'MCP 调用尚未发出。', { cause: error });
  if (typeof error?.name === 'string') failure.name = error.name;
  if (typeof error?.code === 'string' || typeof error?.code === 'number') failure.code = error.code;
  Object.defineProperty(failure, executionNotDispatched, { value: true });
  return failure;
}

function waitForBrowserDispatch(operation, signal, hasDispatched) {
  if (!signal) return operation;
  return new Promise((resolve, reject) => {
    let settled = false;
    const finish = (settle, value) => {
      if (settled) return;
      settled = true; signal.removeEventListener('abort', cancelled); settle(value);
    };
    const cancelled = () => {
      if (!hasDispatched()) finish(reject, markExecutionNotDispatched(signal.reason));
    };
    signal.addEventListener('abort', cancelled, { once: true });
    operation.then(value => finish(resolve, value), error => finish(reject, error));
    if (signal.aborted) cancelled();
  });
}

const MAX_CONNECTIONS = 32;
const MAX_MCP_TOOLS = 128;
const CONNECT_TIMEOUT_MS = 15000;
const MAX_CATALOG_METADATA_ENTRIES = 64;
const MAX_CATALOG_METADATA_BYTES = 8 * 1024 * 1024;

function nestedArgumentSchema(originalInputSchema) {
  const schema = structuredClone(originalInputSchema);
  const schemaMaps = ['properties', 'patternProperties', '$defs', 'definitions', 'dependentSchemas', 'dependencies'];
  const schemaArrays = ['allOf', 'anyOf', 'oneOf', 'prefixItems'];
  const schemaValues = ['additionalProperties', 'unevaluatedProperties', 'propertyNames', 'not', 'if', 'then', 'else',
    'contains', 'items', 'additionalItems', 'unevaluatedItems', 'contentSchema'];
  const visit = (node, hasOwnResource = false) => {
    if (!node || typeof node !== 'object' || Array.isArray(node)) return;
    hasOwnResource ||= typeof node.$id === 'string';
    if (!hasOwnResource) for (const keyword of ['$ref', '$dynamicRef', '$recursiveRef']) {
      const reference = node[keyword];
      if (reference === '#' || (typeof reference === 'string' && reference.startsWith('#/')))
        node[keyword] = '#/properties/arguments' + reference.slice(1);
    }
    for (const keyword of schemaMaps) for (const child of Object.values(node[keyword] ?? {})) visit(child, hasOwnResource);
    for (const keyword of schemaArrays) if (Array.isArray(node[keyword])) for (const child of node[keyword]) visit(child, hasOwnResource);
    for (const keyword of schemaValues) {
      if (Array.isArray(node[keyword])) for (const child of node[keyword]) visit(child, hasOwnResource);
      else visit(node[keyword], hasOwnResource);
    }
  };
  // A local JSON pointer referred to the original root before this application
  // envelope existed. Preserve that target without rewriting literal/default data.
  // 应用包装出现前，本地 JSON 指针指向原 schema 根；保留其目标，不改写字面量或默认值。
  visit(schema);
  return schema;
}

function callSchema(originalInputSchema) {
  // A nested $schema does not select the envelope root dialect. For example,
  // draft-07 tuple items must not be interpreted as 2020-12 prefixItems.
  // 嵌套 $schema 不决定包装根的方言，例如不能把 draft-07 元组误解释为 2020-12 prefixItems。
  return { ...(typeof originalInputSchema.$schema === 'string' ? { $schema: originalInputSchema.$schema } : {}),
    type: 'object', properties: {
    arguments: nestedArgumentSchema(originalInputSchema),
    policy: { type: 'object', properties: { reason: { type: 'string', minLength: 1, maxLength: 2000,
      description: 'Explain this call to the user-enabled external MCP process. This application reason is never a server argument.' } },
    required: ['reason'], additionalProperties: false }
  }, required: ['arguments', 'policy'], additionalProperties: false };
}

function serverArguments(input) {
  objectInput(input); objectInput(input.arguments); objectInput(input.policy);
  if (Object.keys(input).some(key => !['arguments', 'policy'].includes(key)) || Object.keys(input.policy).some(key => key !== 'reason'))
    throw toolFailure('MCP 调用须将业务参数和应用审批理由分开。', 'INVALID_MCP_ENVELOPE');
  if (typeof input.policy.reason !== 'string' || !input.policy.reason.trim() || input.policy.reason.length > 2000)
    throw toolFailure('调用外部 MCP 服务必须提供有效的审批理由。', 'OUTSIDE_WORKSPACE_REASON_REQUIRED', 403);
  return structuredClone(input.arguments);
}

function withoutMetadata(value) {
  if (Array.isArray(value)) return value.map(withoutMetadata);
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value)
    .filter(([key]) => key !== '_meta').map(([key, child]) => [key, withoutMetadata(child)]));
  return value;
}

// Provider text is a projection. The canonical result below retains typed blocks,
// structured content and client-only metadata for separate attachment/viewer use.
// 供应商文本只是视图，正式结果保留带类型的内容块、结构化内容及客户端元信息，供附件和查看器分别使用。
function resultPreview(result) {
  const sections = (result.content ?? []).map(block => {
    if (block.type === 'text') return block.text;
    if (block.type === 'resource') {
      const resource = block.resource ?? {};
      const label = `[MCP resource; URI: ${resource.uri ?? 'unspecified'}; MIME: ${resource.mimeType ?? 'unspecified'}]`;
      return typeof resource.text === 'string' ? `${label}\n${resource.text}` : `${label}\nBinary resource retained in the full result for viewing.`;
    }
    if (block.type === 'resource_link') return `[MCP resource link; URI: ${block.uri ?? 'unspecified'}; MIME: ${block.mimeType ?? 'unspecified'}; name: ${block.name ?? 'unspecified'}]`;
    if (block.type === 'image' || block.type === 'audio')
      return `[MCP ${block.type}; MIME: ${block.mimeType ?? 'unspecified'}; retained in the full result for attachment/viewing, not sent inline.]`;
    return `[MCP unsupported content type: ${block.type ?? 'unspecified'}; retained in the full result for viewing.]`;
  });
  if (result.structuredContent !== undefined) sections.push(`MCP structured content:\n${JSON.stringify(withoutMetadata(result.structuredContent))}`);
  return sections.join('\n') || 'MCP returned an empty result.';
}

export function mcpResourceDescriptors(server, key, capabilities, names = new Set()) {
  if (!capabilities?.resources) return [];
  const cursor = { type: 'string', maxLength: 2000 };
  return [
    ['kynxa_resources_list', 'resources/list', { type: 'object', properties: { cursor }, additionalProperties: false }],
    ['kynxa_resources_templates_list', 'resources/templates/list', { type: 'object', properties: { cursor }, additionalProperties: false }],
    ['kynxa_resources_read', 'resources/read', { type: 'object', properties: { uri: { type: 'string', minLength: 1, maxLength: 8192 } }, required: ['uri'], additionalProperties: false }]
  ].filter(([name]) => !names.has(name)).map(([name, operation, originalInputSchema]) => ({ name: `mcp.${server.id}.${name}`,
    description: `${server.name}: ${operation}. This calls the external MCP service and requires application approval.`,
    inputSchema: callSchema(originalInputSchema), originalInputSchema, source: `mcp:${server.id}`,
    serverId: server.id, toolName: name, operation, key }));
}

function descriptors(server, key, listing, capabilities) {
  if (!Array.isArray(listing.tools) || listing.tools.length > MAX_MCP_TOOLS)
    throw toolFailure('MCP 工具目录超限或无效。', 'MCP_INVALID_CATALOG');
  const names = new Set();
  const tools = listing.tools.map(tool => {
    if (typeof tool.name !== 'string' || !/^[a-zA-Z0-9_.-]{1,128}$/.test(tool.name) || names.has(tool.name) ||
        !tool.inputSchema || tool.inputSchema.type !== 'object' || Buffer.byteLength(JSON.stringify(tool.inputSchema)) > 32000)
      throw toolFailure('MCP 工具定义无效。', 'MCP_INVALID_CATALOG');
    names.add(tool.name);
    const originalInputSchema = structuredClone(tool.inputSchema);
    return { name: `mcp.${server.id}.${tool.name}`, description: `${server.name}: ${String(tool.description ?? tool.name).slice(0, 2000)}`,
      inputSchema: callSchema(originalInputSchema), source: `mcp:${server.id}`, serverId: server.id,
      toolName: tool.name, operation: 'tools/call', key, originalInputSchema };
  });
  return [...tools, ...mcpResourceDescriptors(server, key, capabilities, names)];
}

/**
 * MCP dependencies are externally configured, never a claim that their processes are sandboxed.
 * MCP 依赖由外部配置管理，不能据此声称其进程受到沙箱保护。
 */
export class McpToolClients {
  constructor({ fetch, extensionRoot } = {}) {
    this.connections = new Map(); this.errors = new Map(); this.closed = false;
    this.states = new Map(); this.servers = new Map(); this.fetch = fetch;
    this.extensionRoot = extensionRoot;
    this.failedClosures = new Map();
    this.connectionGeneration = 0;
    this.resetOperation = null;
    this.browserSessions = new BrowserSessionRegistry();
    this.browserApprovals = new WeakMap();
    this.browserRecoveries = new WeakMap();
    this.diagnosticRevisions = new WeakMap();
    this.connectionOwners = new Map();
    this.serverResets = new Map();
    this.serverGenerations = new Map();
    this.serverPolicies = new Map();
    this.catalogMetadata = new Map();
    this.readyConnections = new Map();
    this.catalogMetadataBytes = 0;
  }

  _rememberCatalog(server, key, tools) {
    this._forgetCatalog(key);
    const bytes = Buffer.byteLength(JSON.stringify(tools));
    if (bytes > MAX_CATALOG_METADATA_BYTES) return;
    this.catalogMetadata.set(key, { serverId: server.id, tools: structuredClone(tools), bytes });
    this.catalogMetadataBytes += bytes;
    while (this.catalogMetadata.size > MAX_CATALOG_METADATA_ENTRIES || this.catalogMetadataBytes > MAX_CATALOG_METADATA_BYTES)
      this._forgetCatalog(this.catalogMetadata.keys().next().value);
  }

  _forgetCatalog(key) {
    this.catalogMetadataBytes -= this.catalogMetadata.get(key)?.bytes ?? 0;
    this.catalogMetadata.delete(key);
  }

  /** Configuration headers and previously validated catalogs do not start a
   * process, authorize a call, or claim a disconnected tool is executable.
   * 配置服务头和已验证目录缓存不启动进程、不授权调用，也不将未连接工具标为可执行。
   */
  discovery(config, context) {
    return (config.mcpServers ?? []).filter(server => server.enabled).map(server => {
      const key = this._key(server, context), cached = this.catalogMetadata.get(key);
      const ready = this.readyConnections.get(key);
      const state = ready && !ready.closed ? this.states.get(server.id)?.state ?? 'ready' :
        this.connections.has(key) ? 'connecting' : this.errors.has(server.id) ? 'error' : 'disconnected';
      return { id: server.id, name: `mcp.${server.id}`, description: `${server.name}: configured MCP service; load this name to connect and verify its current tools.`,
        transport: server.transport ?? 'stdio', state,
        // Never expose connection arguments, secret references or unverified schemas as facts.
        // 不暴露连接参数、密钥引用，也不把未经验证的 schema 当成当前事实。
        tools: (cached?.tools ?? []).filter(tool => !server.disabledTools?.includes(tool.toolName)).map(tool => ({ ...structuredClone(tool),
          description: `${server.name}: ${tool.description.slice(tool.description.indexOf(':') + 1).trim()}`,
          discoveryOnly: true, catalogVerified: false, needsConnection: true })) };
    });
  }

  /** Update execution policy synchronously without changing transport ownership.
   * 同步更新执行策略，不改变传输所有权或重启已有连接。
   */
  updateServerPolicies(servers) {
    const configured = new Map(servers.map(server => [server.id, structuredClone(server)]));
    for (const id of new Set([...this.serverPolicies.keys(), ...this.servers.keys()]))
      this.serverPolicies.set(id, configured.get(id) ?? { id, enabled: false });
    for (const [id, server] of configured) this.serverPolicies.set(id, server);
  }

  _assertServerToolPolicy(descriptor, connection) {
    const policy = this.serverPolicies.get(descriptor.serverId) ?? connection.artifactContext?.server;
    if (policy?.enabled === false)
      throw toolFailure('此 MCP 服务已停用，请使用当前启用服务。', 'MCP_NOT_ENABLED', 409);
    if (policy?.disabledTools?.includes(descriptor.toolName))
      throw toolFailure('此 MCP 工具已停用，请使用当前启用工具。', 'TOOL_DISABLED', 409);
  }

  _ownsDiagnosis(connection) {
    return !!connection.diagnosticState && !!connection.operation && !connection.closed && !connection.closing && !this.resetOperation && !this.closed &&
      this.connections.get(connection.key) === connection.operation &&
      this.states.get(connection.serverId) === connection.diagnosticState;
  }

  _recordFailure(connection, code) {
    if (!this._ownsDiagnosis(connection)) return;
    const state = connection.diagnosticState;
    this.diagnosticRevisions.set(state, (this.diagnosticRevisions.get(state) ?? 0) + 1);
    state.code = code;
    if (code === 'MCP_AUTH_REQUIRED') state.state = 'auth-required';
    this.errors.set(connection.serverId, code);
  }

  _recordRecovery(connection, revision) {
    // A late success must not erase a failure observed after its dispatch or
    // revive a disconnected/replaced connection. Diagnostics are not catalog identity.
    // 晚到成功不能清除派发后出现的故障，也不能恢复已断开或替换的连接；诊断恢复不改变目录身份。
    if (!this._ownsDiagnosis(connection) || (this.diagnosticRevisions.get(connection.diagnosticState) ?? 0) !== revision) return;
    const state = connection.diagnosticState;
    state.state = 'ready'; delete state.code;
    this.errors.delete(connection.serverId);
  }

  _assertConnectionGeneration(generation) {
    if (this.closed) throw toolFailure('MCP 客户端已关闭。', 'TOOL_SERVICE_CLOSED', 409);
    if (this.resetOperation || generation !== this.connectionGeneration)
      throw toolFailure('MCP 连接在发现期间已重置，请开始新请求。', 'AGENT_CONFIG_CHANGED', 409);
  }

  _cleanupFailure() {
    return toolFailure('MCP 自有进程未安全关闭，请重启模型服务；未重新连接。', 'MCP_PROCESS_CLEANUP_FAILED', 502);
  }

  async _closeConnection(key, connection, { startup = false } = {}) {
    if (connection.closeOperation) return connection.closeOperation;
    const operation = this.connections.get(key);
    connection.closed = true; connection.closing = true;
    if (this.readyConnections.get(key) === connection) this.readyConnections.delete(key);
    this.browserSessions.remove(key);
    connection.closeOperation = Promise.resolve().then(async () => {
      let failed = false;
      try { await connection.client.close(); } catch { failed = true; }
      // A failed negotiation can leave a started transport detached from Client.
      // 协商失败时，已启动的 transport 可能脱离 Client，仍需要清理。
      if (startup) try { await connection.transport?.close(); } catch { failed = true; }
      if (failed) {
        this.failedClosures.set(key, connection);
        const state = this.states.get(connection.serverId);
        if (state) { state.state = 'error'; state.toolCount = 0; state.code = 'MCP_PROCESS_CLEANUP_FAILED'; }
        this.errors.set(connection.serverId, 'MCP_PROCESS_CLEANUP_FAILED');
        throw this._cleanupFailure();
      }
      if (this.connections.get(key) === operation) {
        this.connections.delete(key); this.connectionOwners.delete(key);
      }
    });
    return connection.closeOperation;
  }

  _key(server, context) {
    return createHash('sha256').update(JSON.stringify([connectionConfiguration(server), context?.workspaceRoot ?? null,
      this.extensionRoot ?? context?.extensionRoot ?? null])).digest('hex');
  }

  async _connect(server, context) {
    this._assertConnectionGeneration(this.connectionGeneration);
    if (this.serverResets.has(server.id))
      throw toolFailure('此 MCP 服务配置正在更新，请重新准备此服务调用。', 'MCP_CATALOG_CHANGED', 409);
    if (this.failedClosures.size) throw this._cleanupFailure();
    const key = this._key(server, context);
    if (this.connections.has(key)) return this.connections.get(key);
    if (this.connections.size >= MAX_CONNECTIONS) throw toolFailure('MCP 连接已达上限，请刷新连接。', 'MCP_CONNECTION_CAPACITY', 409);
    this.servers.set(server.id, structuredClone(server));
    if (!this.serverPolicies.has(server.id)) this.serverPolicies.set(server.id, structuredClone(server));
    this.connectionOwners.set(key, server.id);
    const state = { serverId: server.id, transport: server.transport ?? 'stdio', state: 'connecting', toolCount: 0,
      resourceCapabilities: { resources: false, templates: false }, generation: (this.states.get(server.id)?.generation ?? 0) + 1 };
    this.states.set(server.id, state);
    const operation = (async () => {
      const version = server.protocolVersion ?? LEGACY_MCP_PROTOCOL;
      let connection;
      const changed = () => {
        if (!connection || connection.closed || this.connections.get(key) !== operation) return Promise.resolve();
        if (connection.refreshing) { connection.refreshAgain = true; return connection.refreshing; }
        connection.refreshing = (async () => {
          try {
            do {
              connection.refreshAgain = false;
              const diagnosticRevision = this.diagnosticRevisions.get(state) ?? 0;
              const listing = connection.capabilities.tools
                ? await client.listTools({}, { timeout: CONNECT_TIMEOUT_MS, maxTotalTimeout: CONNECT_TIMEOUT_MS, cacheMode: 'refresh' }) : { tools: [] };
              if (connection.closed || this.connections.get(key) !== operation) return;
              connection.tools = descriptors(server, key, listing, connection.capabilities);
              this._rememberCatalog(server, key, connection.tools);
              this.browserSessions.invalidate(key);
              state.toolCount = connection.tools.length; state.generation++;
              this._recordRecovery(connection, diagnosticRevision);
            } while (connection.refreshAgain);
          } catch (error) {
            this._recordFailure(connection, mcpFailure(error, 'MCP_CATALOG_REFRESH_FAILED').code);
            throw error;
          }
        })().finally(() => { connection.refreshing = null; });
        return connection.refreshing;
      };
      const client = new Client({ name: 'kynxa-tool-client', version: '0.1.0' }, { capabilities: {},
        versionNegotiation: { mode: version === MODERN_MCP_PROTOCOL ? { pin: MODERN_MCP_PROTOCOL } : 'legacy' },
        inputRequired: { autoFulfill: false }, listMaxPages: 32,
        listChanged: { tools: { autoRefresh: false, debounceMs: 100, onChanged: () => { void changed().catch(() => {}); } },
          resources: { autoRefresh: false, debounceMs: 100, onChanged: () => { if (state.state === 'ready') state.generation++; } } } });
      let transport;
      let startupFailure;
      // The SDK may reject connect with a closed-pipe error after reporting the
      // underlying spawn failure. Retain only its sanitized readiness diagnosis.
      // SDK 报告底层进程启动错误后，connect 可能只返回管道关闭错误；仅保留已去除敏感信息的就绪诊断。
      client.onerror = error => {
        const failure = mcpFailure(error);
        if (failure.code === 'MCP_COMMAND_NOT_FOUND') startupFailure = failure;
      };
      try {
        transport = await createMcpTransport(server, context, { fetch: this.fetch, extensionRoot: this.extensionRoot ?? context?.extensionRoot });
        await client.connect(transport, { timeout: server.startupTimeoutMs ?? CONNECT_TIMEOUT_MS });
        if (client.getNegotiatedProtocolVersion() !== version)
          throw toolFailure('MCP 协商版本与明示配置不一致。', 'MCP_PROTOCOL_MISMATCH', 409);
        const capabilities = client.getServerCapabilities() ?? {};
        const listing = capabilities.tools
          ? await client.listTools({}, { timeout: CONNECT_TIMEOUT_MS, maxTotalTimeout: CONNECT_TIMEOUT_MS }) : { tools: [] };
        connection = { client, transport, tools: descriptors(server, key, listing, capabilities), capabilities,
          serverId: server.id, protocolVersion: version, closed: false, refreshCatalog: changed,
          key, operation, diagnosticState: state,
          artifactContext: { server: structuredClone(server), context: { workspaceRoot: context?.workspaceRoot } } };
        state.state = 'ready'; state.toolCount = connection.tools.length; state.lastConnectedAt = new Date().toISOString();
        this._rememberCatalog(server, key, connection.tools);
        this.readyConnections.set(key, connection);
        if (this.states.get(server.id) === state) { delete state.code; this.errors.delete(server.id); }
        state.resourceCapabilities = { resources: !!capabilities.resources, templates: !!capabilities.resources };
        client.onclose = () => {
          connection.closed = true;
          if (connection.closing) return; // The explicit owner records success/failure before releasing its reference. 显式所有者先记录清理成功或失败，再释放自己拥有的引用。
          if (this.connections.get(key) !== operation) return;
          this.connections.delete(key); state.state = 'disconnected'; state.toolCount = 0; state.code = 'MCP_CONNECTION_LOST';
          if (this.readyConnections.get(key) === connection) this.readyConnections.delete(key);
          this.connectionOwners.delete(key);
          this.browserSessions.remove(key);
          this.errors.set(server.id, state.code);
        };
        client.onerror = error => {
          if (connection.closed || this.connections.get(key) !== operation) return;
          this._recordFailure(connection, mcpFailure(error).code);
        };
        return connection;
      } catch (error) {
        await this._closeConnection(key, connection ?? { client, transport, serverId: server.id, closed: true }, { startup: true });
        throw mcpFailure(startupFailure ?? error);
      }
    })();
    this.connections.set(key, operation);
    try { return await operation; }
    catch (error) {
      if (this.connections.get(key) === operation) {
        this.connections.delete(key); this.connectionOwners.delete(key);
      }
      this.errors.set(server.id, error.code ?? 'MCP_CONNECTION_FAILED');
      state.state = error.code === 'MCP_AUTH_REQUIRED' ? 'auth-required' : 'error'; state.code = error.code;
      throw error;
    }
  }

  async catalog(config, context, { connect = false, refresh = false, serverIds } = {}) {
    const generation = this.connectionGeneration;
    this._assertConnectionGeneration(generation);
    const tools = [];
    const requested = serverIds === undefined ? null : new Set(serverIds);
    const servers = config.mcpServers.filter(server => server.enabled);
    const serverGenerations = new Map(servers.map(server => [server.id, this.serverGenerations.get(server.id) ?? 0]));
    const assertServerCurrent = server => {
      if (this.serverResets.has(server.id) || (this.serverGenerations.get(server.id) ?? 0) !== serverGenerations.get(server.id))
        throw toolFailure('此 MCP 服务目录在发现期间变化，请重新准备此服务调用。', 'MCP_CATALOG_CHANGED', 409);
    };
    const discover = async server => {
      this._assertConnectionGeneration(generation);
      assertServerCurrent(server);
      this.servers.set(server.id, structuredClone(server));
      if (!this.states.has(server.id)) this.states.set(server.id, { serverId: server.id, transport: server.transport ?? 'stdio', state: 'disconnected',
        toolCount: 0, resourceCapabilities: { resources: false, templates: false }, generation: 0 });
      const key = this._key(server, context);
      const existing = this.connections.has(key);
      try {
        if (!connect || requested && !requested.has(server.id)) {
          // Reuse ready transports without joining an unrelated in-flight handshake.
          // 复用已就绪连接，不等待其他请求正在启动的不相关服务。
          const ready = this.readyConnections.get(key);
          if (!ready || ready.closed) return [];
          if (refresh) await ready.refreshCatalog();
          this._assertConnectionGeneration(generation); assertServerCurrent(server);
          return ready.closed ? [] : ready.tools;
        }
        const connection = await this._connect(server, context);
        this._assertConnectionGeneration(generation);
        assertServerCurrent(server);
        if (refresh && existing && !connection.closed) await connection.refreshCatalog();
        this._assertConnectionGeneration(generation);
        assertServerCurrent(server);
        if (!connection.closed) return connection.tools;
      } catch (error) {
        this._assertConnectionGeneration(generation);
        assertServerCurrent(server);
        this.errors.set(server.id, error.code ?? 'MCP_CONNECTION_FAILED');
      }
      return [];
    };
    // Each catalog overlaps at most four handshakes, preserving configured order.
    // Reading settings/catalog without connect still starts no external process or network request.
    // 每次发现最多并行四次握手并保持配置顺序；未请求连接时，读取设置或工具目录不启动外部进程，也不发网络请求。
    for (let offset = 0; offset < servers.length; offset += 4) {
      this._assertConnectionGeneration(generation);
      const batch = await Promise.all(servers.slice(offset, offset + 4).map(discover));
      this._assertConnectionGeneration(generation);
      tools.push(...batch.flat());
    }
    return tools;
  }

  diagnostics() { return [...this.states.values()].map(value => structuredClone(value)); }

  async disconnect(serverId) {
    const pending = [...this.connections.entries()];
    const results = await Promise.allSettled(pending.map(async ([key, operation]) => {
      const connection = await operation.catch(() => null);
      if (connection?.serverId === serverId) await this._closeConnection(key, connection);
    }));
    if (results.some(result => result.status === 'rejected') ||
        [...this.failedClosures.values()].some(connection => connection.serverId === serverId)) throw this._cleanupFailure();
    const state = this.states.get(serverId);
    if (state) { state.state = 'disconnected'; state.toolCount = 0; delete state.code; }
    this.errors.delete(serverId);
  }

  async reconnect(serverOrId, context) {
    const generation = this.connectionGeneration;
    this._assertConnectionGeneration(generation);
    const server = typeof serverOrId === 'string' ? this.servers.get(serverOrId) : serverOrId;
    if (!server?.enabled) throw toolFailure('MCP 服务未配置或未启用。', 'MCP_NOT_ENABLED', 409);
    await this.disconnect(server.id);
    this._assertConnectionGeneration(generation);
    await this._connect(server, context);
    this._assertConnectionGeneration(generation);
    return this.diagnostics().find(state => state.serverId === server.id);
  }

  /**
   * Revalidate an existing connection and call envelope without reconnecting or invoking the server.
   * 只重新校验已有连接和调用封装，不重连，也不调用服务。
   */
  async validateExecution(descriptor, input) {
    if (this.failedClosures.size) throw this._cleanupFailure();
    if (this.serverResets.has(descriptor.serverId))
      throw toolFailure('此 MCP 服务配置正在更新，请重新准备此服务调用。', 'MCP_CATALOG_CHANGED', 409);
    const pending = this.connections.get(descriptor.key);
    if (!pending) throw toolFailure('MCP 工具尚未连接或配置已变化，请刷新。', 'MCP_NOT_CONNECTED', 409);
    const connection = await pending;
    this._assertServerToolPolicy(descriptor, connection);
    if (this.serverResets.has(descriptor.serverId))
      throw toolFailure('此 MCP 服务配置正在更新，请重新准备此服务调用。', 'MCP_CATALOG_CHANGED', 409);
    const args = serverArguments(input);
    if (connection.closed) throw toolFailure('MCP 连接已中断，请手动重新连接。', 'MCP_NOT_CONNECTED', 409);
    const current = connection.tools?.find(tool => tool.name === descriptor.name);
    if (connection.tools && (!current || current.operation !== descriptor.operation || current.toolName !== descriptor.toolName ||
        JSON.stringify(current.originalInputSchema) !== JSON.stringify(descriptor.originalInputSchema)))
      throw toolFailure('MCP 工具目录已变化，请重新准备调用和审批。', 'MCP_CATALOG_CHANGED', 409);
    return { connection, args };
  }

  /**
   * Normalize browser defaults before approval; this performs no RPC or page action.
   * 审批前规范化浏览器默认参数，此处不发 RPC，也不操作页面。
   */
  async prepareBrowserExecution(descriptor, input, options = {}) {
    const knownServer = this.servers.get(descriptor.serverId) ?? (await this.connections.get(descriptor.key))?.artifactContext?.server;
    if (!browserOperation(descriptor, knownServer)) return structuredClone(input);
    const { connection, args } = await this.validateExecution(descriptor, input);
    let prepared;
    try { prepared = this.browserSessions.prepare(descriptor, args, connection.artifactContext?.server, options); }
    catch (error) {
      const recovery = this.browserSessions.observationRecovery(descriptor, args, connection.artifactContext?.server, options, error.code);
      if (!recovery || !this._browserObservationTools(connection, recovery)) throw error;
      const envelope = structuredClone(input);
      this.browserRecoveries.set(envelope, { connection, ...recovery });
      return envelope;
    }
    const envelope = { ...structuredClone(input), arguments: prepared.args };
    this.browserApprovals.set(envelope, { connection, identity: this.browserSessions.approvalIdentity(prepared) });
    return envelope;
  }

  browserDiagnostics(sessionId) { return this.browserSessions.diagnostics(sessionId); }

  _browserObservationTools(connection, recovery) {
    const policy = this.serverPolicies.get(connection.serverId ?? connection.artifactContext?.server?.id) ?? connection.artifactContext?.server;
    if (policy?.enabled === false) return undefined;
    const disabled = new Set(policy?.disabledTools ?? []);
    const find = name => connection.tools?.find(tool => tool.operation === 'tools/call' && tool.toolName === name && !disabled.has(name));
    const listing = find(recovery.engine === 'playwright' ? 'browser_tabs' : 'list_pages');
    const snapshot = find(recovery.engine === 'playwright' ? 'browser_snapshot' : 'take_snapshot');
    return listing && snapshot ? { listing, snapshot } : undefined;
  }

  async _recoverBrowserObservation(descriptor, args, connection, recovery, options, signal) {
    const plan = this.browserSessions.observationRecovery(descriptor, args, connection.artifactContext?.server,
      options, recovery.tabId == null ? 'BROWSER_TAB_ID_REQUIRED' : 'BROWSER_STALE_REFERENCE');
    if (!plan || plan.tabId !== recovery.tabId || plan.connectionSelectedTabId !== recovery.connectionSelectedTabId)
      throw toolFailure('等待期间浏览器目标已变化，请重新读取当前任务页面。', 'BROWSER_TAB_CHANGED', 409);
    const tools = this._browserObservationTools(connection, recovery);
    if (!tools) throw toolFailure('此连接缺少页面发现或快照工具，请刷新工具目录。', 'MCP_CATALOG_CHANGED', 409);
    const observations = [];
    if (recovery.tabId == null) {
      const listingArgs = recovery.engine === 'playwright' ? { action: 'list' } : {};
      const prepared = this.browserSessions.prepare(tools.listing, listingArgs, connection.artifactContext.server, options);
      const listing = await this._executePrepared(tools.listing, prepared.args, connection, signal, prepared);
      observations.push(listing.canonical);
      if (listing.isError || !listing.browser?.targetVerified || (recovery.url && listing.browser.url !== recovery.url))
        throw toolFailure('当前浏览器页面与原快照不一致，请选择任务页面并重新读取。', 'BROWSER_TAB_CHANGED', 409);
    }
    signal?.throwIfAborted();
    const snapshotArgs = tools.snapshot.originalInputSchema?.properties?.pageId && recovery.tabId != null
      ? { pageId: Number(recovery.tabId) } : {};
    const prepared = this.browserSessions.prepare(tools.snapshot, snapshotArgs, connection.artifactContext.server, options);
    const snapshot = await this._executePrepared(tools.snapshot, prepared.args, connection, signal, prepared);
    if (snapshot.isError) return snapshot;
    observations.push(snapshot.canonical);
    // A new snapshot never authorizes blindly remapping an old element reference.
    // Preserve a paired receipt and let the next model step select the fresh ref.
    // 新快照不得授权盲目映射旧元素引用；保存配对回执，让下一模型步骤选择新引用。
    const notice = 'Browser target and fresh snapshot recovered. The original action was not executed. Select the intended element from this fresh snapshot and call the action again.';
    const canonical = { content: [{ type: 'text', text: notice }, ...(snapshot.canonical.content ?? [])], isError: true,
      structuredContent: { code: 'BROWSER_REFERENCE_REFRESHED', originalActionExecuted: false, observations, browser: snapshot.browser } };
    return { content: `${notice}\n\n${snapshot.content}`, canonical, browser: snapshot.browser, isError: true,
      code: 'BROWSER_REFERENCE_REFRESHED', outsideWorkspace: true };
  }

  /** Dispose only changed servers; unrelated connections and pending calls retain ownership.
   * 仅清理配置变化的服务；其他连接与待执行调用保留自身所有权。
   */
  async resetServers(serverIds) {
    if (this.resetOperation) { await this.resetOperation; return; }
    const ids = [...new Set(serverIds)];
    const operations = ids.map(serverId => {
      if (this.serverResets.has(serverId)) return this.serverResets.get(serverId);
      this.serverGenerations.set(serverId, (this.serverGenerations.get(serverId) ?? 0) + 1);
      for (const [key, cached] of this.catalogMetadata) if (cached.serverId === serverId) this._forgetCatalog(key);
      const pending = [...this.connections.entries()].filter(([key]) => this.connectionOwners.get(key) === serverId);
      let resetting;
      resetting = Promise.resolve().then(async () => {
        const closed = await Promise.allSettled(pending.map(async ([key, startup]) => {
          let connection;
          try { connection = await startup; } catch { return; }
          await this._closeConnection(key, connection);
        }));
        if (closed.some(item => item.status === 'rejected') || [...this.failedClosures.values()].some(item => item.serverId === serverId))
          throw this._cleanupFailure();
        for (const [key, startup] of pending) {
          if (this.connections.get(key) === startup) this.connections.delete(key);
          this.connectionOwners.delete(key);
        }
        this.states.delete(serverId); this.errors.delete(serverId); this.servers.delete(serverId);
      }).finally(() => { if (this.serverResets.get(serverId) === resetting) this.serverResets.delete(serverId); });
      this.serverResets.set(serverId, resetting);
      return resetting;
    });
    await Promise.all(operations);
  }

  async execute(descriptor, input, signal, browserOptions = {}) {
    let validated;
    try { validated = await this.validateExecution(descriptor, input); }
    catch (error) { throw markExecutionNotDispatched(error); }
    const { connection, args } = validated;
    if (!browserOperation(descriptor, connection.artifactContext?.server))
      return this._executePrepared(descriptor, args, connection, signal);
    // A selected-page protocol shares state even across distinct conversations.
    // Serialize its calls and revalidate after waiting; never switch tabs by a hidden RPC.
    // 隐式选中页面协议跨聊天共享状态；调用需排队并在等待后重新验证，不能用隐藏 RPC 切换标签页。
    const previous = connection.browserQueue ?? Promise.resolve();
    let dispatched = false;
    const operation = previous.catch(() => {}).then(async () => {
      let current, prepared;
      try {
        signal?.throwIfAborted();
        current = await this.validateExecution(descriptor, input);
        signal?.throwIfAborted();
        const approval = this.browserApprovals.get(input);
        if (approval && approval.connection !== current.connection)
          throw toolFailure('审批期间浏览器连接已替换，请重新准备调用和审批。', 'BROWSER_CONNECTION_CHANGED', 409);
        const recovery = this.browserRecoveries.get(input);
        if (recovery) {
          if (recovery.connection !== current.connection)
            throw toolFailure('审批期间浏览器连接已替换，请重新准备调用和审批。', 'BROWSER_CONNECTION_CHANGED', 409);
          return await this._recoverBrowserObservation(descriptor, current.args, current.connection, recovery, browserOptions, signal);
        }
        prepared = this.browserSessions.prepare(descriptor, current.args, current.connection.artifactContext?.server, browserOptions);
        this.browserSessions.verifyApprovalIdentity(approval?.identity, prepared);
      } catch (error) { throw markExecutionNotDispatched(error); }
      dispatched = true;
      return this._executePrepared(descriptor, prepared.args, current.connection, signal, prepared);
    });
    connection.browserQueue = operation.catch(() => {});
    return waitForBrowserDispatch(operation, signal, () => dispatched);
  }

  async _executePrepared(descriptor, args, connection, signal, prepared) {
    try { this._assertServerToolPolicy(descriptor, connection); }
    catch (error) { throw markExecutionNotDispatched(error); }
    const diagnosticRevision = this.diagnosticRevisions.get(connection.diagnosticState) ?? 0;
    const timeoutMs = prepared?.browser?.timeoutMs ?? 30000;
    const options = { signal, timeout: timeoutMs, maxTotalTimeout: timeoutMs, cacheMode: 'refresh' };
    let result;
    try {
      if (descriptor.operation === 'resources/list') {
        const listing = await connection.client.listResources(args, options);
        result = { content: [], structuredContent: listing, ...(listing._meta ? { _meta: listing._meta } : {}) };
      } else if (descriptor.operation === 'resources/templates/list') {
        const listing = await connection.client.listResourceTemplates(args, options);
        result = { content: [], structuredContent: listing, ...(listing._meta ? { _meta: listing._meta } : {}) };
      } else if (descriptor.operation === 'resources/read') {
        const resource = await connection.client.readResource(args, options);
        result = { content: resource.contents.map(item => ({ type: 'resource', resource: item })),
          ...(resource._meta ? { _meta: resource._meta } : {}) };
      } else {
        if (prepared) this.browserSessions.dispatch(prepared);
        result = await connection.client.callTool({ name: descriptor.toolName, arguments: args }, { ...options, allowInputRequired: true });
      }
    } catch (error) {
      const cancelled = signal?.aborted || error?.name === 'AbortError';
      if (cancelled && !prepared) throw error;
      const failure = cancelled ? toolFailure('浏览器操作已取消；已发出的动作需要核验。', 'TOOL_CANCELLED', 409)
        : mcpFailure(error, 'MCP_REQUEST_FAILED');
      const ownsDiagnosis = this._ownsDiagnosis(connection);
      this._recordFailure(connection, failure.code);
      if (failure.code === 'MCP_CONNECTION_LOST' && ownsDiagnosis) {
        await this.disconnect(descriptor.serverId);
        const state = this.states.get(descriptor.serverId);
        if (state === connection.diagnosticState && state.state === 'disconnected') {
          state.state = 'error'; state.code = failure.code;
          this.errors.set(descriptor.serverId, failure.code);
        }
      }
      if (prepared) {
        const unknown = !prepared.browser.readOnly && ['MCP_TIMEOUT', 'MCP_CONNECTION_LOST', 'TOOL_CANCELLED'].includes(failure.code);
        const browser = this.browserSessions.observe(prepared, { content: [] }, unknown ? 'unknown' : 'failed');
        const status = unknown ? 'unknown' : cancelled ? 'cancelled' : 'error';
        const canonical = { content: [{ type: 'text', text: failure.message }], isError: true,
          structuredContent: { status, code: failure.code, browser } };
        return { content: resultPreview(canonical), canonical, browser, status, code: failure.code, isError: true, outsideWorkspace: true };
      }
      throw failure;
    }
    if (isInputRequiredResult(result))
      throw toolFailure('MCP 服务需要额外交互；此请求未自动授权或重试。', 'MCP_INPUT_REQUIRED', 409);
    // Chrome DevTools reports caught navigation failures as normal MCP text.
    // Classify only its explicit navigation failure lines; preserve the raw result unchanged.
    // Chrome DevTools 可能以普通 MCP 文本返回捕获的导航错误；只分类明确失败行，完整原始结果保持不变。
    const navigationFailed = descriptor.toolName === 'navigate_page' && (result.content ?? []).some(block =>
      block.type === 'text' && /^Unable to (?:navigate(?: back| forward)? in the selected page|reload the selected page): /m.test(block.text));
    const browserTimedOut = prepared && (result.isError === true || navigationFailed) && (result.content ?? []).some(block =>
      block.type === 'text' && /TimeoutError|(?:timed out|timeout).{0,40}(?:exceeded|after|ms)|Timeout \d+ms exceeded/i.test(block.text));
    const outcomeUnknown = browserTimedOut && !prepared.browser.readOnly;
    if (browserTimedOut) this._recordFailure(connection, 'MCP_TIMEOUT');
    else this._recordRecovery(connection, diagnosticRevision);
    const browser = prepared ? this.browserSessions.observe(prepared, result,
      outcomeUnknown ? 'unknown' : result.isError === true || navigationFailed ? 'failed' : 'completed') : undefined;
    const artifact = await archiveBrowserScreenshot(result, { descriptor, ...connection.artifactContext, args, signal });
    const screenshotNotice = artifact.screenshotStatus === 'available'
      ? '\nScreenshot archived for the local sidebar. No image pixels were sent to this text model.'
      : artifact.screenshotStatus === 'unavailable' ? `\nScreenshot preview unavailable (${artifact.screenshotCode}). The browser operation result is preserved; do not claim the screenshot was viewed.` : '';
    const canonical = browser ? { ...artifact.canonical, _meta: { ...artifact.canonical._meta, kynxaBrowser: browser } } : artifact.canonical;
    return { content: resultPreview(canonical) + screenshotNotice + (browser ? '\nBrowser receipt:\n' + JSON.stringify(browser) : ''),
      isError: result.isError === true || navigationFailed || !!browserTimedOut,
      ...(browser ? { browser } : {}), ...(outcomeUnknown ? { status: 'unknown' } : {}),
      ...(browserTimedOut ? { code: 'MCP_TIMEOUT' } : navigationFailed ? { code: 'MCP_BROWSER_NAVIGATION_FAILED' } : {}),
      ...(artifact.screenshotStatus ? { screenshotStatus: artifact.screenshotStatus, screenshotCode: artifact.screenshotCode } : {}),
      outsideWorkspace: true, canonical };
  }

  reset() {
    if (this.resetOperation) return this.resetOperation;
    // Revoke discovery before awaiting startup/teardown. Otherwise a later
    // discovery batch can start outside this owner's cleanup snapshot.
    // 等待启动或清理前先撤销目录发现，避免后续发现批次启动不在本次所有者清理快照中的连接。
    this.connectionGeneration++;
    const pending = [...this.connections.entries()];
    let operation;
    operation = Promise.resolve().then(async () => {
      await Promise.all([...this.serverResets.values()]);
      const settled = await Promise.allSettled(pending.map(async ([key, startup]) => {
        let connection;
        try { connection = await startup; }
        catch { return; } // Startup failures retain their own bounded cleanup. 启动失败由自身保留有时间上限的清理责任。
        await this._closeConnection(key, connection);
      }));
      if (settled.some(result => result.status === 'rejected') || this.failedClosures.size) throw this._cleanupFailure();
      // Only release references owned by this reset, even if a future caller
      // changes connection scheduling. New work is barred until this completes.
      // 只释放本次重置拥有的引用；即使以后调整调度，重置完成前仍禁止新连接。
      for (const [key, startup] of pending) if (this.connections.get(key) === startup) this.connections.delete(key);
      this.errors.clear(); this.states.clear(); this.servers.clear();
      this.catalogMetadata.clear();
      this.catalogMetadataBytes = 0; this.readyConnections.clear();
      this.connectionOwners.clear();
    }).finally(() => { if (this.resetOperation === operation) this.resetOperation = null; });
    this.resetOperation = operation;
    return operation;
  }

  async close() { this.closed = true; await this.reset(); }
}

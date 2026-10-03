import { createHash } from 'node:crypto';
import { Client, isInputRequiredResult } from '@modelcontextprotocol/client';
import { LEGACY_MCP_PROTOCOL, MODERN_MCP_PROTOCOL } from './agent-config.mjs';
import { createMcpTransport, mcpFailure } from './mcp-transport.mjs';
import { objectInput, toolFailure } from './tool-paths.mjs';

const MAX_CONNECTIONS = 32;
const MAX_MCP_TOOLS = 128;
const CONNECT_TIMEOUT_MS = 15000;

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
  visit(schema);
  return schema;
}

function callSchema(originalInputSchema) {
  // A nested $schema does not select the envelope root dialect. For example,
  // draft-07 tuple items must not be interpreted as 2020-12 prefixItems.
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

/** MCP dependencies are externally configured, never a claim that their processes are sandboxed. */
export class McpToolClients {
  constructor({ fetch, extensionRoot } = {}) {
    this.connections = new Map(); this.errors = new Map(); this.closed = false;
    this.states = new Map(); this.servers = new Map(); this.fetch = fetch;
    this.extensionRoot = extensionRoot;
    this.failedClosures = new Map();
  }

  _cleanupFailure() {
    return toolFailure('MCP 自有进程未安全关闭，请重启模型服务；未重新连接。', 'MCP_PROCESS_CLEANUP_FAILED', 502);
  }

  async _closeConnection(key, connection, { startup = false } = {}) {
    if (connection.closeOperation) return connection.closeOperation;
    const operation = this.connections.get(key);
    connection.closed = true; connection.closing = true;
    connection.closeOperation = Promise.resolve().then(async () => {
      let failed = false;
      try { await connection.client.close(); } catch { failed = true; }
      // A failed negotiation can leave a started transport detached from Client.
      if (startup) try { await connection.transport?.close(); } catch { failed = true; }
      if (failed) {
        this.failedClosures.set(key, connection);
        const state = this.states.get(connection.serverId);
        if (state) { state.state = 'error'; state.toolCount = 0; state.code = 'MCP_PROCESS_CLEANUP_FAILED'; }
        this.errors.set(connection.serverId, 'MCP_PROCESS_CLEANUP_FAILED');
        throw this._cleanupFailure();
      }
      if (this.connections.get(key) === operation) this.connections.delete(key);
    });
    return connection.closeOperation;
  }

  _key(server, context) {
    return createHash('sha256').update(JSON.stringify([server, context?.workspaceRoot ?? null, this.extensionRoot ?? context?.extensionRoot ?? null])).digest('hex');
  }

  async _connect(server, context) {
    if (this.closed) throw toolFailure('MCP 客户端已关闭。', 'TOOL_SERVICE_CLOSED', 409);
    if (this.failedClosures.size) throw this._cleanupFailure();
    const key = this._key(server, context);
    if (this.connections.has(key)) return this.connections.get(key);
    if (this.connections.size >= MAX_CONNECTIONS) throw toolFailure('MCP 连接已达上限，请刷新连接。', 'MCP_CONNECTION_CAPACITY', 409);
    this.servers.set(server.id, structuredClone(server));
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
              const listing = connection.capabilities.tools
                ? await client.listTools({}, { timeout: CONNECT_TIMEOUT_MS, maxTotalTimeout: CONNECT_TIMEOUT_MS, cacheMode: 'refresh' }) : { tools: [] };
              if (connection.closed || this.connections.get(key) !== operation) return;
              connection.tools = descriptors(server, key, listing, connection.capabilities);
              state.toolCount = connection.tools.length; state.generation++;
              state.state = 'ready'; delete state.code; this.errors.delete(server.id);
            } while (connection.refreshAgain);
          } catch (error) {
            if (!connection.closed) { state.code = mcpFailure(error, 'MCP_CATALOG_REFRESH_FAILED').code; this.errors.set(server.id, state.code); }
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
      try {
        transport = await createMcpTransport(server, context, { fetch: this.fetch, extensionRoot: this.extensionRoot ?? context?.extensionRoot });
        await client.connect(transport, { timeout: server.startupTimeoutMs ?? CONNECT_TIMEOUT_MS });
        if (client.getNegotiatedProtocolVersion() !== version)
          throw toolFailure('MCP 协商版本与明示配置不一致。', 'MCP_PROTOCOL_MISMATCH', 409);
        const capabilities = client.getServerCapabilities() ?? {};
        const listing = capabilities.tools
          ? await client.listTools({}, { timeout: CONNECT_TIMEOUT_MS, maxTotalTimeout: CONNECT_TIMEOUT_MS }) : { tools: [] };
        connection = { client, transport, tools: descriptors(server, key, listing, capabilities), capabilities,
          serverId: server.id, protocolVersion: version, closed: false, refreshCatalog: changed };
        state.state = 'ready'; state.toolCount = connection.tools.length; state.lastConnectedAt = new Date().toISOString();
        state.resourceCapabilities = { resources: !!capabilities.resources, templates: !!capabilities.resources };
        client.onclose = () => {
          connection.closed = true;
          if (connection.closing) return; // The explicit owner records success/failure before releasing its reference.
          if (this.connections.get(key) !== operation) return;
          this.connections.delete(key); state.state = 'disconnected'; state.toolCount = 0; state.code = 'MCP_CONNECTION_LOST';
          this.errors.set(server.id, state.code);
        };
        client.onerror = error => {
          if (connection.closed || this.connections.get(key) !== operation) return;
          state.code = mcpFailure(error).code; this.errors.set(server.id, state.code);
        };
        return connection;
      } catch (error) {
        await this._closeConnection(key, connection ?? { client, transport, serverId: server.id, closed: true }, { startup: true });
        throw mcpFailure(error);
      }
    })();
    this.connections.set(key, operation);
    try { return await operation; }
    catch (error) {
      if (this.connections.get(key) === operation) this.connections.delete(key);
      this.errors.set(server.id, error.code ?? 'MCP_CONNECTION_FAILED');
      state.state = error.code === 'MCP_AUTH_REQUIRED' ? 'auth-required' : 'error'; state.code = error.code;
      throw error;
    }
  }

  async catalog(config, context, { connect = false, refresh = false } = {}) {
    const tools = [];
    for (const server of config.mcpServers.filter(server => server.enabled)) {
      this.servers.set(server.id, structuredClone(server));
      if (!this.states.has(server.id)) this.states.set(server.id, { serverId: server.id, transport: server.transport ?? 'stdio', state: 'disconnected',
        toolCount: 0, resourceCapabilities: { resources: false, templates: false }, generation: 0 });
      const key = this._key(server, context);
      const existing = this.connections.has(key);
      if (!connect && !existing) continue;
      try {
        const connection = await this._connect(server, context);
        if (refresh && existing && !connection.closed) await connection.refreshCatalog();
        if (!connection.closed) tools.push(...connection.tools);
      } catch (error) { this.errors.set(server.id, error.code ?? 'MCP_CONNECTION_FAILED'); }
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
    const server = typeof serverOrId === 'string' ? this.servers.get(serverOrId) : serverOrId;
    if (!server?.enabled) throw toolFailure('MCP 服务未配置或未启用。', 'MCP_NOT_ENABLED', 409);
    await this.disconnect(server.id);
    await this._connect(server, context);
    return this.diagnostics().find(state => state.serverId === server.id);
  }

  async execute(descriptor, input, signal) {
    if (this.failedClosures.size) throw this._cleanupFailure();
    const pending = this.connections.get(descriptor.key);
    if (!pending) throw toolFailure('MCP 工具尚未连接或配置已变化，请刷新。', 'MCP_NOT_CONNECTED', 409);
    const connection = await pending;
    const args = serverArguments(input);
    if (connection.closed) throw toolFailure('MCP 连接已中断，请手动重新连接。', 'MCP_NOT_CONNECTED', 409);
    const current = connection.tools?.find(tool => tool.name === descriptor.name);
    if (connection.tools && (!current || current.operation !== descriptor.operation || current.toolName !== descriptor.toolName ||
        JSON.stringify(current.originalInputSchema) !== JSON.stringify(descriptor.originalInputSchema)))
      throw toolFailure('MCP 工具目录已变化，请重新准备调用和审批。', 'MCP_CATALOG_CHANGED', 409);
    const options = { signal, timeout: 30000, maxTotalTimeout: 30000, cacheMode: 'refresh' };
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
      } else result = await connection.client.callTool({ name: descriptor.toolName, arguments: args }, { ...options, allowInputRequired: true });
    } catch (error) {
      if (signal?.aborted || error?.name === 'AbortError') throw error;
      const failure = mcpFailure(error, 'MCP_REQUEST_FAILED');
      if (failure.code === 'MCP_CONNECTION_LOST') await this.disconnect(descriptor.serverId);
      const state = this.states.get(descriptor.serverId);
      if (state) { state.code = failure.code; if (failure.code === 'MCP_AUTH_REQUIRED') state.state = 'auth-required';
        else if (failure.code === 'MCP_CONNECTION_LOST') state.state = 'error'; }
      this.errors.set(descriptor.serverId, failure.code);
      throw failure;
    }
    if (isInputRequiredResult(result))
      throw toolFailure('MCP 服务需要额外交互；此请求未自动授权或重试。', 'MCP_INPUT_REQUIRED', 409);
    // Chrome DevTools reports caught navigation failures as normal MCP text.
    // Classify only its explicit navigation failure lines; preserve the raw result unchanged.
    const navigationFailed = descriptor.toolName === 'navigate_page' && (result.content ?? []).some(block =>
      block.type === 'text' && /^Unable to (?:navigate(?: back| forward)? in the selected page|reload the selected page): /m.test(block.text));
    return { content: resultPreview(result), isError: result.isError === true || navigationFailed,
      ...(navigationFailed ? { code: 'MCP_BROWSER_NAVIGATION_FAILED' } : {}),
      outsideWorkspace: true, canonical: structuredClone(result) };
  }

  async reset() {
    const pending = [...this.connections.entries()];
    const settled = await Promise.allSettled(pending.map(async ([key, operation]) => {
      let connection;
      try { connection = await operation; }
      catch { return; } // Startup failures already perform and retain their own bounded cleanup.
      await this._closeConnection(key, connection);
    }));
    if (settled.some(result => result.status === 'rejected') || this.failedClosures.size) throw this._cleanupFailure();
    this.connections.clear(); this.errors.clear(); this.states.clear(); this.servers.clear();
  }

  async close() { this.closed = true; await this.reset(); }
}

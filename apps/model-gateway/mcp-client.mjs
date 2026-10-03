import { createHash } from 'node:crypto';
import { Client, isInputRequiredResult } from '@modelcontextprotocol/client';
import { StdioClientTransport } from '@modelcontextprotocol/client/stdio';
import { LEGACY_MCP_PROTOCOL, MODERN_MCP_PROTOCOL } from './agent-config.mjs';
import { toolFailure } from './tool-paths.mjs';

const MAX_CONNECTIONS = 32;
const MAX_MCP_TOOLS = 128;
const MAX_MCP_OUTPUT = 65536;
const CONNECT_TIMEOUT_MS = 15000;
// The SDK probes the base stdio class using a disposable sibling process, even when pinned.
// Its documented subclass path probes in place: opt-in modern servers explicitly support
// server/discover, so use one process without an automatic sibling or legacy fallback.
class PinnedModernStdioTransport extends StdioClientTransport {}

/** Enabled stdio processes are user-configured dependencies, never a claim that their tools are sandboxed. */
export class McpToolClients {
  constructor() { this.connections = new Map(); this.errors = new Map(); this.closed = false; }

  _key(server, context) {
    return createHash('sha256').update(JSON.stringify([server, context?.workspaceRoot ?? null])).digest('hex');
  }

  async _connect(server, context) {
    if (this.closed) throw toolFailure('MCP 客户端已关闭。', 'TOOL_SERVICE_CLOSED', 409);
    const key = this._key(server, context);
    if (this.connections.has(key)) return this.connections.get(key);
    if (this.connections.size >= MAX_CONNECTIONS) throw toolFailure('MCP 连接已达上限，请刷新连接。', 'MCP_CONNECTION_CAPACITY', 409);
    const operation = (async () => {
      const version = server.protocolVersion ?? LEGACY_MCP_PROTOCOL;
      const client = new Client({ name: 'kynxa-tool-client', version: '0.1.0' }, { capabilities: {},
        versionNegotiation: { mode: version === MODERN_MCP_PROTOCOL ? { pin: MODERN_MCP_PROTOCOL } : 'legacy' },
        inputRequired: { autoFulfill: false } });
      const Transport = version === MODERN_MCP_PROTOCOL ? PinnedModernStdioTransport : StdioClientTransport;
      const transport = new Transport({ command: server.command, args: server.args,
        ...(context?.workspaceRoot ? { cwd: context.workspaceRoot } : {}), stderr: 'ignore', maxBufferSize: 2 * 1024 * 1024 });
      try {
        await client.connect(transport, { timeout: CONNECT_TIMEOUT_MS });
        if (client.getNegotiatedProtocolVersion() !== version)
          throw toolFailure('MCP 协商版本与明示配置不一致。', 'MCP_PROTOCOL_MISMATCH', 409);
        const listing = await client.listTools({}, { timeout: CONNECT_TIMEOUT_MS, maxTotalTimeout: CONNECT_TIMEOUT_MS });
        if (!Array.isArray(listing.tools) || listing.tools.length > MAX_MCP_TOOLS)
          throw toolFailure('MCP 工具目录超限或无效。', 'INVALID_MCP_CATALOG');
        const names = new Set();
        const tools = listing.tools.map(tool => {
          if (typeof tool.name !== 'string' || !/^[a-zA-Z0-9_.-]{1,128}$/.test(tool.name) || names.has(tool.name) ||
              !tool.inputSchema || tool.inputSchema.type !== 'object' || Buffer.byteLength(JSON.stringify(tool.inputSchema)) > 32000)
            throw toolFailure('MCP 工具定义无效。', 'INVALID_MCP_CATALOG');
          names.add(tool.name);
          const inputSchema = structuredClone(tool.inputSchema);
          const originalHasReason = Boolean(inputSchema.properties && Object.hasOwn(inputSchema.properties, 'reason'));
          inputSchema.properties = { ...inputSchema.properties, ...(!originalHasReason ? {
            reason: { type: 'string', description: 'Explain this call to the user-enabled external MCP process. Required for all permission modes.' }
          } : {}) };
          inputSchema.required = [...new Set([...(inputSchema.required ?? []), 'reason'])];
          return { name: `mcp.${server.id}.${tool.name}`, description: `${server.name}: ${String(tool.description ?? tool.name).slice(0, 2000)}`,
            inputSchema, source: `mcp:${server.id}`, serverId: server.id, toolName: tool.name, key, originalHasReason };
        });
        return { client, transport, tools, serverId: server.id, protocolVersion: version };
      } catch (error) {
        await client.close().catch(() => {});
        await transport.close().catch(() => {});
        throw error;
      }
    })();
    this.connections.set(key, operation);
    try { return await operation; }
    catch (error) {
      if (this.connections.get(key) === operation) this.connections.delete(key);
      this.errors.set(server.id, error.code ?? 'MCP_CONNECTION_FAILED');
      throw error;
    }
  }

  async catalog(config, context, { connect = false } = {}) {
    const tools = [];
    for (const server of config.mcpServers.filter(server => server.enabled)) {
      const key = this._key(server, context);
      if (!connect && !this.connections.has(key)) continue;
      try {
        const connection = await this._connect(server, context);
        tools.push(...connection.tools);
        this.errors.delete(server.id);
      } catch (error) { this.errors.set(server.id, error.code ?? 'MCP_CONNECTION_FAILED'); }
    }
    return tools;
  }

  async execute(descriptor, input, signal) {
    const pending = this.connections.get(descriptor.key);
    if (!pending) throw toolFailure('MCP 工具尚未连接或配置已变化，请刷新。', 'MCP_NOT_CONNECTED', 409);
    const connection = await pending;
    const args = structuredClone(input);
    if (!descriptor.originalHasReason) delete args.reason;
    const result = await connection.client.callTool({ name: descriptor.toolName, arguments: args },
      { signal, timeout: 30000, maxTotalTimeout: 30000, allowInputRequired: true });
    if (isInputRequiredResult(result))
      throw toolFailure('MCP 服务需要额外交互；此请求未自动授权或重试。', 'MCP_INPUT_REQUIRED', 409);
    const blocks = (result.content ?? []).map(block => block.type === 'text' ? block.text :
      block.type === 'resource' && typeof block.resource?.text === 'string' ? block.resource.text : `[MCP ${block.type} content]`);
    const content = blocks.join('\n') || JSON.stringify(result.structuredContent ?? {});
    const marker = '\n[MCP result truncated]';
    return { content: content.length > MAX_MCP_OUTPUT ? content.slice(0, MAX_MCP_OUTPUT - marker.length) + marker : content,
      isError: result.isError === true, outsideWorkspace: true };
  }

  async reset() {
    const pending = [...this.connections.values()];
    this.connections.clear();
    this.errors.clear();
    const settled = await Promise.allSettled(pending);
    await Promise.allSettled(settled.filter(result => result.status === 'fulfilled').map(result => result.value.client.close()));
  }

  async close() { this.closed = true; await this.reset(); }
}

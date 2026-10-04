import { randomUUID } from 'node:crypto';
import { addMcpPreset, mcpPresetCatalog } from './mcp-presets.mjs';
import { readJsonBody, sendJson } from './http-transport.mjs';
import { objectInput, toolFailure } from './tool-paths.mjs';

/**
 * Settings APIs own no model permission grants; execution still goes through ToolService.
 * 设置接口不授予模型操作权限，实际执行仍统一经过 ToolService。
 */
export async function handleAgentRoute(request, response, url, tools) {
  const pathname = url.pathname;
  if (!pathname.startsWith('/api/agent/')) return false;
  const send = value => { sendJson(response, 200, value); return true; };
  if (pathname === '/api/agent/config') {
    if (request.method === 'GET') return send(await tools.getConfig());
    if (request.method === 'PUT') return send(await tools.updateConfig(await readJsonBody(request)));
    throw toolFailure('工具配置接口不支持此操作。', 'METHOD_NOT_ALLOWED', 405);
  }
  if (pathname === '/api/agent/approvals' && request.method === 'POST') return send(tools.approve(await readJsonBody(request)));
  if (pathname === '/api/agent/mcp/catalog' && request.method === 'GET') return send(mcpPresetCatalog(await tools.getConfig()));
  const presetRoute = /^\/api\/agent\/mcp\/catalog\/([a-z0-9-]+)\/add$/.exec(pathname);
  if (presetRoute && request.method === 'POST') return send(await addMcpPreset(tools, presetRoute[1], await readJsonBody(request)));
  if (pathname === '/api/agent/skills/import' && request.method === 'POST') {
    const input = objectInput(await readJsonBody(request));
    const result = await tools.skills.importPackage(input.directory);
    const config = await tools.getConfig();
    return send({ ...result, skill: { ...result.skill, enabled: !(config.disabledSkills ?? []).includes(result.skill.id) } });
  }
  const conversationId = url.searchParams.get('conversationId');
  const context = conversationId ? await tools.createContext(conversationId,
    { requestId: randomUUID(), permissionMode: 'ask', message: '用户查看工具设置' }) : undefined;
  try {
    if (request.method === 'GET' && pathname === '/api/agent/skills') return send({ skills: await tools.listSkills(context, { includeDisabled: true }) });
    const skillRoute = /^\/api\/agent\/skills\/([a-zA-Z0-9_-]+)(?:\/(inspect|check|resource))?$/.exec(pathname);
    if (request.method === 'GET' && skillRoute) {
      const config = await tools.getConfig();
      switch (skillRoute[2]) {
        case 'inspect': return send(await tools.skills.inspect(skillRoute[1], context, config));
        case 'check': return send(await tools.checkSkill(skillRoute[1], context));
        case 'resource': return send(await tools.skills.readResource(skillRoute[1], url.searchParams.get('path'), context, config,
          { offset: Number(url.searchParams.get('offset') ?? 0), limit: Number(url.searchParams.get('limit') ?? 16000) }));
        default: return send({ skill: await tools.readSkill(skillRoute[1], context) });
      }
    }
    const toolResponse = list => ({ tools: list,
      errors: [...tools.mcp.errors].map(([id, code]) => `${id}: ${code}`), connections: tools.mcp.diagnostics() });
    if (request.method === 'GET' && pathname === '/api/agent/tools')
      return send(toolResponse(await tools.catalog(context, { connectMcp: false, includeDisabled: true })));
    if (request.method === 'POST' && pathname === '/api/agent/mcp/refresh')
      return send(toolResponse(await tools.refreshMcp(context, { includeDisabled: true })));
    if (request.method === 'POST' && ['/api/agent/mcp/disconnect', '/api/agent/mcp/reconnect'].includes(pathname)) {
      const input = objectInput(await readJsonBody(request));
      const config = await tools.getConfig();
      const server = config.mcpServers.find(item => item.id === input.serverId);
      if (!server) throw toolFailure('MCP 服务不存在。', 'MCP_SERVER_NOT_FOUND', 404);
      tools.configGeneration++;
      await tools.mcp.disconnect(server.id);
      if (pathname.endsWith('/disconnect')) return send({ connections: tools.mcp.diagnostics() });
      await tools.mcp.reconnect(server, context);
      return send(toolResponse(await tools.catalog(context, { includeDisabled: true })));
    }
    return false;
  } finally { if (context) await tools.releaseContext(context); }
}

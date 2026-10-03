import { createServer } from 'node:http';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { randomUUID } from 'node:crypto';
import { ModelStore, validateConnection } from './store.mjs';
import { ModelRuntime } from './runtime.mjs';
import { discoverModels } from './model-discovery.mjs';
import { modelHome } from './storage.mjs';
import { storageMigrationActive } from './storage-maintenance.mjs';
import { StreamFailure } from './streaming.mjs';
import { DATA_LAYOUT_VERSION } from './data-layout.mjs';
import { readJsonBody, sendJson, openEventStream } from './http-transport.mjs';

const dataHome = modelHome();
const port = Number(process.env.KYNXA_MODEL_API_PORT ?? 5218);
const store = new ModelStore({ dataHome });
const runtime = new ModelRuntime({ modelStore: store, dataHome });

async function probe(input, modelStore) {
  const connection = validateConnection(input, { requireModels: false });
  const apiKey = connection.apiKey || await modelStore.savedKeyFor?.(connection.providerId, connection.baseUrl);
  const start = performance.now();
  const models = await discoverModels({ ...connection, apiKey });
  return { ok: true, latencyMs: Math.round(performance.now() - start),
    models };
}

export function createModelServer(options = {}) {
  let modelStore = options.modelStore ?? store, modelRuntime = options.modelRuntime ?? runtime;
  const managed = !options.modelStore && !options.modelRuntime;
  let activeRequests = 0;
  const streamControllers = new Set();
  const retiringRuntimes = new Set();
  let runtimeCleanupError;
  const retireRuntime = previous => {
    const cleanup = Promise.resolve().then(() => previous.close?.()).catch(() => {
      // Keep cleanup failures observable without exposing paths, tool arguments,
      // or private upstream errors, and without terminating the replacement.
      runtimeCleanupError = 'RUNTIME_CLEANUP_FAILED';
      console.error(runtimeCleanupError);
    });
    retiringRuntimes.add(cleanup);
    void cleanup.then(() => retiringRuntimes.delete(cleanup));
  };
  const server = createServer(async (request, response) => {
    const url = new URL(request.url ?? '/', 'http://127.0.0.1');
    const pathname = url.pathname;
    let counted = false;
    try {
      // Native clients do not send Origin. Block browser-origin access to this local
      // execution/configuration surface, including same-loopback malicious pages.
      if (request.headers.origin) return sendJson(response, 403, { error: '浏览器不能直接调用本机模型与工具服务。', code: 'BROWSER_ORIGIN_DENIED' });
      const migrating = managed && storageMigrationActive();
      if (managed && !migrating && activeRequests === 0 && modelStore.dataHome !== modelHome()) {
        // No requests are active here; swap synchronously so simultaneous first
        // requests after a migration cannot install separate stores/queues.
        retireRuntime(modelRuntime);
        modelStore = new ModelStore({ dataHome: modelHome() });
        modelRuntime = new ModelRuntime({ modelStore, dataHome: modelStore.dataHome });
      }
      if (request.method === 'GET' && pathname === '/health')
        return sendJson(response, 200, { status: 'ok', service: 'kynxa-model-gateway', storageProtocol: 1,
          agentProtocol: 1, toolStreamProtocol: 1, streamProtocol: 1, conversationProtocol: 1, memoryProtocol: 1, memoryManagementProtocol: 1, contextProtocol: 1, dataLayoutVersion: DATA_LAYOUT_VERSION,
          activeRequests, migrating, modelDataHome: modelStore.dataHome,
          ...(runtimeCleanupError ? { runtimeCleanupError } : {}) });
      if (migrating) return sendJson(response, 503, { error: '正在迁移数据，请完成后再试。' });
      activeRequests++;
      counted = true;
      if (pathname === '/api/agent/config') {
        if (request.method === 'GET') return sendJson(response, 200, await modelRuntime.tools.getConfig());
        if (request.method === 'PUT') return sendJson(response, 200, await modelRuntime.tools.updateConfig(await readJsonBody(request)));
        return sendJson(response, 405, { error: '工具配置接口不支持此操作。' });
      }
      if (pathname === '/api/agent/approvals' && request.method === 'POST')
        return sendJson(response, 200, modelRuntime.tools.approve(await readJsonBody(request)));
      if (pathname.startsWith('/api/agent/')) {
        const conversationId = url.searchParams.get('conversationId');
        const context = conversationId ? await modelRuntime.tools.createContext(conversationId,
          { requestId: randomUUID(), permissionMode: 'ask', message: '用户查看工具设置' }) : undefined;
        if (request.method === 'GET' && pathname === '/api/agent/skills')
          return sendJson(response, 200, { skills: await modelRuntime.tools.listSkills(context) });
        const skillRoute = /^\/api\/agent\/skills\/([a-zA-Z0-9_-]+)$/.exec(pathname);
        if (request.method === 'GET' && skillRoute)
          return sendJson(response, 200, { skill: await modelRuntime.tools.readSkill(skillRoute[1], context) });
        if (request.method === 'GET' && pathname === '/api/agent/tools')
          return sendJson(response, 200, { tools: await modelRuntime.tools.catalog(context, { connectMcp: false }),
            errors: [...modelRuntime.tools.mcp.errors].map(([id, code]) => `${id}: ${code}`) });
        if (request.method === 'POST' && pathname === '/api/agent/mcp/refresh')
          return sendJson(response, 200, { tools: await modelRuntime.tools.refreshMcp(context),
            errors: [...modelRuntime.tools.mcp.errors].map(([id, code]) => `${id}: ${code}`) });
      }
      const userMemoryRoute = /^\/api\/memory\/user(?:\/([a-zA-Z0-9_-]+))?$/.exec(pathname);
      const projectMemoryRoute = /^\/api\/projects\/([a-zA-Z0-9_-]+)\/memory(?:\/([a-zA-Z0-9_-]+))?$/.exec(pathname);
      if (userMemoryRoute || projectMemoryRoute) {
        const scope = userMemoryRoute ? 'user' : 'project';
        const scopeId = userMemoryRoute ? 'user' : projectMemoryRoute[1];
        const entryId = userMemoryRoute ? userMemoryRoute[1] : projectMemoryRoute[2];
        if (request.method === 'GET' && !entryId)
          return sendJson(response, 200, await modelRuntime.memory.listScope(scope, scopeId));
        if (request.method === 'POST' && !entryId)
          return sendJson(response, 201, await modelRuntime.memory.createScope(scope, scopeId, await readJsonBody(request)));
        if (request.method === 'PATCH' && entryId)
          return sendJson(response, 200, await modelRuntime.memory.updateScope(scope, scopeId, entryId, await readJsonBody(request)));
        if (request.method === 'DELETE' && entryId)
          return sendJson(response, 200, await modelRuntime.memory.deleteScope(scope, scopeId, entryId, await readJsonBody(request)));
        return sendJson(response, 405, { error: '此记忆接口不支持该操作。' });
      }
      const memoryRoute = /^\/api\/conversations\/([0-9a-f-]{36})\/memory(?:\/([a-zA-Z0-9_-]+))?$/i.exec(pathname);
      if (memoryRoute) {
        const [, conversationId, memoryId] = memoryRoute;
        if (request.method === 'GET' && !memoryId)
          return sendJson(response, 200, await modelRuntime.memory.listFor(conversationId));
        if (request.method === 'POST' && !memoryId)
          return sendJson(response, 201, await modelRuntime.memory.create(conversationId, await readJsonBody(request)));
        if (request.method === 'PATCH' && memoryId)
          return sendJson(response, 200, await modelRuntime.memory.update(conversationId, memoryId, await readJsonBody(request)));
        if (request.method === 'DELETE' && memoryId)
          return sendJson(response, 200, await modelRuntime.memory.delete(conversationId, memoryId, await readJsonBody(request)));
        return sendJson(response, 405, { error: '此记忆接口不支持该操作。' });
      }
      const relationshipRoute = /^\/api\/conversations\/([0-9a-f-]{36})\/relationships$/i.exec(pathname);
      if (request.method === 'GET' && relationshipRoute)
        return sendJson(response, 200, await modelRuntime.conversations.relationships(relationshipRoute[1]));
      if (pathname === '/api/conversations/catalog') {
        if (request.method === 'GET') return sendJson(response, 200, await modelRuntime.conversations.catalog());
        if (request.method === 'PUT') return sendJson(response, 200,
          await modelRuntime.conversations.saveCatalog(await readJsonBody(request, 32 * 1024 * 1024)));
      }
      if (request.method === 'GET' && pathname === '/api/models')
        return sendJson(response, 200, { providers: await modelStore.list() });
      if (request.method === 'POST' && pathname === '/api/models') {
        const provider = await modelStore.save(await readJsonBody(request, 8 * 1024 * 1024));
        await modelRuntime.invalidate?.(provider.providerId);
        return sendJson(response, 200, { provider });
      }
      if (request.method === 'POST' && pathname === '/api/models/test')
        return sendJson(response, 200, await probe(await readJsonBody(request, 8 * 1024 * 1024), modelStore));
      if (request.method === 'POST' && ['/api/chat', '/api/chat/stream'].includes(pathname)) {
        const body = await readJsonBody(request);
        if (!body || typeof body.message !== 'string' || !body.message.trim() ||
            typeof body.conversationId !== 'string' || !/^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$/i.test(body.conversationId))
          throw new Error('会话 ID 或消息无效。');
        if (body.permissionMode != null && !['ask', 'smart', 'full'].includes(body.permissionMode))
          throw new Error('权限模式无效。');
        if (body.requestId != null && (typeof body.requestId !== 'string' ||
            !/^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$/i.test(body.requestId)))
          throw new Error('请求 ID 无效。');
        if (body.userMessageId != null && (typeof body.userMessageId !== 'string' ||
            !/^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$/i.test(body.userMessageId)))
          throw new Error('用户消息 ID 无效。');
        if (body.userMessageId && body.requestId && body.userMessageId.toLowerCase() === body.requestId.toLowerCase())
          throw new Error('用户消息 ID 与请求 ID 不能相同。');
        if (pathname === '/api/chat/stream') {
          const controller = new AbortController();
          streamControllers.add(controller);
          const identity = { conversationId: body.conversationId,
            requestId: body.requestId ?? randomUUID(), createdAt: new Date().toISOString() };
          const stream = openEventStream(response, identity, controller);
          const { emit } = stream;
          emit({ type: 'started' });
          try {
            const result = await modelRuntime.replyStream({ conversationId: body.conversationId,
              message: body.message, provider: body.provider, model: body.model, requestId: identity.requestId,
              userMessageId: body.userMessageId, permissionMode: body.permissionMode }, emit, controller.signal);
            emit({ type: 'completed', ...result });
          } catch (error) {
            emit({ type: error instanceof StreamFailure ? error.type : 'error',
              content: error.content ?? '', reasoning: error.reasoning ?? '',
              error: error instanceof StreamFailure ? error.message : '模型调用失败，已保留生成的内容。',
              ...(error instanceof StreamFailure && error.code ? { code: error.code } : {}) });
          } finally {
            streamControllers.delete(controller);
            stream.end();
          }
          return;
        }
        const requestId = body.requestId ?? randomUUID();
        const content = await modelRuntime.reply({ conversationId: body.conversationId,
          message: body.message, provider: body.provider, model: body.model,
          requestId, userMessageId: body.userMessageId, permissionMode: body.permissionMode });
        return sendJson(response, 200, { conversationId: body.conversationId,
          requestId, role: 'assistant', content, createdAt: new Date().toISOString() });
      }
      sendJson(response, 404, { error: '接口不存在。' });
    } catch (error) {
      const clientError = error.message?.includes('无效') || error.message?.includes('填写') ||
        error.message?.includes('Provider ID') || error.message?.includes('连接名称') ||
        error.message?.includes('API Key') || error.message?.includes('Base URL') || error.message?.includes('HTTPS') ||
        error.message?.includes('模型') || error.message?.includes('请求体') ||
        error.message?.includes('权限模式') || error.message?.includes('会话 ID');
      sendJson(response, error.statusCode ?? (clientError ? 400 : 502),
        { error: error.message ?? '模型调用失败。', ...(error.code ? { code: error.code } : {}) });
    } finally {
      if (counted) activeRequests--;
    }
  });
  // Shutdown aborts upstream generations before waiting for open HTTP streams.
  server.shutdownModelRuntime = async () => {
    for (const controller of streamControllers) controller.abort();
    await Promise.all([...retiringRuntimes, Promise.resolve().then(() => modelRuntime.close?.())]);
  };
  return server;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const server = createModelServer();
  server.listen(port, '127.0.0.1', () => console.log(`KYNXA model gateway: http://127.0.0.1:${port}`));
  const shutdown = () => {
    server.close();
    void server.shutdownModelRuntime().catch(() => {
      console.error('RUNTIME_CLEANUP_FAILED');
      process.exitCode = 1;
    });
  };
  process.once('SIGINT', shutdown);
  process.once('SIGTERM', shutdown);
}

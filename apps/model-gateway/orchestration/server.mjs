import { createServer } from 'node:http';
import { randomUUID } from 'node:crypto';
import { ModelStore, validateConnection } from '../models/store.mjs';
import { ModelRuntime } from './runtime.mjs';
import { discoverModels } from '../models/model-discovery.mjs';
import { conversationDataRoot, modelHome } from '../data/storage.mjs';
import { extensionHome, EXTENSION_STORAGE_PROTOCOL } from '../data/extension-storage.mjs';
import { storageMigrationActive } from '../data/storage-maintenance.mjs';
import { StreamFailure } from '../models/streaming.mjs';
import { DATA_LAYOUT_VERSION } from '../data/data-layout.mjs';
import { readJsonBody, sendJson, openEventStream } from './http-transport.mjs';
import { handleAgentRoute } from './agent-http-routes.mjs';
import { handleRetrievalRoute } from './retrieval/http-routes.mjs';
import { toolRunLimits } from './tool-run.mjs';

const port = Number(process.env.KYNXA_MODEL_API_PORT ?? 5218);

async function probe(input, modelStore) {
  const connection = validateConnection(input, { requireModels: false });
  const apiKey = connection.apiKey || await modelStore.savedKeyFor?.(connection.providerId, connection.baseUrl);
  const start = performance.now();
  const models = await discoverModels({ ...connection, apiKey });
  return { ok: true, latencyMs: Math.round(performance.now() - start),
    models };
}

export function createModelServer(options = {}) {
  const managed = !options.modelStore && !options.modelRuntime;
  let modelStore = options.modelStore ?? options.modelRuntime?.store ?? new ModelStore({ dataHome: modelHome() });
  let modelRuntime = options.modelRuntime ?? new ModelRuntime({ modelStore, dataHome: modelStore.dataHome,
    extensionRoot: options.extensionRoot ?? (managed ? extensionHome(modelStore.dataHome) : conversationDataRoot(modelStore.dataHome)) });
  let activeRequests = 0;
  const streamControllers = new Set();
  const runtimeClosures = new WeakMap();
  let storageTransition;
  let runtimeRetired = false;
  let runtimeCleanupError;
  let storageConfigError;
  const closeRuntime = previous => {
    if (runtimeClosures.has(previous)) return runtimeClosures.get(previous);
    const cleanup = Promise.resolve().then(() => previous.close?.()).then(() => true, () => {
      // A failed teardown cannot start a second MCP process world or accept writes on the retired runtime.
      // 清理失败后不能启动第二套 MCP 进程，也不能让已退役运行时继续接受写入。
      runtimeCleanupError = 'RUNTIME_CLEANUP_FAILED';
      console.error(runtimeCleanupError);
      return false;
    });
    runtimeClosures.set(previous, cleanup);
    return cleanup;
  };
  const currentExtensionRoot = () => modelRuntime.extensionRoot ?? modelRuntime.tools?.extensionRoot ??
    conversationDataRoot(modelStore.dataHome);
  const refreshStorage = async () => {
    if (!managed || runtimeCleanupError) return;
    if (storageTransition) { await storageTransition; return; }
    if (activeRequests || storageMigrationActive()) return;
    let nextDataHome, nextExtensionRoot;
    try { nextDataHome = modelHome(); nextExtensionRoot = extensionHome(nextDataHome); storageConfigError = null; }
    catch (error) {
      storageConfigError = ['INVALID_EXTENSION_STORAGE', 'UNSUPPORTED_EXTENSION_STORAGE'].includes(error.code) ? error.code : 'INVALID_STORAGE_CONFIGURATION';
      return; // Keep the previously valid runtime until the native owner repairs its pointer. 原生所有者修复指针之前，保留原先有效的运行时。
    }
    if (!runtimeRetired && modelStore.dataHome === nextDataHome && currentExtensionRoot() === nextExtensionRoot) return;
    const transition = Promise.resolve().then(async () => {
      if (!await closeRuntime(modelRuntime)) return;
      runtimeRetired = true;
      // Maintenance may have started while the last owned MCP processes were being closed.
      // 最后一批自有 MCP 进程关闭期间，维护流程可能已经开始。
      if (storageMigrationActive()) return;
      const dataHome = modelHome(), extensionRoot = extensionHome(dataHome);
      const replacementStore = new ModelStore({ dataHome });
      const replacementRuntime = new ModelRuntime({ modelStore: replacementStore, dataHome, extensionRoot });
      modelStore = replacementStore; modelRuntime = replacementRuntime;
      runtimeRetired = false;
    });
    storageTransition = transition;
    try { await transition; }
    finally { if (storageTransition === transition) storageTransition = null; }
  };
  const server = createServer(async (request, response) => {
    const url = new URL(request.url ?? '/', 'http://127.0.0.1');
    const pathname = url.pathname;
    let counted = false;
    try {
      // Native clients do not send Origin. Block browser-origin access to this local
      // execution/configuration surface, including same-loopback malicious pages.
      // 原生客户端不发送 Origin；拒绝浏览器来源访问此本机执行和配置接口，包括同回环地址上的恶意页面。
      if (request.headers.origin) return sendJson(response, 403, { error: '浏览器不能直接调用本机模型与工具服务。', code: 'BROWSER_ORIGIN_DENIED' });
      await refreshStorage();
      let migrating = managed && storageMigrationActive();
      if (migrating && activeRequests === 0 && !runtimeRetired && !runtimeCleanupError) {
        // Background indexing is an owned writer even with zero HTTP requests. Drain it and checkpoint WAL before copying.
        // HTTP 请求为零时后台索引仍可能写入；迁移复制前停止自有写入、完成回执并关闭 WAL。
        if (!storageTransition) storageTransition = closeRuntime(modelRuntime).then(closed => { if (closed) runtimeRetired = true; });
        const transition = storageTransition;
        try { await transition; }
        finally { if (storageTransition === transition) storageTransition = null; }
      }
      if (!migrating && !runtimeCleanupError && !runtimeRetired && !storageConfigError) {
        // Native migration polls activeRequests; initialization is an owned writer too.
        // 原生迁移会轮询 activeRequests，初始化也是有所有者的写入流程。
        activeRequests++;
        try { await modelRuntime.initializeExtensionStorage?.({ maintenanceActive: () => managed && storageMigrationActive() }); }
        catch (error) {
          if (error.code !== 'STORAGE_MAINTENANCE_ACTIVE') storageConfigError = error.code ?? 'INVALID_EXTENSION_LAYOUT';
        }
        finally { activeRequests--; }
        migrating = managed && storageMigrationActive();
      }
      if (request.method === 'GET' && pathname === '/health')
        return sendJson(response, 200, { status: 'ok', service: 'kynxa-model-gateway', storageProtocol: 1,
          agentProtocol: 5, officialToolsProtocol: 2, hostTerminalProtocol: 3, browserAutomationProtocol: 2, extensionStorageProtocol: EXTENSION_STORAGE_PROTOCOL,
          toolStreamProtocol: 3, replyTimingProtocol: 1, streamProtocol: 1, conversationProtocol: 1, memoryProtocol: 1, memoryManagementProtocol: 1, memoryCandidateProtocol: 1, contextProtocol: 3, retrievalProtocol: 1, dataLayoutVersion: DATA_LAYOUT_VERSION,
          activeRequests, migrating, migrationReady: migrating && runtimeRetired && !runtimeCleanupError && activeRequests === 0,
          modelDataHome: modelStore.dataHome, extensionRoot: currentExtensionRoot(),
          ...(storageConfigError ? { storageConfigError } : {}),
          ...(runtimeCleanupError ? { runtimeCleanupError } : {}) });
      if (migrating) return sendJson(response, 503, { error: '正在迁移数据，请完成后再试。' });
      if (runtimeCleanupError || runtimeRetired)
        return sendJson(response, 503, { error: '旧运行时未安全关闭，请重新启动模型服务。', code: runtimeCleanupError ?? 'RUNTIME_STORAGE_TRANSITION' });
      if (storageConfigError) return sendJson(response, 503, { error: '存储位置配置无效，请在设置中修复。', code: storageConfigError });
      activeRequests++;
      counted = true;
      if (await handleAgentRoute(request, response, url, modelRuntime.tools)) return;
      if (await handleRetrievalRoute(request, response, url, modelRuntime.retrieval)) return;
      const runRoute = /^\/api\/conversations\/([0-9a-f-]{36})\/runs\/([0-9a-f-]{36})$/i.exec(pathname);
      if (request.method === 'GET' && runRoute) {
        const message = (await modelRuntime.conversations.readMessages(runRoute[1]))
          .find(item => item.Role === 'assistant' && item.Id.toLowerCase() === runRoute[2].toLowerCase());
        if (!message?.ToolRun) return sendJson(response, 404, { error: '此请求没有连续执行记录。', code: 'TOOL_RUN_NOT_FOUND' });
        return sendJson(response, 200, { conversationId: runRoute[1], requestId: message.Id,
          status: message.Status, run: message.ToolRun });
      }
      const toolResultRoute = /^\/api\/conversations\/([0-9a-f-]{36})\/tool-results\/([0-9a-f-]{36})$/i.exec(pathname);
      if (request.method === 'GET' && toolResultRoute) {
        const resultContext = { conversationId: toolResultRoute[1] };
        if (url.searchParams.has('offset') || url.searchParams.has('limit'))
          return sendJson(response, 200, await modelRuntime.tools.results.read(resultContext, toolResultRoute[2], {
            offset: url.searchParams.has('offset') ? Number(url.searchParams.get('offset')) : 0,
            limit: url.searchParams.has('limit') ? Number(url.searchParams.get('limit')) : 16000, allowArchived: true }));
        return sendJson(response, 200, { result: await modelRuntime.tools.results.get(resultContext, toolResultRoute[2]) });
      }
      if (pathname === '/api/memory/candidates' || pathname === '/api/memory/candidates/settings') {
        if (request.method === 'GET') {
          await modelRuntime.memory.initializeCandidates();
          return sendJson(response, 200, modelRuntime.memory.candidateStatus());
        }
        if (request.method === 'PATCH' && pathname.endsWith('/settings'))
          return sendJson(response, 200, await modelRuntime.memory.updateCandidateSettings(await readJsonBody(request)));
        return sendJson(response, 405, { error: '此方法不支持。' });
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
        if (request.method === 'GET' || request.method === 'PUT') {
          const catalog = request.method === 'GET' ? await modelRuntime.conversations.catalog()
            : await modelRuntime.conversations.saveCatalog(await readJsonBody(request, 32 * 1024 * 1024));
          modelRuntime.retrieval?.scheduleMountedProjects?.(catalog);
          return sendJson(response, 200, catalog);
        }
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
        const runLimits = toolRunLimits(body.runLimits);
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
              userMessageId: body.userMessageId, permissionMode: body.permissionMode, runLimits }, emit, controller.signal);
            emit({ type: result.completionStatus ?? 'completed', ...result,
              ...(result.completionStatus === 'interrupted' ? { error: '部分步骤尚未完成，已保留执行记录和结果说明。' } : {}) });
          } catch (error) {
            emit({ type: error instanceof StreamFailure ? error.type : 'error',
              content: error.content ?? '', reasoning: error.reasoning ?? '',
              durationMs: error.durationMs ?? 0,
              error: error instanceof StreamFailure ? error.message : '模型调用失败，已保留生成的内容。',
              ...(error.assistantSegments ? { assistantSegments: error.assistantSegments, toolStreamProtocol: 3 } : {}),
              ...(error instanceof StreamFailure && error.code ? { code: error.code } : {}) });
          } finally {
            streamControllers.delete(controller);
            stream.end();
          }
          return;
        }
        const requestId = body.requestId ?? randomUUID();
        const result = await modelRuntime.replyResult({ conversationId: body.conversationId,
          message: body.message, provider: body.provider, model: body.model,
          requestId, userMessageId: body.userMessageId, permissionMode: body.permissionMode, runLimits });
        return sendJson(response, 200, { conversationId: body.conversationId,
          requestId, role: 'assistant', ...result, createdAt: new Date().toISOString() });
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
  // 关闭时先中止上游生成，再等待开放的 HTTP 流结束。
  server.shutdownModelRuntime = async () => {
    for (const controller of streamControllers) controller.abort();
    if (storageTransition) await storageTransition;
    await closeRuntime(modelRuntime);
  };
  return server;
}

export function startModelServer() {
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
  return server;
}

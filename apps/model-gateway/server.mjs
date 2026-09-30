import { createServer } from 'node:http';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { randomUUID } from 'node:crypto';
import { ModelStore, validateConnection } from './store.mjs';
import { ModelRuntime } from './runtime.mjs';
import { authorization } from './protocols.mjs';
import { modelHome } from './storage.mjs';
import { storageMigrationActive } from './storage-maintenance.mjs';
import { StreamFailure } from './streaming.mjs';

const dataHome = modelHome();
const port = Number(process.env.KYNXA_MODEL_API_PORT ?? 5218);
const store = new ModelStore({ dataHome });
const runtime = new ModelRuntime({ modelStore: store, dataHome });

function json(response, status, value) {
  response.writeHead(status, { 'Content-Type': 'application/json; charset=utf-8', 'Cache-Control': 'no-store' });
  response.end(JSON.stringify(value));
}

async function bodyOf(request) {
  const chunks = [];
  let bytes = 0;
  for await (const chunk of request) {
    bytes += chunk.length;
    if (bytes > 64 * 1024) throw new Error('请求体过大。');
    chunks.push(chunk);
  }
  try { return JSON.parse(Buffer.concat(chunks).toString('utf8')); }
  catch { throw new Error('请求体不是有效 JSON。'); }
}

async function probe(input, modelStore) {
  const connection = validateConnection(input, { requireModels: false });
  const apiKey = connection.apiKey || await modelStore.savedKeyFor?.(connection.providerId, connection.baseUrl);
  const start = performance.now();
  const response = await fetch(`${connection.baseUrl}/models`, {
    redirect: 'error',
    headers: authorization({ ...connection, apiKey }),
    signal: AbortSignal.timeout(10000)
  });
  if (!response.ok) {
    await response.body?.cancel();
    throw new Error(`模型列表接口返回 HTTP ${response.status}。`);
  }
  const result = await response.json();
  const models = Array.isArray(result.data) ? result.data : Array.isArray(result.models) ? result.models : [];
  return { ok: true, latencyMs: Math.round(performance.now() - start),
    models: [...new Set(models.map(model => model?.id ?? model?.name)
      .filter(value => typeof value === 'string' && /^[^\s\x00-\x1f]{1,160}$/.test(value)))].slice(0, 100) };
}

export function createModelServer(options = {}) {
  let modelStore = options.modelStore ?? store, modelRuntime = options.modelRuntime ?? runtime;
  const managed = !options.modelStore && !options.modelRuntime;
  let activeRequests = 0;
  const streamControllers = new Set();
  const server = createServer(async (request, response) => {
    const pathname = new URL(request.url ?? '/', 'http://127.0.0.1').pathname;
    let counted = false;
    try {
      const migrating = managed && storageMigrationActive();
      if (managed && !migrating && activeRequests === 0 && modelStore.dataHome !== modelHome()) {
        // No requests are active here; swap synchronously so simultaneous first
        // requests after a migration cannot install separate stores/queues.
        void modelRuntime.close?.();
        modelStore = new ModelStore({ dataHome: modelHome() });
        modelRuntime = new ModelRuntime({ modelStore, dataHome: modelStore.dataHome });
      }
      if (request.method === 'GET' && pathname === '/health')
        return json(response, 200, { status: 'ok', service: 'kynxa-model-gateway', storageProtocol: 1,
          streamProtocol: 1, activeRequests, migrating, modelDataHome: modelStore.dataHome });
      if (migrating) return json(response, 503, { error: '正在迁移数据，请完成后再试。' });
      activeRequests++;
      counted = true;
      if (request.method === 'GET' && pathname === '/api/models')
        return json(response, 200, { providers: await modelStore.list() });
      if (request.method === 'POST' && pathname === '/api/models') {
        const provider = await modelStore.save(await bodyOf(request));
        await modelRuntime.invalidate?.(provider.providerId);
        return json(response, 200, { provider });
      }
      if (request.method === 'POST' && pathname === '/api/models/test')
        return json(response, 200, await probe(await bodyOf(request), modelStore));
      if (request.method === 'POST' && ['/api/chat', '/api/chat/stream'].includes(pathname)) {
        const body = await bodyOf(request);
        if (!body || typeof body.message !== 'string' || !body.message.trim() ||
            typeof body.conversationId !== 'string' || !/^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$/i.test(body.conversationId))
          throw new Error('会话 ID 或消息无效。');
        if (body.permissionMode && !['ask', 'smart', 'full'].includes(body.permissionMode))
          throw new Error('权限模式无效。');
        if (body.requestId != null && (typeof body.requestId !== 'string' ||
            !/^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$/i.test(body.requestId)))
          throw new Error('请求 ID 无效。');
        const provider = (await modelStore.list()).find(item => item.providerId === body.provider);
        if (!provider || !provider.models.includes(body.model)) throw new Error('请先选择已配置的模型。');
        if (pathname === '/api/chat/stream') {
          const controller = new AbortController();
          streamControllers.add(controller);
          const cancel = () => { if (!response.writableEnded) controller.abort(); };
          response.on('close', cancel);
          response.on('error', cancel);
          const identity = { conversationId: body.conversationId,
            requestId: body.requestId ?? randomUUID(), createdAt: new Date().toISOString() };
          const emit = event => {
            if (response.destroyed || response.writableEnded) return;
            if (response.writableLength > 1024 * 1024) {
              controller.abort(); response.destroy(); return;
            }
            response.write(`event: ${event.type}\ndata: ${JSON.stringify({ ...identity, ...event })}\n\n`);
          };
          response.writeHead(200, { 'Content-Type': 'text/event-stream; charset=utf-8',
            'Cache-Control': 'no-cache, no-transform', 'X-Accel-Buffering': 'no' });
          response.flushHeaders();
          emit({ type: 'started' });
          const heartbeat = setInterval(() => {
            if (!response.destroyed && !response.writableEnded) response.write(': keep-alive\n\n');
          }, 15000);
          heartbeat.unref();
          try {
            const result = await modelRuntime.replyStream({ conversationId: body.conversationId,
              message: body.message, provider: body.provider, model: body.model, requestId: identity.requestId }, emit, controller.signal);
            emit({ type: 'completed', ...result });
          } catch (error) {
            emit({ type: error instanceof StreamFailure ? error.type : 'error',
              content: error.content ?? '', reasoning: error.reasoning ?? '',
              error: error instanceof StreamFailure ? error.message : '模型调用失败，已保留生成的内容。' });
          } finally {
            clearInterval(heartbeat);
            streamControllers.delete(controller);
            response.off('close', cancel); response.off('error', cancel);
            response.end();
          }
          return;
        }
        const content = await modelRuntime.reply({ conversationId: body.conversationId,
          message: body.message, provider: body.provider, model: body.model });
        return json(response, 200, { conversationId: body.conversationId,
          requestId: randomUUID(), role: 'assistant', content, createdAt: new Date().toISOString() });
      }
      json(response, 404, { error: '接口不存在。' });
    } catch (error) {
      const clientError = error.message?.includes('无效') || error.message?.includes('填写') ||
        error.message?.includes('Provider ID') || error.message?.includes('连接名称') ||
        error.message?.includes('API Key') || error.message?.includes('Base URL') || error.message?.includes('HTTPS') ||
        error.message?.includes('模型') || error.message?.includes('请求体') ||
        error.message?.includes('权限模式') || error.message?.includes('会话 ID');
      json(response, clientError ? 400 : 502, { error: error.message ?? '模型调用失败。' });
    } finally {
      if (counted) activeRequests--;
    }
  });
  // Shutdown aborts upstream generations before waiting for open HTTP streams.
  server.shutdownModelRuntime = async () => {
    for (const controller of streamControllers) controller.abort();
    await modelRuntime.close?.();
  };
  return server;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const server = createModelServer();
  server.listen(port, '127.0.0.1', () => console.log(`KYNXA model gateway: http://127.0.0.1:${port}`));
  const shutdown = async () => { server.close(); await server.shutdownModelRuntime(); };
  process.once('SIGINT', shutdown);
  process.once('SIGTERM', shutdown);
}

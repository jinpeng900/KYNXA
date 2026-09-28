import { createServer } from 'node:http';
import { homedir } from 'node:os';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { randomUUID } from 'node:crypto';
import { HarnessModelStore, validateConnection } from './store.mjs';
import { HarnessRuntime } from './harness.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const harnessRoot = resolve(process.env.KYNXA_DSH_ROOT ?? resolve(here, '../../../../deepseek-harness/deepseek-harness'));
const dataHome = resolve(process.env.KYNXA_DSH_HOME ?? resolve(homedir(), '.kynxa/harness'));
const workspaceRoot = resolve(process.env.KYNXA_WORKSPACE_ROOT ?? process.cwd());
const port = Number(process.env.KYNXA_MODEL_API_PORT ?? 5218);
const store = new HarnessModelStore({ harnessRoot, dataHome });
const runtime = new HarnessRuntime({ harnessRoot, dataHome, workspaceRoot,
  patchPath: resolve(here, 'kynxa-sdk.cordis.patch.yml') });

function json(response, status, value) {
  response.writeHead(status, { 'Content-Type': 'application/json; charset=utf-8', 'Cache-Control': 'no-store' });
  response.end(JSON.stringify(value));
}

async function bodyOf(request) {
  let body = '';
  for await (const chunk of request) {
    body += chunk;
    if (body.length > 64 * 1024) throw new Error('请求体过大。');
  }
  try { return JSON.parse(body); }
  catch { throw new Error('请求体不是有效 JSON。'); }
}

async function probe(input, modelStore) {
  const connection = validateConnection(input, { requireModels: false });
  const apiKey = connection.apiKey || await modelStore.savedKeyFor?.(connection.providerId, connection.baseUrl);
  const start = performance.now();
  const response = await fetch(`${connection.baseUrl}/models`, {
    headers: apiKey ? { Authorization: `Bearer ${apiKey}` } : {},
    signal: AbortSignal.timeout(10000)
  });
  if (!response.ok) throw new Error(`模型列表接口返回 HTTP ${response.status}。`);
  const result = await response.json();
  const models = Array.isArray(result.data) ? result.data : Array.isArray(result.models) ? result.models : [];
  return { ok: true, latencyMs: Math.round(performance.now() - start),
    models: models.map(model => model?.id ?? model?.name).filter(value => typeof value === 'string') };
}

export function createModelServer({ modelStore = store, modelRuntime = runtime } = {}) {
  return createServer(async (request, response) => {
    const pathname = new URL(request.url ?? '/', 'http://127.0.0.1').pathname;
    try {
      if (request.method === 'GET' && pathname === '/health')
        return json(response, 200, { status: 'ok', service: 'kynxa-model-gateway' });
      if (request.method === 'GET' && pathname === '/api/models')
        return json(response, 200, { providers: await modelStore.list() });
      if (request.method === 'POST' && pathname === '/api/models') {
        const provider = await modelStore.save(await bodyOf(request));
        await modelRuntime.invalidate?.(provider.providerId);
        return json(response, 200, { provider });
      }
      if (request.method === 'POST' && pathname === '/api/models/test')
        return json(response, 200, await probe(await bodyOf(request), modelStore));
      if (request.method === 'POST' && pathname === '/api/chat') {
        const body = await bodyOf(request);
        if (!body || typeof body.message !== 'string' || !body.message.trim() ||
            typeof body.conversationId !== 'string' || !/^[0-9a-f-]{36}$/i.test(body.conversationId))
          throw new Error('会话 ID 或消息无效。');
        if (body.permissionMode && !['ask', 'smart', 'full'].includes(body.permissionMode))
          throw new Error('权限模式无效。');
        const provider = (await modelStore.list()).find(item => item.providerId === body.provider);
        if (!provider || !provider.models.includes(body.model)) throw new Error('请先选择已配置的模型。');
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
    }
  });
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const server = createModelServer();
  server.listen(port, '127.0.0.1', () => console.log(`KYNXA model gateway: http://127.0.0.1:${port}`));
  const shutdown = async () => { server.close(); await runtime.close(); };
  process.once('SIGINT', shutdown);
  process.once('SIGTERM', shutdown);
}

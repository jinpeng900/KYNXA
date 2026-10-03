import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { mkdtemp, readFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { createModelServer } from '../server.mjs';
import { ModelStore } from '../store.mjs';

async function listening(server) {
  await new Promise(resolveReady => server.listen(0, '127.0.0.1', resolveReady));
  return `http://127.0.0.1:${server.address().port}`;
}

test('stores a redacted route and uses the configured model for chat', async t => {
  const home = await mkdtemp(join(tmpdir(), 'kynxa-model-gateway-'));
  const upstream = createServer((request, response) => {
    response.writeHead(200, { 'Content-Type': 'application/json' });
    response.end(JSON.stringify({ data: [{ id: 'test-model' }] }));
  });
  const upstreamUrl = await listening(upstream);
  t.after(() => upstream.close());
  let invocation;
  const gateway = createModelServer({
    modelStore: new ModelStore({ dataHome: home }),
    modelRuntime: { replyResult: async input => { invocation = input; return { content: '模型回复', durationMs: 1234 }; } }
  });
  const url = await listening(gateway);
  t.after(() => gateway.close());
  const input = { providerId: 'local-test', displayName: '本地测试', baseUrl: `${upstreamUrl}/v1`,
    models: ['test-model'] };
  const testResponse = await fetch(`${url}/api/models/test`, { method: 'POST',
    body: JSON.stringify({ ...input, models: [] }) });
  assert.equal(testResponse.status, 200);
  assert.deepEqual((await testResponse.json()).models, ['test-model']);

  const saved = await fetch(`${url}/api/models`, { method: 'POST', body: JSON.stringify(input) });
  assert.equal(saved.status, 200);
  const listed = await (await fetch(`${url}/api/models`)).json();
  assert.equal(listed.providers[0].providerId, 'local-test');
  assert.equal(listed.providers[0].hasApiKey, false);
  assert.equal(listed.providers[0].apiKey, undefined);
  const settings = JSON.parse(await readFile(join(home, 'connections.json'), 'utf8'));
  assert.equal(settings.providers[0].providerId, 'local-test');
  assert.equal(settings.providers[0].apiKey, undefined);

  const conversationId = '8cb45842-7b8a-44d1-b621-3188d1f5d4d0';
  const reply = await fetch(`${url}/api/chat`, { method: 'POST', body: JSON.stringify({
    conversationId, message: '你好', provider: 'local-test', model: 'test-model', permissionMode: 'ask'
  }) });
  assert.equal(reply.status, 200);
  const payload = await reply.json();
  assert.equal(payload.content, '模型回复'); assert.equal(payload.durationMs, 1234);
  assert.equal(invocation.provider, 'local-test');
  assert.equal(invocation.model, 'test-model');
});

test('rejects non-local HTTP routes and never includes an API key in model listings', async t => {
  const home = await mkdtemp(join(tmpdir(), 'kynxa-model-security-'));
  const gateway = createModelServer({
    modelStore: new ModelStore({ dataHome: home }),
    modelRuntime: { replyResult: async () => ({ content: 'unused', durationMs: 0 }) }
  });
  const url = await listening(gateway);
  t.after(() => gateway.close());
  const rejected = await fetch(`${url}/api/models`, { method: 'POST', body: JSON.stringify({
    providerId: 'remote-http', displayName: 'Remote', baseUrl: 'http://example.com/v1',
    apiKey: 'secret-token', models: ['model']
  }) });
  assert.equal(rejected.status, 400);
  const saved = await fetch(`${url}/api/models`, { method: 'POST', body: JSON.stringify({
    providerId: 'secure-api', displayName: 'Secure', baseUrl: 'https://example.com/v1',
    apiKey: 'secret-token', models: ['model']
  }) });
  assert.equal(saved.status, 200);
  assert.equal(JSON.stringify(await saved.json()).includes('secret-token'), false);
  assert.equal(JSON.stringify(await (await fetch(`${url}/api/models`)).json()).includes('secret-token'), false);
});

test('keeps JSON headers, validation errors and explicit runtime status codes', async t => {
  const home = await mkdtemp(join(tmpdir(), 'kynxa-model-http-'));
  const conversationId = '8cb45842-7b8a-44d1-b621-3188d1f5d4d0';
  let invoked = 0;
  const gateway = createModelServer({
    modelStore: new ModelStore({ dataHome: home }),
    modelRuntime: { replyResult: async input => {
      invoked++;
      if (input.message === 'conflict') throw Object.assign(new Error('记录已更新。'),
        { statusCode: 409, code: 'CATALOG_CONFLICT' });
      throw new Error('服务暂不可用。');
    } }
  });
  const url = await listening(gateway);
  t.after(() => { gateway.closeAllConnections(); gateway.close(); });
  const cases = [
    { path: '/api/chat', method: 'POST', body: '{', status: 400, value: { error: '请求体不是有效 JSON。' } },
    { path: '/api/chat/stream', method: 'POST', body: '{', status: 400, value: { error: '请求体不是有效 JSON。' } },
    { path: '/api/chat/stream', method: 'POST', body: 'null', status: 400, value: { error: '会话 ID 或消息无效。' } },
    { path: '/api/chat/stream', method: 'POST', body: JSON.stringify({ conversationId, message: '你好',
      permissionMode: 'invalid' }), status: 400, value: { error: '权限模式无效。' } },
    { path: '/missing', method: 'GET', status: 404, value: { error: '接口不存在。' } },
    { path: `/api/conversations/${conversationId}/memory`, method: 'PUT', status: 405,
      value: { error: '此记忆接口不支持该操作。' } },
    { path: '/api/chat', method: 'POST', body: JSON.stringify({ conversationId, message: 'conflict' }),
      status: 409, value: { error: '记录已更新。', code: 'CATALOG_CONFLICT' } },
    { path: '/api/chat', method: 'POST', body: JSON.stringify({ conversationId, message: 'failure' }),
      status: 502, value: { error: '服务暂不可用。' } }
  ];
  for (const input of cases) {
    const response = await fetch(`${url}${input.path}`, { method: input.method, body: input.body });
    assert.equal(response.status, input.status, input.path);
    assert.equal(response.headers.get('content-type'), 'application/json; charset=utf-8');
    assert.equal(response.headers.get('cache-control'), 'no-store');
    assert.deepEqual(await response.json(), input.value);
  }
  assert.equal(invoked, 2);
  const health = await fetch(`${url}/health`);
  assert.equal(health.status, 200);
  assert.equal(health.headers.get('content-type'), 'application/json; charset=utf-8');
  assert.equal(health.headers.get('cache-control'), 'no-store');
  assert.equal((await health.json()).activeRequests, 0);
});

function paddedBody(input, byteLength) {
  const value = { ...input, padding: '' };
  value.padding = 'x'.repeat(byteLength - Buffer.byteLength(JSON.stringify(value)));
  return JSON.stringify(value);
}

test('preserves byte limits for chat, memory, model connections and catalog requests', async t => {
  const home = await mkdtemp(join(tmpdir(), 'kynxa-model-body-limits-'));
  const conversationId = '8cb45842-7b8a-44d1-b621-3188d1f5d4d0';
  let invocations = 0;
  const accept = async () => { invocations++; return { ok: true }; };
  const gateway = createModelServer({
    modelStore: new ModelStore({ dataHome: home }),
    modelRuntime: {
      replyResult: async () => { invocations++; return { content: '回复', durationMs: 0 }; },
      replyStream: async () => { invocations++; return { content: '回复', reasoning: '' }; },
      memory: { create: accept, update: accept, delete: accept },
      conversations: { saveCatalog: accept }
    }
  });
  const url = await listening(gateway);
  t.after(() => { gateway.closeAllConnections(); gateway.close(); });
  const post = (path, body, method = 'POST') => fetch(`${url}${path}`, { method, body });
  const chat = { conversationId, message: '你好' };
  for (const [method, path, status] of [
    ['POST', '/api/chat', 200], ['POST', '/api/chat/stream', 200],
    ['POST', `/api/conversations/${conversationId}/memory`, 201],
    ['PATCH', `/api/conversations/${conversationId}/memory/test-memory`, 200],
    ['DELETE', `/api/conversations/${conversationId}/memory/test-memory`, 200]
  ]) {
    const accepted = await post(path, paddedBody(chat, 64 * 1024), method);
    assert.equal(accepted.status, status, path);
    await accepted.text();
    const rejected = await post(path, paddedBody(chat, 64 * 1024 + 1), method);
    assert.equal(rejected.status, 400, path);
    assert.deepEqual(await rejected.json(), { error: '请求体过大。' });
  }
  assert.equal(invocations, 5);

  const connection = { providerId: 'local-test', displayName: '测试',
    baseUrl: 'http://127.0.0.1:1/v1', models: ['test-model'] };
  const saved = await post('/api/models', paddedBody(connection, 8 * 1024 * 1024));
  assert.equal(saved.status, 200);
  assert.equal((await saved.json()).provider.providerId, 'local-test');
  const invalidConnection = await post('/api/models/test', paddedBody({ providerId: 'invalid id' }, 8 * 1024 * 1024));
  assert.equal(invalidConnection.status, 400);
  assert.match((await invalidConnection.json()).error, /^Provider ID/);
  for (const path of ['/api/models', '/api/models/test']) {
    const rejected = await post(path, paddedBody(connection, 8 * 1024 * 1024 + 1));
    assert.equal(rejected.status, 400, path);
    assert.deepEqual(await rejected.json(), { error: '请求体过大。' });
  }

  const acceptedCatalog = await post('/api/conversations/catalog', paddedBody({}, 32 * 1024 * 1024), 'PUT');
  assert.equal(acceptedCatalog.status, 200);
  assert.deepEqual(await acceptedCatalog.json(), { ok: true });
  const rejectedCatalog = await post('/api/conversations/catalog', paddedBody({}, 32 * 1024 * 1024 + 1), 'PUT');
  assert.equal(rejectedCatalog.status, 400);
  assert.deepEqual(await rejectedCatalog.json(), { error: '请求体过大。' });
  assert.equal(invocations, 6);
});

import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { mkdtemp } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { ModelStore, validateConnection } from '../store.mjs';
import { ModelRuntime } from '../runtime.mjs';
import { createModelServer } from '../server.mjs';
import { isolateFixtureMcpCatalog } from './tool-fixture.mjs';

async function listen(server, t) {
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(() => server.close());
  return `http://127.0.0.1:${server.address().port}`;
}

test('direct API chat carries the selected ID, auth and persisted history without agent tools', async t => {
  const requests = [];
  let fail = false;
  const upstream = createServer(async (request, response) => {
    let body = '';
    for await (const chunk of request) body += chunk;
    requests.push({ url: request.url, auth: request.headers.authorization, ...JSON.parse(body) });
    response.writeHead(fail ? 401 : 200, { 'Content-Type': 'application/json' });
    response.end(JSON.stringify(fail ? { error: 'do not expose upstream error or keys' }
      : { choices: [{ message: { role: 'assistant', content: '你好' } }] }));
  });
  const baseUrl = `${await listen(upstream, t)}/v1`;
  const dataHome = await mkdtemp(join(tmpdir(), 'kynxa-direct-api-'));
  const store = new ModelStore({ dataHome });
  await store.save({ providerId: 'test-api', displayName: 'Test', baseUrl,
    models: ['deepseek-flash'], apiKey: 'local-test-key' });
  let runtime = new ModelRuntime({ modelStore: store, dataHome });
  isolateFixtureMcpCatalog(runtime.tools);
  t.after(() => runtime.close());
  const input = { conversationId: '8cb45842-7b8a-44d1-b621-3188d1f5d4d0',
    message: '第一条', provider: 'test-api', model: 'deepseek-flash' };
  assert.equal(await runtime.reply(input), '你好');
  assert.equal(requests[0].url, '/v1/chat/completions');
  assert.equal(requests[0].auth, 'Bearer local-test-key');
  assert.equal(requests[0].model, 'deepseek-flash');
  assert.equal(requests[0].stream, false);
  assert.equal(requests[0].tools, undefined);
  await runtime.close();
  runtime = new ModelRuntime({ modelStore: store, dataHome });
  isolateFixtureMcpCatalog(runtime.tools);
  fail = true;
  await assert.rejects(runtime.reply({ ...input, message: '失败消息' }), /HTTP 401/);
  fail = false;
  const gatewayUrl = await listen(createModelServer({ modelStore: store, modelRuntime: runtime }), t);
  const response = await fetch(`${gatewayUrl}/api/chat`, { method: 'POST',
    body: JSON.stringify({ ...input, message: '第二条', permissionMode: 'ask' }) });
  assert.equal(response.status, 200);
  assert.equal((await response.json()).content, '你好');
  assert.deepEqual(requests.at(-1).messages.filter(m => m.role !== 'system').map(m => m.content), ['第一条', '你好', '第二条']);
  await runtime.reply({ ...input, conversationId: 'another-chat', message: '独立会话' });
  assert.equal(requests.at(-1).messages.length, 1);
});

test('preserves saved keys only for unchanged endpoints; never returns secrets', async () => {
  const dataHome = await mkdtemp(join(tmpdir(), 'kynxa-store-'));
  const store = new ModelStore({ dataHome });
  const input = { providerId: 'deepseek', displayName: 'DeepSeek',
    baseUrl: 'https://api.deepseek.com', models: ['deepseek-flash'] };
  await store.save({ ...input, apiKey: 'test-secret' });
  await store.save({ ...input, displayName: 'Renamed' });
  assert.equal(await store.savedKeyFor(input.providerId, input.baseUrl), 'test-secret');
  assert.equal(await store.savedKeyFor(input.providerId, 'https://example.com'), undefined);
  await assert.rejects(store.save({ ...input, baseUrl: 'https://example.com' }), /API Key/);
  assert.equal((await store.list())[0].baseUrl, input.baseUrl);
  assert.equal(JSON.stringify(await store.list()).includes('test-secret'), false);
  // Serial writes do not lose either connection.
  await Promise.all([store.save({ ...input, providerId: 'second', apiKey: 'second-key' }),
    store.save({ ...input, providerId: 'third', apiKey: 'third-key' })]);
  assert.equal((await store.list()).length, 3);
});

test('serializes simultaneous turns in one conversation', async t => {
  const messages = [];
  const url = await listen(createServer(async (request, response) => {
    let body = '';
    for await (const chunk of request) body += chunk;
    messages.push(JSON.parse(body).messages);
    response.setHeader('Content-Type', 'application/json');
    response.end(JSON.stringify({ choices: [{ message: { content: 'reply' } }] }));
  }), t);
  const dataHome = await mkdtemp(join(tmpdir(), 'kynxa-ordered-'));
  const store = new ModelStore({ dataHome });
  await store.save({ providerId: 'local', displayName: 'Local', baseUrl: url, models: ['test'] });
  const runtime = new ModelRuntime({ modelStore: store, dataHome });
  isolateFixtureMcpCatalog(runtime.tools);
  t.after(() => runtime.close());
  const input = { conversationId: 'test', provider: 'local', model: 'test' };
  await Promise.all([runtime.reply({ ...input, message: 'one' }), runtime.reply({ ...input, message: 'two' })]);
  assert.deepEqual(messages[1].map(m => m.content), ['one', 'reply', 'two']);
});

test('generic local API discovers, saves and chats directly without credentials or Ollama', async t => {
  const requests = [];
  const endpoint = await listen(createServer(async (request, response) => {
    let body = '';
    for await (const chunk of request) body += chunk;
    requests.push({ path: request.url, authorization: request.headers.authorization,
      body: body ? JSON.parse(body) : undefined });
    response.setHeader('Content-Type', 'application/json');
    if (request.url === '/v1/models') response.end(JSON.stringify({ data: [{ id: 'my-local-model' }] }));
    else if (request.url === '/v1/chat/completions')
      response.end(JSON.stringify({ choices: [{ message: { content: '本地接口回复' } }] }));
    else { response.statusCode = 404; response.end('{}'); }
  }), t);
  const dataHome = await mkdtemp(join(tmpdir(), 'kynxa-local-direct-'));
  const modelStore = new ModelStore({ dataHome });
  const modelRuntime = new ModelRuntime({ modelStore, dataHome });
  isolateFixtureMcpCatalog(modelRuntime.tools);
  t.after(() => modelRuntime.close());
  const gateway = await listen(createModelServer({ modelStore, modelRuntime }), t);
  const post = (path, body) => fetch(`${gateway}${path}`, { method: 'POST', body: JSON.stringify(body) });
  const connection = { providerId: 'local-api', displayName: '我的本地服务', baseUrl: `${endpoint}/v1`, models: [] };
  const probe = await post('/api/models/test', connection);
  assert.equal(probe.status, 200);
  connection.models = (await probe.json()).models;
  assert.deepEqual(connection.models, ['my-local-model']);
  const saved = await post('/api/models', connection);
  assert.equal(saved.status, 200);
  assert.equal((await saved.json()).provider.hasApiKey, false);
  const reply = await post('/api/chat', { conversationId: '8cb45842-7b8a-44d1-b621-3188d1f5d4d0',
    message: '你好', provider: 'local-api', model: 'my-local-model' });
  assert.equal(reply.status, 200);
  assert.equal((await reply.json()).content, '本地接口回复');
  assert.deepEqual(requests.map(r => r.path), ['/v1/models', '/v1/chat/completions']);
  assert.ok(requests.every(r => r.authorization === undefined));
  assert.equal(requests[1].body.model, 'my-local-model');
});

test('local and private IP routes allow HTTP and optional keys; public routes retain HTTPS requirement', async () => {
  const dataHome = await mkdtemp(join(tmpdir(), 'kynxa-lan-api-'));
  const store = new ModelStore({ dataHome });
  const connection = { providerId: 'local-api', displayName: 'Local', models: ['model'] };
  for (const host of ['localhost', '127.0.0.2', '10.1.2.3', '172.16.0.1', '172.31.255.254',
    '192.168.1.25', '[::1]', '[fd12::1]', '[fc00::1]', '[::ffff:192.168.1.2]']) {
    const saved = await store.save({ ...connection, baseUrl: `http://${host}:8080/v1` });
    assert.equal(saved.hasApiKey, false, host);
  }
  for (const host of ['example.com', '192.168.1.2.example.com', '172.15.0.1', '172.32.0.1',
    '192.169.1.1', '8.8.8.8', '[2001:4860::1]', '[::ffff:8.8.8.8]']) {
    assert.throws(() => validateConnection({ ...connection, baseUrl: `http://${host}/v1` }), /HTTPS/, host);
  }
  // Private servers can also require authentication; supplied keys are preserved.
  const secured = { ...connection, baseUrl: 'http://192.168.1.25:8080/v1', apiKey: 'local-secret' };
  await store.save(secured);
  assert.equal(await store.savedKeyFor(secured.providerId, secured.baseUrl), 'local-secret');
});

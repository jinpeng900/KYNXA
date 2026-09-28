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
    modelRuntime: { reply: async input => { invocation = input; return '模型回复'; } }
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
  assert.equal((await reply.json()).content, '模型回复');
  assert.equal(invocation.provider, 'local-test');
  assert.equal(invocation.model, 'test-model');
});

test('rejects non-local HTTP routes and never includes an API key in model listings', async t => {
  const home = await mkdtemp(join(tmpdir(), 'kynxa-model-security-'));
  const gateway = createModelServer({
    modelStore: new ModelStore({ dataHome: home }),
    modelRuntime: { reply: async () => 'unused' }
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

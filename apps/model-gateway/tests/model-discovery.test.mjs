import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { mkdtemp } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { discoverModels } from '../model-discovery.mjs';
import { MAX_CONNECTION_MODELS, ModelStore, validateConnection } from '../store.mjs';
import { createModelServer } from '../server.mjs';

async function listening(t, handler) {
  const server = createServer(handler);
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(() => { server.closeAllConnections(); server.close(); });
  return `http://127.0.0.1:${server.address().port}`;
}

const connection = baseUrl => ({ providerId: 'discovery-test', displayName: 'Discovery',
  baseUrl: `${baseUrl}/v1`, protocol: 'openai-completions', models: [] });

test('complete discovery can fetch, save and reload more than 100 models or 64 KiB', async t => {
  const expected = Array.from({ length: 1500 }, (_, i) => `model-${i}-${'a'.repeat(60)}`);
  const upstream = await listening(t, (request, response) => {
    assert.equal(request.url, '/v1/models');
    response.end(JSON.stringify({ data: [...expected, expected[0]].map(id => ({ id })) }));
  });
  const modelStore = new ModelStore({ dataHome: await mkdtemp(join(tmpdir(), 'kynxa-discovery-')) });
  const gateway = createModelServer({ modelStore, modelRuntime: {} });
  await new Promise(resolve => gateway.listen(0, '127.0.0.1', resolve));
  t.after(() => { gateway.closeAllConnections(); gateway.close(); });
  const gatewayUrl = `http://127.0.0.1:${gateway.address().port}`;
  const post = (path, body) => fetch(`${gatewayUrl}${path}`, { method: 'POST', body: JSON.stringify(body) });
  const discovered = await post('/api/models/test', connection(upstream));
  assert.equal(discovered.status, 200);
  const { models } = await discovered.json();
  assert.deepEqual(models, expected);
  const saved = await post('/api/models', { ...connection(upstream), models });
  assert.equal(saved.status, 200);
  assert.deepEqual((await saved.json()).provider.models, expected);
  assert.deepEqual((await (await fetch(`${gatewayUrl}/api/models`)).json()).providers[0].models, expected);
  const reprobe = await post('/api/models/test', { ...connection(upstream), models });
  assert.equal(reprobe.status, 200);
});

test('Anthropic follows all cursor pages, keeps credentials on the configured endpoint and deduplicates', async t => {
  const requests = [];
  const upstream = await listening(t, (request, response) => {
    const url = new URL(request.url, 'http://localhost');
    requests.push(url.searchParams.get('after_id'));
    assert.equal(url.pathname, '/v1/models');
    assert.equal(url.searchParams.get('limit'), '1000');
    assert.equal(request.headers['x-api-key'], 'test-only-key');
    assert.equal(request.headers['anthropic-version'], '2023-06-01');
    const next = url.searchParams.get('after_id');
    response.end(JSON.stringify(next === null
      ? { data: [{ id: 'model-a' }, { id: 'model-b' }], has_more: true, last_id: 'model-b' }
      : { data: [{ id: 'model-b' }, { id: 'model-c' }], has_more: false, last_id: 'model-c' }));
  });
  assert.deepEqual(await discoverModels({ ...connection(upstream), protocol: 'anthropic-messages', apiKey: 'test-only-key' }),
    ['model-a', 'model-b', 'model-c']);
  assert.deepEqual(requests, [null, 'model-b']);
});

test('models/name responses remain supported without inventing pagination', async t => {
  const upstream = await listening(t, (request, response) => {
    response.end(JSON.stringify({ models: [{ name: 'local-model:latest' }, { name: 'local-model:latest' }] }));
  });
  assert.deepEqual(await discoverModels(connection(upstream)), ['local-model:latest']);
});

for (const [description, payload, protocol, error] of [
  ['repeated cursor', { data: [{ id: 'one' }], has_more: true, last_id: 'one' }, 'anthropic-messages', /游标/],
  ['missing cursor', { data: [{ id: 'one' }], has_more: true }, 'anthropic-messages', /游标/],
  ['empty page with more results', { data: [], has_more: true, last_id: 'one' }, 'anthropic-messages', /游标/],
  ['unknown cursor scheme', { data: [{ id: 'one' }], has_more: true, last_id: 'one' }, 'openai-completions', /分页协议/],
  ['token pagination', { data: [{ id: 'one' }], nextPageToken: 'next' }, 'openai-completions', /分页格式/],
  ['invalid model IDs', { data: [{ id: 'one' }, { id: 'invalid model' }] }, 'openai-completions', /无效 Model ID/],
  ['wrong result shape', { error: 'service failed' }, 'openai-completions', /格式无效/]
]) {
  test(`refuses partial success for ${description}`, async t => {
    let requests = 0;
    const upstream = await listening(t, (request, response) => { requests++; response.end(JSON.stringify(payload)); });
    await assert.rejects(discoverModels({ ...connection(upstream), protocol }), error);
    assert.ok(requests <= 2);
  });
}

test('a failed later page does not return the first page as a complete list', async t => {
  const upstream = await listening(t, (request, response) => {
    if (new URL(request.url, 'http://localhost').searchParams.has('after_id')) {
      response.writeHead(503); response.end('upstream unavailable');
    } else response.end(JSON.stringify({ data: [{ id: 'one' }], has_more: true, last_id: 'one' }));
  });
  await assert.rejects(discoverModels({ ...connection(upstream), protocol: 'anthropic-messages' }), /HTTP 503/);
});

test('discovery and saved connections report the same explicit model count limit', async t => {
  const models = Array.from({ length: MAX_CONNECTION_MODELS + 1 }, (_, i) => `model-${i}`);
  const upstream = await listening(t, (request, response) => {
    response.end(JSON.stringify({ data: models.map(id => ({ id })) }));
  });
  await assert.rejects(discoverModels(connection(upstream)), new RegExp(String(MAX_CONNECTION_MODELS)));
  assert.throws(() => validateConnection({ ...connection(upstream), models }), new RegExp(String(MAX_CONNECTION_MODELS)));
});

test('unbounded pages terminate with an explicit error', async t => {
  let requests = 0;
  const upstream = await listening(t, (request, response) => {
    const id = `model-${++requests}`;
    response.end(JSON.stringify({ data: [{ id }], has_more: true, last_id: id }));
  });
  await assert.rejects(discoverModels({ ...connection(upstream), protocol: 'anthropic-messages' }), /分页超过限制/);
  assert.equal(requests, 100);
});

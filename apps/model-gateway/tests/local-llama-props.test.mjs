import assert from 'node:assert/strict';
import { test } from 'node:test';
import { LocalModelResourceObserver } from '../models/local-model-resources.mjs';
import { projectLocalToolCatalog } from '../models/local-tool-compatibility.mjs';

const connection = { providerId: 'qwen-local', baseUrl: 'http://127.0.0.1:8080/v1', models: ['qwen-synthetic'],
  apiKey: 'synthetic-key' };
const props = { default_generation_settings: { n_ctx: 8192, params: { temperature: 0.6 } }, total_slots: 1,
  chat_template: 'Private template stays in the HTTP response only.', model_path: 'private-model.gguf',
  build_info: 'b9999-synthetic', is_sleeping: false };

test('read-only llama props observes actual slot nctx, backend evidence and unknown memory without autoloading', async () => {
  const calls = [];
  const observer = new LocalModelResourceObserver({ fetchImpl: async (url, options) => {
    calls.push({ url, options }); return new Response(JSON.stringify(props));
  } });
  const observed = await observer.observe(connection, { contextTokens: 1_000_000 });
  assert.equal(observed.backend, 'llama.cpp');
  assert.equal(observed.source, 'llama-cpp-props');
  assert.equal(observed.endpointOrigin, 'http://127.0.0.1:8080');
  assert.equal(observed.loaded, true);
  assert.equal(observed.runtimeContextTokens, 8192);
  assert.equal(observed.contextTokens, 8192);
  assert.equal(observed.parallelSlots, 1);
  assert.equal(observed.observedMemoryBytes, null);
  assert.equal(observed.observedGpuMemoryBytes, null);
  assert.equal(observed.estimatedKvCacheBytes, null);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].url.pathname, '/props');
  assert.equal(calls[0].url.searchParams.get('model'), 'qwen-synthetic');
  assert.equal(calls[0].url.searchParams.get('autoload'), 'false');
  assert.equal(calls[0].options.method, 'GET');
  assert.equal(calls[0].options.redirect, 'error');
  const serialized = JSON.stringify(observed);
  for (const secret of [connection.apiKey, props.model_path, props.chat_template]) assert.ok(!serialized.includes(secret));
  const projected = projectLocalToolCatalog([{ name: 'synthetic', wireName: 'synthetic',
    inputSchema: { type: 'string', maxLength: 2000 } }], { connection, localModel: observed });
  assert.equal(projected.applied, true);
  const stop = observer.beginGeneration(connection);
  const cached = await observer.observe(connection, { contextTokens: 1_000_000 });
  assert.equal(cached.cached, true);
  assert.equal(cached.applicationGenerationState, 'generating');
  assert.equal(cached.globalGenerationState, 'unknown');
  assert.equal(calls.length, 1);
  stop();
});

test('a sleeping llama server never claims allocated runtime context or exact residency', async () => {
  const observer = new LocalModelResourceObserver({ fetchImpl: async () =>
    new Response(JSON.stringify({ ...props, is_sleeping: true })) });
  const observed = await observer.observe(connection);
  assert.equal(observed.loaded, false);
  assert.equal(observed.runtimeContextTokens, null);
  assert.equal(observed.contextTokens, 8192);
  assert.equal(observed.observedMemoryBytes, null);
});

test('invalid or unavailable props never certifies llama.cpp from a Qwen name or port', async () => {
  for (const invalid of [{}, { n_ctx: 8192 }, { ...props, total_slots: 0 },
    { ...props, default_generation_settings: { n_ctx: Number.MAX_SAFE_INTEGER, params: {} } },
    { ...props, default_generation_settings: { n_ctx: 8192, params: [] } }, { ...props, chat_template: 3 }]) {
    const observer = new LocalModelResourceObserver({ fetchImpl: async () => new Response(JSON.stringify(invalid)) });
    const observed = await observer.observe(connection);
    assert.equal(observed.backend, 'external-openai');
    assert.equal(observed.contextTokens, null);
    assert.equal(observed.diagnostic.code, 'LOCAL_MODEL_RESOURCE_API_UNAVAILABLE');
  }
  const observer = new LocalModelResourceObserver({ fetchImpl: async () => new Response('private failure', { status: 503 }) });
  assert.equal((await observer.observe(connection)).backend, 'external-openai');
});

test('props observations stay loopback-only, response-bounded and cancellable', async () => {
  let calls = 0;
  const observer = new LocalModelResourceObserver({ fetchImpl: async () => { calls++; return new Response(JSON.stringify(props)); } });
  for (const baseUrl of ['https://api.example.com/v1', 'http://192.168.0.2:8080/v1', 'http://user:password@localhost:8080/v1'])
    assert.equal((await observer.observe({ ...connection, baseUrl })).diagnostic.code, 'LOCAL_MODEL_OBSERVATION_NOT_LOCAL');
  assert.equal(calls, 0);
  const oversized = new LocalModelResourceObserver({ fetchImpl: async () => new Response('private'.repeat(200_000)) });
  assert.equal((await oversized.observe(connection)).backend, 'external-openai');
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(observer.observe(connection, { signal: controller.signal }), { code: 'LOCAL_MODEL_OBSERVATION_CANCELLED' });
});

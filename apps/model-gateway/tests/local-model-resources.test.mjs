import assert from 'node:assert/strict';
import { test } from 'node:test';
import { LocalModelResourceObserver } from '../models/local-model-resources.mjs';
import { planExternalModelDemand } from '../models/external-model-demand.mjs';

const connection = { providerId: 'ollama-local', baseUrl: 'http://127.0.0.1:11434/v1',
  models: ['qwen3:8b'], apiKey: 'synthetic-observation-key' };
const running = { models: [{ name: 'qwen3:8b', model: 'qwen3:8b', size: 5_000_000_000,
  size_vram: 4_000_000_000, context_length: 8192 }] };
const details = { parameters: 'temperature 0.7\nnum_ctx 4096', model_info: { 'general.architecture': 'qwen3',
  'qwen3.block_count': 32, 'qwen3.attention.head_count_kv': 8, 'qwen3.attention.key_length': 128,
  'qwen3.attention.value_length': 128, 'qwen3.context_length': 131072 },
  template: 'Private template is not part of the observation.', modelfile: 'Private model path is not returned.' };

test('Qwen on user-owned Ollama exposes only bounded read-only runtime evidence and honest activity scope', async () => {
  const calls = [];
  const observer = new LocalModelResourceObserver({ fetchImpl: async (url, options) => {
    calls.push({ path: url.pathname, method: options.method, options });
    return new Response(JSON.stringify(url.pathname === '/api/ps' ? running : details));
  } });
  const release = observer.beginGeneration(connection);
  const observed = await observer.observe(connection, { contextTokens: 16384 });
  assert.equal(observed.loaded, true);
  assert.equal(observed.contextTokens, 8192, 'actual loaded context wins over the model theoretical maximum');
  assert.equal(observed.modelMaximumContextTokens, 131072);
  assert.equal(observed.generationState, 'generating');
  assert.equal(observed.globalGenerationState, 'unknown');
  assert.equal(observed.observedGpuMemoryBytes, 4_000_000_000);
  assert.equal(observed.estimatedKvCacheBytes, 16384 * 32 * 8 * 256 * 2);
  assert.equal(observed.observationOnly, true);
  assert.equal(observed.memoryOwnership, 'external');
  assert.deepEqual(calls.map(call => [call.path, call.method]).sort(), [['/api/ps', 'GET'], ['/api/show', 'POST']]);
  assert.equal(calls[1].options.redirect, 'error');
  const serialized = JSON.stringify(observed);
  assert.ok(!serialized.includes(connection.apiKey) && !serialized.includes(details.modelfile) && !serialized.includes(details.template));
  release(); release();
  const idle = await observer.observe(connection, { contextTokens: 16384 });
  assert.equal(calls.length, 2, 'short-lived cache avoids redundant metadata requests');
  assert.equal(idle.applicationGenerationState, 'idle');
  assert.equal(idle.generationState, 'unknown', 'a different client may still be generating');
});

test('observation separates absent loaded models from unavailable runtime and never invents RAM or GPU usage', async () => {
  const observer = new LocalModelResourceObserver({ cacheTtlMs: 0, fetchImpl: async url =>
    url.pathname === '/api/ps' ? new Response(JSON.stringify({ models: [] })) : new Response('unavailable', { status: 503 }) });
  const value = await observer.observe(connection);
  assert.equal(value.loaded, false);
  assert.equal(value.observedMemoryBytes, null);
  assert.equal(value.observedGpuMemoryBytes, null);
  assert.equal(value.diagnostic.metadataPartial, true);
  const unknown = await observer.observe({ ...connection, providerId: 'qwen-local', baseUrl: 'http://127.0.0.1:12345/v1' });
  assert.equal(unknown.loaded, null);
  assert.equal(unknown.diagnostic.code, 'LOCAL_MODEL_RESOURCE_API_UNAVAILABLE');
});

test('cloud, LAN and credential-bearing URLs do not enter the local observation transport', async () => {
  let calls = 0;
  const observer = new LocalModelResourceObserver({ fetchImpl: () => { calls++; throw new Error('No request permitted.'); } });
  for (const baseUrl of ['https://api.example.com/v1', 'http://192.168.1.2:11434', 'http://user:password@localhost:11434']) {
    const value = await observer.observe({ ...connection, baseUrl });
    assert.equal(value.diagnostic.code, 'LOCAL_MODEL_OBSERVATION_NOT_LOCAL');
  }
  assert.equal(calls, 0);
});

test('timeouts and explicit cancellation are distinct; private remote error bodies never enter diagnostics', async () => {
  const observer = new LocalModelResourceObserver({ timeoutMs: 50, fetchImpl: (url, { signal }) => new Promise((resolve, reject) => {
    signal.addEventListener('abort', () => reject(Object.assign(new Error('private server error'), { name: 'AbortError' })), { once: true });
  }) });
  const value = await observer.observe(connection);
  assert.equal(value.loaded, null);
  assert.equal(value.diagnostic.code, 'LOCAL_MODEL_OBSERVATION_TIMEOUT');
  assert.ok(!JSON.stringify(value).includes('private server error'));
  const controller = new AbortController();
  const pending = observer.observe(connection, { signal: controller.signal, refresh: true });
  controller.abort();
  await assert.rejects(pending, { code: 'LOCAL_MODEL_OBSERVATION_CANCELLED', name: 'AbortError' });
});

test('untrusted metadata responses are bounded before decoding and oversized response contents stay private', async () => {
  const observer = new LocalModelResourceObserver({ fetchImpl: async () => new Response('private'.repeat(200_000)) });
  const value = await observer.observe(connection);
  assert.equal(value.loaded, null);
  assert.equal(value.diagnostic.code, 'LOCAL_MODEL_OBSERVATION_UNAVAILABLE');
  assert.ok(!JSON.stringify(value).includes('private'));
});

test('read-only model metadata exposes a bounded cold-load estimate without fetching private paths or loading weights', async () => {
  const calls = [];
  const coldDetails = { ...details, details: { quantization_level: 'Q4_K_M' },
    model_info: { ...details.model_info, 'general.parameter_count': 8_200_000_000 } };
  const observer = new LocalModelResourceObserver({ fetchImpl: async (url, options) => {
    calls.push([url.pathname, options.method]);
    return new Response(JSON.stringify(url.pathname === '/api/ps' ? { models: [] } : coldDetails));
  } });
  const value = await observer.observe(connection, { contextTokens: 32768 });
  assert.equal(value.loaded, false);
  assert.equal(value.runtimeContextTokens, null);
  assert.equal(value.kvBytesPerToken, 32 * 8 * 256 * 2);
  assert.equal(value.parameterCount, 8_200_000_000);
  assert.equal(value.estimatedWeightBytes, Math.ceil(8_200_000_000 * 5.5 / 8));
  assert.equal(value.weightEstimate.exact, false);
  assert.equal(value.weightEstimate.includesRuntimeScratch, false);
  assert.deepEqual(calls.sort(), [['/api/ps', 'GET'], ['/api/show', 'POST'], ['/api/tags', 'GET']]);
  assert.ok(!JSON.stringify(value).includes(coldDetails.modelfile));
});

test('unrecognized quantization and malformed parameter metadata remain unknown estimates', async () => {
  const observer = new LocalModelResourceObserver({ cacheTtlMs: 0, fetchImpl: async url =>
    new Response(JSON.stringify(url.pathname === '/api/ps' ? running : { ...details,
      details: { quantization_level: 'future_private_layout' },
      model_info: { ...details.model_info, 'general.parameter_count': Number.MAX_SAFE_INTEGER } })) });
  const value = await observer.observe(connection);
  assert.equal(value.estimatedWeightBytes, null);
  assert.equal(value.weightEstimate, null);
  assert.equal(value.observedGpuMemoryBytes, 4_000_000_000);
});

test('cold serialized weights avoid duplicated quantization padding while retaining the runtime uncertainty margin', async () => {
  const serializedBytes = 5_225_374_496;
  const coldDetails = { ...details, parameters: '', details: { quantization_level: 'Q4_K_M' },
    model_info: { ...details.model_info, 'general.parameter_count': 8_190_735_360 } };
  const calls = [];
  const observer = new LocalModelResourceObserver({ fetchImpl: async (url, options) => {
    calls.push([url.pathname, options.method]);
    if (url.pathname === '/api/ps') return new Response(JSON.stringify({ models: [] }));
    if (url.pathname === '/api/show') return new Response(JSON.stringify(coldDetails));
    return new Response(JSON.stringify({ models: [{ name: 'unrelated:8b', size: 9_000_000_000 },
      { name: 'qwen3:8b', size: serializedBytes, privatePath: 'private-model-file' }] }));
  } });
  const snapshot = await observer.observe(connection);
  assert.equal(snapshot.modelFileBytes, serializedBytes);
  assert.equal(snapshot.estimatedWeightBytes, serializedBytes);
  assert.equal(snapshot.weightEstimate.source, 'serialized-size');
  assert.equal(snapshot.weightEstimate.includesQuantizationMetadata, true);
  assert.equal(snapshot.weightEstimate.includesRuntimeScratch, false);
  assert.equal(snapshot.weightEstimate.exact, false, 'serialized model bytes are not an exact VRAM ownership receipt');
  const plan = planExternalModelDemand(snapshot, { hardware: { gpu: { state: 'available' } } });
  assert.equal(plan.state, 'partial');
  assert.equal(plan.gpuMemoryBytes, serializedBytes + Math.ceil(serializedBytes * 0.2));
  assert.equal(plan.breakdown.kvIncrementBytes, null);
  assert.ok(plan.gpuMemoryBytes < 6_736_273_408);
  assert.ok(!JSON.stringify(snapshot).includes('private-model-file'));
  assert.deepEqual(calls.sort(), [['/api/ps', 'GET'], ['/api/show', 'POST'], ['/api/tags', 'GET']]);
});

test('bounded tag failures, unrelated names and excess lists retain parameter estimates rather than fabricating sizes', async () => {
  const coldDetails = { ...details, details: { quantization_level: 'Q4_K_M' },
    model_info: { ...details.model_info, 'general.parameter_count': 8_200_000_000 } };
  const variants = [() => new Response('private failure', { status: 404 }),
    () => new Response(JSON.stringify({ models: [{ name: 'unrelated:8b', size: 123456 }] })),
    () => new Response(JSON.stringify({ models: Array.from({ length: 129 }, () => ({ name: 'qwen3:8b', size: 123456 })) })),
    () => new Response('private'.repeat(200_000))];
  for (const tagsResponse of variants) {
    const observer = new LocalModelResourceObserver({ fetchImpl: async url =>
      url.pathname === '/api/ps' ? new Response(JSON.stringify({ models: [] })) :
        url.pathname === '/api/show' ? new Response(JSON.stringify(coldDetails)) : tagsResponse() });
    const value = await observer.observe(connection);
    assert.equal(value.modelFileBytes, null);
    assert.equal(value.estimatedWeightBytes, Math.ceil(8_200_000_000 * 5.5 / 8));
    assert.equal(value.weightEstimate.source, 'parameter-count-and-quantization');
    assert.ok(!JSON.stringify(value).includes('private failure'));
  }
});

test('default model aliases match the serialized latest tag while loaded models never reuse ps size as weights', async () => {
  const paths = [];
  const observer = new LocalModelResourceObserver({ fetchImpl: async url => {
    paths.push(url.pathname);
    return new Response(JSON.stringify(url.pathname === '/api/ps' ? { models: [] } :
      url.pathname === '/api/show' ? details : { models: [{ model: 'qwen3:latest', size: 1_000_000_000 }] }));
  } });
  const cold = await observer.observe({ ...connection, models: ['qwen3'] });
  assert.equal(cold.modelFileBytes, 1_000_000_000);
  const warmObserver = new LocalModelResourceObserver({ fetchImpl: async url => {
    assert.notEqual(url.pathname, '/api/tags');
    return new Response(JSON.stringify(url.pathname === '/api/ps' ? running : details));
  } });
  const warm = await warmObserver.observe(connection);
  assert.equal(warm.modelFileBytes, null);
  assert.equal(warm.estimatedWeightBytes, null);
  assert.equal(warm.observedMemoryBytes, 5_000_000_000);
  assert.ok(paths.includes('/api/tags'));
});

test('optional tag timeouts retain weight fallback while explicit cancellation still rejects observation', async () => {
  let tagCalls = 0;
  let signalSecondTagStart;
  const secondTagStarted = new Promise(resolve => { signalSecondTagStart = resolve; });
  const observer = new LocalModelResourceObserver({ timeoutMs: 50, cacheTtlMs: 0,
    fetchImpl: async (url, { signal }) => {
      if (url.pathname === '/api/ps') return new Response(JSON.stringify({ models: [] }));
      if (url.pathname === '/api/show') return new Response(JSON.stringify({ ...details,
        details: { quantization_level: 'Q4_K_M' },
        model_info: { ...details.model_info, 'general.parameter_count': 8_200_000_000 } }));
      if (++tagCalls === 2) signalSecondTagStart();
      return new Promise((resolve, reject) => signal.addEventListener('abort', () =>
        reject(Object.assign(new Error('private tag failure'), { name: 'AbortError' })), { once: true }));
    } });
  const fallback = await observer.observe(connection);
  assert.equal(fallback.weightEstimate.source, 'parameter-count-and-quantization');
  assert.equal(fallback.modelFileBytes, null);
  const controller = new AbortController();
  const cancelled = observer.observe(connection, { signal: controller.signal, refresh: true });
  await secondTagStarted;
  controller.abort();
  await assert.rejects(cancelled, { name: 'AbortError', code: 'LOCAL_MODEL_OBSERVATION_CANCELLED' });
});

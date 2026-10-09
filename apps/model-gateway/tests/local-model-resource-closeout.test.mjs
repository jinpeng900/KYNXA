import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { test } from 'node:test';
import { LocalModelResourceObserver } from '../models/local-model-resources.mjs';
import { planExternalModelDemand } from '../models/external-model-demand.mjs';
import { ExternalModelAdmission } from '../orchestration/external-model-admission.mjs';

const MIB = 1024 ** 2;
const details = { parameters: 'num_ctx 8192', details: { quantization_level: 'Q4_K_M' },
  model_info: { 'general.architecture': 'qwen3', 'general.parameter_count': 1_000_000_000,
    'qwen3.block_count': 32, 'qwen3.attention.head_count_kv': 8,
    'qwen3.attention.key_length': 128, 'qwen3.attention.value_length': 128, 'qwen3.context_length': 131072 } };

// Only an ephemeral synthetic HTTP service is opened; all metadata and resource receipts are test-owned.
// 仅启动临时合成 HTTP 服务；所有模型元数据与资源回执均由测试拥有，不连接用户 Ollama。
async function fixture(t, { loaded = false, gpuBytes = 0, configuredCpu = false } = {}) {
  const calls = [], events = [];
  const server = createServer(async (request, response) => {
    let body = ''; for await (const chunk of request) body += chunk;
    calls.push({ path: request.url, method: request.method, body });
    response.setHeader('Content-Type', 'application/json');
    const show = { ...details, parameters: configuredCpu ? 'num_ctx 8192\nnum_gpu 0' : details.parameters };
    response.end(JSON.stringify(request.url === '/api/ps' ? { models: loaded ? [{ name: 'qwen-synthetic',
      size: 768 * MIB, size_vram: gpuBytes, context_length: 8192, runner: 'llama.cpp',
      kv_cache_type: 'q4_0', active_requests: 0 }] : [] } : request.url === '/api/show' ? show :
      { models: [{ name: 'qwen-synthetic', size: 512 * MIB }] }));
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(() => { const closed = new Promise(resolve => server.close(resolve)); server.closeAllConnections?.(); return closed; });
  const connection = { providerId: 'ollama', baseUrl: `http://127.0.0.1:${server.address().port}/v1`, models: ['qwen-synthetic'] };
  const observer = new LocalModelResourceObserver();
  const requests = [], released = [];
  const resources = { async snapshot() { return { gpu: { state: 'available', mappingStatus: 'verified' } }; },
    async acquire(request) { events.push('admission'); requests.push(request); return { status: 'granted', mode: 'rust',
      leaseId: `synthetic-${requests.length}`, ...request }; },
    async renew() { return { status: 'renewed', mode: 'rust' }; }, async release(id) { released.push(id); } };
  const admission = new ExternalModelAdmission({ resources, observer,
    yieldIdleGpu: async () => { events.push('owned-idle-yield'); return { released: true, owner: 'kynxa' }; } });
  t.after(() => admission.close());
  return { calls, events, connection, observer, admission, requests, released };
}

test('fake Ollama cold loading plans nctx and dtype range through real admission before any dispatch', async t => {
  const value = await fixture(t), handle = await value.admission.acquire(value.connection, { modelId: 'qwen-synthetic' });
  assert.equal(handle.snapshot.backendSelection.state, 'unknown');
  assert.equal(handle.snapshot.kvCacheEstimate.dtype, 'unknown');
  assert.equal(handle.snapshot.kvCacheEstimate.reservationDtype, 'f16-upper-bound');
  assert.equal(handle.snapshot.kvCacheEstimate.supportedDtypeEstimates.q8_0BytesPerToken, 32 * 8 * 8 * 34);
  assert.equal(handle.snapshot.kvCacheEstimate.maximumBytes, 8192 * 32 * 8 * 256 * 2);
  assert.ok(handle.snapshot.kvCacheEstimate.minimumBytes < handle.snapshot.kvCacheEstimate.maximumBytes);
  assert.equal(handle.plan.breakdown.kvIncrementBytes, handle.snapshot.kvCacheEstimate.maximumBytes);
  assert.equal(handle.plan.breakdown.weightIncrementBytes, 512 * MIB);
  assert.deepEqual(handle.plan.unknownComponents, ['kv-parallel-slots']);
  assert.equal(handle.plan.uncertainty.state, 'estimated-per-slot-upper-bound');
  assert.ok(handle.plan.uncertainty.reasons.includes('KV_DTYPE_NOT_REPORTED_F16_UPPER_BOUND'));
  assert.deepEqual(value.events, ['owned-idle-yield', 'admission']);
  assert.equal(value.requests.length, 1);
  handle.dispatched(); handle.settled(); await handle.release();
  assert.equal(value.released.length, 1);
  assert.deepEqual(value.calls.map(call => [call.path, call.method]).sort(),
    [['/api/ps', 'GET'], ['/api/show', 'POST'], ['/api/tags', 'GET']]);
});

test('reported mixed residency yields only owned idle GPU and does not reserve resident external memory again', async t => {
  const value = await fixture(t, { loaded: true, gpuBytes: 512 * MIB });
  const stop = value.observer.beginGeneration(value.connection);
  const handle = await value.admission.acquire(value.connection, { modelId: 'qwen-synthetic' });
  assert.equal(handle.snapshot.backendSelection.state, 'mixed');
  assert.equal(handle.snapshot.applicationGenerationState, 'generating');
  assert.equal(handle.snapshot.globalGenerationState, 'unknown', 'undocumented idle fields never certify other clients');
  assert.equal(handle.snapshot.kvCacheEstimate.dtype, 'unknown', 'undocumented dtype fields are not trusted');
  assert.equal(handle.plan.requiresAdmission, false);
  assert.equal(handle.plan.gpuMemoryBytes, 0);
  assert.deepEqual(value.events, ['owned-idle-yield']);
  assert.equal(value.requests.length, 0);
  handle.dispatched(); handle.settled(); await handle.release(); stop();
  assert.equal((await value.observer.observe(value.connection)).applicationGenerationState, 'idle');
  assert.ok(value.calls.every(call => ['/api/ps', '/api/show'].includes(call.path)));
});

test('cold server-configured CPU placement admits host demand without a false GPU claim', async t => {
  const value = await fixture(t, { configuredCpu: true });
  const snapshot = await value.observer.observe(value.connection);
  const plan = planExternalModelDemand(snapshot, { contextTokens: 8192, hardware: { gpu: { state: 'unknown' } } });
  assert.equal(plan.breakdown.placement, 'cpu');
  assert.equal(plan.gpuMemoryBytes, 0);
  assert.ok(plan.memoryBytes > 512 * MIB);
  const handle = await value.admission.acquire(value.connection, { modelId: 'qwen-synthetic' });
  assert.equal(value.requests[0].gpuMemoryBytes, 0);
  assert.ok(value.requests[0].memoryBytes > 512 * MIB);
  assert.equal(value.events.includes('owned-idle-yield'), false);
  await handle.release();
});

test('impossible KV metadata stays unknown instead of overflowing memory admission', async () => {
  const observer = new LocalModelResourceObserver({ fetchImpl: async url => new Response(JSON.stringify(
    url.pathname === '/api/ps' ? { models: [] } : url.pathname === '/api/tags' ? { models: [] } :
      { ...details, model_info: { ...details.model_info, 'qwen3.block_count': Number.MAX_SAFE_INTEGER } })) });
  const snapshot = await observer.observe({ providerId: 'ollama', baseUrl: 'http://127.0.0.1:11434', models: ['qwen-synthetic'] });
  assert.equal(snapshot.estimatedKvCacheBytes, null);
  assert.equal(snapshot.kvBytesPerToken, null);
  assert.equal(snapshot.kvCacheEstimate, null);
  assert.equal(planExternalModelDemand(snapshot, { contextTokens: 8192, hardware: { gpu: { state: 'available' } } }).state, 'unknown');
});

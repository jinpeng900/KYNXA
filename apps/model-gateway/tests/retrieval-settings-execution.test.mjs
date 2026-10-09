import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { Readable } from 'node:stream';
import { test } from 'node:test';
import { DEFAULT_RETRIEVAL_SETTINGS } from '../data/retrieval/settings.mjs';
import { RetrievalCoordinator } from '../orchestration/retrieval/coordinator.mjs';
import { handleRetrievalRoute } from '../orchestration/retrieval/http-routes.mjs';

const CPU_EMBEDDING = 'builtin-multilingual', GPU_EMBEDDING = 'builtin-multilingual-dml-q8';
const CPU_RERANK = 'builtin-multilingual-reranker', GPU_RERANK = 'builtin-multilingual-reranker-dml-q8';

// Exercise application policy with pure inference doubles: no worker, model, gateway or user data is accessed.
// 以纯推理替身验证应用策略；不启动 worker、模型或网关，也不访问用户数据。
function coordinatorFixture(settings = structuredClone(DEFAULT_RETRIEVAL_SETTINGS)) {
  settings.local.rerankProfileId = CPU_RERANK;
  const retrieval = Object.create(RetrievalCoordinator.prototype), calls = [];
  Object.assign(retrieval, { settings, shutdown: new AbortController(), closed: false,
    queue: Promise.resolve(), resources: {}, acquisitions: new WeakMap(), evaluationPolicy: { gaps: false },
    effective: async () => settings, initialize: async () => {},
    embeddings: { configure: async options => calls.push(['embedding-configure', options]),
      retire: async options => calls.push(['embedding-retire', options]),
      status: (profileId, options) => { calls.push(['embedding-status', { profileId, ...options }]);
        return { profileId, state: 'ready', loaded: true, dimensions: 2, modelVersion: 'mock-v1' }; },
      embedQuery: async (query, options) => { calls.push(['embedding-query', options]);
        return { profileId: options.profileId, vector: [1, 0], dimensions: 2, modelVersion: 'mock-v1' }; } },
    reranker: { configure: async options => calls.push(['rerank-configure', options]),
      retire: async options => calls.push(['rerank-retire', options]),
      status: (profileId, options) => { calls.push(['rerank-status', { profileId, ...options }]);
        return { profileId, state: 'ready', modelVersion: 'mock-rerank-v1' }; },
      rerank: async options => { calls.push(['rerank', options]);
        return { profileId: options.profileId, modelVersion: 'mock-rerank-v1',
          items: options.candidates.map((item, index) => ({ sourceRef: item.sourceRef, rerankScore: 1 - index / 10 })) }; } },
    spacePolicy: { target: async effective => effective.local.embeddingDevicePolicy === 'gpu' ? GPU_EMBEDDING : CPU_EMBEDDING,
      select: async () => ({ profileId: CPU_EMBEDDING, targetProfileId: CPU_EMBEDDING,
        devicePreference: settings.local.embeddingDevicePolicy === 'cpu' ? 'cpu' : 'auto', state: 'ready' }) },
    sourceService: { syncFormalSources: async () => {} },
    index: { search: async () => ({ items: [], strategy: 'lexical' }) },
    _snapshot: async () => ({ settings, scopes: ['user'], relationship: { conversationId: 'synthetic-chat' },
      sources: [{ sourceId: 'synthetic-source', scopeKey: 'user', sourceType: 'knowledge' }] }),
    _assertCurrent: async () => {} });
  return { retrieval, settings, calls };
}

test('settings configure explicit CPU embedding and independent strict GPU rerank, then retire disabled services', async () => {
  const { retrieval, settings, calls } = coordinatorFixture();
  settings.local.embeddingDevicePolicy = 'cpu'; settings.local.rerankDevicePolicy = 'gpu';
  await retrieval.configureInferenceSettings();
  const embedding = calls.find(([kind]) => kind === 'embedding-configure')[1];
  const rerank = calls.find(([kind]) => kind === 'rerank-configure')[1];
  assert.deepEqual([embedding.profileId, embedding.devicePreference, embedding.retireOtherProfiles], [CPU_EMBEDDING, 'cpu', true]);
  assert.deepEqual([rerank.profileId, rerank.devicePreference, rerank.retireOtherProfiles], [GPU_RERANK, 'auto', true]);
  assert.equal(embedding.signal, retrieval.shutdown.signal);
  const previous = structuredClone(settings);
  settings.local.semantic = 'off'; settings.local.rerankProfileId = null;
  await retrieval.configureInferenceSettings(null, { previous });
  assert.equal(calls.filter(([kind]) => kind === 'embedding-retire').length, 1);
  assert.equal(calls.filter(([kind]) => kind === 'rerank-retire').length, 1);
  const count = calls.length;
  await retrieval.configureInferenceSettings(null, { previous: structuredClone(settings) });
  assert.equal(calls.length, count, 'unrelated or unchanged policy cannot rebuild inference sessions');
});

test('status observes the selected execution identity without configuring or changing CPU preference', async () => {
  const { retrieval, settings, calls } = coordinatorFixture();
  settings.local.embeddingDevicePolicy = 'cpu'; settings.local.rerankDevicePolicy = 'gpu';
  Object.assign(retrieval, { index: { status: async () => ({ sources: 0, chunks: 0 }) },
    jobs: { list: async () => [] }, resources: { snapshot: async () => ({}) }, structures: {},
    spacePolicy: { ...retrieval.spacePolicy, status: () => ({}) } });
  await retrieval.status();
  assert.deepEqual(calls[0], ['embedding-status', { profileId: CPU_EMBEDDING, devicePreference: 'cpu' }]);
  assert.ok(calls.some(([kind, options]) => kind === 'rerank-status' && options.profileId === GPU_RERANK));
  assert.ok(calls.every(([kind]) => kind.endsWith('status')));
});

test('rerank policy sends an exact GPU profile independently from CPU embedding', async () => {
  const { retrieval, settings, calls } = coordinatorFixture();
  settings.local.embeddingDevicePolicy = 'cpu'; settings.local.rerankDevicePolicy = 'gpu';
  const candidates = [{ sourceRef: 'mock:1' }, { sourceRef: 'mock:2' }];
  const result = await retrieval._rerank({}, 'synthetic query', candidates, { settings }, 'research',
    retrieval.shutdown.signal, { shouldRerank: true });
  const request = calls.find(([kind]) => kind === 'rerank')[1];
  assert.deepEqual([request.profileId, request.devicePreference], [GPU_RERANK, 'auto']);
  assert.equal(result.rerank.scoredCandidates, 2);
  settings.local.rerankDevicePolicy = 'cpu'; settings.local.rerankProfileId = GPU_RERANK;
  await retrieval._rerank({}, 'synthetic query', candidates, { settings }, 'research',
    retrieval.shutdown.signal, { shouldRerank: true });
  assert.equal(calls.at(-1)[1].profileId, CPU_RERANK);
  assert.equal(calls.at(-1)[1].devicePreference, 'cpu');
});

test('semantic query sends CPU preference, including compatible fallback from a GPU target', async () => {
  for (const fallback of [false, true]) {
    const { retrieval, settings, calls } = coordinatorFixture();
    settings.local.embeddingDevicePolicy = fallback ? 'auto' : 'cpu';
    if (fallback) retrieval.spacePolicy.select = async () => ({ profileId: GPU_EMBEDDING, targetProfileId: GPU_EMBEDDING,
      devicePreference: 'auto', fallbackProfileId: CPU_EMBEDDING, state: 'ready' });
    const original = retrieval.embeddings.embedQuery;
    retrieval.embeddings.embedQuery = async (query, options) => {
      if (options.profileId === GPU_EMBEDDING) { calls.push(['embedding-query', options]);
        throw Object.assign(new Error('Synthetic GPU capacity'), { code: 'RESOURCE_GPU_CAPACITY' }); }
      return original(query, options);
    };
    await retrieval.search({ conversationId: 'synthetic-chat' }, { query: 'synthetic natural language query' });
    const cpu = calls.filter(([kind, options]) => kind === 'embedding-query' && options.profileId === CPU_EMBEDDING);
    assert.equal(cpu.length, 1); assert.equal(cpu[0][1].devicePreference, 'cpu');
  }
});

async function patchRoute(path, retrieval) {
  const request = Readable.from([Buffer.from(JSON.stringify({ expectedRevision: 0, patch: {} }))]);
  request.method = 'PATCH';
  const response = new EventEmitter(); response.writableEnded = false;
  response.writeHead = code => assert.equal(code, 200);
  response.end = body => { response.body = JSON.parse(body); response.writableEnded = true; response.emit('close'); };
  assert.equal(await handleRetrievalRoute(request, response, new URL(path, 'http://synthetic.invalid'), retrieval), true);
  return response.body;
}

test('global PATCH waits for the settings execution hook without creating a chat or resetting MCP', async () => {
  const previous = structuredClone(DEFAULT_RETRIEVAL_SETTINGS), events = [];
  const retrieval = { effective: async () => previous,
    settings: { patchGlobal: async () => { events.push('persist'); return { revision: 1 }; } },
    configureInferenceSettings: async (projectId, options) => {
      events.push('configure'); assert.equal(projectId, null); assert.equal(options.previous, previous);
      assert.equal(options.signal, undefined);
    } };
  const response = await patchRoute('/api/retrieval/settings', retrieval);
  assert.deepEqual(events, ['persist', 'configure']); assert.equal(response.revision, 1);
});

test('project PATCH schedules dirty rebuild for unmount and knowledge selection changes', async () => {
  for (const selection of ['unmount', 'knowledge']) {
    const previous = structuredClone(DEFAULT_RETRIEVAL_SETTINGS), effective = structuredClone(previous), events = [];
    previous.projectIndexing = { mountedFolder: selection === 'unmount', bindingRevision: 1, knowledgeIds: ['old'] };
    effective.projectIndexing = { mountedFolder: false, bindingRevision: selection === 'unmount' ? 2 : 1,
      knowledgeIds: selection === 'knowledge' ? ['new'] : ['old'] };
    let persisted = false;
    const retrieval = { effective: async () => persisted ? effective : previous,
      settings: { patchProject: async () => { persisted = true; events.push('persist'); return { revision: 1 }; } },
      configureInferenceSettings: async (projectId, options) => {
        assert.equal(projectId, 'synthetic-work'); assert.equal(options.previous, previous); events.push('configure'); },
      rebuild: async options => { assert.deepEqual(options, { projectId: 'synthetic-work', dirty: true }); events.push('rebuild'); } };
    await patchRoute('/api/projects/synthetic-work/retrieval/settings', retrieval);
    assert.deepEqual(events, ['persist', 'configure', 'rebuild']);
  }
});

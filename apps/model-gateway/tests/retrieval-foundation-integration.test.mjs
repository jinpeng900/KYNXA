import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { EventEmitter } from 'node:events';
import { Readable } from 'node:stream';
import { test } from 'node:test';
import { RetrievalCoordinator } from '../orchestration/retrieval/coordinator.mjs';
import { handleRetrievalRoute } from '../orchestration/retrieval/http-routes.mjs';
import { retrievalPlan } from '../orchestration/retrieval/query-plan.mjs';
import { projectConversationSources } from '../orchestration/retrieval/source-projection.mjs';
import { RetrievalIndex } from '../data/retrieval/index.mjs';
import { resolveRetrievalModelProfile } from '../models/retrieval/model-registry.mjs';
import { toolFixture } from './tool-fixture.mjs';

async function foundationFixture(t, embeddings) {
  const f = await toolFixture(t);
  const entry = { id: randomUUID(), scope: 'user', status: 'confirmed', active: true,
    kind: 'CALYX reference', revision: 1, content: 'CALYX architecture keeps citations bound to current source versions.' };
  const retrieval = new RetrievalCoordinator({ conversations: f.conversations, tools: f.service,
    memory: { contextFor: async () => ({ entries: [entry] }) }, reranker: null,
    embeddings: embeddings ?? { status: () => ({ state: 'unavailable', profileId: 'builtin-multilingual' }),
      close: async () => {} } });
  f.service.retrieval = retrieval;
  t.after(() => retrieval.close());
  await retrieval.settings.patchGlobal({ expectedRevision: 0, patch: { local: { semantic: 'off' } } });
  const context = await f.context('full');
  return { ...f, retrieval, entry, context };
}

test('query plans keep natural-language domains soft and exhaustive navigation distinct from bounded search', () => {
  assert.equal(retrievalPlan('你好').shouldRetrieve, false);
  assert.equal(retrievalPlan('Explain coordinator.mjs cancellation').domain, 'mixed');
  assert.equal(retrievalPlan('Explain coordinator.mjs cancellation').preferredDomain, 'code');
  assert.equal(retrievalPlan('比较论文和项目代码实现').domain, 'mixed');
  assert.equal(retrievalPlan('总结文档中的条件').domain, 'mixed');
  assert.equal(retrievalPlan('总结文档中的条件').preferredDomain, 'knowledge');
  const references = retrievalPlan('列出该函数所有引用');
  assert.equal(references.operation, 'references');
  assert.equal(references.operationSupported, false);
  assert.deepEqual(references.supportedOperations, ['search', 'read', 'indexed-definition']);
  assert.equal(retrievalPlan('查找函数定义').operationSupported, true);
});

test('direct source read without a search registers final dependencies and rejects a changed source at final validation', async t => {
  const f = await foundationFixture(t);
  const relationship = await f.conversations.describeConversation(f.conversationId);
  const source = () => projectConversationSources(relationship, [f.entry], [])[0];
  await f.retrieval.index.upsertSources([source()]);
  const reference = (await f.retrieval.index.search({ query: 'CALYX', scopeKeys: ['user'] })).items[0].sourceRef;
  assert.equal(f.retrieval.acquisitions.get(f.context), undefined);
  const read = await f.retrieval.read(f.context, { sourceRef: reference });
  assert.equal(read.evidenceDecision.contentRead, true);
  assert.equal(f.retrieval.acquisitions.get(f.context).finalSourceDependencies.size, 1);
  const current = await f.retrieval.validateFinal(f.context);
  assert.equal(current.current, true); assert.equal(current.checked, 1);
  f.entry.content = 'CALYX source changed after the model read its earlier version.';
  f.entry.revision++;
  await f.retrieval.index.upsertSources([source()]);
  const stale = await f.retrieval.validateFinal(f.context);
  assert.equal(stale.current, false); assert.equal(stale.checked, 1);
  assert.equal(stale.invalidSources[0].sourceId, source().sourceId);
  assert.equal(stale.invalidSources[0].code, 'STALE_RETRIEVAL_SOURCE');
});

test('search-only excerpts register final version dependencies without becoming read support', async t => {
  const f = await foundationFixture(t);
  const result = await f.retrieval.search(f.context, { query: 'CALYX architecture' });
  assert.ok(result.items.length);
  const acquisition = f.retrieval.acquisitions.get(f.context);
  assert.equal(acquisition.sourceReads.size, 0);
  assert.equal(acquisition.finalSourceDependencies.size, new Set(result.items.map(item => item.sourceId)).size);
  const current = await f.retrieval.validateFinal(f.context);
  assert.equal(current.current, true); assert.equal(current.checked, acquisition.finalSourceDependencies.size);
  f.entry.content = 'CALYX changed after the search excerpt was projected.'; f.entry.revision++;
  const relationship = await f.conversations.describeConversation(f.conversationId);
  await f.retrieval.index.upsertSources([projectConversationSources(relationship, [f.entry], [])[0]]);
  const stale = await f.retrieval.validateFinal(f.context);
  assert.equal(stale.current, false);
  assert.ok(stale.invalidSources.some(item => item.code === 'STALE_RETRIEVAL_SOURCE'));
});

test('a 128-source internal pool registers only the five returned source dependencies, including cache reuse', async t => {
  const f = await foundationFixture(t), entries = Array.from({ length: 128 }, (_, index) => ({ ...f.entry,
    id: randomUUID(), content: `CALYX architecture source item ${index}: distinct factual condition ${index * 17}.` }));
  f.retrieval.memory.contextFor = async () => ({ entries });
  const search = f.retrieval.index.search.bind(f.retrieval.index);
  let internalCandidates = 0;
  f.retrieval.index.search = async input => {
    const result = await search({ ...input, limit: 128, channelCandidates: 128 });
    internalCandidates = result.items.length;
    return result;
  };
  const result = await f.retrieval.search(f.context, { query: 'CALYX architecture', limit: 5, maximumTokens: 32768 });
  assert.equal(internalCandidates, 128); assert.equal(result.items.length, 5);
  const acquisition = f.retrieval.acquisitions.get(f.context);
  const expected = result.items.map(item => item.sourceId).sort();
  assert.deepEqual([...acquisition.finalSourceDependencies.values()].map(item => item.sourceId).sort(), expected);
  assert.equal(acquisition.sourceReads.size, 0);
  const cached = await f.retrieval.search(f.context, { query: 'CALYX architecture', limit: 5, maximumTokens: 32768 });
  assert.equal(cached.acquisition.reused, true);
  assert.deepEqual([...acquisition.finalSourceDependencies.values()].map(item => item.sourceId).sort(), expected);
});

test('unsupported selected embeddings preserve lexical evidence without invoking a substituted model', async t => {
  let calls = 0;
  const f = await foundationFixture(t, { status: () => ({ state: 'ready', profileId: 'builtin-multilingual' }),
    embedQuery: async () => { calls++; throw new Error('The wrong model must never be called.'); }, close: async () => {} });
  await f.retrieval.settings.patchGlobal({ expectedRevision: 1,
    patch: { local: { semantic: 'auto', embeddingProfileId: 'unimplemented-profile' } } });
  const result = await f.retrieval.search(f.context, { query: 'CALYX architecture' });
  assert.equal(calls, 0);
  assert.equal(result.embeddingDiagnostic, 'RETRIEVAL_MODEL_PROFILE_UNSUPPORTED');
  assert.ok(result.items.length);
  assert.deepEqual(result.evidenceState, { authorization: 'checked', freshness: 'current', conclusion: 'not-verified' });
  assert.equal(result.coverage.complete, false);
});

test('a result from a different model space cannot enter the vector query', async t => {
  const f = await foundationFixture(t, {
    status: () => ({ state: 'ready', profileId: 'builtin-multilingual', embeddingSpaceId: 'a'.repeat(64), modelVersion: 'fixture-v1' }),
    embedQuery: async () => ({ profileId: 'builtin-multilingual', embeddingSpaceId: 'b'.repeat(64),
      modelVersion: 'fixture-v1', vector: [1, 0] }), close: async () => {} });
  await f.retrieval.settings.patchGlobal({ expectedRevision: 1, patch: { local: { semantic: 'auto' } } });
  const search = f.retrieval.index.search.bind(f.retrieval.index);
  f.retrieval.index.search = input => { assert.equal(input.queryVector, undefined); return search(input); };
  const result = await f.retrieval.search(f.context, { query: 'CALYX architecture' });
  assert.equal(result.embeddingDiagnostic, 'RETRIEVAL_MODEL_SPACE_MISMATCH');
  assert.ok(result.items.length);
});

test('declared and registered query dimensions are checked before entering the vector index', async t => {
  const profile = resolveRetrievalModelProfile('embedding', 'builtin-multilingual');
  for (const mode of ['declared-result', 'declared-status', 'actual-vector', 'registered-space', 'registered-model']) {
    await t.test(mode, async child => {
      const registered = ['registered-space', 'registered-model'].includes(mode);
      const space = mode === 'registered-model' ? {} : { embeddingSpaceId: profile.embeddingSpaceId };
      const f = await foundationFixture(child, {
        status: () => ({ state: 'ready', profileId: profile.id, ...space,
          modelVersion: profile.modelVersion, ...(registered ? {} : { dimensions: mode === 'declared-status' ? 2 : 384 }) }),
        embedQuery: async () => ({ profileId: profile.id, ...space,
          modelVersion: profile.modelVersion, ...(registered ? {} : { dimensions: mode === 'declared-result' ? 2 : 384 }),
          vector: mode === 'declared-result' ? Array(384).fill(0.1) : [1, 0] }), close: async () => {} });
      await f.retrieval.settings.patchGlobal({ expectedRevision: 1, patch: { local: { semantic: 'auto' } } });
      const search = f.retrieval.index.search.bind(f.retrieval.index);
      f.retrieval.index.search = input => { assert.equal(input.queryVector, undefined); return search(input); };
      const result = await f.retrieval.search(f.context, { query: 'CALYX architecture' });
      assert.equal(result.embeddingDiagnostic, 'RETRIEVAL_MODEL_SPACE_MISMATCH');
      assert.ok(result.items.length, 'lexical evidence remains available after rejection');
    });
  }
});

test('reranking rejects contradictory model metadata and keeps the original candidate order', async () => {
  const candidates = [{ sourceRef: 'first', excerpt: 'first public source' }, { sourceRef: 'second', excerpt: 'second public source' }];
  const selected = { profileId: 'builtin-multilingual-reranker', modelVersion: 'expected-model-v1', inputProjectionVersion: 'expected-projection-v1' };
  for (const field of ['profileId', 'modelVersion', 'inputProjectionVersion']) {
    const result = await RetrievalCoordinator.prototype._rerank.call({ reranker: {
      status: () => ({ state: 'ready', ...selected }), rerank: async () => ({ ...selected,
        [field]: 'another-model-value', items: candidates.toReversed().map(item => ({ ...item, rerankScore: 0.8 })) })
    } }, {}, 'query', candidates, { settings: { local: { rerankProfileId: selected.profileId } } }, 'complex', undefined,
    { shouldRerank: true });
    assert.equal(result.diagnostic, 'RERANK_PROFILE_MISMATCH', field);
    assert.deepEqual(result.items, candidates);
    assert.equal(result.rerank, undefined);
  }
});

test('legacy reranker seams may omit optional model metadata while retaining verified source content', async () => {
  const candidates = [{ sourceRef: 'first', excerpt: 'first public source' }, { sourceRef: 'second', excerpt: 'second public source' }];
  const profileId = 'builtin-multilingual-reranker';
  const result = await RetrievalCoordinator.prototype._rerank.call({ reranker: {
    status: () => ({ state: 'ready', profileId }), rerank: async () => ({
      items: candidates.toReversed().map(item => ({ ...item, rerankScore: 0.8 })) })
  } }, {}, 'query', candidates, { settings: { local: { rerankProfileId: profileId } } }, 'complex', undefined,
  { shouldRerank: true });
  assert.equal(result.diagnostic, undefined);
  assert.deepEqual(result.items.map(item => item.sourceRef), ['second', 'first']);
  assert.equal(result.items[0].excerpt, candidates[1].excerpt);
});

test('same-text rederivation invalidates a prepared evidence handle before archive publication', async t => {
  const f = await foundationFixture(t);
  const draft = await f.retrieval.evidence(f.context, 'CALYX资料架构', { deferArchive: true });
  assert.ok(draft.prepared);
  const relationship = await f.conversations.describeConversation(f.conversationId);
  const source = projectConversationSources(relationship, [f.entry], [])[0];
  await f.retrieval.index.upsertSources([{ ...source, parserVersion: 'test-parser-v2' }]);
  let archives = 0;
  const save = f.service.results.save.bind(f.service.results);
  f.service.results.save = (...args) => { archives++; return save(...args); };
  const final = await f.retrieval.finalizeEvidence(f.context, draft);
  assert.equal(final.prompt, '');
  assert.deepEqual(final.references, []);
  assert.equal(archives, 0);
});

test('cancellation after evidence commit retains its receipt and consumes the handle exactly once', async t => {
  const f = await foundationFixture(t), controller = new AbortController();
  const draft = await f.retrieval.evidence(f.context, 'CALYX资料架构', { deferArchive: true });
  assert.ok(draft.prepared);
  let archives = 0;
  const save = f.service.results.save.bind(f.service.results);
  f.service.results.save = async (...args) => {
    archives++;
    const receipt = await save(...args);
    controller.abort();
    return receipt;
  };
  const final = await f.retrieval.finalizeEvidence(f.context, draft, { signal: controller.signal });
  assert.ok(final.resultRef);
  assert.equal(archives, 1);
  await assert.rejects(f.retrieval.finalizeEvidence(f.context, draft), { code: 'RETRIEVAL_PREPARATION_INVALID' });
  assert.equal(archives, 1);
});

test('shutdown cancels queued retrieval work and drains the index while keeping formal messages', async t => {
  let entered, executions = 0;
  const ready = new Promise(resolve => { entered = resolve; });
  const f = await foundationFixture(t, { status: () => ({ state: 'ready', profileId: 'builtin-multilingual' }),
    embedQuery: async (_query, { signal }) => {
      executions++; entered();
      return new Promise((_resolve, reject) => signal.addEventListener('abort', () =>
        reject(Object.assign(new Error('Synthetic cancellation.'), { name: 'AbortError' })), { once: true }));
    }, close: async () => {} });
  await f.retrieval.settings.patchGlobal({ expectedRevision: 1, patch: { local: { semantic: 'auto' } } });
  const before = await f.conversations.readMessages(f.conversationId);
  const first = f.retrieval.search(f.context, { query: 'CALYX architecture' });
  const firstStopped = assert.rejects(first, { name: 'AbortError' });
  await ready;
  const second = f.retrieval.search(f.context, { query: 'CALYX versions' });
  const secondStopped = assert.rejects(second, error => ['RETRIEVAL_CLOSED', 'ABORT_ERR'].includes(error.code) || error.name === 'AbortError');
  await f.retrieval.close();
  await Promise.all([firstStopped, secondStopped]);
  assert.equal(executions, 1);
  assert.equal(f.retrieval.index.worker, null);
  assert.deepEqual(await f.conversations.readMessages(f.conversationId), before);
});

test('a disconnected retrieval HTTP response cancels source import and removes request listeners', async () => {
  const request = Readable.from([Buffer.from(JSON.stringify({ path: 'synthetic' }))]);
  request.method = 'POST'; request.url = '/api/retrieval/sources';
  const response = new EventEmitter(); response.writableEnded = false;
  let entered, observed;
  const ready = new Promise(resolve => { entered = resolve; });
  const routing = handleRetrievalRoute(request, response, new URL('http://localhost/api/retrieval/sources'), {
    importSource: async (_input, { signal }) => {
      observed = signal; entered();
      return new Promise((_resolve, reject) => signal.addEventListener('abort', () =>
        reject(Object.assign(new Error('Synthetic disconnect.'), { name: 'AbortError' })), { once: true }));
    }
  });
  const stopped = assert.rejects(routing, { name: 'AbortError' });
  await ready;
  response.emit('close');
  await stopped;
  assert.equal(observed.aborted, true);
  assert.equal(request.listenerCount('aborted'), 0);
  assert.equal(response.listenerCount('close'), 0);
});

test('index shutdown rejects uncommitted queued batches without losing a committed source', async t => {
  const f = await foundationFixture(t);
  const source = projectConversationSources(await f.conversations.describeConversation(f.conversationId), [f.entry], [])[0];
  await f.retrieval.index.upsertSources([source]);
  const pending = f.retrieval.index.upsertSources([{ ...source, sourceId: 'queued-new-source' }]);
  const stopped = assert.rejects(pending, { name: 'AbortError' });
  await f.retrieval.index.close();
  await stopped;
  const reopened = new RetrievalIndex({ root: f.conversations.root });
  try {
    const sources = await reopened.listSources({ scopeKeys: ['user'] });
    assert.equal(sources.length, 1);
    assert.equal(sources[0].sourceId, source.sourceId);
  } finally { await reopened.close(); }
});

test('an unexpected owned index worker exit settles shutdown and pending calls', async t => {
  const f = await foundationFixture(t);
  const pending = f.retrieval.index.status();
  const stopped = assert.rejects(pending, { code: 'RETRIEVAL_WORKER_EXITED' });
  const closing = f.retrieval.index.close();
  const closeFailed = assert.rejects(closing, { code: 'RETRIEVAL_WORKER_EXITED' });
  await f.retrieval.index.worker.terminate();
  await Promise.all([stopped, closeFailed]);
  assert.equal(f.retrieval.index.worker, null);
});

test('query vectors must remain finite and nonzero in their stored float32 format', async t => {
  const f = await foundationFixture(t);
  for (const queryVector of [[Number.MAX_VALUE, 1], [1e-100, 0], 1000000000])
    await assert.rejects(f.retrieval.index.search({ query: 'CALYX', scopeKeys: ['user'],
      embeddingProfileId: 'fixture', queryVector }), { code: 'INVALID_RETRIEVAL_INPUT' });
  assert.equal(f.retrieval.index.worker, null);
});

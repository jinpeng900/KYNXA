import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from 'node:test';
import { RetrievalCoordinator } from '../orchestration/retrieval/coordinator.mjs';
import { toolFixture } from './tool-fixture.mjs';

function memoryEntry(content, title = 'CALYX release notes') {
  return { id: randomUUID(), scope: 'user', status: 'confirmed', active: true,
    kind: title, revision: 1, content };
}

async function acquisitionFixture(t, { entries = [], message = '', reranker = null } = {}) {
  const fixture = await toolFixture(t);
  const retrieval = new RetrievalCoordinator({ conversations: fixture.conversations,
    memory: { contextFor: async () => ({ entries }) }, tools: fixture.service, reranker,
    embeddings: { status: () => ({ state: 'unavailable' }), close: async () => {} } });
  fixture.service.retrieval = retrieval;
  await retrieval.settings.patchGlobal({ expectedRevision: 0, patch: { local: { semantic: 'off' } } });
  const context = await fixture.service.createContext(fixture.conversationId,
    { requestId: randomUUID(), permissionMode: 'smart', message });
  await fixture.service.catalog(context);
  let searchCount = 0;
  const search = retrieval.index.search.bind(retrieval.index);
  retrieval.index.search = (...args) => { searchCount++; return search(...args); };
  // Canonical receipts stay authoritative even when the model receives short reference handles.
  // 模型投影可使用短引用；断言仍读取正式公开结果归档中的原始回执。
  const execute = async (name, argumentsValue) => {
    const receipt = await fixture.run(context, name, argumentsValue);
    assert.equal(receipt.isError, false, receipt.content);
    assert.equal(receipt.status, 'completed');
    assert.ok(receipt.resultRef, 'each successful call has its own durable receipt');
    const canonical = await fixture.service.results.get(context, receipt.resultRef.id);
    assert.equal(canonical.isError, false);
    assert.ok(canonical.structuredContent);
    return { receipt, value: canonical.structuredContent };
  };
  return { ...fixture, retrieval, context, entries, execute, searchCount: () => searchCount };
}

test('simultaneous same-gap searches perform one index search and keep independent durable tool results', async t => {
  const fixture = await acquisitionFixture(t, { entries: [memoryEntry('CALYX release date is 2031-02-07.')] });
  const indexSearch = fixture.retrieval.index.search.bind(fixture.retrieval.index);
  let enterSearch, releaseSearch;
  const entered = new Promise(resolveEntered => { enterSearch = resolveEntered; });
  const pendingSearch = new Promise(resolveSearch => { releaseSearch = resolveSearch; });
  fixture.retrieval.index.search = async (...args) => {
    enterSearch(); await pendingSearch; return indexSearch(...args);
  };
  const input = { query: 'CALYX release date', gap: 'The exact CALYX release date' };
  const first = fixture.execute('knowledge.search', input);
  const second = fixture.execute('knowledge.search', input);
  try { await entered; } finally { releaseSearch(); }
  const [initial, repeated] = await Promise.all([first, second]);
  assert.equal(fixture.searchCount(), 1);
  assert.equal(initial.value.items.length, 1);
  const canonicalItems = items => items.map(({ modelSourceRef, ...item }) => item);
  assert.deepEqual(canonicalItems(repeated.value.items), canonicalItems(initial.value.items));
  // A short handle belongs to its own archive; only the full source identity is reusable.
  // 短句柄绑定各自归档，只有完整来源身份可以复用，不能复用上一次调用的句柄。
  const initialHandle = initial.value.items[0].modelSourceRef, repeatedHandle = repeated.value.items[0].modelSourceRef;
  assert.equal(typeof initialHandle, 'string'); assert.equal(initialHandle.length, 29);
  assert.equal(typeof repeatedHandle, 'string'); assert.equal(repeatedHandle.length, 29);
  assert.notEqual(initialHandle, repeatedHandle);
  assert.equal(initial.value.acquisition.executed, true);
  assert.equal(initial.value.acquisition.reused, false);
  assert.equal(repeated.value.acquisition.executed, false);
  assert.equal(repeated.value.acquisition.reused, true);
  assert.equal(repeated.value.acquisition.newEvidenceCount, 0);
  assert.equal(repeated.value.acquisition.sufficiency, 'not-evaluated');
  assert.notEqual(initial.receipt.resultRef.id, repeated.receipt.resultRef.id);
  for (const result of [initial, repeated]) {
    assert.match(result.receipt.resultRef.sha256, /^[a-f0-9]{64}$/);
    assert.ok(Number.isSafeInteger(result.receipt.resultRef.bytes) && result.receipt.resultRef.bytes > 0);
  }
});

test('empty searches are retried without caching or declaring sufficiency and stop only their exhausted gap', async t => {
  const fixture = await acquisitionFixture(t);
  const input = { query: 'UNOBSERVED_8319 launch', gap: 'The UNOBSERVED_8319 launch date' };
  for (let attempt = 0; attempt < 4; attempt++) {
    const result = await fixture.execute('knowledge.search', input);
    assert.deepEqual(result.value.items, []);
    assert.equal(result.value.acquisition.state, 'no-evidence');
    assert.equal(result.value.acquisition.executed, true);
    assert.equal(result.value.acquisition.reused, false);
    assert.equal(result.value.acquisition.sufficiency, 'not-evaluated');
  }
  const exhausted = await fixture.execute('knowledge.search', input);
  assert.equal(fixture.searchCount(), 4);
  assert.equal(exhausted.value.acquisition.state, 'search-budget-exhausted');
  assert.equal(exhausted.value.acquisition.executed, false);
  assert.equal(exhausted.value.acquisition.sufficiency, 'not-evaluated');
  const independent = await fixture.execute('knowledge.search', { query: 'UNOBSERVED_8319 cancellation',
    gap: 'Whether UNOBSERVED_8319 was cancelled' });
  assert.equal(independent.value.acquisition.executed, true);
  assert.equal(fixture.searchCount(), 5);
});

test('changed memory versions and revoked sources cannot replay a previously successful search', async t => {
  const entry = memoryEntry('CALYX release date is 2031-02-07.'), entries = [entry];
  const fixture = await acquisitionFixture(t, { entries });
  const input = { query: 'CALYX release date', gap: 'The current CALYX release date' };
  const initial = await fixture.execute('knowledge.search', input);
  const oldReference = initial.value.items[0].sourceRef;
  assert.match(oldReference, /^rag1:/);
  entry.content = 'CALYX release date was revised to 2032-03-08.'; entry.revision++;
  const changed = await fixture.execute('knowledge.search', input);
  assert.equal(fixture.searchCount(), 2);
  assert.equal(changed.value.acquisition.reused, false);
  assert.match(changed.value.items[0].excerpt, /2032-03-08/);
  assert.notEqual(changed.value.items[0].sourceRef, oldReference);
  const staleRead = await fixture.run(fixture.context, 'knowledge.read', { sourceRef: oldReference });
  assert.equal(staleRead.isError, true);
  assert.equal(staleRead.code, 'STALE_RETRIEVAL_SOURCE');
  entry.active = false;
  const revoked = await fixture.execute('knowledge.search', input);
  assert.equal(fixture.searchCount(), 3);
  assert.deepEqual(revoked.value.items, []);
  assert.equal(revoked.value.acquisition.reused, false);
  assert.equal(revoked.value.acquisition.sufficiency, 'not-evaluated');
  const revokedRead = await fixture.run(fixture.context, 'knowledge.read', { sourceRef: changed.value.items[0].sourceRef });
  assert.equal(revokedRead.isError, true);
  assert.equal(revokedRead.code, 'RETRIEVAL_SOURCE_NOT_FOUND');
});

test('an imported source changed behind an unchanged catalog invalidates the request cache before reuse', async t => {
  const fixture = await acquisitionFixture(t);
  const imported = await fixture.retrieval.library.add([{ path: join(fixture.workspace, 'notes.md'),
    title: 'CALYX release notes', text: 'CALYX release date is 2031-02-07.' }], { scope: 'user' });
  const input = { query: 'CALYX release date', gap: 'The exact CALYX release date' };
  const initial = await fixture.execute('knowledge.search', input);
  assert.equal(initial.value.items.length, 1);
  // Only this fixture's owned snapshot is changed; the registry intentionally remains unchanged.
  // 仅改动本测试拥有的快照，并故意保持登记版本不变，验证复用前真实磁盘校验。
  await writeFile(join(fixture.retrieval.library.folder, imported.sources[0].id, 'source', 'document.txt'),
    'CALYX release date is an unregistered replacement.', 'utf8');
  const changed = await fixture.execute('knowledge.search', input);
  assert.equal(fixture.searchCount(), 2);
  assert.deepEqual(changed.value.items, []);
  assert.equal(changed.value.acquisition.reused, false);
  const read = await fixture.run(fixture.context, 'knowledge.read', { sourceRef: initial.value.items[0].sourceRef });
  assert.equal(read.isError, true);
  assert.equal(read.code, 'STALE_RETRIEVAL_SOURCE');
});

test('contradictory high-overlap evidence does not become a semantic sufficiency verdict', async t => {
  const fixture = await acquisitionFixture(t, { entries: [
    memoryEntry('CALYX release date is 2031-02-07.', 'CALYX release announcement'),
    memoryEntry('CALYX release date is 2030-05-09, not 2031-02-07.', 'CALYX correction'),
  ] });
  const result = await fixture.execute('knowledge.search', { query: 'CALYX release date 2031',
    gap: 'Which CALYX release date applies to the requested year?' });
  assert.equal(result.value.items.length, 2);
  assert.equal(result.value.evidenceAssessment.state, 'usable');
  assert.ok(result.value.items.some(item => item.excerpt.includes('not 2031')));
  assert.equal(result.value.acquisition.sufficiency, 'not-evaluated');
  assert.equal(result.value.acquisition.state, 'evidence-available');
});

test('a simulated cross-language semantic candidate remains readable without being labelled sufficient', async t => {
  const fixture = await acquisitionFixture(t, { entries: [
    memoryEntry('Reset your password from Account Settings.', 'Account manual'),
  ] });
  const indexSearch = fixture.retrieval.index.search.bind(fixture.retrieval.index);
  // Simulate a semantic provider's candidate, never its quality: source text and reference are real.
  // 仅模拟语义检索返回候选这一功能路径，不评分模型；正文、引用与正式新鲜度校验均真实。
  fixture.retrieval.index.search = async input => {
    const result = await indexSearch({ ...input, query: 'password' });
    return { ...result, strategy: 'hybrid', items: result.items.map(item => ({ ...item, vectorRank: 1, distance: 0.05 })) };
  };
  const result = await fixture.execute('knowledge.search', { query: '账户密码如何重置', gap: '账户的密码重置入口' });
  assert.equal(result.value.items.length, 1);
  assert.equal(result.value.evidenceAssessment.reason, 'semantic-only-unverified');
  assert.equal(result.value.acquisition.sufficiency, 'not-evaluated');
  const read = await fixture.execute('knowledge.read', { sourceRef: result.value.items[0].sourceRef });
  assert.equal(read.value.text, 'Reset your password from Account Settings.');
});

test('gap exhaustion preserves canonical source reads and actual workspace file write/read operations', async t => {
  const fixture = await acquisitionFixture(t, { entries: [memoryEntry('CALYX release date is 2031-02-07.')] });
  const gap = 'The exact CALYX release date'; let reference;
  for (let attempt = 0; attempt < 4; attempt++) {
    const result = await fixture.execute('knowledge.search', { query: `CALYX release date detail${attempt}`, gap });
    assert.equal(result.value.items.length, 1);
    reference ??= result.value.items[0].sourceRef;
  }
  const exhausted = await fixture.execute('knowledge.search', { query: 'CALYX release date final attempt', gap });
  assert.equal(exhausted.value.acquisition.state, 'search-budget-exhausted');
  assert.equal(exhausted.value.acquisition.executed, false);
  assert.equal(fixture.searchCount(), 4);
  assert.match(reference, /^rag1:/);
  const source = await fixture.execute('knowledge.read', { sourceRef: reference, offset: 0, limit: 1000 });
  assert.equal(source.value.text, 'CALYX release date is 2031-02-07.');
  const output = 'Verified CALYX release note\r\n2031-02-07';
  await fixture.execute('filesystem.write', { path: 'delivery.txt', content: output, expectedHash: null });
  const read = await fixture.execute('filesystem.read', { path: 'delivery.txt' });
  assert.equal(read.value.content, output);
  assert.equal(await readFile(join(fixture.workspace, 'delivery.txt'), 'utf8'), output);
  assert.equal(fixture.searchCount(), 4);
});

test('the total request budget cannot be bypassed by inventing more gaps and does not affect a new request', async t => {
  const fixture = await acquisitionFixture(t);
  for (let index = 0; index < 16; index++) {
    const result = await fixture.execute('knowledge.search', { query: `UNOBSERVED_8319 fact${index}`,
      gap: `UNOBSERVED_8319 missing fact ${index}` });
    assert.equal(result.value.acquisition.executed, true);
  }
  const exhausted = await fixture.execute('knowledge.search', { query: 'UNOBSERVED_8319 additional fact',
    gap: 'UNOBSERVED_8319 an entirely new fact' });
  assert.equal(exhausted.value.acquisition.remainingSearches, 0);
  assert.equal(exhausted.value.acquisition.executed, false);
  assert.equal(exhausted.value.acquisition.sufficiency, 'not-evaluated');
  assert.equal(fixture.searchCount(), 16);
  const nextContext = await fixture.service.createContext(fixture.conversationId,
    { requestId: randomUUID(), permissionMode: 'smart' });
  const next = await fixture.run(nextContext, 'knowledge.search', { query: 'UNOBSERVED_8319 additional fact' });
  assert.equal(next.isError, false);
  const archived = await fixture.service.results.get(nextContext, next.resultRef.id);
  assert.equal(archived.structuredContent.acquisition.executed, true);
  assert.equal(fixture.searchCount(), 17);
});

test('invalidating a cached source at the gap limit cannot dispatch a fifth index search', async t => {
  const fixture = await acquisitionFixture(t);
  const imported = await fixture.retrieval.library.add([{ path: join(fixture.workspace, 'date.md'),
    title: 'CALYX release notes', text: 'CALYX release date is 2031-02-07.' }], { scope: 'user' });
  const gap = 'The exact CALYX release date';
  for (let attempt = 0; attempt < 4; attempt++) {
    const result = await fixture.execute('knowledge.search', { query: `CALYX release date detail${attempt}`, gap });
    assert.equal(result.value.items.length, 1);
  }
  await writeFile(join(fixture.retrieval.library.folder, imported.sources[0].id, 'source', 'document.txt'),
    'CALYX release date is a changed unregistered snapshot.', 'utf8');
  const invalidated = await fixture.execute('knowledge.search', { query: 'CALYX release date detail0', gap });
  assert.deepEqual(invalidated.value.items, []);
  assert.equal(invalidated.value.acquisition.executed, false);
  assert.equal(invalidated.value.acquisition.state, 'search-budget-exhausted');
  assert.equal(invalidated.value.acquisition.sufficiency, 'not-evaluated');
  assert.equal(fixture.searchCount(), 4);
});

test('configured optional research reranking is checked live rather than concealed by a cached success', async t => {
  let state = 'ready', rerankCalls = 0, healthChecks = 0;
  const reranker = { status: () => { healthChecks++; return { state, profileId: 'builtin-multilingual-reranker' }; },
    close: async () => {}, rerank: async ({ candidates }) => {
      rerankCalls++;
      return { profileId: 'builtin-multilingual-reranker', modelVersion: 'synthetic-reranker',
        items: candidates.map((item, index) => ({ ...item, rerankScore: 1 / (index + 1) })) };
    } };
  const fixture = await acquisitionFixture(t, { reranker, entries: [
    memoryEntry('CALYX release date is 2031-02-07.'), memoryEntry('CALYX deployment requires a signed audit.', 'CALYX deployment'),
  ] });
  await fixture.retrieval.settings.patchGlobal({ expectedRevision: 1,
    patch: { local: { rerankProfileId: 'builtin-multilingual-reranker' } } });
  // The public coordinator accepts taskType; model-facing tools keep their narrower schema.
  // 协调器公开接口接收 taskType，模型工具仍保留更窄的参数合同。
  const input = { query: 'CALYX', gap: 'CALYX release and deployment constraints', taskType: 'research' };
  const initial = await fixture.retrieval.search(fixture.context, input);
  assert.equal(initial.rerank.profileId, 'builtin-multilingual-reranker');
  state = 'unavailable';
  const repeated = await fixture.retrieval.search(fixture.context, input);
  assert.equal(fixture.searchCount(), 2);
  assert.equal(rerankCalls, 1);
  assert.ok(healthChecks >= 2);
  assert.equal(repeated.acquisition.reused, false);
  assert.equal(repeated.rerankDiagnostic, 'RERANK_UNAVAILABLE');
  assert.equal(repeated.acquisition.sufficiency, 'not-evaluated');
});

import assert from 'node:assert/strict';
import { test } from 'node:test';
import { randomUUID } from 'node:crypto';
import { mkdtemp, rm } from 'node:fs/promises';
import { join, relative, resolve, sep } from 'node:path';
import { tmpdir } from 'node:os';
import { setTimeout as delay } from 'node:timers/promises';
import { runResourceTask } from '../platform/resources/resource-task.mjs';
import { retrievalBudget } from '../orchestration/retrieval/retrieval-budget.mjs';
import { retrievalPlan } from '../orchestration/retrieval/query-plan.mjs';
import { EvidenceAcquisition } from '../orchestration/retrieval/evidence-acquisition.mjs';
import { RetrievalIndex, chunkSource } from '../data/retrieval/index.mjs';
import { RetrievalCoordinator } from '../orchestration/retrieval/coordinator.mjs';
import { TaskExperienceStore } from '../data/retrieval/task-experience-store.mjs';
import { toolFixture } from './tool-fixture.mjs';

function allocator() {
  const leases = new Map(), registrations = [], releases = [];
  return { leases, registrations, releases, snapshot: async () => ({ memory: { availableBytes: 16 * 1024 ** 3 } }),
    acquire: async options => {
      const lease = { status: 'granted', ...options, cpuThreads: Math.min(2, options.cpuThreads), leaseId: randomUUID(),
        suggestions: { annBuildConcurrency: 2, candidateLimit: 96, fusedCandidateLimit: 128, evidenceBudgetTokens: 16384 } };
      leases.set(lease.leaseId, lease); return lease;
    },
    renew: async () => ({ status: 'renewed' }), report: async () => ({ status: 'reported' }),
    registerExecutor: async (leaseId, executor) => { registrations.push({ leaseId, ...executor }); return { status: 'registered' }; },
    release: async leaseId => { leases.delete(leaseId); releases.push(leaseId); }, close: async () => {} };
}

async function temporary(t, closers = []) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-unified-'));
  t.after(async () => {
    for (const close of closers) await close();
    const suffix = relative(resolve(tmpdir()), resolve(root));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(root, { recursive: true, force: true });
  });
  return root;
}

test('research budgets expand candidates and evidence while respecting explicit context limits', () => {
  const simple = retrievalBudget(), research = retrievalBudget({ taskType: 'research' });
  assert.ok(research.fusedCandidates > simple.fusedCandidates);
  assert.equal(research.limit, 18); assert.equal(research.maximumTokens, 16384);
  assert.equal(retrievalBudget({ taskType: 'research', maximumTokens: 500, limit: 2 }).maximumTokens, 500);
  assert.equal(retrievalPlan('研究项目架构').evidenceTokens, 16384);
  assert.equal(retrievalPlan('你好').evidenceTokens > 0 && retrievalPlan('你好').shouldRetrieve, false);
});

test('resource task keeps its lease until an already dispatched operation acknowledges completion', async () => {
  const resources = allocator(), controller = new AbortController();
  let finish, started;
  const admission = new Promise(resolveStarted => { started = resolveStarted; });
  const operation = runResourceTask(resources, {}, () => { started(); return new Promise(resolveDone => { finish = resolveDone; }); },
    { signal: controller.signal });
  await admission; controller.abort();
  assert.equal(resources.leases.size, 1);
  finish('actual receipt'); assert.equal(await operation, 'actual receipt');
  assert.equal(resources.leases.size, 0);
});

test('explicit support requires a source read and exact quote, and source changes invalidate dependent conclusions', () => {
  const acquisition = new EvidenceAcquisition();
  const item = { sourceId: 'source', scopeKey: 'user', sourceRef: 'v1', sourceRevision: 1,
    contentHash: 'a'.repeat(64), excerpt: 'The retry limit is three.' };
  const ticket = acquisition.prepare({ query: 'retry limit', snapshotKey: 'one', cacheKey: 'one' });
  acquisition.observe(ticket, { items: [item] });
  const claim = { statement: 'Retry limit is three', support: [{ sourceRef: 'v1', quote: 'retry limit is three' }] };
  assert.equal(acquisition.assess({ claims: [claim] }).state, 'needs-evidence-or-validation');
  acquisition.observeRead(item);
  const supported = acquisition.assess({ claims: [claim] });
  assert.equal(supported.state, 'ready-to-answer'); assert.equal(supported.correctnessCertified, false);
  assert.equal(acquisition.assess({ conclusionId: 'bad-quote', claims: [{ ...claim,
    support: [{ sourceRef: 'v1', quote: 'retry limit is nine' }] }] }).state, 'needs-evidence-or-validation');
  acquisition.observe(acquisition.prepare({ query: 'retry limit', snapshotKey: 'two', cacheKey: 'two' }),
    { items: [{ ...item, sourceRevision: 2, sourceRef: 'v2', contentHash: 'b'.repeat(64) }] });
  assert.equal(acquisition.conclusions.get('current').state, 'stale');
});

test('current-chat experience cannot be read from a different chat or promoted to user scope', async t => {
  const root = await temporary(t), store = new TaskExperienceStore(root);
  await store.save({ scopeKeys: ['user', 'project:one', 'chat:one'], query: 'current version', sourceRefs: [],
    conclusionId: 'one', state: 'ready-to-answer' });
  assert.equal((await store.find({ scopeKeys: ['user', 'project:one', 'chat:two'] })).length, 0);
  assert.equal((await store.find({ scopeKeys: ['chat:one'] })).length, 1);
});

test('large same-scope vector spaces split into disjoint bounded graphs without dropping semantic retrieval', async t => {
  const closers = [], root = await temporary(t, closers), index = new RetrievalIndex({ root });
  closers.push(() => index.close());
  const document = { sourceId: 'split', scopeKey: 'project:one', sourceType: 'code', title: 'split.mjs',
    text: 'bounded source facts\n'.repeat(600), sourceRevision: 1, locator: { relativePath: 'split.mjs' },
    embeddingProfileId: 'fixture', embeddingModelVersion: 'one', embeddingSpaceId: 'a'.repeat(64) };
  document.chunks = chunkSource(document, { maxChars: 80 });
  const vector = Array.from({ length: 4096 }, (_, index) => index === 0 ? 1 : 0);
  document.vectors = document.chunks.map(() => vector);
  await index.upsertSources([document]);
  const result = await index.search({ query: 'unrelated', scopeKeys: ['project:one'], queryVector: vector,
    embeddingProfileId: 'fixture', embeddingModelVersion: 'one', embeddingSpaceId: 'a'.repeat(64), limit: 100,
    channelCandidates: 100, ann: { mode: 'ann', maxShardBytes: 1024 * 1024, maxCachedShards: 16 } });
  assert.equal(result.semanticBackend, 'ann'); assert.ok(result.items.length > 40);
  const status = (await index.status()).ann;
  assert.ok(status.built > 1); assert.ok(status.cachedVectors <= document.chunks.length);
});

test('frozen tool contexts can search, read, assess support and recall version-checked experience', async t => {
  const fixture = await toolFixture(t), resources = allocator();
  const memory = { id: randomUUID(), scope: 'user', status: 'confirmed', active: true, kind: 'CALYX manual', revision: 1,
    content: 'CALYX retry limit is three. The timeout is ten seconds.' };
  const retrieval = new RetrievalCoordinator({ conversations: fixture.conversations, tools: fixture.service, resources,
    memory: { contextFor: async () => ({ entries: [memory] }) }, reranker: null });
  fixture.service.retrieval = retrieval;
  const context = await fixture.context('full'); assert.equal(Object.isFrozen(context), true);
  const search = await retrieval.search(context, { query: 'CALYX retry limit', maximumTokens: 32768 });
  assert.ok(search.items.length); assert.ok(search.budget.fusedCandidates >= 96);
  const read = await retrieval.read(context, { sourceRef: search.items[0].sourceRef });
  const result = await retrieval.assess(context, { claims: [{ statement: 'retry limit is three',
    support: [{ sourceRef: read.sourceRef, quote: 'retry limit is three' }] }] });
  assert.equal(result.state, 'ready-to-answer');
  const call = fixture.call('knowledge.search', { query: 'CALYX retry limit' });
  const archived = await fixture.service.execute(context, call);
  assert.equal(archived.isError, false);
  const projected = JSON.parse(archived.content);
  const opaque = projected.structuredContent?.items?.[0]?.sourceRef ?? projected.items?.[0]?.sourceRef;
  assert.ok(opaque?.startsWith('ev1:'), JSON.stringify(projected));
  await fixture.conversations.upsertMessage(context.conversationId, { Id: context.requestId, Role: 'assistant',
    Content: 'Evidence collected.', Status: 'completed', ToolActivities: [{ toolCallId: call.id, name: call.name,
      arguments: call.arguments, status: 'completed', resultRef: archived.resultRef }] });
  await retrieval.read(context, { sourceRef: opaque });
  const opaqueSupport = await retrieval.assess(context, { conclusionId: 'opaque-read', claims: [{ statement: 'retry limit is three',
    support: [{ sourceRef: opaque, quote: 'retry limit is three' }] }] });
  assert.equal(opaqueSupport.state, 'ready-to-answer');
  const experience = await retrieval.experience(context, {});
  assert.equal(experience.items.length, 2); assert.equal(experience.items[0].current, true);
  assert.equal(experience.items[0].usableAsAnswer, false);
  fixture.service.registerTaskVerification(context, () => ({ mutationRevision: 2, pendingValidation: true,
    state: 'checks-failed', receipts: [{ toolCallId: 'old-pass', mutationRevision: 2, passed: true }] }));
  const pendingValidation = await retrieval.assess(context, { conclusionId: 'validation-remains-required',
    claims: [{ statement: 'retry limit is three', support: [{ sourceRef: opaque, quote: 'retry limit is three' }] }],
    verification: [{ toolCallId: 'old-pass', description: 'Prior executed check.' }] });
  assert.equal(pendingValidation.state, 'needs-evidence-or-validation');
  assert.equal(pendingValidation.requiredValidation.pendingValidation, true);
  assert.equal((await retrieval.validateFinal(context)).current, true);
  memory.content = 'CALYX retry limit is four.'; memory.revision++;
  assert.equal((await retrieval.experience(context, {})).items[0].current, false);
  const staleFinal = await retrieval.validateFinal(context);
  assert.equal(staleFinal.current, false); assert.equal(staleFinal.invalidSources.length, 1);
  assert.equal(staleFinal.invalidSources[0].code, 'STALE_RETRIEVAL_SOURCE');
  const current = await retrieval.search(context, { query: 'CALYX retry limit' });
  const reread = await retrieval.read(context, { sourceRef: current.items[0].sourceRef });
  assert.match(reread.text, /four/);
  assert.equal((await retrieval.validateFinal(context)).current, true);
});

test('a new caller recovers a terminated SQLite worker without replaying or losing committed sources', async t => {
  const closers = [], root = await temporary(t, closers), index = new RetrievalIndex({ root });
  closers.push(() => index.close());
  await index.upsertSources([{ sourceId: 'original', scopeKey: 'user', sourceType: 'document', title: 'original',
    text: 'Committed cancellation conditions are retained.', sourceRevision: 1, locator: {} }]);
  const old = index.worker;
  await old.terminate();
  const result = await index.search({ query: 'cancellation', scopeKeys: ['user'] });
  assert.equal(result.items[0].sourceId, 'original');
  assert.notEqual(index.worker, old); assert.equal((await index.status()).workerRestarts, 1);
});

test('resource-backed background graphs use independent native owners and release all reservations after exit', async t => {
  const closers = [], root = await temporary(t, closers), resources = allocator();
  const index = new RetrievalIndex({ root, resourceService: resources });
  closers.push(() => index.close());
  const sources = ['one', 'two'].map(name => {
    const source = { sourceId: name, scopeKey: `project:${name}`, sourceType: 'code', title: `${name}.mjs`,
      text: `const ${name} = 1;\n`.repeat(9000), sourceRevision: 1, locator: { relativePath: `${name}.mjs` },
      embeddingProfileId: 'fixture', embeddingModelVersion: 'one', embeddingSpaceId: 'a'.repeat(64) };
    source.chunks = chunkSource(source, { maxChars: 80 }); source.vectors = source.chunks.map(() => [1, 0]);
    return source;
  });
  await index.upsertSources(sources);
  await index.prepareVectors({ scopeKeys: ['project:one', 'project:two'], ann: { mode: 'ann', threshold: 1 } });
  let status;
  const deadline = performance.now() + 10000;
  do { await delay(20); status = (await index.status()).ann; } while (status.pendingBuilds && performance.now() < deadline);
  assert.equal(status.completedBuilds, 2, JSON.stringify(status));
  assert.equal(status.buildConcurrency, 2);
  assert.ok(new Set(resources.registrations.map(item => item.processId).filter(pid => pid !== process.pid)).size >= 2);
  await index.close();
  assert.equal(resources.leases.size, 0);
});

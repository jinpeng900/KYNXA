import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { test } from 'node:test';
import { MemoryService } from '../data/memory-service.mjs';
import { chunkSource } from '../data/retrieval/index.mjs';
import { RetrievalCoordinator } from '../orchestration/retrieval/coordinator.mjs';
import { sourceIdentity } from '../orchestration/retrieval/source-projection.mjs';
import { toolFixture } from './tool-fixture.mjs';

// Real memory files and SQLite publications exercise the lifecycle; model inference stays synthetic.
// 使用真实记忆文件和 SQLite 发布验证生命周期；推理使用合成替身，不读取用户数据或调用付费模型。
async function fixture(t, { semantic = false, embedDocuments } = {}) {
  const f = await toolFixture(t);
  const memory = new MemoryService({ conversationStore: f.conversations });
  const embeddings = { status: () => ({ state: semantic ? 'ready' : 'unavailable', profileId: 'builtin-multilingual',
    modelVersion: 'synthetic-memory-v1', embeddingSpaceId: '1'.repeat(64), dimensions: 2 }),
  embedDocuments: embedDocuments ?? (async texts => ({ profileId: 'builtin-multilingual',
    modelVersion: 'synthetic-memory-v1', embeddingSpaceId: '1'.repeat(64), dimensions: 2, vectors: texts.map(() => [1, 0]) })),
  close: async () => {} };
  const structures = { derivationVersion: 'synthetic-parser-v1', status: () => ({ state: 'ready' }), close: async () => {},
    parse: async source => ({ structure: { domain: 'knowledge', language: 'text', parserVersion: 'synthetic-parser-v1',
      parseStatus: 'parsed', diagnosticCodes: [] }, parserVersion: 'synthetic-parser-v1', chunkerVersion: 'character-v1',
      embeddingInputVersion: 'source-context-v1', chunks: chunkSource(source) }) };
  const retrieval = new RetrievalCoordinator({ conversations: f.conversations, memory, tools: f.service, embeddings, structures });
  f.service.retrieval = retrieval;
  await retrieval.settings.patchGlobal({ expectedRevision: 0, patch: { local: { semantic: semantic ? 'auto' : 'off' } } });
  const unsubscribe = memory.onChange(event => retrieval.onMemoryChanged(event));
  t.after(async () => { unsubscribe(); await memory.flushCandidates(); await retrieval.close(); });
  return { ...f, memory, retrieval, entries: scopes => retrieval.index.listSources({ scopeKeys: scopes, sourceType: 'memory' }) };
}

test('candidate drafts never enter SQLite or context; confirmation publishes and deletion revokes their evidence', async t => {
  const f = await fixture(t), userId = randomUUID();
  await f.conversations.upsertMessage(f.conversationId, { Id: userId, Role: 'user', Status: 'completed',
    Content: '纠正：本工作要求先保留完整原话。' });
  await f.memory.scheduleCandidates(f.conversationId, { phase: 'user', userMessageId: userId });
  const scope = `chat:${f.conversationId}`;
  let document = await f.memory.repository.read(f.conversationId, 'chat');
  assert.equal(document.entries.length, 1);
  assert.equal(f.retrieval.index.worker, null, 'draft extraction does not start optional indexing');
  assert.deepEqual((await f.memory.contextFor(f.conversationId)).entries, []);
  document = await f.memory.update(f.conversationId, document.entries[0].id,
    { scope: 'chat', status: 'confirmed', expectedRevision: document.revision });
  await f.retrieval.flushMemoryIndex();
  const indexed = await f.entries([scope]);
  assert.equal(indexed.length, 1, JSON.stringify(f.retrieval.memoryIndexState)); assert.equal(indexed[0].sourceRevision, document.entries[0].revision);
  assert.equal((await f.memory.contextFor(f.standaloneId)).entries.length, 0);
  const found = await f.retrieval.index.search({ query: '完整原话', scopeKeys: [scope], limit: 4 });
  assert.equal(found.items.length, 1);
  await f.memory.delete(f.conversationId, document.entries[0].id, { scope: 'chat', expectedRevision: document.revision });
  assert.equal((await f.entries([scope])).length, 0);
  assert.equal((await f.retrieval.index.verifyReference({ sourceRef: found.items[0].sourceRef, scopeKeys: [scope] })).current, false);
});

test('global, project and chat memories retain separate visible scopes during selective publication', async t => {
  const f = await fixture(t);
  await f.memory.createScope('user', 'user', { content: '全局偏好：简短总结。', expectedRevision: 0 });
  await f.memory.createScope('project', f.projectId, { content: '工作决策：先检查接口兼容。', expectedRevision: 0 });
  await f.memory.create(f.conversationId, { scope: 'chat', content: '本聊天目标：完善来源状态。' });
  await f.retrieval.flushMemoryIndex();
  assert.equal((await f.entries(['user'])).length, 1);
  assert.equal((await f.entries([`project:${f.projectId}`])).length, 1, JSON.stringify(f.retrieval.memoryIndexState));
  assert.equal((await f.entries([`chat:${f.conversationId}`])).length, 1);
  assert.equal((await f.memory.contextFor(f.standaloneId)).entries.length, 1, 'another chat sees only the global entry');
  assert.equal((await f.memory.contextFor(f.conversationId)).entries.length, 3);
});

test('updates and deletion during embedding cannot republish an obsolete confirmed memory', async t => {
  let entered, release;
  const ready = new Promise(resolve => { entered = resolve; }), blocked = new Promise(resolve => { release = resolve; });
  t.after(() => release());
  let calls = 0;
  const f = await fixture(t, { semantic: true, embedDocuments: async texts => {
    if (++calls === 1) { entered(); await blocked; }
    return { profileId: 'builtin-multilingual', modelVersion: 'synthetic-memory-v1',
      embeddingSpaceId: '1'.repeat(64), dimensions: 2, vectors: texts.map(() => [1, 0]) };
  } });
  let document = await f.memory.createScope('user', 'user', { content: '旧工作目标。', expectedRevision: 0 });
  await ready;
  document = await f.memory.updateScope('user', 'user', document.entries[0].id,
    { content: '更新后的工作目标。', expectedRevision: document.revision });
  await f.memory.deleteScope('user', 'user', document.entries[0].id, { expectedRevision: document.revision });
  release(); await f.retrieval.flushMemoryIndex();
  assert.equal((await f.entries(['user'])).length, 0);
  assert.equal(calls, 1, 'deleted queued revision is not embedded');
});

test('invalid memory identities are rejected before touching the index', async t => {
  const f = await fixture(t);
  await assert.rejects(f.retrieval.onMemoryChanged({ scope: 'chat', scopeId: f.conversationId,
    scopeKey: 'user', memoryId: 'synthetic-id', sourceId: sourceIdentity('memory', 'user', 'synthetic-id'), status: 'confirmed' }),
  { code: 'INVALID_MEMORY_CHANGE' });
  assert.equal(f.retrieval.index.worker, null);
});

test('an event arriving at worker cleanup is not stranded and flush waits for its successor', async t => {
  const f = await fixture(t);
  let passes = 0;
  f.retrieval._indexChangedMemories = async () => {
    f.retrieval.memoryIndexPending.clear();
    if (++passes === 1) queueMicrotask(() => f.retrieval.memoryIndexPending.set('late-event', {}));
  };
  f.retrieval.memoryIndexPending.set('first-event', {});
  f.retrieval._startMemoryIndex();
  await f.retrieval.flushMemoryIndex();
  assert.equal(passes, 2); assert.equal(f.retrieval.memoryIndexPending.size, 0);
});

test('failed publication is reported as unverified instead of a successful memory update', async t => {
  const f = await fixture(t);
  f.retrieval.index.upsertSources = async () => {
    throw Object.assign(new Error('Synthetic disk failure'), { code: 'SYNTHETIC_DISK_FAILURE' });
  };
  await f.memory.createScope('user', 'user', { content: '不可虚报已索引的工作决定。', expectedRevision: 0 });
  await f.retrieval.flushMemoryIndex();
  assert.equal(f.retrieval.memoryIndexState.publishedUpdates, 0);
  assert.equal(f.retrieval.memoryIndexState.errorCode, 'SYNTHETIC_DISK_FAILURE');
  assert.equal((await f.entries(['user'])).length, 0);
});

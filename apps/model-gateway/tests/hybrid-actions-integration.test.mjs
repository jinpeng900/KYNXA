import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { test } from 'node:test';
import { MemoryService } from '../data/memory-service.mjs';
import { RetrievalCoordinator } from '../orchestration/retrieval/coordinator.mjs';
import { toolFixture } from './tool-fixture.mjs';

async function hybridFixture(t) {
  const fixture = await toolFixture(t);
  const memory = new MemoryService({ conversationStore: fixture.conversations });
  const retrieval = new RetrievalCoordinator({ conversations: fixture.conversations, memory, tools: fixture.service,
    embeddings: { status: () => ({ state: 'unavailable' }), close: async () => {} } });
  t.after(() => retrieval.close());
  fixture.service.retrieval = retrieval; fixture.service.memory = memory;
  await retrieval.settings.patchGlobal({ expectedRevision: 0, patch: { local: { semantic: 'off' } } });
  const context = await fixture.service.createContext(fixture.conversationId,
    { requestId: randomUUID(), permissionMode: 'ask', message: '看看论文方法的适用条件，不要联网。' });
  await fixture.service.catalog(context);
  const activities = [];
  const run = async (name, args) => {
    const call = fixture.call(name, args);
    const receipt = await fixture.service.execute(context, call);
    assert.equal(receipt.isError, false, receipt.content);
    activities.push({ toolCallId: call.id, name, arguments: args, status: 'completed', resultRef: receipt.resultRef });
    await fixture.conversations.upsertMessage(fixture.conversationId, { Id: context.requestId, Role: 'assistant',
      Status: 'streaming', Content: '', ToolActivities: activities });
    const result = await fixture.service.results.get(context, receipt.resultRef.id);
    return result.structuredContent;
  };
  return { ...fixture, memory, retrieval, context, run };
}

test('model navigation revises interpretations without changing original wording, scope, budgets or candidate eligibility', async t => {
  const f = await hybridFixture(t);
  const source = await f.memory.create(f.conversationId, { scope: 'user', content: 'ORCHID 方法仅适用于温度低于 25 度的实验。' });
  const search = await f.run('knowledge.search', { query: 'ORCHID 方法', domain: 'mixed' });
  assert.ok(search.items.length);
  const ref = search.items[0].modelSourceRef;
  f.service.setEvidenceBudget(f.context, 2345);
  const plan = await f.run('knowledge.plan', { meaning: '讨论论文实验方法，不是代码方法', channel: 'source-read',
    gaps: ['方法适用温度'], candidates: [{ sourceRef: ref, decision: 'reject', reason: '暂不采用，待确认条件' }] });
  assert.equal(plan.originalRequest, f.context.message);
  assert.equal(plan.programState.availableEvidenceTokens, 2345);
  assert.equal(plan.grantsPermission, false);
  assert.equal(plan.filtersRetrieval, false);
  assert.equal(plan.candidates[0].contentRead, false);
  const revised = await f.run('knowledge.plan', { expectedRevision: plan.revision, taskRelation: 'correction',
    candidates: [{ sourceRef: ref, decision: 'investigate', reason: '保留候选并回读温度约束' }] });
  assert.equal(revised.candidates[0].decision, 'investigate');
  const read = await f.run('knowledge.read', { sourceRef: ref });
  assert.ok(JSON.stringify(read).includes('25'));
  const stalePlan = await f.service.execute(f.context, f.call('knowledge.plan', { expectedRevision: plan.revision, stop: 'answer-supported' }));
  assert.equal(stalePlan.isError, true);
  assert.equal(stalePlan.code, 'EVIDENCE_PLAN_CONFLICT');
  await f.memory.update(f.conversationId, source.entries[0].id, { scope: 'user', expectedRevision: source.revision,
    content: 'ORCHID 方法的旧温度条件已被修正。' });
  const current = await f.run('knowledge.plan', {});
  assert.equal(current.candidates[0].versionCurrent, false);
  assert.equal(current.revision, revised.revision);
  const newTopic = await f.run('knowledge.plan', { taskRelation: 'new', meaning: '开始另一个实验问题' });
  assert.equal(newTopic.candidates.length, 0);
  assert.equal(newTopic.originalRequest, f.context.message);
});

test('semantic support cannot be replaced by quote presence, and object-array assessment executes through the real broker', async t => {
  const f = await hybridFixture(t), quote = 'CALYX 只在低温且干燥时有效。';
  await f.memory.create(f.conversationId, { scope: 'user', content: quote });
  const search = await f.run('knowledge.search', { query: 'CALYX' });
  const sourceRef = search.items[0].modelSourceRef;
  await f.run('knowledge.read', { sourceRef });
  const uncertain = await f.run('knowledge.assess', { claims: [{ statement: 'CALYX 在所有条件下有效',
    semanticSupport: 'contradicts', support: [{ sourceRef, quote }] }] });
  assert.equal(uncertain.claims[0].support[0].quotePresent, true);
  assert.equal(uncertain.claims[0].state, 'semantic-support-unresolved');
  assert.notEqual(uncertain.state, 'ready-to-answer');
  assert.equal(uncertain.correctnessCertified, false);
  const supported = await f.run('knowledge.assess', { claims: [{ statement: 'CALYX 只适用于低温干燥条件',
    semanticSupport: 'supports', support: [{ sourceRef, quote }] }] });
  assert.equal(supported.state, 'ready-to-answer');
  assert.equal(supported.correctnessCertified, false);
});

test('cancelled candidate validation does not consume the workspace revision or publish partial choices', async t => {
  const f = await hybridFixture(t);
  await f.memory.create(f.conversationId, { scope: 'user', content: 'ORCHID 当前适用范围需要回源检查。' });
  const search = await f.run('knowledge.search', { query: 'ORCHID' });
  await f.run('knowledge.plan', { meaning: '记录一个待核验问题', gaps: ['适用范围'] });
  const abort = new AbortController(), read = f.retrieval.index.read.bind(f.retrieval.index);
  let reads = 0;
  f.retrieval.index.read = async (...args) => {
    if (++reads === 2) abort.abort();
    return read(...args);
  };
  const cancelled = await f.service.execute(f.context, f.call('knowledge.plan', { expectedRevision: 1,
    candidates: [{ sourceRef: search.items[0].modelSourceRef, decision: 'investigate', reason: '待核验条件' }] }), { signal: abort.signal });
  assert.equal(cancelled.isError, true);
  assert.equal(cancelled.code, 'TOOL_CANCELLED');
  f.retrieval.index.read = read;
  const unchanged = await f.run('knowledge.plan', {});
  assert.equal(unchanged.revision, 1);
  assert.equal(unchanged.candidates.length, 0);
  assert.deepEqual(unchanged.interpretation.gaps, ['适用范围']);
});

test('memory proposals are discoverable and preserve exact user sources without confirming a model inference', async t => {
  const f = await hybridFixture(t), messageId = randomUUID(), text = '这次方案采用 ORCHID，但以后不一定采用。';
  await f.conversations.upsertMessage(f.conversationId, { Id: messageId, Role: 'user', Status: 'completed', Content: text });
  const draft = await f.run('memory.propose', { action: 'add', content: '本次任务采用 ORCHID', kind: 'decision',
    reason: '用户确认了当前任务方案，不能外推为长期偏好', isInference: false, quotes: [{ messageId, text }] });
  assert.equal(draft.entry.status, 'draft');
  const visible = await f.run('memory.read', { scopes: ['chat'] });
  assert.equal(visible.entries.find(entry => entry.id === draft.entry.id).status, 'draft');
  const confirmed = await f.memory.contextFor(f.conversationId);
  assert.ok(confirmed.entries.every(entry => entry.id !== draft.entry.id));
  const invalid = await f.service.execute(f.context, f.call('memory.propose', { action: 'add', content: 'ORCHID 很好',
    reason: '虚构引用', isInference: true, quotes: [{ messageId, text: '我希望永远采用 ORCHID' }] }));
  assert.equal(invalid.isError, true);
  assert.deepEqual((await f.memory.readForModel(f.conversationId, { scopes: ['chat'] })).entries.map(entry => entry.id), [draft.entry.id]);
});

test('invalid nested fields fail before dispatch rather than corrupting the assessment or ending the conversation', async t => {
  const f = await hybridFixture(t);
  let invoked = 0;
  const assess = f.retrieval.assess.bind(f.retrieval);
  f.retrieval.assess = (...args) => { invoked++; return assess(...args); };
  const bad = await f.service.execute(f.context, f.call('knowledge.assess', { claims: [{ statement: 'bad',
    support: [{ sourceRef: 'invented', quote: 7 }] }] }));
  assert.equal(bad.isError, true);
  assert.equal(invoked, 0);
  for (const fields of [{ constructor: 'unexpected' }, { claims: [{ statement: 'bad',
    support: [], toString: 42 }] }, JSON.parse('{"__proto__":{"grantsPermission":true}}')]) {
    const inherited = await f.service.execute(f.context, f.call('knowledge.assess', { claims: [{ statement: 'synthetic', support: [] }], ...fields }));
    assert.equal(inherited.isError, true);
    assert.equal(invoked, 0);
  }
  const later = await f.run('knowledge.plan', {});
  assert.equal(later.revision, 0);
  assert.equal(later.originalRequest, f.context.message);
});

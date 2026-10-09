import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { test } from 'node:test';
import { ModelStore } from '../models/store.mjs';
import { ModelRuntime } from '../orchestration/runtime.mjs';
import { toolFixture } from './tool-fixture.mjs';
import { RetrievalEvidenceService } from '../orchestration/retrieval/evidence-service.mjs';
import { retrievalPlan } from '../orchestration/retrieval/query-plan.mjs';
import { requestInterpretation, requestInterpretationPrompt } from '../orchestration/request-interpretation.mjs';

async function runtimeFixture(t) {
  const fixture = await toolFixture(t), requests = [];
  const upstream = createServer(async (request, response) => {
    let body = ''; for await (const part of request) body += part;
    requests.push(JSON.parse(body));
    response.setHeader('Content-Type', 'application/json');
    response.end(JSON.stringify({ choices: [{ finish_reason: 'stop',
      message: { role: 'assistant', content: 'Fixture answer.' } }] }));
  });
  await new Promise(resolve => upstream.listen(0, '127.0.0.1', resolve));
  t.after(() => new Promise(resolve => upstream.close(resolve)));
  const models = new ModelStore({ dataHome: fixture.dataHome });
  await models.save({ providerId: 'fixture', displayName: 'Fixture',
    baseUrl: `http://127.0.0.1:${upstream.address().port}/v1`, models: ['mock-model'],
    contextWindowTokens: 32768, maxOutputTokens: 1024 });
  const runtime = new ModelRuntime({ modelStore: models, dataHome: fixture.dataHome,
    conversationStore: fixture.conversations, toolService: fixture.service });
  t.after(() => runtime.close());
  await runtime.retrieval.settings.patchGlobal({ expectedRevision: 0, patch: { local: { semantic: 'off' } } });
  const seed = async content => {
    const id = randomUUID();
    await fixture.conversations.upsertMessage(fixture.conversationId,
      { Id: id, Role: 'user', Status: 'completed', Content: content });
    await fixture.conversations.upsertMessage(fixture.conversationId,
      { Id: randomUUID(), Role: 'assistant', Status: 'completed', ReplyTo: id, Content: 'Earlier fixture answer.' });
  };
  const reply = message => runtime.reply({ conversationId: fixture.conversationId,
    provider: 'fixture', model: 'mock-model', message });
  return { ...fixture, runtime, requests, seed, reply };
}

test('topic switches keep the raw user message and do not retrieve the former repository task', async t => {
  const fixture = await runtimeFixture(t);
  await fixture.seed('查找 src/queue.ts 中的函数定义');
  let searches = 0;
  fixture.runtime.retrieval.evidence = async () => { searches++; return { prompt: '', references: [] }; };
  const message = '换个话题，蛋糕为什么干？';
  await fixture.reply(message);
  assert.equal(searches, 0);
  assert.equal(fixture.requests.length, 1, 'interpretation adds no model request');
  const request = fixture.requests[0];
  assert.equal(request.messages.at(-1).content, message);
  const records = await fixture.conversations.readModelMessages(fixture.conversationId);
  assert.equal(records.filter(item => item.Role === 'user').at(-1).Content, message);
  const trace = records.filter(item => item.Role === 'assistant').at(-1).RequestInterpretation;
  assert.equal(trace.taskRelation.type, 'topic-switch');
  assert.equal(trace.taskRelation.allowsInheritance, false);
  assert.deepEqual(trace.queryDerivation.additions, []);
  assert.equal(trace.grantsPermission, false);
});

test('a correction replaces the search condition while retaining the old conversation log', async t => {
  const fixture = await runtimeFixture(t);
  const original = '比较论文在 2025 年的中文实验结果';
  await fixture.seed(original);
  let plan;
  fixture.runtime.retrieval.evidence = async (_context, _query, options) => {
    plan = options.plan; return { prompt: '', references: [] };
  };
  const message = '更正：我要的是 2026 年的英文论文实验，不是 2025 年。';
  await fixture.reply(message);
  assert.equal(plan.taskRelation.type, 'correction');
  assert.equal(plan.originalQuery, message);
  assert.match(plan.query, /2026 年的英文论文实验/u);
  assert.doesNotMatch(plan.query, /2025/u);
  assert.ok(plan.queryDerivation.replacements.some(item => item.rejected.includes('2025')));
  assert.deepEqual(plan.queryDerivation.additions, []);
  const records = await fixture.conversations.readModelMessages(fixture.conversationId);
  assert.ok(records.some(item => item.Role === 'user' && item.Content === original));
  assert.equal(fixture.requests.length, 1);
});

test('retrieval errors preserve a typed recovery outcome without guessing missing facts', async t => {
  const fixture = await runtimeFixture(t);
  fixture.runtime.retrieval.evidence = async () => {
    throw Object.assign(new Error('Changed source.'), { code: 'STALE_RETRIEVAL_SOURCE' });
  };
  await fixture.reply('解释项目资料中的恢复步骤');
  const records = await fixture.conversations.readModelMessages(fixture.conversationId);
  const assistant = records.filter(item => item.Role === 'assistant').at(-1);
  assert.equal(assistant.RetrievalDiagnostic.code, 'STALE_RETRIEVAL_SOURCE');
  assert.equal(assistant.RetrievalOutcome.state, 'source-changed');
  assert.equal(assistant.RetrievalOutcome.next, 'resolve-or-read-current-source');
  assert.equal(fixture.requests.length, 1);
});

test('automatic follow-up evidence carries original intent separately from old file mentions', async () => {
  const plan = retrievalPlan('继续比较它们的恢复方案', {
    history: [{ Id: 'prior', Role: 'user', Status: 'completed',
      Content: '比较 src/storage.ts 和 docs/recovery.md 的资料' }] });
  let observed;
  const service = new RetrievalEvidenceService({ signalFor: signal => signal,
    search: async (_context, args, options) => {
      observed = { args, options };
      return { items: [], indexingPending: true, evidenceAssessment: { state: 'empty' } };
    } });
  const result = await service.prepare({ conversationId: randomUUID() }, plan.originalQuery,
    { plan, maximumTokens: 2048 });
  assert.equal(plan.queryDerivation.additions.length, 1);
  assert.match(observed.args.query, /src\/storage\.ts/);
  assert.equal(observed.options.retrievalIntent.path, undefined,
    'a historical path is not a current hard target');
  assert.deepEqual(observed.options.retrievalIntent, plan.retrievalIntent);
  assert.equal(result.outcome.state, 'coverage-incomplete');
  assert.equal(result.outcome.correctnessCertified, false);
});

test('system hints do not promote raw candidate or historical content into confirmed facts', () => {
  const plan = retrievalPlan('继续研究 MEM_EFF_DISMISSED_SOURCE 的 method', {
    history: [{ Id: 'prior-user', Role: 'user', Status: 'completed',
      Content: '分析 MEM_EFF_OLD_CONTEXT 对这个方法的影响' }] });
  const interpretation = requestInterpretation(plan);
  const prompt = requestInterpretationPrompt(interpretation);
  assert.ok(prompt);
  assert.doesNotMatch(prompt, /MEM_EFF_DISMISSED_SOURCE|MEM_EFF_OLD_CONTEXT/);
  assert.equal(interpretation.queryDerivation.originalQuery, plan.originalQuery);
  assert.equal(interpretation.interpretationVerified, false);
});

test('final source validation updates the actual model request and saved outcome', async t => {
  const fixture = await runtimeFixture(t);
  fixture.runtime.retrieval.evidence = async () => ({ prompt: '', references: [], prepared: true,
    outcome: { state: 'evidence-found', next: 'read-and-check-support', correctnessCertified: false } });
  fixture.runtime.retrieval.finalizeEvidence = async () => ({ prompt: '', references: [],
    outcome: { state: 'source-changed', next: 'resolve-or-read-current-source', correctnessCertified: false } });
  await fixture.reply('根据项目资料解释恢复步骤');
  const system = fixture.requests[0].messages.filter(item => item.role === 'system').map(item => item.content).join('\n');
  assert.match(system, /"state":"source-changed"/);
  assert.doesNotMatch(system, /"state":"evidence-found"/);
  const records = await fixture.conversations.readModelMessages(fixture.conversationId);
  assert.equal(records.filter(item => item.Role === 'assistant').at(-1).RetrievalOutcome.state, 'source-changed');
});

test('negated sources and natural topic changes do not trigger prior code retrieval', async t => {
  for (const message of [
    '先不说代码了，聊聊这个蛋糕为什么会干。',
    '现在聊另一件事：这个方法在心理学实验中有什么用途？',
    '不是让你查看 src/worker.ts，我是想了解人际网络的方法。',
    '暂且搁置仓库，OpenAI 这个品牌是怎么命名的？',
    '取消订单后，退款为何超时？'
  ]) await t.test(message, async child => {
    const fixture = await runtimeFixture(child);
    await fixture.seed('解释 src/worker.ts 里任务取消的实现');
    let searches = 0;
    fixture.runtime.retrieval.evidence = async () => { searches++; return { prompt: '', references: [] }; };
    await fixture.reply(message);
    assert.equal(searches, 0, 'old technical words cannot schedule a local search for a new request');
    assert.equal(fixture.requests[0].messages.at(-1).content, message);
    const records = await fixture.conversations.readModelMessages(fixture.conversationId);
    const trace = records.filter(item => item.Role === 'assistant').at(-1).RequestInterpretation;
    assert.equal(trace.retrieval.domain, 'mixed');
    assert.deepEqual(trace.queryDerivation.additions, []);
    assert.equal(trace.taskRelation.allowsInheritance, false);
    assert.equal(trace.grantsPermission, false);
  });
});

test('independent three-turn ambiguity scenarios preserve current intent through real conversation assembly', async t => {
  // The human labels were frozen before execution, rather than inferred from observed routing.
  // 人工预期在运行前冻结，不按观察到的路由结果生成；模拟上游只验证网关投影。
  const scenarios = JSON.parse(await readFile(new URL('./fixtures/ambiguity-conversations.json', import.meta.url), 'utf8')).cases;
  for (const scenario of scenarios) await t.test(scenario.id, async child => {
    const fixture = await runtimeFixture(child), evidenceCalls = [];
    fixture.runtime.retrieval.evidence = async (_context, _query, options) => {
      evidenceCalls.push(options.plan);
      return { prompt: '', references: [] };
    };
    let final;
    for (const message of scenario.turns) {
      const before = evidenceCalls.length;
      await fixture.reply(message);
      const records = await fixture.conversations.readModelMessages(fixture.conversationId);
      const trace = records.filter(item => item.Role === 'assistant').at(-1).RequestInterpretation;
      assert.equal(records.filter(item => item.Role === 'user').at(-1).Content, message);
      assert.equal(fixture.requests.at(-1).messages.filter(item => item.role === 'user').at(-1).content, message);
      assert.equal(trace.grantsPermission, false);
      assert.equal(trace.retrieval.domain, 'mixed');
      final = { automatic: evidenceCalls.length > before, trace,
        plan: evidenceCalls.length > before ? evidenceCalls.at(-1) : null };
    }
    assert.equal(final.automatic, scenario.expected.automatic, scenario.id);
    const derived = final.plan?.query ?? final.trace.queryDerivation.originalQuery;
    for (const required of scenario.expected.requiredDerived ?? []) assert.ok(derived.includes(required), `missing ${required}`);
    for (const rejected of scenario.expected.forbiddenDerived ?? []) {
      assert.ok(!final.plan?.query.includes(rejected), `revived ${rejected}`);
      assert.ok(!final.trace.queryDerivation.additions.some(item => item.text.includes(rejected)), `inherited ${rejected}`);
    }
    assert.equal(fixture.requests.length, scenario.turns.length, 'no extra classifier model request');
  });
});

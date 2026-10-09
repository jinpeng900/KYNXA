import assert from 'node:assert/strict';
import { test } from 'node:test';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { ModelStore } from '../models/store.mjs';
import { ConversationStore } from '../data/conversations.mjs';
import { ModelRuntime } from '../orchestration/runtime.mjs';
import { chatRequest } from '../models/protocols.mjs';
import { createServer } from 'node:http';
import { setTimeout as delay } from 'node:timers/promises';

async function upstreamFixture(t) {
  const requests = [];
  const upstream = createServer(async (request, response) => {
    let text = '';
    for await (const chunk of request) text += chunk;
    if (request.method === 'POST') requests.push(JSON.parse(text));
    response.writeHead(200, { 'Content-Type': 'application/json' });
    response.end(JSON.stringify({ choices: [{ finish_reason: 'stop', message: { content: 'Fixture answer.' } }] }));
  });
  await new Promise(ready => upstream.listen(0, '127.0.0.1', ready));
  t.after(() => { upstream.closeAllConnections(); return new Promise(done => upstream.close(done)); });
  return { baseUrl: `http://127.0.0.1:${upstream.address().port}/v1`, requests };
}

async function fixture(t, connection) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-runtime-model-budget-'));
  const store = new ModelStore({ dataHome: join(root, 'Models') });
  await store.save({ providerId: 'budget-test', displayName: 'Budget test', apiKey: 'fixture-not-a-real-key', ...connection });
  const conversations = new ConversationStore({ dataHome: join(root, 'Models'), legacyDesktopDirectory: null });
  const runtime = new ModelRuntime({ modelStore: store, conversationStore: conversations,
    dataHome: join(root, 'Models'), extensionRoot: join(root, 'Extensions') });
  t.after(async () => {
    await runtime.close();
    const suffix = relative(resolve(tmpdir()), root);
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(root, { recursive: true, force: true });
  });
  return { runtime, store };
}

// Only prepare the request: no request is sent to any official API or user account.
// 仅准备请求；不向任何官方 API 或用户账号发送请求。
test('runtime clamps a known official model without rewriting configured context or output ceilings', async t => {
  const { runtime, store } = await fixture(t, { baseUrl: 'https://api.openai.com/v1', protocol: 'openai-responses',
    models: ['gpt-6-astra'], contextWindowTokens: 2_000_000, maxOutputTokens: 262_144 });
  const input = { conversationId: 'known-budget-chat', message: 'Continue the code review', provider: 'budget-test', model: 'gpt-6-astra' };
  const prepared = await runtime.prepare(input, input.conversationId);
  assert.equal(prepared.contextMetrics.contextWindowTokens, 1_050_000);
  assert.equal(prepared.requestOptions.maxOutputTokens, 128_000);
  assert.equal(prepared.inputBudgetTokens, 913_808);
  assert.equal(prepared.contextMetrics.outputBudgetReductionReason, 'provider_limit');
  assert.doesNotMatch(JSON.stringify(prepared.contextMetrics), /fixture-not-a-real-key/);
  const saved = await store.connectionFor('budget-test');
  assert.equal(saved.contextWindowTokens, 2_000_000);
  assert.equal(saved.maxOutputTokens, 262_144);
});

test('cold local metadata caps the request before loading without rewriting the user context setting', async t => {
  const { runtime, store } = await fixture(t, { baseUrl: 'http://127.0.0.1:1234/v1', protocol: 'openai-completions',
    models: ['qwen-local'], contextWindowTokens: 1_048_576, maxOutputTokens: 1024 });
  runtime.localModels.observe = async () => ({ backend: 'ollama', loaded: false, runtimeContextTokens: null,
    modelMaximumContextTokens: 40960 });
  const input = { conversationId: 'cold-local-budget', message: 'Continue the code review', provider: 'budget-test', model: 'qwen-local' };
  const prepared = await runtime.prepare(input, input.conversationId);
  assert.equal(prepared.contextMetrics.contextWindowTokens, 40960);
  assert.equal(prepared.requestOptions.maxOutputTokens, 1024);
  assert.equal((await store.connectionFor('budget-test')).contextWindowTokens, 1_048_576);
  assert.equal(runtime.retrieval.embeddings.status().loaded, false);
});

test('runtime keeps explicit legacy small values and does not infer cloud limits for local aliases', async t => {
  const { runtime } = await fixture(t, { baseUrl: 'http://127.0.0.1:1234/v1', protocol: 'openai-completions',
    models: ['gpt-6-astra'], contextWindowTokens: 8192, maxOutputTokens: 2048 });
  const input = { conversationId: 'local-budget-chat', message: 'Continue work', provider: 'budget-test', model: 'gpt-6-astra' };
  const prepared = await runtime.prepare(input, input.conversationId);
  assert.equal(prepared.contextMetrics.contextWindowTokens, 8192);
  assert.equal(prepared.requestOptions.maxOutputTokens, 2048);
  assert.equal(prepared.contextMetrics.providerMaxInputTokens, undefined);
  assert.equal(prepared.contextMetrics.providerMaxOutputTokens, undefined);
});

test('a small configured output still respects the independent official input maximum', async t => {
  const { runtime } = await fixture(t, { baseUrl: 'https://api.openai.com/v1', protocol: 'openai-responses',
    models: ['gpt-6-astra'], contextWindowTokens: 1_050_000, maxOutputTokens: 2048 });
  const input = { conversationId: 'input-cap-chat', message: 'Review the task', provider: 'budget-test', model: 'gpt-6-astra' };
  const prepared = await runtime.prepare(input, input.conversationId);
  assert.equal(prepared.inputBudgetTokens, 913_808);
  assert.equal(prepared.requestOptions.maxOutputTokens, 2048);
});

test('the default outer lifetime honors a one-hour task while an explicit runtime override remains auditable', async t => {
  const { runtime } = await fixture(t, { baseUrl: 'http://127.0.0.1:1234/v1', protocol: 'openai-completions', models: ['fixture'] });
  const turn = { runLimits: { maxDurationMs: 3_600_000 }, contextMetrics: {} };
  const requested = runtime.runTimingAudit(turn);
  assert.equal(requested.effectiveDurationMs, 3_600_000);
  assert.equal(requested.earlyCutReason, null);
  runtime.hasStreamTimeoutOverride = true;
  runtime.streamTimeoutMs = 1_800_000;
  const capped = runtime.runTimingAudit(turn);
  assert.equal(capped.effectiveDurationMs, 1_800_000);
  assert.equal(capped.earlyCutReason, 'explicit-runtime-duration-cap');
  assert.deepEqual(turn.contextMetrics.runTimingAudit, capped);
});

for (const protocol of ['openai-completions', 'openai-responses', 'anthropic-messages']) {
  test(`${protocol}: revoked projected memory is removed before generation and final checks detect later edits`, async t => {
    const upstream = await upstreamFixture(t);
    const { runtime } = await fixture(t, { baseUrl: upstream.baseUrl, protocol, models: ['fixture'], maxOutputTokens: 2048 });
    runtime.externalAdmissions.acquire = async () => ({ dispatched() {}, settled() {}, async release() {} });
    await runtime.conversations.ensureConversation('memory-freshness-chat');
    const document = await runtime.memory.create('memory-freshness-chat', { scope: 'chat', content: 'SYNTHETIC_CURRENT_MEMORY' });
    const memoryId = document.entries[0].id;
    const input = { conversationId: 'memory-freshness-chat', message: 'Explain the saved requirements', provider: 'budget-test', model: 'fixture' };
    const turn = await runtime.prepare(input, input.conversationId);
    assert.match(turn.requestOptions.system, /SYNTHETIC_CURRENT_MEMORY/);
    await runtime.memory.update(input.conversationId, memoryId, { scope: 'chat', expectedRevision: 1, content: 'SYNTHETIC_UPDATED_MEMORY' });
    const validation = await runtime.validateFinalEvidence(turn);
    assert.equal(validation.current, false);
    assert.equal(validation.invalidSources[0].code, 'MEMORY_SOURCE_CHANGED');
    await runtime.refreshMemorySystem(turn, turn.requestOptions.system);
    const body = chatRequest(turn.connection, 'fixture', turn.messages, turn.requestOptions).body;
    assert.doesNotMatch(JSON.stringify(body), /SYNTHETIC_CURRENT_MEMORY|SYNTHETIC_UPDATED_MEMORY/);
    assert.deepEqual(turn.contextMetrics.memoryRefresh.removedMemoryIds, [memoryId]);
    assert.equal((await runtime.validateFinalEvidence(turn)).current, false, 'removal alone is not proof that the model received the revocation notice');
    await runtime.consumeModelResponse(turn, 'fixture', system => chatRequest(turn.connection, 'fixture', turn.messages,
      { ...turn.requestOptions, system }), undefined, response => response.json());
    assert.equal((await runtime.validateFinalEvidence(turn)).current, true);
    assert.equal((await runtime.memory.contextFor(input.conversationId)).entries[0].content, 'SYNTHETIC_UPDATED_MEMORY');
    const next = await runtime.prepare({ ...input, message: 'Read the current saved requirements' }, input.conversationId);
    runtime.externalAdmissions.acquire = async () => {
      await runtime.memory.delete(input.conversationId, memoryId, { scope: 'chat', expectedRevision: 2 });
      return { dispatched() {}, settled() {}, async release() {} };
    };
    await runtime.consumeModelResponse(next, 'fixture', system => chatRequest(next.connection, 'fixture', next.messages,
      { ...next.requestOptions, system }), undefined, response => response.json());
    assert.doesNotMatch(JSON.stringify(upstream.requests.at(-1)), /SYNTHETIC_UPDATED_MEMORY|SYNTHETIC_CURRENT_MEMORY/);
    assert.match(JSON.stringify(upstream.requests.at(-1)), /MEMORY_CHANGED/);
    assert.equal(next.lastDispatchedSystem, next.requestOptions.system);
  });
}

test('preparation cannot exhaust the generation lifetime before the prepared request is dispatched', async t => {
  const upstream = await upstreamFixture(t);
  const { runtime } = await fixture(t, { baseUrl: upstream.baseUrl, protocol: 'openai-completions', models: ['fixture'] });
  runtime.streamTimeoutMs = 15;
  const prepare = runtime.prepare.bind(runtime);
  runtime.prepare = async (...args) => { await delay(40); return prepare(...args); };
  const input = { conversationId: 'slow-preparation-chat', message: '你好', provider: 'budget-test', model: 'fixture', runLimits: { maxDurationMs: 1000 } };
  const result = await runtime.sendStream(input, input.conversationId, () => {});
  assert.equal(result.content, 'Fixture answer.');
  assert.equal(result.contextUsage.runTimingAudit.effectiveDurationMs, 1000);
  assert.equal(result.contextUsage.runTimingAudit.includesPreparation, false);
  assert.equal(upstream.requests.length, 1);
});

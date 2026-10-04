import assert from 'node:assert/strict';
import { test } from 'node:test';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { ModelStore } from '../store.mjs';
import { ConversationStore } from '../conversations.mjs';
import { ModelRuntime } from '../runtime.mjs';

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

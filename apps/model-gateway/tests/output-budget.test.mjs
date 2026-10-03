import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { ModelStore, atomicJson, validateConnection } from '../store.mjs';
import { resolveOutputBudget, validateOutputTokens, DEFAULT_MAX_OUTPUT_TOKENS } from '../output-budget.mjs';

test('output ceiling defaults to 256K and stays bounded by the physical window', () => {
  assert.equal(DEFAULT_MAX_OUTPUT_TOKENS, 262_144);
  const result = resolveOutputBudget({ contextWindowTokens: 1_000_000 });
  assert.equal(result.maxOutputTokens, 262_144);
  assert.equal(result.outputBudgetReduced, false);
  const smaller = resolveOutputBudget({ contextWindowTokens: 128_000 });
  assert.equal(smaller.maxOutputTokens, 35_942);
  assert.equal(smaller.inputBudgetTokens, 83_866, 'code/history inputs retain most of a larger window');
  assert.equal(smaller.outputBudgetReduced, true);
  assert.equal(resolveOutputBudget({ contextWindowTokens: 1_000_000, requestedOutputTokens: 65_536 }).maxOutputTokens, 65_536);
});

test('actual provider ceilings constrain 256K without reducing a user configured small ceiling', () => {
  for (const providerMaxOutputTokens of [128_000, 256_000]) {
    const result = resolveOutputBudget({ contextWindowTokens: 1_000_000, providerMaxOutputTokens });
    assert.equal(result.maxOutputTokens, providerMaxOutputTokens);
    assert.equal(result.outputBudgetReductionReason, 'provider_limit');
    assert.ok(result.inputBudgetTokens > 700_000);
    const small = resolveOutputBudget({ contextWindowTokens: 1_000_000, providerMaxOutputTokens, requestedOutputTokens: 2048 });
    assert.equal(small.maxOutputTokens, 2048);
    assert.equal(small.outputBudgetReductionReason, null);
  }
  for (const providerMaxOutputTokens of [null, '128000', 128.5, -1, Infinity])
    assert.throws(() => resolveOutputBudget({ contextWindowTokens: 1_000_000, providerMaxOutputTokens }), { code: 'INVALID_OUTPUT_BUDGET' });
});

test('independent input caps remain enforced even with a small output reserve', () => {
  const result = resolveOutputBudget({ contextWindowTokens: 1_050_000, requestedOutputTokens: 2048,
    providerMaxOutputTokens: 128_000, providerMaxInputTokens: 922_000 });
  assert.equal(result.maxOutputTokens, 2048);
  assert.equal(result.inputBudgetTokens, 922_000 - result.safetyMarginTokens);
  assert.ok(result.inputBudgetTokens + result.maxOutputTokens + result.safetyMarginTokens <= 1_050_000);
  assert.throws(() => resolveOutputBudget({ contextWindowTokens: 1_050_000,
    providerMaxInputTokens: 922_000, requiredInputTokens: 921_000 }), { code: 'CONTEXT_INPUT_TOO_LARGE' });
  for (const providerMaxInputTokens of [null, '922000', 128.5, -1, Infinity])
    assert.throws(() => resolveOutputBudget({ contextWindowTokens: 1_000_000, providerMaxInputTokens }), { code: 'INVALID_OUTPUT_BUDGET' });
});

test('output and required input share the physical window with a bounded safety reserve', () => {
  for (const window of [2048, 8192, 32_768, 128_000, 1_000_000]) {
    const result = resolveOutputBudget({ contextWindowTokens: window });
    assert.equal(result.inputBudgetTokens + result.maxOutputTokens + result.safetyMarginTokens, window);
    assert.ok(result.maxOutputTokens >= 256);
    assert.ok(result.safetyMarginTokens <= 8192);
  }
  const reduced = resolveOutputBudget({ contextWindowTokens: 8192, requestedOutputTokens: 16_384, requiredInputTokens: 6000 });
  assert.equal(reduced.maxOutputTokens, 8192 - 820 - 6000);
  assert.equal(reduced.outputBudgetReduced, true);
  assert.throws(() => resolveOutputBudget({ contextWindowTokens: 2048, requiredInputTokens: 1800 }),
    { code: 'CONTEXT_INPUT_TOO_LARGE', statusCode: 400 });
});

test('output configuration rejects coerced or out of range values', () => {
  for (const value of [null, '16384', 0, 1023, 262145, 16_384.5, Infinity, NaN])
    assert.throws(() => validateOutputTokens(value), { code: 'INVALID_OUTPUT_BUDGET', statusCode: 400 });
  for (const value of [1024, 2048, 8192, 16_384, 65_536, 262_144]) assert.equal(validateOutputTokens(value), value);
});

test('connection persistence keeps explicit small ceilings and upgrades missing fields without losing keys', async t => {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-output-budget-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const store = new ModelStore({ dataHome: root });
  const input = { providerId: 'output-test', displayName: 'Output', baseUrl: 'http://127.0.0.1:1234/v1', models: ['model'], apiKey: 'fixture-key' };
  await atomicJson(store.settingsPath, { version: 1, providers: [{ ...input, contextWindowTokens: 128_000 }] });
  assert.equal((await store.list())[0].maxOutputTokens, 262_144);
  await store.save({ ...input, maxOutputTokens: 2048 });
  await store.save({ ...input, apiKey: '', displayName: 'Renamed' });
  assert.equal((await new ModelStore({ dataHome: root }).connectionFor(input.providerId)).maxOutputTokens, 2048);
  assert.equal((await store.list())[0].contextWindowTokens, 128_000);
  assert.equal((await store.list())[0].hasApiKey, true);
  await store.save({ ...input, maxOutputTokens: 32_768 });
  assert.equal((await store.list())[0].maxOutputTokens, 32_768);
  assert.equal(JSON.stringify(await store.list()).includes('fixture-key'), false);
  assert.throws(() => validateConnection({ ...input, maxOutputTokens: null }), { code: 'INVALID_OUTPUT_BUDGET' });
});

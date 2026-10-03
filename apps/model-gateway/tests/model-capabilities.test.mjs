import assert from 'node:assert/strict';
import { test } from 'node:test';
import { resolveModelCapabilities } from '../model-capabilities.mjs';

test('official model ceilings distinguish context, independent input and output budgets', () => {
  const deepseek = resolveModelCapabilities({ baseUrl: 'https://api.deepseek.com' }, 'deepseek-flash');
  assert.equal(deepseek.contextWindowTokens, 1_048_576);
  assert.equal(deepseek.maxOutputTokens, 393_216, 'adapter defaults must not replace the official maximum');
  const current = resolveModelCapabilities({ baseUrl: 'https://api.openai.com/v1/' }, 'gpt-6-astra');
  assert.equal(current.contextWindowTokens, 1_050_000);
  assert.equal(current.maxInputTokens, 922_000);
  assert.equal(current.maxOutputTokens, 128_000);
  const legacy = resolveModelCapabilities({ baseUrl: 'https://api.openai.com/v1' }, 'gpt-4o');
  assert.equal(legacy.contextWindowTokens, 128_000);
  assert.equal(legacy.maxOutputTokens, 16_384, 'mixed provider catalogs must not inherit newer model limits');
  assert.match(current.source, /^https:\/\/developers\.openai\.com\//);
});

test('only verified exact aliases and dated snapshots acquire limits', () => {
  const official = { baseUrl: 'https://api.anthropic.com/v1' };
  assert.deepEqual(resolveModelCapabilities(official, 'claude-haiku-4-5'),
    resolveModelCapabilities(official, 'claude-haiku-4-5-20251001'));
  assert.equal(resolveModelCapabilities(official, 'claude-opus-5-5').contextWindowTokens, 1_000_000);
  assert.equal(resolveModelCapabilities(official, 'claude-haiku-4-5').maxOutputTokens, 64_000);
  for (const model of ['claude-opus-5-5-custom', 'claude-opus-5-5-20261030', 'CLAUDE-OPUS-5-5'])
    assert.deepEqual(resolveModelCapabilities(official, model), {});
  const openai = { baseUrl: 'https://api.openai.com/v1' };
  assert.equal(resolveModelCapabilities(openai, 'gpt-5.4-mini-2026-03-17').maxInputTokens, 272_000);
  assert.deepEqual(resolveModelCapabilities(openai, 'gpt-4o-2024-05-13'), {}, 'older snapshots can have different ceilings');
});

test('unknown endpoints, local models and unofficial paths do not inherit official cloud limits', () => {
  for (const baseUrl of ['http://127.0.0.1:11434/v1', 'https://proxy.example/v1', 'http://api.openai.com/v1',
    'https://api.openai.com:8443/v1', 'https://api.openai.com.evil.example/v1',
    'https://api.openai.com/another-product/v1', 'https://api.openai.com/v1?key=fixture',
    'https://fixture@api.openai.com/v1', 'https://api.openai.com/v1#fragment', 'invalid'])
    assert.deepEqual(resolveModelCapabilities({ providerId: 'openai', baseUrl }, 'gpt-6-astra'), {});
  assert.deepEqual(resolveModelCapabilities({ baseUrl: 'https://api.openai.com/v1' }, 'ft:gpt-6-astra:fixture'), {});
  assert.deepEqual(resolveModelCapabilities(null, 'gpt-6-astra'), {});
});

test('Kimi output defaults are not mistaken for hard limits, and lookups do not mutate configuration', () => {
  const connection = { baseUrl: 'https://api.moonshot.cn/v1', contextWindowTokens: 8192, maxOutputTokens: 2048 };
  const original = structuredClone(connection);
  const older = resolveModelCapabilities(connection, 'kimi-k2.7-code');
  assert.equal(older.contextWindowTokens, 262_144);
  assert.equal(Object.hasOwn(older, 'maxOutputTokens'), false);
  const current = resolveModelCapabilities(connection, 'kimi-k3');
  assert.equal(current.contextWindowTokens, 1_048_576);
  assert.equal(current.maxOutputTokens, 1_048_576);
  current.contextWindowTokens = 1;
  assert.equal(resolveModelCapabilities(connection, 'kimi-k3').contextWindowTokens, 1_048_576);
  assert.deepEqual(connection, original);
});

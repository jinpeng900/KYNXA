import assert from 'node:assert/strict';
import { test } from 'node:test';
import { resolveModelCapabilities, resolveAutomaticContext } from '../models/model-capabilities.mjs';
import { contextRejection, contextRecoveryLimits, readContextRejection } from '../models/context-recovery.mjs';

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

test('automatic local metadata is bounded, honors runtime over configuration and never widens a small cap', () => {
  const connection = { baseUrl: 'http://localhost:11434/v1', contextWindowTokens: 2_000_000 };
  const current = resolveAutomaticContext(connection, 'local', { backend: 'ollama',
    runtimeContextTokens: 32768, configuredContextTokens: 4096, modelMaximumContextTokens: 65536 });
  assert.equal(current.effectiveTokens, 32768);
  assert.equal(current.source, 'local-runtime');
  for (const invalid of [3_000_000, '1000000', NaN, Infinity, -1]) {
    const result = resolveAutomaticContext(connection, 'local', { backend: 'ollama',
      runtimeContextTokens: invalid, modelMaximumContextTokens: 1_000_000 });
    assert.equal(result.effectiveTokens, 32_768);
    assert.equal(result.reason, 'invalid-local-capability-metadata');
    assert.equal(result.verified, false);
  }
  assert.equal(resolveAutomaticContext(connection, 'local', { backend: 'ollama',
    runtimeContextTokens: 1024, modelMaximumContextTokens: 65536 }).effectiveTokens, 1024,
    'a service window below the supported minimum must be rejected downstream, never rounded up');
});

test('unknown services honor valid saved windows as unverified declarations without inheriting official limits', () => {
  for (const baseUrl of ['https://proxy.example/v1', 'http://localhost:1234/v1']) {
    const connection = Object.freeze({ baseUrl, contextWindowTokens: 131_072 });
    const result = resolveAutomaticContext(connection, 'gpt-6-astra');
    assert.equal(result.effectiveTokens, 131_072);
    assert.equal(result.source, 'configured-unverified');
    assert.equal(result.verified, false);
    assert.equal(result.reason, 'unverified-configured-context');
    assert.equal(result.legacyFieldIgnored, false);
    assert.equal(result.capabilitySource, undefined);
  }
  const local = resolveAutomaticContext({ baseUrl: 'http://localhost:11434/v1', contextWindowTokens: 65_536 },
    'local', { backend: 'ollama' });
  assert.equal(local.effectiveTokens, 65_536);
  assert.equal(local.verified, false);
});

test('missing or invalid declarations use a 32K unverified fallback and verified smaller limits still win', () => {
  for (const contextWindowTokens of [undefined, null, 0, 1024, 2047, 32768.5, '32768', NaN, Infinity, 2_000_001]) {
    const result = resolveAutomaticContext({ baseUrl: 'https://proxy.example/v1', contextWindowTokens }, 'unknown');
    assert.equal(result.effectiveTokens, 32_768);
    assert.equal(result.source, 'conservative-fallback');
    assert.equal(result.verified, false);
  }
  assert.equal(resolveAutomaticContext({ baseUrl: 'invalid', contextWindowTokens: 131_072 }, 'unknown').effectiveTokens, 32_768);
  const local = resolveAutomaticContext({ baseUrl: 'http://localhost:11434/v1', contextWindowTokens: 131_072 },
    'local', { backend: 'ollama', runtimeContextTokens: 4096 });
  assert.equal(local.effectiveTokens, 4096);
  assert.equal(local.verified, true);
  const official = resolveAutomaticContext({ baseUrl: 'https://api.openai.com/v1', contextWindowTokens: 8192 }, 'gpt-4o');
  assert.equal(official.effectiveTokens, 128_000);
  assert.equal(official.verified, true);
  assert.equal(official.legacyFieldIgnored, true);
});

test('context recovery recognizes explicit service limits without confusing unrelated numbers or output caps', () => {
  const rejection = contextRejection({ error: { code: 'context_length_exceeded', message:
    'Maximum context length is 8,192 tokens. The request uses 20000 tokens.' } });
  assert.deepEqual(rejection, { kind: 'context', contextWindowTokens: 8192, verified: true });
  assert.equal(contextRejection({ error: { code: 'invalid_api_key', message: 'Request 8192 failed' } }), null);
  assert.equal(contextRejection({ error: { code: 'invalid_request_error', message: 'Model error 8192' } }), null);
  const output = contextRejection({ error: { message: 'max_tokens must be less than or equal to 4096' } });
  assert.deepEqual(output, { kind: 'output', providerMaxOutputTokens: 4096, verified: true });
  assert.deepEqual(contextRecoveryLimits(output, { contextWindowTokens: 32768, maxOutputTokens: 8192 }),
    { kind: 'output', contextWindowTokens: 32768, providerMaxOutputTokens: 4096, verified: true });
  const input = contextRejection({ error: { message: 'prompt is too long: 12000 tokens > 8192 maximum' } });
  assert.deepEqual(input, { kind: 'input', providerMaxInputTokens: 8192, verified: true });
  assert.equal(contextRecoveryLimits(input, { contextWindowTokens: 32768 }).contextWindowTokens, 32768,
    'an input-only ceiling is not evidence that the total window or output ceiling is 8K');
  assert.equal(contextRecoveryLimits(rejection, { contextWindowTokens: 8192, maxOutputTokens: 1024 }), null);
  assert.equal(contextRecoveryLimits(null, { contextWindowTokens: 32768 }), null);
});

test('rejections without a reported limit use a decreasing, explicitly unverified recovery estimate', () => {
  const rejection = contextRejection({ error: { code: 'context_length_exceeded', message: 'Context is too long.' } });
  const first = contextRecoveryLimits(rejection, { contextWindowTokens: 32768 });
  const second = contextRecoveryLimits(rejection, first);
  assert.equal(first.contextWindowTokens, 16384);
  assert.equal(second.contextWindowTokens, 8192);
  assert.equal(first.verified, false);
  assert.equal(contextRecoveryLimits(rejection, { contextWindowTokens: 2048 }), null);
});

test('HTTP rejection reading is bounded and never classifies authentication, success or oversized bodies as recoverable', async () => {
  const diagnostic = JSON.stringify({ error: { code: 'context_length_exceeded', maximum_context_length: 8192 } });
  assert.equal((await readContextRejection(new Response(diagnostic, { status: 400 }))).contextWindowTokens, 8192);
  for (const status of [200, 401, 403, 500])
    assert.equal(await readContextRejection(new Response(diagnostic, { status })), null);
  assert.equal(await readContextRejection(new Response(JSON.stringify({ error: { code: 'context_length_exceeded',
    message: 'x'.repeat(65_536) } }), { status: 400 })), null);
});

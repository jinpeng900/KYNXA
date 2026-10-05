import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { ModelStore, validateConnection } from '../models/store.mjs';

const connection = { providerId: 'budget-test', displayName: '预算测试',
  baseUrl: 'http://127.0.0.1:8080/v1', models: ['test-model'] };

test('context window accepts supported integer budgets and rejects invalid configuration', () => {
  assert.equal(validateConnection(connection).contextWindowTokens, undefined);
  for (const budget of [2048, 8192, 128000, 1000000, 2000000])
    assert.equal(validateConnection({ ...connection, contextWindowTokens: budget }).contextWindowTokens, budget);
  for (const budget of [null, '8192', 0, 2047, 2000001, 8192.5, Infinity])
    assert.throws(() => validateConnection({ ...connection, contextWindowTokens: budget }), /上下文预算/);
});

test('older model settings clients preserve the saved context window when saving without that field', async t => {
  const dataHome = await mkdtemp(join(tmpdir(), 'kynxa-budget-'));
  t.after(() => rm(dataHome, { recursive: true, force: true }));
  const store = new ModelStore({ dataHome });
  await store.save({ ...connection, contextWindowTokens: 32768 });
  await store.save({ ...connection, displayName: '更新名称' });
  assert.equal((await new ModelStore({ dataHome }).connectionFor(connection.providerId)).contextWindowTokens, 32768);
  assert.equal((await store.list())[0].contextWindowTokens, 32768);
  await store.save({ ...connection, contextWindowTokens: 16384 });
  assert.equal((await store.connectionFor(connection.providerId)).contextWindowTokens, 16384);
});

import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { RerankerService } from '../models/retrieval/reranker-service.mjs';
import { BUILTIN_RERANKER_PROFILE } from '../models/retrieval/reranker-profile.mjs';

test('optional reranking with missing assets stays offline and never loads a model', async t => {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-reranker-missing-'));
  const service = new RerankerService({ modelRoot: root });
  t.after(async () => { await service.close(); await rm(root, { recursive: true, force: true }); });
  assert.equal(service.status().state, 'unavailable');
  assert.equal(service.status().loaded, false);
  assert.equal(service.status().network, false);
  await assert.rejects(service.rerank({ query: 'test', candidates: [{ excerpt: 'test passage' }] }), { code: 'RERANK_ASSET_MISSING' });
  assert.equal(service.status().workerPhase, 'stopped');
});

test('reranking rejects invalid work, pre-cancellation and requests after retirement', async () => {
  const service = new RerankerService();
  const controller = new AbortController(); controller.abort();
  await assert.rejects(service.rerank({ query: 'test', candidates: [], signal: controller.signal }), { name: 'AbortError' });
  await assert.rejects(service.rerank({ query: '', candidates: [] }), { code: 'RERANK_INVALID_INPUT' });
  await assert.rejects(service.rerank({ query: 'test', candidates: [{ excerpt: '' }] }), { code: 'RERANK_INVALID_INPUT' });
  assert.deepEqual((await service.rerank({ query: 'test', candidates: [] })).items, []);
  assert.equal(service.status().loaded, false);
  await service.close();
  await assert.rejects(service.rerank({ query: 'test', candidates: [] }), { code: 'RERANK_CLOSED' });
});

test('a corrupt bundle fails before native inference and does not fabricate a ranking', async t => {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-reranker-corrupt-'));
  for (const asset of BUILTIN_RERANKER_PROFILE.files) {
    await mkdir(join(root, asset.path, '..'), { recursive: true });
    await writeFile(join(root, asset.path), 'synthetic corruption');
  }
  const service = new RerankerService({ modelRoot: root });
  t.after(async () => { await service.close(); await rm(root, { recursive: true, force: true }); });
  await assert.rejects(service.rerank({ query: 'source', candidates: [{ excerpt: 'public fixture' }] }), { code: 'RERANK_ASSET_INVALID' });
  assert.equal(service.status().loaded, false);
  assert.equal(service.status().state, 'error');
});

// The native acceptance pass is explicit: ordinary regression never downloads or starts large models.
// 原生验收需显式启用；常规回归不下载权重，也不启动较大的模型。
test('native reranking retains references, ranks Chinese/English evidence and drains after cancellation',
  { skip: process.env.KYNXA_TEST_NATIVE_RERANK !== '1', timeout: 120_000 }, async t => {
    const service = new RerankerService(); t.after(() => service.close());
    const relevant = { sourceRef: 'public-password', excerpt: 'Reset your account password in Account Settings using the recovery email.' };
    const unrelated = { sourceRef: 'public-weather', excerpt: 'The weather is sunny and the temperature is twenty degrees Celsius.' };
    assert.equal(service.status().loaded, false);
    const original = [unrelated, relevant];
    for (const query of ['How do I reset my account password?', '如何重置账号密码？']) {
      const result = await service.rerank({ query, candidates: original, limit: 2 });
      assert.equal(result.items[0].sourceRef, relevant.sourceRef);
      assert.equal(result.items[0].excerpt, relevant.excerpt);
      assert.deepEqual(original, [unrelated, relevant]);
      assert.equal(result.truncatedInputsCount, 0);
      assert.ok(result.items.every(item => Number.isFinite(item.rerankScore)));
    }
    const controller = new AbortController();
    const pending = service.rerank({ query: 'password recovery', candidates: Array(32).fill(relevant), limit: 32, signal: controller.signal });
    controller.abort();
    await assert.rejects(pending, { name: 'AbortError' });
    const bounded = await service.rerank({ query: '密码', candidates: [{ sourceRef: 'long', excerpt: '密码恢复 '.repeat(1000) }], limit: 1 });
    assert.equal(bounded.truncatedInputsCount, 1);
    assert.equal(bounded.items[0].excerpt.length, 5000);
    assert.equal(service.status().network, false);
  });

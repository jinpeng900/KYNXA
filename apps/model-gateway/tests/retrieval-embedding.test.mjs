import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { EmbeddingService } from '../models/retrieval/embedding-service.mjs';
import { BUILTIN_EMBEDDING_PROFILE, defaultEmbeddingModelRoot } from '../models/retrieval/embedding-profile.mjs';
import { verifyEmbeddingBundle } from '../models/retrieval/embedding-assets.mjs';
import { prepareEmbeddingModel } from '../models/retrieval/prepare-embedding-model.mjs';
import { RetrievalStructureService } from '../data/retrieval/structure-service.mjs';
import { embeddingTextForChunk } from '../data/retrieval/retrieval-text.mjs';

const cosine = (left, right) => left.reduce((sum, value, index) => sum + value * right[index], 0);

test('a stale legacy build lock cannot block local asset preparation after a terminated build', { timeout: 5000 }, async t => {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-embedding-stale-lock-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  await writeFile(join(root, '.prepare.lock'), 'synthetic leftover from a terminated build');
  await assert.rejects(prepareEmbeddingModel({ modelRoot: root, offline: true }), /Pinned embedding asset is unavailable/);
  // The existing file is unrelated to the new OS-owned lock and is preserved.
  // 旧文件不属于新的操作系统锁，保留它也不会阻断准备流程。
});

test('missing local embedding assets do not fall back to downloads or remote text uploads', async t => {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-embedding-missing-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const service = new EmbeddingService({ modelRoot: root });
  t.after(() => service.close());
  assert.equal(service.status().state, 'unavailable');
  assert.equal(service.status().network, false);
  await assert.rejects(service.embedQuery('本地资料'), { code: 'EMBEDDING_ASSET_MISSING' });
  assert.equal(service.status().pendingRequests, 0);
});

test('embedding validates batches and handles cancellation/close before worker startup', async () => {
  const service = new EmbeddingService();
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(service.embedQuery('test', { signal: controller.signal }), { name: 'AbortError' });
  await assert.rejects(service.embedQuery(''), { code: 'EMBEDDING_INVALID_INPUT' });
  await assert.rejects(service.embedQuery('a'.repeat(16_385)), { code: 'EMBEDDING_INPUT_TOO_LONG' });
  await assert.rejects(service.embedDocuments(Array(service.status().requestLimits.maxBatchDocuments + 1).fill('test')), { code: 'EMBEDDING_INVALID_INPUT' });
  assert.deepEqual((await service.embedDocuments([])).vectors, []);
  assert.equal(service.status().loaded, false);
  await service.close();
  await assert.rejects(service.embedQuery('test'), { code: 'EMBEDDING_CLOSED' });
});

test('embedding integrity checking rejects tampered assets before loading native inference', async t => {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-embedding-tampered-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  for (const asset of BUILTIN_EMBEDDING_PROFILE.files) {
    await mkdir(join(root, asset.path, '..'), { recursive: true });
    await writeFile(join(root, asset.path), 'synthetic corrupt asset');
  }
  await assert.rejects(verifyEmbeddingBundle(root), { code: 'EMBEDDING_ASSET_INVALID' });
  const service = new EmbeddingService({ modelRoot: root });
  t.after(() => service.close());
  await assert.rejects(service.embedQuery('text'), { code: 'EMBEDDING_ASSET_INVALID' });
  assert.equal(service.status().state, 'error');
});

test('bundled CPU model embeds Chinese/English, preserves prefixes, and remains asynchronous/offline', { timeout: 120_000 }, async t => {
  const service = new EmbeddingService({ modelRoot: defaultEmbeddingModelRoot() });
  t.after(() => service.close());
  assert.equal(service.status().state, 'ready', 'Run npm run prepare:embedding once when developing from a fresh clone.');
  assert.equal(service.status().loaded, false);
  let ticks = 0;
  const timer = setInterval(() => ticks += 1, 10);
  t.after(() => clearInterval(timer));
  const pending = service.embedDocuments([
    '用户忘记账户密码，可以在账户设置中点击重置密码。',
    '猫喜欢在窗边晒太阳，主人每天给它喂食。',
    'The weather forecast says tomorrow will be sunny.',
  ]);
  assert.equal(service.status().state, 'loading');
  const documents = await pending;
  assert.ok(ticks > 0, 'worker model load and inference leave the main event loop responsive');
  assert.equal(documents.dimensions, 384);
  assert.equal(documents.profileId, 'builtin-multilingual');
  assert.equal(documents.vectors.length, 3);
  for (const vector of documents.vectors) {
    assert.equal(vector.length, 384);
    assert.ok(Math.abs(cosine(vector, vector) - 1) < 1e-4);
  }
  for (const question of ['怎么重置账号密码？', 'How do I reset my account password?']) {
    const query = await service.embedQuery(question);
    const scores = documents.vectors.map(vector => cosine(vector, query.vector));
    assert.equal(scores.indexOf(Math.max(...scores)), 0, 'both languages find the Chinese account document');
  }
  const structures = new RetrievalStructureService();
  t.after(() => structures.close());
  const passages = [];
  for (const [filename, text] of [
    ['account-guide.md', '# Account Settings\n\n## Password Recovery\n\nWhen you forget your account password, select Reset Password in Account Settings.'],
    ['weather-guide.md', '# Weather Guide\n\n## Forecast\n\nThe weather forecast says tomorrow will be sunny.']
  ]) {
    const source = { sourceId: filename, scopeKey: 'user', sourceType: 'document', title: filename,
      locator: { relativePath: filename }, text };
    const prepared = await structures.parse(source);
    const chunk = prepared.chunks.find(item => item.structure.kind === 'paragraph');
    assert.ok(chunk);
    passages.push(embeddingTextForChunk(source, chunk));
  }
  const structuralVectors = await service.embedDocuments(passages);
  const chineseQuery = await service.embedQuery('如何重置账户密码？');
  const structuralScores = structuralVectors.vectors.map(vector => cosine(vector, chineseQuery.vector));
  assert.equal(structuralScores.indexOf(Math.max(...structuralScores)), 0,
    'real local vectors preserve Chinese-to-English retrieval with structural headers');
  await assert.rejects(service.embedQuery('中文句子 '.repeat(600)), error => error.code === 'EMBEDDING_INPUT_TOO_LONG'
    && error.details.tokenCount > error.details.maxInputTokens);
  const controller = new AbortController();
  const cancelled = service.embedDocuments(Array(64).fill('A document about account password recovery.'), { signal: controller.signal });
  controller.abort();
  await assert.rejects(cancelled, { name: 'AbortError' });
  assert.equal((await service.embedQuery('取消之后仍然可用')).vector.length, 384);
  assert.equal(service.status().state, 'ready');
  assert.equal(service.status().network, false);
  assert.ok(service.status().cpuThreads >= 1 && service.status().cpuThreads <= 32);
  assert.equal(service.status().inferenceBackend.cpuThreads, service.status().cpuThreads);
});

import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { test } from 'node:test';
import { EmbeddingService } from '../models/retrieval/embedding-service.mjs';
import { BUILTIN_EMBEDDING_PROFILE } from '../models/retrieval/embedding-profile.mjs';
import { EMBEDDING_DOCUMENT_FITTING_VERSION, fitEmbeddingDocuments } from '../models/retrieval/embedding-document-fit.mjs';

class FittingProcess extends EventEmitter {
  messages = [];
  ref() {}
  unref() {}
  postMessage(message) {
    this.messages.push(message);
    if (message.type === 'close') setImmediate(() => {
      this.emit('message', { type: 'closed', disposed: true });
      this.emit('exit', 0);
    });
  }
}

async function waitForDispatch(worker, previousCount = 0) {
  for (let attempt = 0; attempt < 100; attempt++) {
    const message = worker.messages.slice(previousCount).find(item => item.type === 'fit-documents');
    if (message) return message;
    await new Promise(resolve => setTimeout(resolve, 10));
  }
  assert.fail('Fitting request dispatches after acquiring its resource budget.');
}

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-embedding-fit-'));
  for (const asset of BUILTIN_EMBEDDING_PROFILE.files) {
    await mkdir(dirname(join(root, asset.path)), { recursive: true });
    await writeFile(join(root, asset.path), 'Synthetic transport assets; not loaded by a tokenizer or model.');
  }
  const worker = new FittingProcess();
  const service = new EmbeddingService({ modelRoot: root, workerFactory: () => worker });
  t.after(async () => { await service.close(); await rm(root, { recursive: true, force: true }); });
  return { service, worker };
}

function assertCoverage(text, document) {
  let cursor = 0;
  for (const segment of document.segments) {
    assert.equal(segment.start, cursor);
    assert.ok(segment.end > cursor && segment.end <= text.length);
    assert.equal(segment.end < text.length && /[\uD800-\uDBFF]/u.test(text[segment.end - 1]) &&
      /[\uDC00-\uDFFF]/u.test(text[segment.end]), false, 'a range cannot split a Unicode codepoint');
    cursor = segment.end;
  }
  assert.equal(cursor, text.length);
  assert.equal(document.segments.map(segment => text.slice(segment.start, segment.end)).join(''), text);
}

test('token fitting preserves every raw Unicode character and counts repeated context in each segment', async () => {
  const text = '😀界🙂汉字 e\u0301\nZ'.repeat(5), context = 'H\n';
  const countTokens = input => [...input].length + 2;
  const [document] = await fitEmbeddingDocuments([{ text, context }], {
    countTokens, documentPrefix: 'passage: ', maxInputTokens: 24,
  });
  assert.ok(document.segments.length > 1);
  assertCoverage(text, document);
  assert.equal(document.tokenCount, countTokens(`passage: ${context}${text}`));
  for (const segment of document.segments) {
    assert.equal(segment.tokenCount, countTokens(`passage: ${context}${text.slice(segment.start, segment.end)}`));
    assert.ok(segment.tokenCount <= 24);
  }
});

test('fitting supports strings and context objects without claiming that a native session loaded', async t => {
  const { service, worker } = await fixture(t);
  const pending = service.fitDocuments(['A😀B', { text: 'Document body.', context: 'Title: Fixture\n\n' }]);
  const dispatched = await waitForDispatch(worker);
  assert.deepEqual(dispatched.documents, [{ text: 'A😀B', context: '' }, { text: 'Document body.', context: 'Title: Fixture\n\n' }]);
  worker.emit('message', { type: 'tokenizer-ready' });
  worker.emit('message', { type: 'result', id: dispatched.id, documents: [
    { tokenCount: 8, segments: [{ start: 0, end: 4, tokenCount: 8 }] },
    { tokenCount: 10, segments: [{ start: 0, end: 14, tokenCount: 10 }] },
  ] });
  const result = await pending;
  assert.equal(result.maxInputTokens, 512);
  assert.equal(result.fittingVersion, EMBEDDING_DOCUMENT_FITTING_VERSION);
  assert.equal(result.embeddingSpaceId, service.status().embeddingSpaceId);
  assert.equal(service.status().fittingVersion, result.fittingVersion);
  assert.equal(service.status().assetVerification, 'verified');
  assert.equal(service.status().state, 'ready');
  assert.equal(service.status().loaded, false);
});

test('fitting rejects gaps, omitted body, excess tokens and surrogate splits before exposing source ranges', async t => {
  const { service, worker } = await fixture(t);
  const malformed = [
    [{ start: 0, end: 2, tokenCount: 7 }, { start: 2, end: 4, tokenCount: 7 }],
    [{ start: 0, end: 1, tokenCount: 7 }, { start: 2, end: 4, tokenCount: 7 }],
    [{ start: 0, end: 1, tokenCount: 7 }],
    [{ start: 0, end: 4, tokenCount: 513 }],
  ];
  for (const segments of malformed) {
    const previousCount = worker.messages.length;
    const pending = service.fitDocuments(['A😀B']);
    const { id } = await waitForDispatch(worker, previousCount);
    worker.emit('message', { type: 'result', id, documents: [{ tokenCount: 9, segments }] });
    await assert.rejects(pending, { code: 'EMBEDDING_INVALID_FIT_RESULT' });
    assert.equal(service.status().pendingRequests, 0);
  }
});

test('fitting validates inputs and ignores late responses after cancellation or close', async t => {
  const { service, worker } = await fixture(t);
  assert.deepEqual((await service.fitDocuments([])).documents, []);
  await assert.rejects(service.fitDocuments(['']), { code: 'EMBEDDING_INVALID_INPUT' });
  await assert.rejects(service.fitDocuments([{ text: 'Body', context: 2 }]), { code: 'EMBEDDING_INVALID_INPUT' });
  await assert.rejects(service.fitDocuments(Array(service.status().requestLimits.maxBatchDocuments + 1).fill('Body')), { code: 'EMBEDDING_INVALID_INPUT' });
  await assert.rejects(service.fitDocuments(['a'.repeat(16_385)]), { code: 'EMBEDDING_INPUT_TOO_LONG' });
  const preCancelled = new AbortController();
  preCancelled.abort();
  await assert.rejects(service.fitDocuments([], { signal: preCancelled.signal }), { name: 'AbortError' });
  const controller = new AbortController();
  const cancelled = service.fitDocuments(['A😀B'], { signal: controller.signal });
  controller.abort();
  await assert.rejects(cancelled, { name: 'AbortError', code: 'EMBEDDING_CANCELLED' });
  worker.emit('message', { type: 'result', id: 1, documents: [] });
  assert.equal(service.status().pendingRequests, 0);
  const pending = service.fitDocuments(['A😀B']);
  const close = service.close();
  await assert.rejects(pending, { code: 'EMBEDDING_CLOSED' });
  await close;
  await assert.rejects(service.fitDocuments([]), { code: 'EMBEDDING_CLOSED' });
});

// This small acceptance test needs existing local assets; it never prepares/downloads weights or opens product data.
// 小型验收只使用现有本地资产，不准备或下载权重，也不打开产品数据。
test('real E5 tokenizer fits Chinese, dense code and Unicode bodies for unchanged offline embedding',
  { skip: process.env.KYNXA_TEST_NATIVE_EMBEDDING_FIT !== '1', timeout: 45000 }, async t => {
    const service = new EmbeddingService();
    t.after(() => service.close());
    assert.equal(service.status().state, 'ready', 'The pinned local model assets must already exist.');
    const inputs = [
      { text: '中文说明取消重试权限索引并发异步路径恢复😀🙂𠀋。\n'.repeat(35), context: 'Title: 真实预算验收\n\n' },
      { text: 'const a9_ß😀 = savedSourceValue42 + 0xFf;\n'.repeat(45), context: 'Symbol: savedSourceValue42\n\n' },
    ];
    const fitted = await service.fitDocuments(inputs);
    assert.equal(service.status().loaded, false, 'measurement loads the tokenizer without a native model session');
    assert.equal(service.status().assetVerification, 'verified');
    assert.ok(fitted.documents.every(document => document.tokenCount > 512 && document.segments.length > 1));
    const projected = [];
    for (let index = 0; index < inputs.length; index += 1) {
      const input = inputs[index], document = fitted.documents[index];
      assertCoverage(input.text, document);
      for (const segment of document.segments) {
        assert.ok(segment.tokenCount <= 512);
        const text = input.text.slice(segment.start, segment.end);
        const measured = await service.fitDocuments([{ text, context: input.context }]);
        assert.equal(measured.documents[0].tokenCount, segment.tokenCount);
        assert.equal(measured.documents[0].segments.length, 1);
        projected.push(`${input.context}${text}`);
      }
    }
    await assert.rejects(service.fitDocuments([{ text: '正文', context: '上下文 '.repeat(600) }]),
      { code: 'EMBEDDING_CONTEXT_TOO_LONG' });
    assert.equal((await service.fitDocuments(['正文'])).documents[0].segments.length, 1);
    const result = await service.embedDocuments(projected);
    assert.equal(result.vectors.length, projected.length);
    assert.ok(result.vectors.every(vector => vector.length === 384 && vector.every(Number.isFinite)));
    assert.equal(service.status().loaded, true);
    assert.equal(service.status().network, false);
    t.diagnostic(`Actual token counts ${fitted.documents.map(document => document.tokenCount).join(', ')}; ` +
      `${projected.length} complete slices embedded as 384-dimensional vectors on ${process.platform} ${process.version}.`);
  });

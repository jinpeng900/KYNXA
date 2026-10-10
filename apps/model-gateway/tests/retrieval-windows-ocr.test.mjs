import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { PassThrough } from 'node:stream';
import { test } from 'node:test';
import { recognizePdfPages } from '../tools/retrieval/windows-document-ocr.mjs';

// Native protocol and ownership tests use inert executables and stream doubles; no OCR model or personal file is read.
// 原生协议与生命周期测试使用不可执行样本和流替身；不读取 OCR 模型或个人文件。
async function fixture(t, action) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-windows-ocr-')), toolHostPath = join(root, 'synthetic-helper.exe');
  await writeFile(toolHostPath, 'Inert test fixture');
  t.after(async () => {
    const suffix = relative(resolve(tmpdir()), root);
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(root, { recursive: true, force: true });
  });
  const events = [], child = new EventEmitter();
  child.pid = 123; child.stdin = new PassThrough(); child.stdout = new PassThrough();
  child.kill = () => { events.push('kill'); queueMicrotask(() => child.emit('close', 1)); };
  child.stdin.on('data', chunk => action(JSON.parse(chunk.toString('utf8')), child, events));
  const spawnFactory = (executable, args, options) => {
    assert.equal(executable, toolHostPath); assert.deepEqual(args, ['--document-ocr']);
    assert.equal(options.windowsHide, true); assert.equal(options.shell, false); events.push('spawn'); return child;
  };
  const resourceService = { acquire: async request => { assert.ok(request.memoryBytes >= 384 * 1024 * 1024);
    assert.equal(request.kind, 'background'); events.push('acquire'); return { leaseId: 'synthetic-ocr', status: 'granted' }; },
  registerExecutor: async (_lease, request) => { assert.equal(request.processId, 123); events.push('register'); return { status: 'registered' }; },
  renew: async () => {}, report: async () => {}, release: async () => events.push('release') };
  return { child, events, options: { toolHostPath, spawnFactory, resourceService } };
}

function receipt(pages) {
  return { protocolVersion: 1, schemaVersion: 1, boundary: 'document-ocr', completed: true,
    version: 'windows-media-ocr-v1|fixture-model', language: 'en-US', pages };
}

test('Windows OCR sends only authorized bytes and page IDs after executor registration and releases after close', async t => {
  const f = await fixture(t, (request, child, events) => {
    assert.equal(request.operation, 'document_ocr'); assert.equal(request.bytesBase64, Buffer.from('PDF bytes').toString('base64'));
    assert.deepEqual(request.pages, [101]); assert.equal(request.path, undefined);
    assert.equal(events.at(-1), 'register'); assert.equal(child.stdin.writableEnded, false);
    events.push('recognize'); child.stdout.write(JSON.stringify(receipt([{ page: 101, text: 'Known recognized text' }])));
    queueMicrotask(() => { events.push('close'); child.emit('close', 0); });
  });
  const result = await recognizePdfPages(Buffer.from('PDF bytes'), [101], f.options);
  assert.equal(result.pages[0].text, 'Known recognized text');
  assert.deepEqual(f.events, ['acquire', 'spawn', 'register', 'recognize', 'close', 'release']);
});

test('OCR cancellation sends native cancel and holds its reservation until recognition actually closes', async t => {
  let entered, acknowledge;
  const ready = new Promise(resolveReady => { entered = resolveReady; }), controller = new AbortController();
  const f = await fixture(t, (request, child, events) => {
    if (request.operation === 'document_ocr') { entered(); return; }
    assert.equal(request.operation, 'cancel'); events.push('cancel');
    acknowledge = () => { events.push('close'); child.emit('close', 0); };
  });
  const request = recognizePdfPages(Buffer.from('PDF bytes'), [1], { ...f.options, signal: controller.signal });
  const stopped = assert.rejects(request, { name: 'AbortError' });
  await ready; controller.abort(); await new Promise(resolveWait => setImmediate(resolveWait));
  assert.equal(f.events.includes('release'), false); acknowledge(); await stopped;
  assert.deepEqual(f.events.slice(-3), ['cancel', 'close', 'release']);
});

test('OCR language failures and mismatched page/version receipts cannot become extracted source text', async t => {
  for (const result of [{ errorCode: 'OCR_LANGUAGE_UNAVAILABLE' }, receipt([{ page: 2, text: 'Wrong page' }]),
    { ...receipt([{ page: 1, text: 'Untrusted provider' }]), version: 'other-provider' }]) {
    await t.test(JSON.stringify(result), async childTest => {
      const f = await fixture(childTest, (_request, child) => {
        child.stdout.write(JSON.stringify(result)); queueMicrotask(() => child.emit('close', result.errorCode ? 1 : 0));
      });
      await assert.rejects(recognizePdfPages(Buffer.from('PDF bytes'), [1], f.options),
        { code: result.errorCode ?? 'OCR_INVALID_RESULT' });
      assert.equal(f.events.at(-1), 'release');
    });
  }
});

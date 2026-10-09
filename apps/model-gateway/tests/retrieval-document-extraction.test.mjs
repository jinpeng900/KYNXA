import assert from 'node:assert/strict';
import { fork } from 'node:child_process';
import { createHash } from 'node:crypto';
import { EventEmitter } from 'node:events';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { test } from 'node:test';
import JSZip from 'jszip';
import { extractDocumentBytes } from '../tools/retrieval/document-extraction.mjs';
import { DOCUMENT_EXTRACTION_LIMITS, validateDocumentExtractionLimits } from '../tools/retrieval/document-extraction-contracts.mjs';
import { classifySourceFailure, readSourceFile, readSourceFileDocumentWindows, scanSourcePaths, scanSourceTree, sourceExtractionVersion } from '../tools/retrieval/source-reader.mjs';
import { SourceLibrary } from '../data/retrieval/source-library.mjs';
import { RetrievalIndex } from '../data/retrieval/index.mjs';
import { RetrievalJobStore } from '../data/retrieval/job-store.mjs';
import { DEFAULT_RETRIEVAL_SETTINGS } from '../data/retrieval/settings.mjs';
import { SourceIndexService } from '../orchestration/retrieval/source-manager.mjs';
import { RetrievalCoordinator } from '../orchestration/retrieval/coordinator.mjs';
import { sourceFileRevision } from '../data/retrieval/retrieval-contracts.mjs';

const sha256 = value => createHash('sha256').update(value).digest('hex');
const xmlEscape = value => value.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;');

async function docxFixture(paragraphs = ['Known hello 中文。', 'Second known paragraph.'], extra = {}) {
  const zip = new JSZip();
  zip.file('[Content_Types].xml', '<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">' +
    '<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>');
  zip.file('word/document.xml', '<?xml version="1.0"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>' +
    paragraphs.map(text => `<w:p><w:r><w:t>${xmlEscape(text)}</w:t></w:r></w:p>`).join('') + '</w:body></w:document>');
  for (const [path, body] of Object.entries(extra)) zip.file(path, body);
  return zip.generateAsync({ type: 'nodebuffer', compression: 'DEFLATE' });
}

/** Small authored PDF fixtures exercise the actual library; this generates known objects, never parses source PDFs.
 * 小型自有 PDF 样本验证真实库；这里只生成已知对象，不解析来源 PDF，也不使用用户资料。 */
function pdfFixture(pageTexts, { chineseFont = false, paddingBytes = 0 } = {}) {
  const objects = ['', ''];
  const add = body => { objects.push(body); return objects.length; };
  const stream = value => `<< /Length ${Buffer.byteLength(value)} >>\nstream\n${value}\nendstream`;
  let fontId;
  if (chineseFont) {
    const cidId = add('<< /Type /Font /Subtype /CIDFontType0 /BaseFont /STSong-Light /CIDSystemInfo << /Registry (Adobe) /Ordering (GB1) /Supplement 4 >> >>');
    const glyphs = [...new Set(pageTexts.join(''))].map(character => Buffer.from(character, 'utf16le').swap16().toString('hex'));
    const unicodeId = add(stream('/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n' +
      '/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n/CMapName /Known-Unicode def /CMapType 2 def\n' +
      `1 begincodespacerange <0000> <FFFF> endcodespacerange\n${glyphs.length} beginbfchar\n` +
      glyphs.map(hex => `<${hex}> <${hex}>`).join('\n') + '\nendbfchar endcmap CMapName currentdict /CMap defineresource pop end end'));
    fontId = add(`<< /Type /Font /Subtype /Type0 /BaseFont /STSong-Light /Encoding /UniGB-UCS2-H /DescendantFonts [${cidId} 0 R] /ToUnicode ${unicodeId} 0 R >>`);
  } else fontId = add('<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>');
  const pageIds = pageTexts.map(text => {
    const encoded = chineseFont ? `<${Buffer.from(text, 'utf16le').swap16().toString('hex')}>`
      : `(${text.replaceAll('\\', '\\\\').replaceAll('(', '\\(').replaceAll(')', '\\)')})`;
    const contentId = add(stream(text ? `BT /F1 18 Tf 30 150 Td ${encoded} Tj ET` : ''));
    return add(`<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources << /Font << /F1 ${fontId} 0 R >> >> /Contents ${contentId} 0 R >>`);
  });
  objects[0] = '<< /Type /Catalog /Pages 2 0 R >>';
  objects[1] = `<< /Type /Pages /Count ${pageIds.length} /Kids [${pageIds.map(id => `${id} 0 R`).join(' ')}] >>`;
  let body = '%PDF-1.7\n' + (paddingBytes ? `%${' '.repeat(paddingBytes)}\n` : '');
  const offsets = [0];
  for (const [index, object] of objects.entries()) {
    offsets.push(Buffer.byteLength(body)); body += `${index + 1} 0 obj\n${object}\nendobj\n`;
  }
  const xrefOffset = Buffer.byteLength(body);
  body += `xref\n0 ${offsets.length}\n0000000000 65535 f \n` + offsets.slice(1).map(offset => `${String(offset).padStart(10, '0')} 00000 n \n`).join('') +
    `trailer\n<< /Size ${offsets.length} /Root 1 0 R >>\nstartxref\n${xrefOffset}\n%%EOF\n`;
  return Buffer.from(body);
}

async function sourceFixture(t) {
  const directory = await mkdtemp(join(tmpdir(), 'kynxa-document-extraction-'));
  t.after(async () => {
    const suffix = relative(resolve(tmpdir()), directory);
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(directory, { recursive: true, force: true });
  });
  return directory;
}

function fakeDecoder(action) {
  const child = new EventEmitter();
  child.kill = () => { queueMicrotask(() => { child.emit('exit', 0); child.emit('close', 0); }); return true; };
  child.send = message => queueMicrotask(() => action(child, message));
  return child;
}

test('real offline PDF extraction preserves Chinese CMap text and exact UTF-16 page ranges without rewriting bytes', async t => {
  const root = await sourceFixture(t), path = join(root, 'manual.pdf');
  const bytes = pdfFixture(['中文离线提取', '第二页中文'], { chineseFont: true });
  await writeFile(path, bytes);
  const file = await readSourceFile(path, { root });
  assert.equal(file.text, '中文离线提取\n\n第二页中文');
  assert.deepEqual(file.extraction.pages, [{ page: 1, startOffset: 0, endOffset: 6 }, { page: 2, startOffset: 8, endOffset: 13 }]);
  assert.equal(file.extraction.pageCount, 2);
  assert.equal(file.extraction.version, sourceExtractionVersion(path));
  assert.equal(file.extraction.rawContentHash, sha256(bytes));
  assert.equal(file.contentHash, sha256(file.text));
  assert.equal(file.textBytes, Buffer.byteLength(file.text));
  assert.deepEqual(Object.keys(file.metadata).sort(), ['ctimeMs', 'device', 'inode', 'mtimeMs', 'sizeBytes']);
  assert.deepEqual(await readFile(path), bytes);
});

test('real DOCX raw text keeps Chinese paragraphs, byte provenance and no invented rendered pages', async t => {
  const root = await sourceFixture(t), path = join(root, 'manual.docx'), bytes = await docxFixture();
  await writeFile(path, bytes);
  const file = await readSourceFile(path, { root });
  assert.equal(file.text, 'Known hello 中文。\n\nSecond known paragraph.\n\n');
  assert.deepEqual(file.extraction, { format: 'docx', version: sourceExtractionVersion(path), rawContentHash: sha256(bytes) });
  assert.equal(file.contentHash, sha256(file.text));
  assert.deepEqual(await readFile(path), bytes);
  assert.equal(sourceExtractionVersion('existing.py'), null);
});

test('configured PDF byte budgets admit larger containers and register the actual decoder process', async t => {
  const root = await sourceFixture(t), path = join(root, 'larger-container.pdf');
  const bytes = pdfFixture(['Known bounded text'], { paddingBytes: 3 * 1024 * 1024 });
  await writeFile(path, bytes); let processId, released = false;
  const resourceService = { acquire: async () => ({ status: 'granted', leaseId: 'real-decoder' }), renew: async () => {},
    registerExecutor: async (leaseId, executor) => { processId = executor.processId; assert.doesNotThrow(() => process.kill(processId, 0));
      return { status: 'registered' }; },
    release: async () => { assert.throws(() => process.kill(processId, 0), { code: 'ESRCH' }); released = true; } };
  const file = await readSourceFile(path, { root, resourceService });
  assert.equal(file.text, 'Known bounded text'); assert.equal(file.extraction.rawContentHash, sha256(bytes));
  assert.ok(Number.isSafeInteger(processId)); assert.equal(released, true);
  await assert.rejects(readSourceFile(path, { root, maximumSourceBytes: 2 * 1024 * 1024 }), { code: 'DOCUMENT_BYTES_LIMIT' });
});

test('PDF page limits create explicit recoverable windows while oversized pages and missing text fail honestly', async () => {
  const first = await extractDocumentBytes(pdfFixture(Array.from({ length: 101 }, () => 'Known page')), 'pdf');
  assert.equal(first.extraction.complete, false); assert.equal(first.extraction.pageCount, 101);
  assert.equal(first.extraction.pages.length, 100); assert.equal(first.extraction.pageWindow.nextPage, 101);
  assert.deepEqual(first.extraction.uncoveredRanges, [{ startPage: 101, endPage: 101 }]);
  const limited = await extractDocumentBytes(pdfFixture(['Hello', 'Again']), 'pdf', { maximumPages: 1 });
  assert.equal(limited.extraction.pages.length, 1); assert.equal(limited.extraction.pageWindow.nextPage, 2);
  await assert.rejects(extractDocumentBytes(pdfFixture(['Known visible text']), 'pdf', { maximumOutputBytes: 4 }), error => {
    assert.equal(error.code, 'DOCUMENT_OUTPUT_LIMIT');
    assert.equal(error.details.documentCoverage.pageCount, 1); assert.equal(error.details.documentCoverage.processedPages, 0);
    assert.equal(error.details.documentCoverage.limit.dimension, 'maximumOutputBytes');
    return true;
  });
  await assert.rejects(extractDocumentBytes(pdfFixture(['']), 'pdf', { ocrBackend: null }), error => error.code === 'OCR_UNAVAILABLE' &&
    assert.deepEqual(error.details.pages, [1]) === undefined);
  await assert.rejects(extractDocumentBytes(pdfFixture(['Visible', '']), 'pdf', { ocrBackend: null }), error => error.code === 'OCR_UNAVAILABLE' &&
    assert.deepEqual(error.details.pages, [2]) === undefined &&
    assert.equal(error.details.documentCoverage.processedPages, 2) === undefined &&
    assert.equal(error.details.documentCoverage.publishedPages, 0) === undefined);
});

test('PDF windows continue beyond 100 pages, preserve absolute page numbers and reject a cursor from changed bytes', async t => {
  const root = await sourceFixture(t), path = join(root, 'large.pdf');
  const bytes = pdfFixture(Array.from({ length: 205 }, (_, index) => `Known page ${index + 1}`));
  await writeFile(path, bytes);
  const windows = [];
  for await (const file of readSourceFileDocumentWindows(path)) windows.push(file);
  assert.deepEqual(windows.map(file => [file.extraction.pageWindow.startPage, file.extraction.pageWindow.endPage]), [[1, 100], [101, 200], [201, 205]]);
  assert.ok(windows.every(file => file.extraction.pageCount === 205 && file.extraction.complete === false));
  assert.ok(windows.every(file => file.extraction.rawContentHash === sha256(bytes)));
  assert.equal(new Set(windows.map(file => file.documentWindowKey)).size, 3);
  assert.equal(windows[1].extraction.pages[0].page, 101); assert.equal(windows[1].extraction.pages[0].startOffset, 0);
  const library = new SourceLibrary({ root: join(root, 'data') });
  const imported = await library.add(windows);
  assert.equal(imported.sources.length, 3);
  assert.equal((await library.add(windows)).sources.length, 3);
  assert.equal((await library.list()).sources.length, 3, 'same page windows deduplicate without merging partial text into a fake full source');
  assert.equal((await library.readSource(imported.sources[1].id, { scopeKeys: ['user'] })).locator.extraction.pageWindow.startPage, 101);
  const cursor = readSourceFileDocumentWindows(path, { maximumPdfPages: 2 });
  await cursor.next();
  await writeFile(path, pdfFixture(['Different revision', 'New second page', 'New third page']));
  await assert.rejects(cursor.next(), { code: 'DOCUMENT_SOURCE_CHANGED' });
});

test('output budgets split complete original pages into resumable windows without truncating a page', async t => {
  const root = await sourceFixture(t), path = join(root, 'bounded.pdf');
  await writeFile(path, pdfFixture(['First', 'Second', 'Third']));
  const windows = [];
  for await (const file of readSourceFileDocumentWindows(path, { maximumDocumentOutputBytes: 8 })) windows.push(file);
  assert.deepEqual(windows.map(file => file.text), ['First', 'Second', 'Third']);
  assert.deepEqual(windows.map(file => file.extraction.pages[0].page), [1, 2, 3]);
  const controller = new AbortController(), cancelled = readSourceFileDocumentWindows(path, { maximumPdfPages: 1, signal: controller.signal });
  await cancelled.next(); controller.abort();
  await assert.rejects(cancelled.next(), { name: 'AbortError' });
});

test('OCR broker fills image pages, binds its model identity and keeps rendered windows small', async () => {
  const calls = [], bytes = pdfFixture(['Visible text', '', '', 'Last visible page']);
  const ocrBackend = async (input, pages, { signal }) => {
    assert.equal(sha256(input), sha256(bytes)); signal.throwIfAborted(); calls.push(...pages);
    return { version: 'windows-media-ocr-v1|fixture-model', language: 'en-US', pages: pages.map(page => ({ page, text: `Recognized original image page ${page}` })) };
  };
  const first = await extractDocumentBytes(bytes, 'pdf', { ocrBackend });
  assert.deepEqual(calls, [2, 3]); assert.equal(first.extraction.pageWindow.nextPage, 4);
  assert.equal(first.extraction.complete, false);
  assert.equal(first.extraction.pages[1].method, 'ocr'); assert.equal(first.extraction.ocr.version, 'windows-media-ocr-v1|fixture-model');
  assert.notEqual(sourceFileRevision({ ...first, contentHash: sha256(first.text) }), sourceFileRevision({ ...first,
    contentHash: sha256(first.text), extraction: { ...first.extraction, ocr: { ...first.extraction.ocr, version: 'windows-media-ocr-v1|new-model' } } }));
  const last = await extractDocumentBytes(bytes, 'pdf', { ocrBackend,
    pageWindow: { startPage: 4, rawContentHash: first.extraction.rawContentHash } });
  assert.equal(last.text, 'Last visible page'); assert.equal(last.extraction.pages[0].page, 4);
  assert.notEqual(sourceFileRevision({ ...first, contentHash: sha256(first.text) }), sourceFileRevision({ ...last, contentHash: sha256(last.text) }));
});

test('unchanged image PDFs recover after OCR capability becomes available without a permanent failure cache', async t => {
  const root = await sourceFixture(t), path = join(root, 'recovered-ocr.pdf'), failureCache = new Map();
  await writeFile(path, pdfFixture(['']));
  let available = false;
  const ocrBackend = async (_bytes, pages) => {
    if (!available) throw Object.assign(new Error('Synthetic missing language'), { code: 'OCR_LANGUAGE_UNAVAILABLE' });
    return { version: 'windows-media-ocr-v1|fixture-model', language: 'en-US', pages: pages.map(page => ({ page, text: 'Recovered known image text' })) };
  };
  const failedStats = {};
  for await (const file of scanSourceTree(root, { failureCache, stats: failedStats, ocrBackend })) assert.fail(file);
  assert.equal(failedStats.failures[0].errorCode, 'OCR_LANGUAGE_UNAVAILABLE');
  failureCache.get('recovered-ocr.pdf').cachedAt -= 6000; available = true;
  const recovered = [];
  for await (const file of scanSourceTree(root, { failureCache, ocrBackend })) recovered.push(file);
  assert.equal(recovered[0].text, 'Recovered known image text'); assert.equal(failureCache.size, 0);
});

test('decoder cancellation waits for admitted OCR broker settlement before acknowledging the extraction', async () => {
  const controller = new AbortController();
  let entered, settle;
  const ready = new Promise(resolve => { entered = resolve; });
  const ocrBackend = async (_bytes, _pages, { signal }) => {
    entered();
    await new Promise(resolve => { settle = resolve; signal.addEventListener('abort', () => {}, { once: true }); });
    signal.throwIfAborted();
  };
  const forkFactory = () => fakeDecoder((child, message) => {
    if (message.type === 'decode') child.emit('message', { type: 'ocr-request', requestId: 1, pages: [1] });
  });
  let acknowledged = false;
  const extraction = extractDocumentBytes(Buffer.from('authorized fixture bytes'), 'pdf', { signal: controller.signal, forkFactory, ocrBackend });
  const stopped = assert.rejects(extraction, { name: 'AbortError' }).then(() => { acknowledged = true; });
  await ready; controller.abort(); await new Promise(resolve => setImmediate(resolve));
  assert.equal(acknowledged, false); settle(); await stopped;
});

test('configured document limits invalidate old receipts and keep original navigation in scans and failure-cache reuse', async t => {
  const root = await sourceFixture(t), path = join(root, 'limited.pdf'), bytes = pdfFixture(['First', 'Second']);
  await writeFile(path, bytes);
  const previous = await readSourceFile(path), failureCache = new Map(), stats = {};
  const windows = [];
  for await (const file of scanSourceTree(root, { stats, failureCache, maximumPdfPages: 1,
    previousFiles: new Map([['limited.pdf', previous]]) })) windows.push(file);
  assert.equal(windows.length, 2); assert.equal(windows[0].reused, undefined);
  assert.equal(windows[0].extraction.pageWindow.endPage, 1); assert.equal(windows[0].extraction.complete, false);
  await assert.rejects(readSourceFile(path, { maximumDocumentInputBytes: 4 }), error => {
    assert.equal(error.code, 'DOCUMENT_BYTES_LIMIT');
    assert.deepEqual(error.details.documentCoverage.limit, { dimension: 'maximumInputBytes', limit: 4, observed: bytes.length });
    return true;
  });
  await writeFile(path, pdfFixture(['Visible', '']));
  const ocrStats = {};
  for await (const file of scanSourceTree(root, { stats: ocrStats, failureCache, ocrBackend: null })) assert.fail(file);
  const reuseStats = {};
  for await (const file of scanSourceTree(root, { stats: reuseStats, failureCache, ocrBackend: null })) assert.fail(file);
  assert.equal(reuseStats.reusedFailures, 1);
  assert.deepEqual(reuseStats.failures[0].documentCoverage, ocrStats.failures[0].documentCoverage);
});

test('a new over-output PDF flows through real mounted discovery into a durable partial job and scoped SQLite coverage', async t => {
  const root = await sourceFixture(t), workspace = join(root, 'workspace'), data = join(root, 'data');
  await mkdir(workspace);
  await writeFile(join(workspace, 'over-page.pdf'), pdfFixture(['First', 'Second']));
  const settings = { ...structuredClone(DEFAULT_RETRIEVAL_SETTINGS), projectId: 'coverage-fixture',
    projectIndexing: { mountedFolder: true, bindingRevision: 1, knowledgeIds: [] } };
  settings.local.semantic = 'off'; settings.local.indexing.maximumDocumentOutputBytes = 4;
  const index = new RetrievalIndex({ root: data, vectorEnabled: false }), jobs = new RetrievalJobStore(data);
  const service = new SourceIndexService({ index, jobs,
    library: { describeSources: async () => ({ sources: [], coverage: { discovered: 0, complete: true } }) },
    embeddings: { status: () => ({ state: 'unavailable' }) }, effectiveSettings: async () => settings,
    getProject: async () => ({ FolderPath: workspace }), serialize: operation => operation() });
  try {
    const admitted = await service.rebuild({ projectId: 'coverage-fixture' });
    await service.lifecycle.active.get(admitted.jobId)?.promise;
    const job = await new RetrievalJobStore(data).get(admitted.jobId);
    assert.equal(job.status, 'partial'); assert.equal(job.completedSources, 1); assert.equal(job.totalSources, 1);
    assert.equal(job.coverage.complete, false); assert.equal(job.coverage.failed, 1);
    assert.equal(job.coverage.failures[0].documentCoverage.pageCount, 2);
    const item = (await index.coverage({ scopeKeys: ['project:coverage-fixture'] })).items[0];
    assert.equal(item.relativePath, 'over-page.pdf'); assert.equal(item.parser, 'failed');
    assert.equal(item.sourceRevision, null); assert.equal(item.documentCoverage.publishedPages, 0);
    assert.deepEqual(item.documentCoverage.uncoveredRanges, [{ startPage: 1, endPage: 2 }]);
    assert.equal((await index.coverage({ scopeKeys: ['user'] })).items.length, 0);
  } finally { await service.close(); await index.close(); }
});

test('production import and mounted indexing publish every PDF page window and retain stable metadata after restart', async t => {
  const root = await sourceFixture(t), workspace = join(root, 'workspace'), data = join(root, 'data');
  await mkdir(workspace); const path = join(workspace, 'windows.pdf');
  await writeFile(path, pdfFixture(['First known page', 'Second known page', 'Third known page']));
  const settings = structuredClone(DEFAULT_RETRIEVAL_SETTINGS);
  settings.local.semantic = 'off'; settings.local.indexing.maximumPdfPages = 1;
  settings.projectIndexing = { mountedFolder: true, bindingRevision: 1, knowledgeIds: [] };
  const index = new RetrievalIndex({ root: data, vectorEnabled: false }), library = new SourceLibrary({ root: data });
  const options = { library, index, jobs: new RetrievalJobStore(data),
    embeddings: { status: () => ({ state: 'unavailable' }) }, effectiveSettings: async () => settings,
    getProject: async () => ({ FolderPath: workspace }), serialize: operation => operation() };
  const service = new SourceIndexService(options); let reopened;
  try {
    const imported = await service.import({ scope: 'user', path });
    assert.equal(imported.importedCount, 3);
    assert.equal(imported.sourceCoverage.complete, true); assert.equal(imported.sourceCoverage.documentWindows, 3);
    await service.lifecycle.active.get(imported.jobId)?.promise;
    const registered = await library.describeSources(['user'], { limits: settings.local.indexing });
    assert.deepEqual(registered.sources.map(source => source.locator.extraction.pageWindow.startPage), [1, 2, 3]);
    const snapshot = await service.sync.mountedSnapshot('pdf-work', settings);
    assert.equal(snapshot.sources.length, 3); assert.equal(snapshot.scan.coverage.complete, true);
    const identities = snapshot.sources.map(source => source.sourceId);
    assert.equal(new Set(identities).size, 3);
    const report = await service.indexer.upsert(snapshot.sources, settings, undefined, undefined,
      { loadSource: snapshot.loadSource, isCurrent: snapshot.isCurrent });
    assert.equal(report.coverage.complete, true);
    const result = await index.search({ query: 'Third known page', scopeKeys: ['project:pdf-work'] });
    const third = result.items.find(item => item.locator.startPage === 3);
    assert.ok(third); assert.equal((await index.read({ sourceRef: third.sourceRef, scopeKeys: ['project:pdf-work'] })).text, 'Third known page');
    await service.close();
    reopened = new SourceIndexService(options);
    const recovered = await reopened.sync.mountedSnapshot('pdf-work', settings);
    assert.deepEqual(recovered.sources.map(source => source.sourceId), identities);
    assert.equal(recovered.scan.reusedFiles, 1, 'the persisted aggregate reuses one physical PDF while retaining all three source windows');
    assert.equal((await recovered.loadSource(recovered.sources[2])).text, 'Third known page');
    await writeFile(path, pdfFixture(['Changed first', 'Changed second', 'Changed third']));
    assert.equal(await recovered.isCurrent(recovered.sources[2]), false);
    await assert.rejects(recovered.loadSource(recovered.sources[2]), error => ['DOCUMENT_SOURCE_CHANGED', 'STALE_RETRIEVAL_SOURCE'].includes(error.code));
  } finally { await service.close(); await reopened?.close(); await index.close(); }
});

test('first PDF window becomes lexical evidence while later window decoding still waits for resources', async t => {
  const root = await sourceFixture(t), workspace = join(root, 'workspace'), data = join(root, 'data');
  await mkdir(workspace); await writeFile(join(workspace, 'progressive.pdf'), pdfFixture(['Early known evidence', 'Later known evidence']));
  const settings = structuredClone(DEFAULT_RETRIEVAL_SETTINGS);
  settings.local.semantic = 'off'; settings.local.indexing.maximumPdfPages = 1;
  settings.projectIndexing = { mountedFolder: true, bindingRevision: 1, knowledgeIds: [] };
  let entered, resume, acquisitions = 0;
  const paused = new Promise(resolvePaused => { entered = resolvePaused; });
  const resources = { acquire: async () => {
    if (++acquisitions === 2) { entered(); await new Promise(resolveResume => { resume = resolveResume; }); }
    return { leaseId: `document-${acquisitions}`, status: 'granted' };
  }, registerExecutor: async () => ({ status: 'registered' }), release: async () => {}, renew: async () => {} };
  const index = new RetrievalIndex({ root: data, vectorEnabled: false });
  const service = new SourceIndexService({ index, jobs: new RetrievalJobStore(data), resourceService: resources,
    library: { describeSources: async () => ({ sources: [], coverage: { discovered: 0, complete: true } }) },
    embeddings: { status: () => ({ state: 'unavailable' }) }, effectiveSettings: async () => settings,
    getProject: async () => ({ FolderPath: workspace }), serialize: operation => operation() });
  const discovery = service.sync.mountedSnapshot('progressive-work', settings);
  try {
    await paused;
    const foreground = await service.sync.foregroundMountedSnapshot('progressive-work', settings);
    assert.equal(foreground.scan.backgroundPending, true); assert.equal(foreground.scan.coverage.complete, false);
    assert.equal(foreground.sources.length, 1); assert.equal(foreground.sources[0].locator.extraction.pageWindow.startPage, 1);
    await service.indexer.upsert(foreground.sources, settings, undefined, undefined,
      { semantic: false, loadSource: foreground.loadSource, isCurrent: foreground.isCurrent });
    assert.ok((await index.search({ query: 'Early known evidence', scopeKeys: ['project:progressive-work'] })).items.length);
    resume(); const complete = await discovery;
    assert.equal(complete.sources.length, 2); assert.equal(complete.scan.coverage.complete, true);
  } finally { resume?.(); await discovery.catch(() => {}); await service.close(); await index.close(); }
});

test('stored document snapshots honor lower output and PDF page limits without deleting their authoritative text', async t => {
  const root = await sourceFixture(t), path = join(root, 'stored.pdf');
  await writeFile(path, pdfFixture(['First known paragraph', 'Second known paragraph']));
  const file = await readSourceFile(path), library = new SourceLibrary({ root: join(root, 'data') });
  const imported = (await library.add([file])).sources[0];
  for (const limits of [{ maximumPdfPages: 1 }, { maximumDocumentOutputBytes: 4 }]) {
    const described = await library.describeSources(['user'], { limits, allowPartial: true });
    assert.equal(described.sources.length, 0); assert.equal(described.coverage.skipped, 1);
    assert.equal(await library.readSource(imported.id, { scopeKeys: ['user'], limits }), null);
  }
  assert.equal((await library.readSource(imported.id, { scopeKeys: ['user'] })).text, file.text);
  assert.equal((await library.list()).sources.length, 1);
  await assert.rejects(library.add([file], { limits: { maximumPdfPages: 1 } }), { code: 'RETRIEVAL_LIBRARY_LIMIT' });
});

test('coordinator freshness rereads knowledge and mounted documents with current decoder limits', async t => {
  const root = await sourceFixture(t), path = join(root, 'fresh.pdf');
  await writeFile(path, pdfFixture(['First known paragraph', 'Second known paragraph']));
  const file = await readSourceFile(path), library = new SourceLibrary({ root: join(root, 'data') });
  const imported = (await library.add([file])).sources[0];
  const knowledge = await library.readSource(imported.id, { scopeKeys: ['user'] });
  const settings = structuredClone(DEFAULT_RETRIEVAL_SETTINGS);
  settings.projectIndexing = { mountedFolder: true, bindingRevision: 1 };
  const retrieval = Object.assign(Object.create(RetrievalCoordinator.prototype), {
    library, excludedRoots: [], _project: async () => ({ FolderPath: root }),
    sourceService: { invalidateMounted: () => {} } });
  const mounted = { ...knowledge, sourceId: 'synthetic-mounted', sourceType: 'work-file', scopeKey: 'project:synthetic',
    sourceRevision: sourceFileRevision(file), bindingRevision: 1,
    locator: { root, path, relativePath: 'fresh.pdf', extraction: file.extraction } };
  for (const source of [knowledge, mounted]) {
    const snapshot = { settings, scopes: [source.scopeKey], identities: new Map([[source.sourceId, source]]) };
    assert.equal(await retrieval._freshSource(source, snapshot, undefined, new Map(), {}), true, source.sourceType);
    settings.local.indexing.maximumPdfPages = 1;
    assert.equal(await retrieval._freshSource(source, snapshot, undefined, new Map(), {}), false);
    settings.local.indexing.maximumPdfPages = 100;
    settings.local.indexing.maximumDocumentOutputBytes = 4;
    assert.equal(await retrieval._freshSource(source, snapshot, undefined, new Map(), {}), false);
    settings.local.indexing.maximumDocumentOutputBytes = 2097152;
  }
});

test('decoder timeouts retry only once and are never cached as permanent malformed sources', async t => {
  const root = await sourceFixture(t), path = join(root, 'temporary.docx');
  await writeFile(path, await docxFixture(['Recovery remains possible']));
  assert.deepEqual(classifySourceFailure({ code: 'DOCUMENT_DECODER_TIMEOUT' }),
    { errorCode: 'DOCUMENT_DECODER_TIMEOUT', category: 'temporary', retryable: true });
  let attempts = 0;
  const resources = { acquire: async () => { attempts++; throw Object.assign(new Error('Synthetic temporary decoder timeout'),
    { code: 'DOCUMENT_DECODER_TIMEOUT' }); } };
  const failureCache = new Map(), stats = {};
  for await (const file of scanSourceTree(root, { failureCache, stats, resourceService: resources })) assert.fail(file);
  assert.equal(attempts, 2); assert.equal(stats.failures[0].attempts, 2);
  assert.equal(stats.failures[0].retryable, true); assert.equal(failureCache.size, 0);
  const recovered = [];
  for await (const file of scanSourceTree(root, { failureCache })) recovered.push(file);
  assert.equal(recovered.length, 1); assert.match(recovered[0].text, /Recovery remains possible/u);
});

test('deleting or unmounting never-published failures reconciles current coverage and preserves past job receipts', async t => {
  const root = await sourceFixture(t), workspace = join(root, 'workspace'), data = join(root, 'data');
  await mkdir(workspace);
  const path = join(workspace, 'bad.pdf');
  await writeFile(path, pdfFixture(['First', 'Second']));
  const settings = { ...structuredClone(DEFAULT_RETRIEVAL_SETTINGS), projectId: 'coverage-delete',
    projectIndexing: { mountedFolder: true, bindingRevision: 1, knowledgeIds: [] } };
  settings.local.semantic = 'off'; settings.local.indexing.maximumDocumentOutputBytes = 4;
  const index = new RetrievalIndex({ root: data, vectorEnabled: false }), jobs = new RetrievalJobStore(data);
  const service = new SourceIndexService({ index, jobs,
    library: { describeSources: async () => ({ sources: [], coverage: { discovered: 0, complete: true } }) },
    embeddings: { status: () => ({ state: 'unavailable' }) }, effectiveSettings: async () => settings,
    getProject: async () => ({ FolderPath: workspace }), serialize: operation => operation() });
  const rebuild = async () => {
    const job = await service.rebuild({ projectId: settings.projectId, dirty: true });
    await service.lifecycle.active.get(job.jobId)?.promise;
    return jobs.get(job.jobId);
  };
  try {
    const failed = await rebuild();
    assert.equal((await index.status()).sources, 0);
    assert.equal((await index.coverage({ scopeKeys: ['project:coverage-delete'] })).counts.failed, 1);
    await rm(path); service.sync.markChanged(settings.projectId, 'bad.pdf');
    await rebuild();
    assert.equal((await index.coverage({ scopeKeys: ['project:coverage-delete'] })).counts.failed, 0);
    assert.equal((await jobs.get(failed.jobId)).coverage.failed, 1);
    await writeFile(path, pdfFixture(['First', 'Second'])); service.sync.markChanged(settings.projectId, 'bad.pdf');
    await rebuild();
    assert.equal((await index.coverage({ scopeKeys: ['project:coverage-delete'] })).counts.failed, 1);
    settings.projectIndexing.mountedFolder = false; settings.projectIndexing.bindingRevision++;
    await rebuild();
    assert.equal((await index.coverage({ scopeKeys: ['project:coverage-delete'] })).items.length, 0);
  } finally { await service.close(); await index.close(); }
});

test('malformed containers, DOCX expansion/entry/output budgets and XML declarations are rejected', async () => {
  for (const format of ['pdf', 'docx']) await assert.rejects(extractDocumentBytes(Buffer.from('Malformed fixture'), format), { code: 'DOCUMENT_PARSE_FAILED' });
  const expanded = await docxFixture(['A'.repeat(9000)]);
  assert.ok(expanded.length < 2048);
  await assert.rejects(extractDocumentBytes(expanded, 'docx', { maximumExpandedBytes: 2048 }), { code: 'DOCUMENT_ARCHIVE_LIMIT' });
  await assert.rejects(extractDocumentBytes(await docxFixture(), 'docx', { maximumArchiveEntries: 1 }), { code: 'DOCUMENT_ARCHIVE_LIMIT' });
  await assert.rejects(extractDocumentBytes(await docxFixture(), 'docx', { maximumOutputBytes: 4 }), { code: 'DOCUMENT_OUTPUT_LIMIT' });
  await assert.rejects(extractDocumentBytes(await docxFixture(['Known'], { 'word/extra.xml': '<!DOCTYPE x [<!ENTITY e "text">]><x>&e;</x>' }), 'docx'),
    { code: 'DOCUMENT_XML_UNSUPPORTED' });
});

test('reader scans include documents and pyi, reuse current extraction receipts and re-extract older rules', async t => {
  const root = await sourceFixture(t);
  await writeFile(join(root, 'manual.pdf'), pdfFixture(['Known evidence']));
  await writeFile(join(root, 'manual.docx'), await docxFixture());
  await writeFile(join(root, 'types.pyi'), 'class PublicType: ...\n');
  const collect = async options => { const files = []; for await (const file of scanSourceTree(root, options)) files.push(file); return files; };
  const initial = await collect({});
  assert.deepEqual(initial.map(file => file.title).sort(), ['manual.docx', 'manual.pdf', 'types.pyi']);
  const previousFiles = new Map(initial.map(file => [file.title, file]));
  const stats = {}, reused = await collect({ previousFiles, stats });
  assert.equal(stats.reusedFiles, 3); assert.equal(stats.fileReads, 0);
  for (const file of reused) assert.deepEqual(file.extraction, previousFiles.get(file.title).extraction);
  const oldRules = new Map(initial.map(file => [file.title, { ...file,
    ...(file.extraction ? { extraction: { ...file.extraction, version: 'old-extraction-rule' } } : {}) }]));
  const changedStats = {};
  await collect({ previousFiles: oldRules, stats: changedStats });
  assert.equal(changedStats.fileReads, 2); assert.equal(changedStats.reusedFiles, 1);
  const noReceipts = new Map(initial.map(file => { const { extraction, ...rest } = file; return [file.title, rest]; }));
  const missingStats = {};
  await collect({ previousFiles: noReceipts, stats: missingStats });
  assert.equal(missingStats.fileReads, 2);
  const dirty = [];
  for await (const file of scanSourcePaths(root, ['manual.docx', 'types.pyi'])) dirty.push(file);
  assert.equal(dirty.length, 2); assert.ok(dirty.every(file => file.text));
});

test('scan failures never silently drop malformed or oversized supported documents', async t => {
  const root = await sourceFixture(t), path = join(root, 'broken.pdf');
  await writeFile(path, 'Malformed document');
  const treeStats = {}, dirtyStats = {}, dirty = [];
  for await (const file of scanSourceTree(root, { stats: treeStats })) assert.fail(file);
  assert.equal(treeStats.failedFiles, 1); assert.equal(treeStats.failures[0].errorCode, 'DOCUMENT_PARSE_FAILED');
  for await (const file of scanSourcePaths(root, ['broken.pdf'], { stats: dirtyStats })) dirty.push(file);
  assert.equal(dirtyStats.failedFiles, 1); assert.equal(dirtyStats.failures[0].errorCode, 'DOCUMENT_PARSE_FAILED');
  assert.equal(dirty[0].failed, true); assert.equal(dirty[0].missing, undefined, 'parse failure cannot prove deletion');
  await assert.rejects(readSourceFile(path, { maximumSourceBytes: 4 }), { code: 'DOCUMENT_BYTES_LIMIT' });
  await assert.rejects(readSourceFile(path, { root: join(root, 'unauthorized') }), { code: 'PROTECTED_RETRIEVAL_SOURCE' });
});

test('decoder resource reservations precede work and remain held until the owned process closes', async () => {
  const events = [], bytes = Buffer.from('Known authorized bytes');
  const resources = { acquire: async request => { events.push(['acquire', request]); return { status: 'granted', leaseId: 'decoder-lease' }; },
    renew: async () => ({ status: 'renewed' }), registerExecutor: async (leaseId, executor) => {
      events.push(['register', leaseId, executor]); return { status: 'registered' };
    }, release: async leaseId => { events.push(['release', leaseId]); } };
  await extractDocumentBytes(bytes, 'docx', { resourceService: resources, forkFactory: () => {
    const child = fakeDecoder((process, message) => { events.push(['decode', message.limits.maximumInputBytes]);
      process.emit('message', { type: 'result', text: 'Known result.' }); });
    child.pid = 12345;
    child.kill = () => { events.push(['kill']); queueMicrotask(() => { events.push(['close']); child.emit('close', 0); }); return true; };
    return child;
  } });
  assert.equal(events[0][0], 'acquire'); assert.equal(events[0][1].cpuThreads, 1);
  assert.ok(events[0][1].memoryBytes >= DOCUMENT_EXTRACTION_LIMITS.maximumRssBytes);
  assert.deepEqual(events[1], ['register', 'decoder-lease', { processId: 12345 }]);
  assert.ok(events.findIndex(event => event[0] === 'decode') > 1);
  assert.ok(events.findIndex(event => event[0] === 'release') > events.findIndex(event => event[0] === 'close'));
  let spawned = false;
  await assert.rejects(extractDocumentBytes(bytes, 'docx', { resourceService: { acquire: async () => ({ status: 'denied' }) },
    forkFactory: () => { spawned = true; throw new Error('Must not spawn without capacity.'); } }), { code: 'DOCUMENT_RESOURCE_BUSY' });
  assert.equal(spawned, false);
});

test('decoder validates internal resource bounds and sends bytes without paths or inherited credentials', async () => {
  for (const options of [{ maximumHeapMb: 193 }, { maximumRssBytes: 257 * 1024 * 1024 }, { maximumProcesses: 3 },
    { arbitrary: true }, { timeoutMs: 0 }]) assert.throws(() => validateDocumentExtractionLimits(options), { code: 'DOCUMENT_INVALID_LIMITS' });
  await assert.rejects(extractDocumentBytes(Buffer.alloc(DOCUMENT_EXTRACTION_LIMITS.maximumInputBytes + 1), 'pdf'), { code: 'DOCUMENT_BYTES_LIMIT' });
  let capturedOptions, capturedMessage;
  const bytes = Buffer.from('Authorized bytes');
  const result = await extractDocumentBytes(bytes, 'docx', { forkFactory: (url, args, options) => {
    capturedOptions = options;
    return fakeDecoder((child, message) => { capturedMessage = message; child.emit('message', { type: 'result', text: 'Known text.' }); });
  } });
  assert.equal(result.extraction.rawContentHash, sha256(bytes));
  assert.deepEqual(Object.keys(capturedMessage).sort(), ['bytes', 'format', 'limits', 'type']);
  assert.deepEqual(Object.keys(capturedOptions.env).sort(), ['NODE_NO_WARNINGS', 'PATH', 'SystemRoot', 'WINDIR']);
  assert.equal(capturedOptions.windowsHide, true);
  assert.ok(capturedOptions.execArgv.includes('--permission'));
  assert.ok(capturedOptions.execArgv.includes('--max-old-space-size=192'));
});

test('owned processes retire after real success, malformed parse, timeout and abort before publishing results', async () => {
  const children = [];
  const forkFactory = (url, args, options) => { const child = fork(url, args, options); children.push(child); return child; };
  await extractDocumentBytes(await docxFixture(), 'docx', { forkFactory });
  await assert.rejects(extractDocumentBytes(Buffer.from('Invalid'), 'docx', { forkFactory }), { code: 'DOCUMENT_PARSE_FAILED' });
  await assert.rejects(extractDocumentBytes(await docxFixture(), 'docx', { forkFactory, timeoutMs: 1 }), { code: 'DOCUMENT_DECODER_TIMEOUT' });
  await assert.rejects(extractDocumentBytes(await docxFixture(), 'docx', { forkFactory: (url, args, options) => {
    const child = fork(url, args, { ...options, execArgv: ['--eval', 'process.exit(23)'] });
    children.push(child); return child;
  } }), { code: 'DOCUMENT_DECODER_FAILED' });
  const controller = new AbortController();
  const pending = extractDocumentBytes(await docxFixture(), 'docx', { forkFactory, signal: controller.signal });
  controller.abort();
  await assert.rejects(pending, { name: 'AbortError' });
  for (const child of children) {
    assert.notEqual(child.exitCode ?? child.signalCode, null, 'promise settles after the owned process has exited');
    assert.throws(() => process.kill(child.pid, 0), { code: 'ESRCH' });
  }
});

test('crash, memory overflow and simultaneous-process limits fail honestly and leave later work usable', async () => {
  const bytes = Buffer.from('Known authorized bytes');
  await assert.rejects(extractDocumentBytes(bytes, 'docx', { forkFactory: () => fakeDecoder(child => {
    child.emit('exit', 23); child.emit('close', 23);
  }) }), { code: 'DOCUMENT_DECODER_FAILED' });
  await assert.rejects(extractDocumentBytes(bytes, 'docx', { forkFactory: () => fakeDecoder(child => {
    child.emit('message', { type: 'memory', rssBytes: DOCUMENT_EXTRACTION_LIMITS.maximumRssBytes + 1 });
  }) }), { code: 'DOCUMENT_DECODER_MEMORY_LIMIT' });
  const controller = new AbortController();
  const first = extractDocumentBytes(bytes, 'docx', { signal: controller.signal, forkFactory: () => fakeDecoder(() => {}) });
  const second = extractDocumentBytes(bytes, 'docx', { signal: controller.signal, forkFactory: () => fakeDecoder(() => {}) });
  await assert.rejects(extractDocumentBytes(bytes, 'docx'), { code: 'DOCUMENT_DECODER_BUSY' });
  controller.abort();
  await Promise.all([assert.rejects(first, { name: 'AbortError' }), assert.rejects(second, { name: 'AbortError' })]);
  const result = await extractDocumentBytes(await docxFixture(['Still usable.']), 'docx');
  assert.equal(result.text, 'Still usable.\n\n');
});

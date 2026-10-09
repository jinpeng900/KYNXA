import assert from 'node:assert/strict';
import { mkdtemp, mkdir, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { test } from 'node:test';
import { hashText, sourceFileRevision } from '../data/retrieval/retrieval-contracts.mjs';
import { RetrievalIndex } from '../data/retrieval/index.mjs';
import { SourceLibrary } from '../data/retrieval/source-library.mjs';
import { RetrievalJobStore } from '../data/retrieval/job-store.mjs';
import { SourceManifestStore } from '../data/retrieval/source-manifest.mjs';
import { SourceSyncService } from '../orchestration/retrieval/source-sync.mjs';
import { SourceIndexer } from '../orchestration/retrieval/source-indexer.mjs';
import { readSourceFile, readSourceFileWindow, readSourceImportTree, scanSourceTree } from '../tools/retrieval/source-reader.mjs';

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-large-windows-')), workspace = join(root, 'workspace');
  await mkdir(workspace);
  const index = new RetrievalIndex({ root: join(root, 'data'), vectorEnabled: false });
  const settings = { projectId: 'project-a', local: { semantic: 'off', embeddingProfileId: null,
    indexing: { maximumSourceBytes: 32 * 1024 * 1024, maximumFiles: 20000, maximumTotalBytes: 512 * 1024 * 1024, maximumEntries: 200000, batchSize: 4 } },
    projectIndexing: { mountedFolder: true, bindingRevision: 1 }, cache: { memoryLimitBytes: 16 * 1024 * 1024 } };
  const project = { FolderPath: workspace };
  const sync = new SourceSyncService({ library: { root: join(root, 'data') }, getProject: async () => project,
    watchFactory: () => ({ close() {} }) });
  t.after(async () => {
    sync.close(); await index.close();
    const suffix = relative(resolve(tmpdir()), root); assert.ok(suffix && !suffix.startsWith(`..${sep}`));
    await rm(root, { recursive: true, force: true });
  });
  return { root, workspace, index, settings, sync, project };
}

test('configured small-source batches exceed four while unmeasured bodies retain the atomic publication budget', async t => {
  const f = await fixture(t), batchCounts = [];
  const sources = Array.from({ length: 12 }, (_, index) => ({ sourceId: `batch-${index}`, scopeKey: 'user',
    sourceType: 'knowledge', title: `Note ${index}`, sourceRevision: 1, locator: {}, text: `Unique source ${index} content.` }));
  const settings = { ...f.settings, local: { ...f.settings.local, indexing: { ...f.settings.local.indexing, batchSize: 12 } } };
  const create = () => new SourceIndexer({ index: {
    upsertSources: async (batch, options) => { batchCounts.push(batch.length); return f.index.upsertSources(batch, options); },
    recordCoverage: request => f.index.recordCoverage(request)
  }, embeddings: { status: () => ({ state: 'disabled' }) } });
  const measured = await create().upsert(sources, settings, undefined, undefined, { semantic: false });
  assert.deepEqual(batchCounts, [12]); assert.equal(measured.coverage.lexical, 12);
  batchCounts.length = 0;
  const metadata = sources.map(({ text, ...source }) => ({ ...source, contentHash: hashText(text) }));
  const originalById = new Map(sources.map(source => [source.sourceId, source]));
  const unmeasured = await create().upsert(metadata, settings, undefined, undefined,
    { semantic: false, loadSource: async source => originalById.get(source.sourceId) });
  assert.deepEqual(batchCounts, [4, 4, 4]); assert.equal(unmeasured.coverage.lexical, 12);
  assert.equal(unmeasured.coverage.complete, true);
});

test('large UTF-8 and UTF-16 files hash as streams and selected windows preserve exact original offsets', async t => {
  const f = await fixture(t), text = ('line 😀 中文 evidence\r\n'.repeat(105000)) + 'FINAL_UNIQUE_EVIDENCE';
  for (const [name, bytes] of [['utf8.txt', Buffer.from(text)], ['utf16.txt', Buffer.concat([Buffer.from([0xff, 0xfe]), Buffer.from(text, 'utf16le')])]]) {
    const path = join(f.workspace, name); await writeFile(path, bytes);
    const file = await readSourceFile(path, { root: f.workspace });
    assert.equal(file.text, undefined); assert.equal(file.contentHash, hashText(text));
    assert.ok(file.windows.length > 10); assert.ok(file.windows.every(window => window.endOffset - window.startOffset <= 128 * 1024));
    const window = file.windows.at(-1), read = await readSourceFileWindow(path, window, { root: f.workspace });
    assert.equal(read.text, text.slice(window.startOffset, window.endOffset));
    assert.match(read.text, /FINAL_UNIQUE_EVIDENCE/); assert.equal(read.contentHash, window.contentHash);
    assert.equal(sourceFileRevision(read), sourceFileRevision({ contentHash: window.contentHash, fileWindow: window }));
    assert.doesNotMatch(read.text, /^[\uDC00-\uDFFF]|[\uD800-\uDBFF]$/u);
    await f.index.upsertSources([{ sourceId: `part-${name}`, scopeKey: 'project:a', sourceType: 'work-file',
      sourceRevision: sourceFileRevision(read), locator: { relativePath: name, fileWindow: window }, title: name, text: read.text }]);
    const hit = (await f.index.search({ query: 'FINAL_UNIQUE_EVIDENCE', scopeKeys: ['project:a'] })).items.find(item => item.sourceId === `part-${name}`);
    assert.equal(hit.locator.originalStartOffset, window.startOffset + hit.locator.startOffset);
    assert.equal(text.slice(hit.locator.originalStartOffset, hit.locator.originalEndOffset), hit.excerpt);
    await writeFile(path, Buffer.from('Changed elsewhere in the original file.'));
    await assert.rejects(readSourceFileWindow(path, window, { root: f.workspace }), { code: 'STALE_RETRIEVAL_SOURCE' });
  }
});

test('one bad PDF does not block later ordinary files or a streamed formal import', async t => {
  const f = await fixture(t);
  await writeFile(join(f.workspace, '00-broken.pdf'), 'Not a PDF.');
  await writeFile(join(f.workspace, '01-good.md'), '# Reliable\nACTUAL_GOOD_EVIDENCE');
  const stats = {}, files = [];
  for await (const file of scanSourceTree(f.workspace, { stats })) files.push(file);
  assert.equal(files.length, 1); assert.match(files[0].text, /ACTUAL_GOOD_EVIDENCE/);
  assert.equal(stats.failedFiles, 1); assert.equal(stats.failures[0].relativePath, '00-broken.pdf');
  const library = new SourceLibrary({ root: join(f.root, 'formal'), conversationStore: {} });
  const receipt = await library.add(readSourceImportTree(f.workspace, { stats: {} }), { limits: f.settings.local.indexing });
  assert.equal(receipt.sources.length, 1);
  assert.match((await library.readSource(receipt.sources[0].id, { scopeKeys: ['user'] })).text, /ACTUAL_GOOD_EVIDENCE/);
});

test('UTF-16 snapshot expansion is windowed and a stale import window does not discard later independent files', async t => {
  const f = await fixture(t), large = join(f.workspace, '00-large.txt');
  const text = '中文资料'.repeat(200000);
  const bytes = Buffer.concat([Buffer.from([0xff, 0xfe]), Buffer.from(text, 'utf16le')]);
  assert.ok(bytes.length < 2 * 1024 * 1024); assert.ok(Buffer.byteLength(text) > 2 * 1024 * 1024);
  await writeFile(large, bytes); await writeFile(join(f.workspace, '01-independent.md'), 'LATER_INDEPENDENT_EVIDENCE');
  const descriptor = await readSourceFile(large, { root: f.workspace });
  assert.equal(descriptor.text, undefined); assert.ok(descriptor.windows.length > 1);
  const stats = {}, imported = []; let changed = false;
  for await (const file of readSourceImportTree(f.workspace, { stats })) {
    imported.push(file);
    if (!changed && file.path === large) { changed = true; await writeFile(large, 'Changed during import.'); }
  }
  assert.equal(stats.failedFiles, 1); assert.equal(stats.failures[0].errorCode, 'STALE_RETRIEVAL_SOURCE');
  assert.ok(imported.some(file => file.text.includes('LATER_INDEPENDENT_EVIDENCE')));
  const library = new SourceLibrary({ root: join(f.root, 'formal'), conversationStore: {} });
  await assert.rejects(library.add([{ path: large, text }], { limits: f.settings.local.indexing }), { code: 'INVALID_RETRIEVAL_SOURCE' });
  assert.equal((await library.list()).sources.length, 0, 'oversized custom snapshot input never creates an unreadable catalog entry');
});

test('window manifests reject invalid lists and gaps without replacing a previously valid version', async t => {
  const f = await fixture(t), path = join(f.workspace, 'large.txt');
  await writeFile(path, 'Original evidence\n'.repeat(125000));
  const descriptor = await readSourceFile(path, { root: f.workspace });
  const file = { relativePath: 'large.txt', contentHash: descriptor.contentHash, metadata: descriptor.metadata,
    textBytes: descriptor.textBytes, windows: descriptor.windows };
  const store = new SourceManifestStore(join(f.root, 'manifest')), binding = { projectId: 'project-a', root: f.workspace, bindingRevision: 1 };
  await store.write(binding, [file]);
  const before = await store.read(binding);
  await assert.rejects(store.write(binding, [{ ...file, windows: {} }]), { code: 'INVALID_RETRIEVAL_MANIFEST' });
  const reordered = structuredClone(file); reordered.windows.reverse();
  await assert.rejects(store.write(binding, [reordered]), { code: 'INVALID_RETRIEVAL_MANIFEST' });
  const gap = structuredClone(file); gap.windows[1].startOffset++;
  await assert.rejects(store.write(binding, [gap]), { code: 'INVALID_RETRIEVAL_MANIFEST' });
  assert.deepEqual(await store.read(binding), before);
});

test('failed changed sources retain old metadata without blocking publication of another source', async t => {
  const f = await fixture(t);
  const badPath = join(f.workspace, 'bad.md'); await writeFile(badPath, 'Previously valid original');
  await writeFile(join(f.workspace, 'good.md'), 'Good original');
  const first = await f.sync.mountedSnapshot('project-a', f.settings);
  const indexer = new SourceIndexer({ library: {}, index: f.index, embeddings: { status: () => ({ state: 'disabled' }) },
    serialize: operation => operation(), getProject: async () => f.project, effectiveSettings: async () => f.settings });
  await indexer.upsert(first.sources, f.settings, undefined, undefined, { semantic: false, loadSource: first.loadSource, isCurrent: first.isCurrent });
  await writeFile(badPath, Buffer.from([0xff, 0xff]));
  await writeFile(join(f.workspace, 'good.md'), 'Changed good SUCCESSFUL_NEW_EVIDENCE');
  f.sync.markChanged('project-a', null);
  const changed = await f.sync.mountedSnapshot('project-a', f.settings);
  assert.equal(changed.scan.coverage.failed, 1);
  const bad = changed.sources.find(source => source.locator.relativePath === 'bad.md');
  assert.equal(bad.unavailable, true); assert.equal(await changed.isCurrent(bad), false);
  const report = await indexer.upsert(changed.sources, f.settings, undefined, undefined,
    { semantic: false, loadSource: changed.loadSource, isCurrent: changed.isCurrent });
  assert.equal(report.coverage.failed, 1); assert.equal(report.coverage.lexical, 1); assert.equal(report.coverage.complete, false);
  assert.equal((await f.index.read({ sourceId: bad.sourceId, scopeKeys: ['project:project-a'] })).text, 'Previously valid original');
  assert.match((await f.index.search({ query: 'SUCCESSFUL_NEW_EVIDENCE', scopeKeys: ['project:project-a'] })).items[0].excerpt, /SUCCESSFUL_NEW_EVIDENCE/);
});

test('a failed lexical publication isolates its source and cannot be counted as committed progress', async t => {
  const f = await fixture(t), source = (id, text) => ({ sourceId: id, scopeKey: 'user', sourceType: 'knowledge',
    sourceRevision: 1, title: `${id}.md`, locator: {}, text });
  const bad = source('bad-publication', 'Previously committed bad source');
  await f.index.upsertSources([bad]);
  const index = { upsertSources: async sources => {
    if (sources.some(item => item.sourceId === bad.sourceId)) throw Object.assign(new Error('Synthetic publication failed.'), { code: 'SQLITE_SOURCE_FAILED' });
    return f.index.upsertSources(sources);
  }, recordCoverage: options => f.index.recordCoverage(options) };
  const indexer = new SourceIndexer({ index, embeddings: { status: () => ({ state: 'disabled' }) } });
  const report = await indexer.upsert([{ ...bad, text: 'Changed uncommitted bad source' }, source('good-publication', 'Committed good source')],
    f.settings, undefined, undefined, { semantic: false });
  assert.equal(report.coverage.failed, 1); assert.equal(report.coverage.lexical, 1); assert.equal(report.coverage.complete, false);
  assert.equal(indexer.fingerprints.has(`${bad.sourceId}:lexical`), false);
  assert.equal((await f.index.read({ sourceId: bad.sourceId, scopeKeys: ['user'] })).text, bad.text);
  assert.equal((await f.index.read({ sourceId: 'good-publication', scopeKeys: ['user'] })).text, 'Committed good source');
  const coverage = await f.index.coverage({ scopeKeys: ['user'] });
  assert.equal(coverage.counts.failed, 1); assert.equal(coverage.counts.lexical, 1);
});

test('partial indexing is a durable terminal state while old jobs and validated coverage survive reopen', async t => {
  const f = await fixture(t), jobs = new RetrievalJobStore(join(f.root, 'jobs'));
  const legacy = await jobs.create('project-a'); await jobs.update(legacy.jobId, { status: 'running' });
  await jobs.update(legacy.jobId, { status: 'completed', completedSources: 1, totalSources: 1 });
  const partial = await jobs.create('project-a'); await jobs.update(partial.jobId, { status: 'running' });
  const coverage = { discovered: 2, lexical: 1, semantic: 0, failed: 1, skipped: 0, partial: 0, complete: false,
    failures: [{ relativePath: 'bad.pdf', errorCode: 'DOCUMENT_PARSE_FAILED' }] };
  await jobs.update(partial.jobId, { status: 'partial', coverage, completedSources: 1, totalSources: 2 });
  const reopened = new RetrievalJobStore(join(f.root, 'jobs'));
  assert.deepEqual((await reopened.get(partial.jobId)).coverage, coverage);
  assert.equal((await reopened.get(legacy.jobId)).coverage, undefined);
  assert.equal((await reopened.get(legacy.jobId)).status, 'completed');
  assert.deepEqual(await reopened.recover({ resumable: true }), []);
  await assert.rejects(reopened.update(partial.jobId, { status: 'running' }), { code: 'RETRIEVAL_JOB_STATE_CONFLICT' });
  await assert.rejects(reopened.update(partial.jobId, { coverage: { ...coverage, lexical: -1 } }), { code: 'INVALID_RETRIEVAL_JOB_UPDATE' });
  assert.deepEqual((await reopened.get(partial.jobId)).coverage, coverage);
});

import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { test } from 'node:test';
import { SourceLibrary } from '../data/retrieval/source-library.mjs';
import { SourceManifestStore } from '../data/retrieval/source-manifest.mjs';
import { hashText, sourceEvidenceLocator, sourceFileRevision, validateSourceExtraction } from '../data/retrieval/retrieval-contracts.mjs';
import { RetrievalIndex } from '../data/retrieval/index.mjs';
import JSZip from 'jszip';
import { RetrievalJobStore } from '../data/retrieval/job-store.mjs';
import { RetrievalStructureService } from '../data/retrieval/structure-service.mjs';
import { SourceIndexService } from '../orchestration/retrieval/source-manager.mjs';

const text = 'First page.\n第二页原文。';
const extraction = { format: 'pdf', version: 'fixture-pdf-text-v1', rawContentHash: hashText('Synthetic PDF bytes'),
  pageCount: 2, pages: [{ page: 1, startOffset: 0, endOffset: 11 }, { page: 2, startOffset: 12, endOffset: text.length }] };

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-document-provenance-'));
  const library = new SourceLibrary({ root });
  t.after(async () => {
    await library.queue;
    const suffix = relative(resolve(tmpdir()), root);
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(root, { recursive: true, force: true });
  });
  return { root, library };
}

test('document identity validates real page ranges and invalidates when original bytes or extraction rules change', () => {
  assert.deepEqual(validateSourceExtraction(extraction, text.length), extraction);
  const file = { contentHash: hashText(text), extraction };
  const original = sourceFileRevision(file);
  assert.notEqual(sourceFileRevision({ ...file, extraction: { ...extraction, rawContentHash: hashText('Changed original bytes') } }), original);
  assert.notEqual(sourceFileRevision({ ...file, extraction: { ...extraction, version: 'fixture-pdf-text-v2' } }), original);
  assert.equal(sourceFileRevision({ contentHash: file.contentHash }), file.contentHash, 'plain text keeps its existing version contract');
  const locator = sourceEvidenceLocator({ relativePath: 'manual.pdf', extraction }, { startOffset: 12, endOffset: text.length });
  assert.deepEqual(locator, { relativePath: 'manual.pdf', documentFormat: 'pdf', pageCount: 2, startPage: 2, endPage: 2 });
  for (const invalid of [{ ...extraction, rawContentHash: 'bad' }, { ...extraction, pageCount: 3 },
    { ...extraction, pages: [extraction.pages[0], { page: 2, startOffset: 3, endOffset: text.length }] },
    { ...extraction, pages: [extraction.pages[0], { page: 2, startOffset: 12, endOffset: text.length + 1 }] },
    { ...extraction, format: 'docx' }]) {
    assert.throws(() => validateSourceExtraction(invalid, text.length), { code: 'INVALID_RETRIEVAL_EXTRACTION' });
  }
});

test('registered extracted snapshots preserve their byte provenance and page offsets across restart without modifying originals', async t => {
  const { root, library } = await fixture(t);
  const file = { path: join(root, 'source.pdf'), title: 'source.pdf', text, extraction };
  const first = await library.add([file]);
  const duplicate = await library.add([file]);
  assert.equal(duplicate.sources[0].id, first.sources[0].id);
  const changed = await library.add([{ ...file, extraction: { ...extraction, rawContentHash: hashText('Different container bytes') } }]);
  assert.notEqual(changed.sources[0].id, first.sources[0].id, 'same extracted words cannot conceal a new byte identity');
  const restarted = new SourceLibrary({ root });
  const description = await restarted.describeSources(['user']);
  const original = description.sources.find(source => source.sourceId === first.sources[0].id);
  assert.equal(original.text, undefined, 'describing provenance does not load the snapshot body');
  assert.deepEqual(original.locator.extraction, extraction);
  const snapshot = await restarted.readSource(original.sourceId, { scopeKeys: ['user'], sourceRevision: original.sourceRevision });
  assert.equal(snapshot.text, text);
  assert.deepEqual(snapshot.locator.extraction, extraction);
  assert.equal(await restarted.readSource(original.sourceId, { scopeKeys: ['project:unauthorized'] }), null);
  const index = new RetrievalIndex({ root, vectorEnabled: false });
  try {
    await index.upsertSources([snapshot]);
    const hit = (await index.search({ query: '第二页', scopeKeys: ['user'] })).items[0];
    assert.equal(hit.locator.extraction, undefined, 'candidates do not repeat internal page manifests');
    const page = await index.read({ sourceId: original.sourceId, scopeKeys: ['user'], offset: 12, limit: text.length });
    assert.equal(page.text, text.slice(12));
    assert.equal(page.locator.startPage, 2);
    assert.equal(page.locator.endPage, 2);
    assert.equal(page.locator.extraction, undefined);
  } finally { await index.close(); }
  await restarted.queue;
});

test('incremental manifests retain document provenance while remaining compatible with plain text entries', async t => {
  const { root } = await fixture(t), store = new SourceManifestStore(root);
  const binding = { projectId: 'fixture-project', root, bindingRevision: 1 };
  const file = { relativePath: 'manual.pdf', contentHash: hashText(text), textBytes: Buffer.byteLength(text), extraction,
    metadata: { sizeBytes: 128, mtimeMs: 10, ctimeMs: 9, device: 1, inode: 2 } };
  await store.write(binding, [file, { ...file, relativePath: 'manual.txt', extraction: undefined }]);
  const resumed = await store.read(binding);
  assert.deepEqual(resumed.files[0].extraction, extraction);
  assert.equal(sourceFileRevision(resumed.files[0]), sourceFileRevision(file));
  assert.equal(resumed.files[1].extraction, undefined);
  await assert.rejects(store.write(binding, [{ ...file, extraction: { ...extraction, version: '' } }]),
    { code: 'INVALID_RETRIEVAL_EXTRACTION' });
  assert.equal((await store.read(binding)).files.length, 2, 'an invalid replacement cannot overwrite the valid manifest');
  await store.drain();
});

test('a real DOCX import completes the durable source job, is searchable and revokes without changing original bytes', async t => {
  const { root, library } = await fixture(t), archive = new JSZip(), path = join(root, 'actual.docx');
  archive.file('[Content_Types].xml', '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">' +
    '<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>');
  archive.file('word/document.xml', '<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>' +
    '<w:p><w:r><w:t>真实导入中文原文。</w:t></w:r></w:p></w:body></w:document>');
  const bytes = await archive.generateAsync({ type: 'nodebuffer', compression: 'DEFLATE' });
  await writeFile(path, bytes);
  const index = new RetrievalIndex({ root, vectorEnabled: false }), jobs = new RetrievalJobStore(root);
  const structures = new RetrievalStructureService();
  const settings = { local: { enabled: true, semantic: 'off', embeddingProfileId: null },
    cache: { memoryLimitBytes: 16 * 1024 * 1024 }, projectIndexing: { mountedFolder: false, bindingRevision: 0 } };
  const service = new SourceIndexService({ library, index, jobs, structures,
    embeddings: { status: () => ({ state: 'unavailable' }) }, getProject: async () => null,
    effectiveSettings: async () => settings, serialize: operation => operation() });
  try {
    await service.initialize();
    const imported = await service.import({ path, scope: 'user' });
    const deadline = Date.now() + 5000;
    let job;
    do {
      job = await jobs.get(imported.jobId);
      if (['completed', 'failed', 'cancelled'].includes(job.status)) break;
      await new Promise(resolveTick => setTimeout(resolveTick, 5));
    } while (Date.now() < deadline);
    assert.equal(job.status, 'completed', job.error?.code);
    const hit = (await index.search({ query: '中文原文', scopeKeys: ['user'] })).items[0];
    assert.ok(hit);
    assert.equal(hit.locator.documentFormat, 'docx');
    const stored = await index.read({ sourceRef: hit.sourceRef, scopeKeys: ['user'] });
    assert.equal(stored.text, '真实导入中文原文。\n\n');
    const registered = await library.readSource(imported.id, { scopeKeys: ['user'] });
    assert.equal(registered.locator.extraction.rawContentHash, hashText(bytes));
    assert.deepEqual(await readFile(path), bytes);
    await service.remove(imported.id, { expectedRevision: imported.revision });
    assert.equal((await index.search({ query: '中文原文', scopeKeys: ['user'] })).items.length, 0);
    assert.deepEqual(await readFile(path), bytes);
  } finally { await service.close(); await structures.close(); await index.close(); }
});

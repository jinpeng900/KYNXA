import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { test } from 'node:test';
import { RetrievalIndex } from '../data/retrieval/index.mjs';
import { sourceReference, hashText } from '../data/retrieval/retrieval-contracts.mjs';
import { sourceWindow } from '../data/retrieval/source-window.mjs';

const document = '# Introduction\r\nPublic background.\r\n## Safety review\r\nReviewer: Mara Chen.\r\n```md\r\n# Fake code heading\r\n```\r\n### Checklist\r\nSigned checklist.\r\n## Launch\r\nLaunch date is undecided.\r\n';

test('limited chapter parsing falls back to an anchored window without inventing a final section boundary', () => {
  const text = '# x\n\n'.repeat(6000) + '# Final\n\nINDEPENDENT_FINAL_EVIDENCE\n';
  const anchorOffset = text.indexOf('INDEPENDENT_FINAL_EVIDENCE');
  const result = sourceWindow(text, { mode: 'section', anchorOffset, beforeCharacters: 0, limit: 256 });
  assert.equal(result.window.mode, 'window');
  assert.equal(result.window.section, undefined);
  assert.equal(result.window.sectionUnavailable, true);
  assert.equal(result.window.navigationTruncated, true);
  assert.deepEqual(result.window.diagnosticCodes, ['DOCUMENT_STRUCTURE_LIMIT']);
  assert.equal(result.text, text.slice(anchorOffset));
});

test('sections use real Markdown hierarchy and exact original offsets while excluding fenced headings', () => {
  const anchorOffset = document.indexOf('Mara Chen'), result = sourceWindow(document, { mode: 'section', anchorOffset });
  assert.equal(result.window.mode, 'section'); assert.equal(result.window.section.title, 'Safety review');
  assert.equal(result.window.section.level, 2);
  assert.equal(result.offset, document.indexOf('## Safety review'));
  assert.equal(result.nextOffset, document.indexOf('## Launch'));
  assert.equal(result.text, document.slice(result.offset, result.nextOffset));
  assert.match(result.text, /### Checklist/); assert.doesNotMatch(result.text, /Launch date/);
  const codeAnchor = sourceWindow(document, { mode: 'section', anchorOffset: document.indexOf('Fake code heading') });
  assert.equal(codeAnchor.window.section.title, 'Safety review');
  const nested = sourceWindow(document, { mode: 'section', anchorOffset: document.indexOf('Signed checklist') });
  assert.equal(nested.window.section.title, 'Checklist'); assert.equal(nested.nextOffset, document.indexOf('## Launch'));
  const setext = 'Title\n=====\nPublic text.\nSubsection\n----------\nMore text.\n';
  assert.equal(sourceWindow(setext, { mode: 'section', anchorOffset: setext.indexOf('More text') }).window.section.title, 'Subsection');
});

test('plain and oversized sections stay bounded around an anchor and Unicode endpoints remain complete', () => {
  const text = '# Long chapter\n' + '😀public '.repeat(4000) + '\n# Next\nEnd.';
  const anchorOffset = text.indexOf('public ', 15000), result = sourceWindow(text, { mode: 'section', anchorOffset, limit: 100 });
  assert.ok(result.text.length <= 100); assert.equal(result.window.section.startOffset, 0);
  assert.equal(result.window.section.endOffset, text.indexOf('# Next'));
  assert.ok(result.window.clippedAtStart); assert.ok(result.window.clippedAtEnd);
  assert.ok(result.offset <= anchorOffset && result.nextOffset > anchorOffset);
  assert.equal(result.text, text.slice(result.offset, result.nextOffset));
  assert.doesNotMatch(result.text, /^[\uDC00-\uDFFF]|[\uD800-\uDBFF]$/);
  const tiny = sourceWindow('ab😀cd', { anchorOffset: 2, beforeCharacters: 0, limit: 2 });
  assert.equal(tiny.text, '😀');
  const plain = sourceWindow('plain public document', { mode: 'section', anchorOffset: 8, beforeCharacters: 4, limit: 8 });
  assert.equal(plain.window.mode, 'window'); assert.equal(plain.text, 'n public');
  assert.equal(plain.window.section, undefined);
});

test('window validation rejects offsets, split characters, unsupported modes and budgets; long scans are cancellable', () => {
  for (const options of [{ anchorOffset: 7 }, { anchorOffset: 3 }, { limit: 1 }, { limit: 16001 },
    { beforeCharacters: 4097 }, { mode: 'all' }, { anchorOffset: -1 }])
    assert.throws(() => sourceWindow('ab😀cd', options), error => /^INVALID_RETRIEVAL_/.test(error.code));
  let checks = 0;
  assert.throws(() => sourceWindow('line\n'.repeat(10000), { mode: 'section' }, () => {
    if (++checks === 2) throw Object.assign(new Error('cancelled'), { name: 'AbortError' });
  }), { name: 'AbortError' });
});

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-source-window-')), index = new RetrievalIndex({ root, vectorEnabled: false });
  t.after(async () => {
    await index.close();
    const suffix = relative(resolve(tmpdir()), resolve(root));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(root, { recursive: true, force: true });
  });
  return { root, index };
}

test('indexed window anchors the matched chunk and preserves canonical identities and original records', async t => {
  const { root, index } = await fixture(t), text = 'prefix public text.\n'.repeat(50) + document;
  const source = { sourceId: 'window-document', scopeKey: 'project:work-a', sourceRevision: 1, text,
    title: 'Public Markdown', sourceType: 'document', locator: { relativePath: 'public.md' } };
  await index.upsertSources([source]);
  const candidate = (await index.search({ query: 'Mara', scopeKeys: ['project:work-a'] })).items[0];
  assert.ok(candidate.locator.startOffset > 0);
  const registryPath = join(root, 'Retrieval', 'source-identities.json'), before = await readFile(registryPath, 'utf8');
  const window = await index.readWindow({ sourceRef: candidate.sourceRef, scopeKeys: ['project:work-a'], limit: 80 });
  assert.equal(window.window.anchorOffset, candidate.locator.startOffset);
  assert.equal(window.text, text.slice(window.offset, window.nextOffset)); assert.ok(window.text.length <= 80);
  const section = await index.readWindow({ sourceRef: candidate.sourceRef, scopeKeys: ['project:work-a'],
    mode: 'section', anchorOffset: text.indexOf('Mara Chen'), limit: 4000 });
  assert.equal(section.window.section.title, 'Safety review');
  assert.equal(section.text, text.slice(section.offset, section.nextOffset));
  assert.equal(section.contentHash, hashText(text)); assert.equal(section.sourceRevision, 1);
  assert.equal(await readFile(registryPath, 'utf8'), before);
  assert.equal((await index.read({ sourceId: source.sourceId, scopeKeys: ['project:work-a'] })).text, text);
});

test('window source versions, authorization, revocation and cancellation use the same guarded index path', async t => {
  const { index } = await fixture(t), source = { sourceId: 'guarded-window', scopeKey: 'user', sourceRevision: 1, text: document };
  await index.upsertSources([source]);
  const candidate = (await index.search({ query: 'Mara', scopeKeys: ['user'] })).items[0];
  await assert.rejects(index.readWindow({ sourceRef: candidate.sourceRef, scopeKeys: ['project:other'] }), { code: 'RETRIEVAL_SOURCE_NOT_FOUND' });
  const controller = new AbortController(); controller.abort();
  await assert.rejects(index.readWindow({ sourceRef: candidate.sourceRef, scopeKeys: ['user'], signal: controller.signal }), { code: 'ABORT_ERR' });
  await index.upsertSources([{ ...source, sourceRevision: 2, text: document + 'Current revision.' }]);
  await assert.rejects(index.readWindow({ sourceRef: candidate.sourceRef, scopeKeys: ['user'] }), { code: 'STALE_RETRIEVAL_SOURCE' });
  const currentRef = sourceReference({ ...source, sourceRevision: 2, contentHash: hashText(document + 'Current revision.') });
  await assert.rejects(index.readWindow({ sourceRef: currentRef, scopeKeys: ['user'], anchorOffset: 20000 }), { code: 'INVALID_RETRIEVAL_OFFSET' });
  await index.removeSource(source.sourceId, { scopeKeys: ['user'] });
  await assert.rejects(index.readWindow({ sourceRef: currentRef, scopeKeys: ['user'] }), { code: 'RETRIEVAL_SOURCE_NOT_FOUND' });
  await assert.rejects(index.upsertSources([{ ...source, sourceRevision: 3 }]), { code: 'RETRIEVAL_SOURCE_REVOKED' });
});

test('default section reads cover a chunk crossing headings and expose navigation instead of guessing its chapter', async t => {
  const { index } = await fixture(t);
  const text = '# Unrelated introduction\n' + 'Background only. '.repeat(22) + '\n## Safety\nReviewer: Mara Chen.\n## Launch\nUndecided.\n';
  await index.upsertSources([{ sourceId: 'cross-heading', scopeKey: 'user', sourceRevision: 1, text }]);
  const candidate = (await index.search({ query: 'Mara', scopeKeys: ['user'] })).items[0];
  assert.ok(candidate.locator.startOffset < text.indexOf('## Safety'));
  assert.ok(candidate.locator.endOffset > text.indexOf('Mara Chen'));
  const result = await index.readWindow({ sourceRef: candidate.sourceRef, scopeKeys: ['user'], mode: 'section' });
  assert.equal(result.window.mode, 'window'); assert.equal(result.window.spansSections, true);
  assert.equal(result.window.section, undefined); assert.equal(result.window.referenceRangeCovered, true);
  assert.deepEqual(result.window.referenceRange, { startOffset: candidate.locator.startOffset, endOffset: candidate.locator.endOffset });
  assert.match(result.text, /Mara Chen/); assert.deepEqual(result.window.sections.map(value => value.title), ['Unrelated introduction', 'Safety', 'Launch']);
  const explicit = await index.readWindow({ sourceRef: candidate.sourceRef, scopeKeys: ['user'], mode: 'section', anchorOffset: text.indexOf('Mara Chen') });
  assert.equal(explicit.window.section.title, 'Safety'); assert.doesNotMatch(explicit.text, /Background|Launch/);
  const clipped = await index.readWindow({ sourceRef: candidate.sourceRef, scopeKeys: ['user'], mode: 'section', limit: 20 });
  assert.ok(clipped.text.length <= 20); assert.equal(clipped.window.referenceRangeCovered, false);
});

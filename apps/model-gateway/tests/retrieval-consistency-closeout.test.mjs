import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { EventEmitter } from 'node:events';
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { test } from 'node:test';
import { DatabaseSync } from 'node:sqlite';
import { RetrievalIndex } from '../data/retrieval/index.mjs';
import { TaskExperienceStore } from '../data/retrieval/task-experience-store.mjs';
import { matchExpression } from '../data/retrieval/retrieval-text.mjs';
import { EvidenceAcquisition } from '../orchestration/retrieval/evidence-acquisition.mjs';
import { SourceSyncService } from '../orchestration/retrieval/source-sync.mjs';
import { classifySourceFailure, readSourceFile, readSourceTree, scanSourceTree } from '../tools/retrieval/source-reader.mjs';

async function temporary(t) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-consistency-'));
  const closers = [];
  t.after(async () => {
    for (const close of closers.reverse()) await close();
    const suffix = relative(resolve(tmpdir()), resolve(root));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(root, { recursive: true, force: true });
  });
  return { root, own: close => closers.push(close) };
}

const source = (id, scopeKey, text, extra = {}) => ({ sourceId: id, scopeKey, sourceType: 'knowledge',
  sourceRevision: 1, title: id, text, locator: { relativePath: `${id}.md` }, ...extra });
const settings = maximumFiles => ({ local: { indexing: { maximumFiles, maximumSourceBytes: 33554432,
  maximumTotalBytes: 536870912, maximumEntries: 200000 } }, cache: { memoryLimitBytes: 16777216 },
  projectIndexing: { mountedFolder: true, bindingRevision: 1 } });
function watchFactory() { const watcher = new EventEmitter(); watcher.close = () => {}; return watcher; }

test('joint-scope lexical statistics match an independent union and invalidate after removal and domain changes', async t => {
  const f = await temporary(t), index = new RetrievalIndex({ root: f.root, vectorEnabled: false });
  f.own(() => index.close());
  const documents = [source('joint-a', 'user', 'alpha alpha beta'),
    source('joint-b', 'project:joint', 'beta '.repeat(30) + 'alpha'),
    source('joint-c', 'project:joint', 'filler '.repeat(50)),
    source('outside', 'project:outside', 'alpha '.repeat(300))];
  await index.upsertSources(documents);
  const database = new DatabaseSync(join(f.root, 'Index', 'retrieval.sqlite'), { readOnly: true });
  f.own(() => database.close());
  const scopes = ['project:joint', 'user'];
  const oracle = async () => {
    database.exec(`DROP TABLE IF EXISTS temp.union_fts;
      CREATE VIRTUAL TABLE temp.union_fts USING fts5(lexical_text,tokenize='unicode61');
      INSERT INTO temp.union_fts(rowid,lexical_text) SELECT c.id,c.lexical_text FROM chunks c
      JOIN sources s ON s.source_id=c.source_id WHERE s.scope_key IN ('user','project:joint');`);
    const expected = database.prepare(`SELECT c.chunk_id,bm25(union_fts) AS lexical_rank FROM union_fts
      JOIN chunks c ON c.id=union_fts.rowid WHERE union_fts MATCH ? ORDER BY lexical_rank,c.chunk_id`).all(matchExpression('alpha OR beta'));
    const actual = await index.search({ query: 'alpha OR beta', scopeKeys: scopes });
    assert.deepEqual(actual.items.map(item => item.chunkId), expected.map(item => item.chunk_id));
    for (const [position, item] of actual.items.entries()) assert.ok(Math.abs(item.lexicalScore - expected[position].lexical_rank) < 1e-18);
    return actual.items.map(item => [item.chunkId, item.lexicalScore]);
  };
  const initial = await oracle();
  assert.deepEqual((await index.search({ query: 'alpha OR beta', scopeKeys: [...scopes].reverse() })).items
    .map(item => [item.chunkId, item.lexicalScore]), initial);
  await index.removeSource('joint-b', { scopeKeys: scopes });
  assert.notDeepEqual(await oracle(), initial);
  await index.upsertSources([source('joint-domain', 'project:joint', 'alpha code', { sourceType: 'code', locator: { relativePath: 'joint-domain.mjs' } })]);
  const codeResult = await index.search({ query: 'alpha', scopeKeys: scopes, retrievalIntent: { domain: 'code' } });
  assert.ok(codeResult.items.some(item => item.sourceId === 'joint-domain'));
  await index.upsertSources([source('joint-domain', 'project:joint', 'beta knowledge', { sourceRevision: 2 })]);
  assert.ok(!(await index.search({ query: 'alpha', scopeKeys: scopes, retrievalIntent: { domain: 'code' } })).items
    .some(item => item.sourceId === 'joint-domain'));
  await index.invalidateScope('project:joint');
  assert.ok((await index.search({ query: 'alpha', scopeKeys: scopes })).items.every(item => item.scopeKey === 'user'));
});

test('mounted scans retain an admitted partial corpus and name the limit without weakening explicit imports', async t => {
  const f = await temporary(t), workspace = join(f.root, 'workspace'); await mkdir(workspace);
  for (let index = 0; index < 3; index++) await writeFile(join(workspace, `${index}.md`), `Source ${index}.`);
  await assert.rejects(readSourceTree(workspace, { maximumFiles: 1 }), { code: 'RETRIEVAL_SCAN_LIMIT' });
  const service = new SourceSyncService({ library: { root: f.root }, getProject: async () => ({ FolderPath: workspace }), watchFactory });
  f.own(() => service.close());
  const partial = await service.mountedSnapshot('joint', settings(1));
  assert.equal(partial.sources.length, 1);
  assert.equal(partial.scan.coverage.complete, false);
  assert.deepEqual(partial.scan.coverage.limit, { dimension: 'files', limit: 1, observed: 2, unvisitedCountKnown: false });
  assert.equal(await partial.isCurrent(partial.sources[0]), true);
  const complete = await service.mountedSnapshot('joint', settings(3));
  assert.equal(complete.sources.length, 3); assert.equal(complete.scan.coverage.complete, true);
});

test('permanent source failures preserve diagnostics, skip redundant decoding and recover when the file changes', async t => {
  const f = await temporary(t), workspace = join(f.root, 'workspace'); await mkdir(workspace);
  const invalid = join(workspace, 'invalid.txt'); await writeFile(invalid, Buffer.from([0, 1, 2]));
  const failureCache = new Map(), firstStats = {}, secondStats = {}, first = [];
  for await (const item of scanSourceTree(workspace, { stats: firstStats, failureCache })) first.push(item);
  assert.equal(first.length, 0); assert.equal(firstStats.failedFiles, 1);
  assert.equal(firstStats.failures[0].category, 'unsupported'); assert.equal(firstStats.failures[0].retryable, false);
  for await (const _item of scanSourceTree(workspace, { stats: secondStats, failureCache })) assert.fail('Invalid source cannot be published.');
  assert.equal(secondStats.reusedFailures, 1); assert.equal(secondStats.failedFiles, 1);
  await writeFile(invalid, 'Repaired text source.');
  const recoveredStats = {}, recovered = [];
  for await (const item of scanSourceTree(workspace, { stats: recoveredStats, failureCache })) recovered.push(item);
  assert.equal(recovered[0].text, 'Repaired text source.');
  assert.equal(recoveredStats.failedFiles ?? 0, 0); assert.equal(failureCache.size, 0);
});

test('transient missing-source reads retry once while capacity, permission and OCR failures remain distinct', async t => {
  const f = await temporary(t), path = join(f.root, 'appearing.md');
  await assert.rejects(readSourceFile(path), error => error.code === 'ENOENT' && error.sourceReadAttempts === 2);
  await writeFile(path, 'Source appeared after bounded failure.');
  assert.equal((await readSourceFile(path)).text, 'Source appeared after bounded failure.');
  assert.deepEqual(classifySourceFailure({ code: 'DOCUMENT_RESOURCE_BUSY' }), { errorCode: 'DOCUMENT_RESOURCE_BUSY', category: 'resource', retryable: true });
  assert.equal(classifySourceFailure({ code: 'DOCUMENT_OUTPUT_LIMIT' }).category, 'limit');
  assert.equal(classifySourceFailure({ code: 'EPERM' }).retryable, false);
  assert.equal(classifySourceFailure({ code: 'OCR_UNAVAILABLE' }).category, 'ocr-unavailable');
  assert.equal(classifySourceFailure({ code: 'DOCUMENT_PARSE_FAILED' }).retryable, false);
});

test('front-end discovery returns early verified descriptors while the remaining scan waits for resource admission', async t => {
  const f = await temporary(t), workspace = join(f.root, 'workspace'); await mkdir(workspace);
  await writeFile(join(workspace, '00-ready.md'), 'Current source already discovered.');
  await writeFile(join(workspace, '99-pending.pdf'), '%PDF-1.4\ninvalid fixture');
  let releaseAdmission;
  const gate = new Promise(resolveGate => { releaseAdmission = resolveGate; });
  const service = new SourceSyncService({ library: { root: f.root }, getProject: async () => ({ FolderPath: workspace }), watchFactory,
    resourceService: { acquire: async () => { await gate; return { status: 'denied' }; } } });
  f.own(() => service.close());
  const started = performance.now(), foreground = await service.foregroundMountedSnapshot('joint', settings(5));
  assert.ok(performance.now() - started < 1000);
  assert.equal(foreground.scan.backgroundPending, true); assert.equal(foreground.scan.coverage.complete, false);
  assert.equal(foreground.sources[0].title, '00-ready.md');
  assert.equal((await foreground.loadSource(foreground.sources[0])).text, 'Current source already discovered.');
  releaseAdmission();
  await Promise.all([...service.foregroundContinuations.values()]);
  const settled = await service.mountedSnapshot('joint', settings(5));
  assert.equal(settled.scan.coverage.complete, false); assert.equal(settled.scan.failures[0].category, 'resource');
});

test('reworded searches without new observations select recovery rather than claiming sufficiency', () => {
  const acquisition = new EvidenceAcquisition();
  let status;
  for (const query of ['launch date', 'date of release']) status = acquisition.observe(
    acquisition.prepare({ query, snapshotKey: 'stable-corpus', cacheKey: query }), { items: [], strategy: 'lexical' });
  assert.equal(status.stopSameStrategy, true); assert.equal(status.next, 'switch-channel-or-read-known-source');
  assert.equal(status.decision.shouldContinueSearch, false); assert.equal(status.sufficiency, 'not-evaluated');
  const alternative = acquisition.observe(acquisition.prepare({ query: 'release symbol', snapshotKey: 'stable-corpus', cacheKey: 'symbol' }),
    { items: [], strategy: 'structure' });
  assert.equal(alternative.stopSameStrategy, false);
  const newer = acquisition.observe(acquisition.prepare({ query: 'release', snapshotKey: 'new-corpus', cacheKey: 'next' }),
    { items: [], strategy: 'lexical' });
  assert.equal(newer.stopSameStrategy, false);
});

test('reading a newer version invalidates old conclusions and makes the new quotation current', () => {
  const acquisition = new EvidenceAcquisition();
  const old = { sourceId: 'release', scopeKey: 'user', sourceRef: 'old-ref', sourceRevision: 1, contentHash: 'a'.repeat(64), text: 'Old release condition.' };
  acquisition.observe(acquisition.prepare({ query: 'release', snapshotKey: 'old', cacheKey: 'old' }), { items: [old] });
  acquisition.observeRead(old);
  acquisition.assess({ conclusionId: 'before', claims: [{ statement: 'Earlier condition', support: [{ sourceRef: 'old-ref', quote: old.text }] }] });
  const current = { ...old, sourceRef: 'current-ref', sourceRevision: 2, contentHash: 'b'.repeat(64), text: 'Current release condition.' };
  acquisition.observeRead(current);
  assert.equal(acquisition.conclusions.get('before').state, 'stale');
  const assessed = acquisition.assess({ conclusionId: 'after', claims: [{ statement: 'Current condition', support: [{ sourceRef: 'current-ref', quote: current.text }] }] });
  assert.equal(assessed.state, 'ready-to-answer'); assert.equal(assessed.correctnessCertified, false);
});

test('experience expires locally, rejects malformed entries and preserves incomplete records as navigation only', async t => {
  const f = await temporary(t); let now = Date.parse('2026-10-08T00:00:00Z');
  const store = new TaskExperienceStore(f.root, { clock: () => now });
  const input = { scopeKeys: ['chat:one'], query: 'release conditions', sourceRefs: ['rag1:synthetic-reference'], conclusionId: 'release', state: 'ready-to-answer', complete: false };
  await store.save(input);
  const [partial] = await store.find({ scopeKeys: ['chat:one'] });
  assert.equal(partial.recordState, 'partial'); assert.equal(partial.correctnessCertified, false);
  const path = join(f.root, 'Retrieval', 'Experiences', `${createHash('sha256').update('chat:one').digest('hex')}.json`);
  const document = JSON.parse(await readFile(path, 'utf8'));
  document.entries.push({ ...document.entries[0], id: 'c'.repeat(64), query: {} });
  await writeFile(path, JSON.stringify(document));
  assert.equal((await store.find({ scopeKeys: ['chat:one'] })).length, 1);
  assert.equal(store.readDiagnostics.get('chat:one').invalid, 1);
  now += 31 * 24 * 60 * 60 * 1000;
  assert.equal((await store.find({ scopeKeys: ['chat:one'] })).length, 0);
  assert.equal(store.readDiagnostics.get('chat:one').expired, 1);
  assert.equal((await store.find({ scopeKeys: ['user', 'project:one', 'chat:two'] })).length, 0);
});

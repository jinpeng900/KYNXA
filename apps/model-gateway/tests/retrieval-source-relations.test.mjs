import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { test } from 'node:test';
import { DatabaseSync } from 'node:sqlite';
import { RetrievalIndex, chunkSource } from '../data/retrieval/index.mjs';
import { RetrievalStructureService } from '../data/retrieval/structure-service.mjs';
import { SourceCoverageStore } from '../data/retrieval/source-coverage.mjs';

test('syntax relations and uncertain textual references are scoped and replaced with their source version', async t => {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-relations-')), index = new RetrievalIndex({ root, vectorEnabled: false });
  const structures = new RetrievalStructureService();
  t.after(async () => { await structures.close(); await index.close(); const suffix = relative(resolve(tmpdir()), root);
    assert.ok(suffix && !suffix.startsWith(`..${sep}`)); await rm(root, { recursive: true, force: true }); });
  const source = { sourceId: 'code-relations', scopeKey: 'project:a', sourceType: 'work-file', title: 'reader.js',
    locator: { relativePath: 'reader.js' }, sourceRevision: 1, text: 'class Reader { read(value) { return consume(value); } }' };
  await index.upsertSources([{ ...source, ...await structures.parse(source) }]);
  const relations = await index.relations({ scopeKeys: ['project:a'], symbol: 'Reader.read' });
  assert.ok(relations.items.some(item => item.kind === 'definition' && item.certainty === 'syntax'));
  assert.ok(relations.items.some(item => item.kind === 'contains' && item.fromSymbol === 'Reader'));
  const mention = await index.relations({ scopeKeys: ['project:a'], symbol: 'consume' });
  assert.equal(mention.items[0].certainty, 'lexical-unresolved'); assert.equal(mention.exhaustiveReferences, false);
  assert.equal((await index.relations({ scopeKeys: ['project:b'] })).items.length, 0);
  const oldRef = relations.items[0].sourceRef;
  const current = { ...source, sourceRevision: 2, text: 'class Reader { update(value) { return different(value); } }' };
  await index.upsertSources([{ ...current, ...await structures.parse(current) }]);
  await assert.rejects(index.relations({ scopeKeys: ['project:a'], sourceRef: oldRef }), { code: 'STALE_RETRIEVAL_SOURCE' });
  assert.equal((await index.relations({ scopeKeys: ['project:a'], symbol: 'consume' })).items.length, 0);
  assert.ok((await index.relations({ scopeKeys: ['project:a'], symbol: 'Reader.update' })).items.length > 0);
});

test('coverage counts and source rows both verify complete vectors in the current scoped publication', async t => {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-coverage-')), index = new RetrievalIndex({ root, vectorEnabled: false });
  t.after(async () => { await index.close(); const suffix = relative(resolve(tmpdir()), root);
    assert.ok(suffix && !suffix.startsWith(`..${sep}`)); await rm(root, { recursive: true, force: true }); });
  const source = { sourceId: 'coverage-vectors', scopeKey: 'project:a', sourceType: 'knowledge', title: 'Evidence',
    locator: {}, sourceRevision: 1, text: 'Many actual evidence paragraphs. '.repeat(80) };
  const chunks = chunkSource(source);
  assert.ok(chunks.length > 1);
  await index.upsertSources([{ ...source, chunks, vectors: chunks.map((_, number) => number === 0 ? [1, 0] : null), embeddingProfileId: 'fixture' }]);
  await index.recordCoverage({ scopeKeys: ['project:a'], entries: [{ scopeKey: 'project:a', sourceId: source.sourceId,
    sourceRevision: 1, relativePath: 'evidence.md', status: 'ready', lexical: 'ready', semantic: 'ready', parser: 'ready' }] });
  const partial = await index.coverage({ scopeKeys: ['project:a'] });
  assert.equal(partial.counts.lexical, 1); assert.equal(partial.counts.semantic, 0);
  assert.equal(partial.items[0].semantic, 'unverified'); assert.equal(partial.items[0].status, 'unverified');
  assert.equal((await index.coverage({ scopeKeys: ['project:b'] })).items.length, 0);
  await index.upsertSources([{ ...source, chunks, vectors: chunks.map(() => [1, 0]), embeddingProfileId: 'fixture' }]);
  const ready = await index.coverage({ scopeKeys: ['project:a'] });
  assert.equal(ready.counts.semantic, 1); assert.equal(ready.items[0].semantic, 'ready');
  await index.upsertSources([{ ...source, parserVersion: 'new-parser-v2' }]);
  const changed = await index.coverage({ scopeKeys: ['project:a'] });
  assert.equal(changed.counts.semantic, 0); assert.equal(changed.items[0].semantic, 'unverified');
});

test('coverage-only deletion honors scope and permits repaired sources without a fabricated permanent tombstone', async t => {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-coverage-only-')), index = new RetrievalIndex({ root, vectorEnabled: false });
  t.after(async () => { await index.close(); const suffix = relative(resolve(tmpdir()), root);
    assert.ok(suffix && !suffix.startsWith(`..${sep}`)); await rm(root, { recursive: true, force: true }); });
  const failure = { scopeKey: 'project:a', sourceId: 'never-published', sourceType: 'work-file', relativePath: 'failed.pdf',
    status: 'failed', lexical: 'unverified', semantic: 'disabled', parser: 'failed', errorCode: 'DOCUMENT_PARSE_FAILED' };
  await index.recordCoverage({ scopeKeys: ['project:a'], entries: [failure] });
  await index.removeSource(failure.sourceId, { scopeKeys: ['project:b'], permanent: false });
  assert.equal((await index.coverage({ scopeKeys: ['project:a'] })).counts.failed, 1);
  await index.removeSource(failure.sourceId, { scopeKeys: ['project:a'], permanent: false });
  assert.equal((await index.coverage({ scopeKeys: ['project:a'] })).counts.failed, 0);
  await index.upsertSources([{ sourceId: failure.sourceId, scopeKey: failure.scopeKey, sourceType: 'work-file',
    sourceRevision: 1, title: 'Repaired', locator: {}, text: 'Current repaired source remains admissible' }]);
  assert.equal((await index.search({ query: 'repaired', scopeKeys: ['project:a'] })).items.length, 1);
});

test('complete mounted coverage inventories retain active failures, registered knowledge and other scopes', async t => {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-coverage-inventory-')), index = new RetrievalIndex({ root, vectorEnabled: false });
  t.after(async () => { await index.close(); const suffix = relative(resolve(tmpdir()), root);
    assert.ok(suffix && !suffix.startsWith(`..${sep}`)); await rm(root, { recursive: true, force: true }); });
  const failed = (sourceId, sourceType = 'work-file', scopeKey = 'project:a') => ({ sourceId, sourceType, scopeKey,
    relativePath: `${sourceId}.pdf`, status: 'failed', lexical: 'unverified', semantic: 'disabled', parser: 'failed',
    errorCode: 'DOCUMENT_PARSE_FAILED' });
  await index.recordCoverage({ scopeKeys: ['project:a', 'project:b'], entries: [failed('removed'), failed('active'),
    failed('registered', 'knowledge'), failed('other-scope', 'work-file', 'project:b')] });
  const reconciled = await index.reconcileCoverage({ scopeKeys: ['project:a'], sourceTypes: ['work-file'], sourceIds: ['active'] });
  assert.equal(reconciled.removed, 1);
  assert.deepEqual((await index.coverage({ scopeKeys: ['project:a'] })).items.map(item => item.sourceId).sort(), ['active', 'registered']);
  assert.equal((await index.coverage({ scopeKeys: ['project:b'] })).counts.failed, 1);
});

test('older discovery-only failure rows acquire a cleanup type without creating authoritative source rows', () => {
  const database = new DatabaseSync(':memory:');
  try {
    database.exec(`CREATE TABLE source_coverage (scope_key TEXT NOT NULL,source_id TEXT NOT NULL,relative_path TEXT NOT NULL,
      status TEXT NOT NULL,lexical TEXT NOT NULL,semantic TEXT NOT NULL,parser TEXT NOT NULL,error_code TEXT,
      source_revision TEXT,updated_at TEXT NOT NULL,PRIMARY KEY(scope_key,source_id));
      INSERT INTO source_coverage VALUES ('project:a','old-failure','old.pdf','failed','unverified','disabled','failed',
        'DOCUMENT_PARSE_FAILED','null','2000-01-01T00:00:00.000Z');`);
    const store = new SourceCoverageStore(database);
    assert.equal(store.reconcile(['project:a'], ['work-file'], []).removed, 1);
    assert.equal(database.prepare('SELECT count(*) AS count FROM source_coverage').get().count, 0);
    assert.equal(database.prepare("SELECT count(*) AS count FROM sqlite_master WHERE name='sources'").get().count, 0);
    database.exec(`INSERT INTO source_coverage (scope_key,source_id,relative_path,status,lexical,semantic,parser,source_revision,updated_at)
      VALUES ('project:a','interrupted-failure','interrupted.pdf','failed','unverified','disabled','failed','null','2000-01-01T00:00:00.000Z')`);
    assert.equal(new SourceCoverageStore(database).reconcile(['project:a'], ['work-file'], []).removed, 1);
  } finally { database.close(); }
});

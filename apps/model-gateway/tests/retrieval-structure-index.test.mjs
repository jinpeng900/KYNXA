import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { test } from 'node:test';
import { DatabaseSync } from 'node:sqlite';
import { RetrievalIndex, chunkSource } from '../data/retrieval/index.mjs';
import { hashText, validateChunkStructure, validateStructureDescriptor } from '../data/retrieval/retrieval-contracts.mjs';

async function fixture(t, options = {}) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-retrieval-structure-'));
  const index = new RetrievalIndex({ root, ...options });
  t.after(async () => {
    await index.close().catch(() => {});
    const suffix = relative(resolve(tmpdir()), resolve(root));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(root, { recursive: true, force: true });
  });
  return { root, index };
}

function codeSource(sourceId, scopeKey = 'project:one', extra = {}) {
  const text = 'export function performTask() {\n' + '  const usefulValue = 1;\n'.repeat(24) + '  return usefulValue;\n}\n';
  const source = { sourceId, scopeKey, sourceType: 'code', sourceRevision: 1, title: 'Module source',
    text, locator: { relativePath: `modules/${sourceId}.mjs` }, parserVersion: 'fixture-ast-v1',
    structure: { domain: 'code', language: 'javascript', parserVersion: 'fixture-ast-v1', parseStatus: 'parsed', diagnosticCodes: [] }, ...extra };
  source.chunks = chunkSource(source, { maxChars: 96 }).map(chunk => ({ ...chunk, structure: { domain: 'code',
    language: 'javascript', kind: 'function', symbolName: 'performTask', qualifiedName: 'TaskService.performTask',
    parentSymbol: 'TaskService', unitStartOffset: 0, unitEndOffset: source.text.length, parseStatus: 'parsed' } }));
  return source;
}

test('exact symbols retrieve one bounded definition anchor per unit after authorization filtering', async t => {
  const { index } = await fixture(t);
  const unauthorized = Array.from({ length: 60 }, (_, number) => codeSource(`secret-${number}`, 'project:other'));
  const authorized = codeSource('allowed');
  await index.upsertSources([...unauthorized, authorized]);
  const result = await index.search({ query: 'unmatchedintent', scopeKeys: ['project:one'], retrievalIntent: { domain: 'code', symbol: 'TaskService.performTask' } });
  assert.equal(result.items.length, 1);
  assert.equal(result.items[0].sourceId, authorized.sourceId);
  assert.equal(result.items[0].chunkIndex, 0);
  assert.equal(result.items[0].symbolRank, 1);
  assert.equal(result.items[0].structure.symbolName, 'performTask');
  assert.equal(result.structuredChannels.symbolCandidates, 1);
  assert.deepEqual(result.coverage, { symbols: 'indexed-definitions', references: false, callGraph: false });
  const simple = await index.search({ query: 'unmatchedintent', scopeKeys: ['project:one'], retrievalIntent: { symbol: 'performTask' } });
  assert.equal(simple.items[0].sourceId, authorized.sourceId);
  assert.equal((await index.search({ query: 'unmatchedintent', scopeKeys: ['project:one'], retrievalIntent: { symbol: 'performtask' } })).items.length, 0);
});

test('relative-path channel handles separators and suffixes literally without wildcard expansion', async t => {
  const { index } = await fixture(t);
  const literal = codeSource('literal-path', 'project:one', { locator: { relativePath: 'src/literal_%/file_Name.mjs' } });
  const wildcardLookalike = codeSource('other-path', 'project:one', { locator: { relativePath: 'src/literal_x/fileZName.mjs' } });
  await index.upsertSources([literal, wildcardLookalike, codeSource('secret-path', 'project:other', { locator: literal.locator })]);
  const exact = await index.search({ query: 'unmatchedintent', scopeKeys: ['project:one'], retrievalIntent: { path: 'literal_%\\file_Name.mjs' } });
  assert.deepEqual(exact.items.map(item => item.sourceId), [literal.sourceId]);
  assert.equal(exact.items[0].pathRank, 1);
  const directory = await index.search({ query: 'unmatchedintent', scopeKeys: ['project:one'], retrievalIntent: { path: 'src/literal_%/' } });
  assert.deepEqual(directory.items.map(item => item.sourceId), [literal.sourceId]);
});

test('qualified symbol suffixes omit namespaces while retaining case, component boundaries and scopes', async t => {
  const { index } = await fixture(t);
  const qualified = codeSource('namespaced');
  qualified.chunks = qualified.chunks.map(chunk => ({ ...chunk,
    structure: { ...chunk.structure, qualifiedName: 'Kynxa.TaskService.performTask' } }));
  const lookalike = codeSource('lookalike');
  lookalike.chunks = lookalike.chunks.map(chunk => ({ ...chunk,
    structure: { ...chunk.structure, qualifiedName: 'Kynxa.OtherTaskService.performTask' } }));
  const unauthorized = { ...qualified, sourceId: 'private-namespaced', scopeKey: 'project:other' };
  unauthorized.chunks = chunkSource(unauthorized, { maxChars: 96 }).map(chunk => ({ ...chunk, structure: qualified.chunks[0].structure }));
  await index.upsertSources([qualified, lookalike, unauthorized]);
  const result = await index.search({ query: 'unmatchedintent', scopeKeys: ['project:one'], retrievalIntent: { symbol: 'TaskService.performTask' } });
  assert.deepEqual(result.items.map(item => item.sourceId), [qualified.sourceId]);
  assert.equal(result.structuredChannels.symbolCandidates, 1);
  assert.equal((await index.search({ query: 'unmatchedintent', scopeKeys: ['project:one'], retrievalIntent: { symbol: 'taskService.performTask' } })).items.length, 0);
});

test('symbol and path intersection ranks the actual target unit before bounded competing definitions', async t => {
  const { index } = await fixture(t);
  const target = codeSource('zz-target', 'project:one', { locator: { relativePath: 'src/target_%/jobs.mjs' } });
  const prefix = '// file preamble\n', functionText = target.text;
  const originalStructure = target.chunks[0].structure;
  target.text = prefix + functionText;
  target.chunks = [
    { chunkId: 'zz-target:preamble', chunkIndex: 0, text: prefix, chunkHash: hashText(prefix), startOffset: 0, endOffset: prefix.length,
      startLine: 1, endLine: 2, structure: { domain: 'code', language: 'javascript', kind: 'preamble',
        unitStartOffset: 0, unitEndOffset: prefix.length, parseStatus: 'parsed' } },
    { chunkId: 'zz-target:function', chunkIndex: 1, text: functionText, chunkHash: hashText(functionText),
      startOffset: prefix.length, endOffset: target.text.length, startLine: 2, endLine: 2 + (functionText.match(/\n/gu)?.length ?? 0),
      structure: { ...originalStructure, unitStartOffset: prefix.length, unitEndOffset: target.text.length } }
  ];
  const competitors = Array.from({ length: 45 }, (_, number) => codeSource(`competitor-${number}`));
  const lookalike = codeSource('lookalike-path', 'project:one', { locator: { relativePath: 'src/target_x/jobs.mjs' } });
  const unauthorized = codeSource('private-path', 'project:other', { locator: target.locator });
  await index.upsertSources([...competitors, lookalike, unauthorized, target]);
  const result = await index.search({ query: 'unmatchedintent', scopeKeys: ['project:one'], limit: 1,
    retrievalIntent: { symbol: 'performTask', path: 'target_%/jobs.mjs' } });
  assert.equal(result.items[0].sourceId, target.sourceId);
  assert.equal(result.items[0].chunkIndex, 1);
  assert.equal(result.items[0].exactTargetMatch, true);
  assert.equal(result.structuredChannels.symbolCandidates, 40);
  assert.equal(result.structuredChannels.pathCandidates, 1);
});

test('domain intent filters lexical and vector channels before limits while keeping unknown legacy candidates', async t => {
  const { index } = await fixture(t);
  const knowledge = { sourceId: 'notes', scopeKey: 'project:one', sourceType: 'document', sourceRevision: 1, title: 'Notes',
    text: 'sharedterm documentation', embeddingProfileId: 'fixture', vectors: [[1, 0]] };
  const code = codeSource('code');
  code.text = code.text.replace('usefulValue', 'sharedterm');
  code.chunks = chunkSource(code, { maxChars: 96 }).map(chunk => ({ ...chunk, structure: { ...code.chunks[0].structure, unitEndOffset: code.text.length } }));
  code.embeddingProfileId = 'fixture'; code.vectors = code.chunks.map(() => [0, 1]);
  const unknown = { sourceId: 'old-uncategorized', scopeKey: 'project:one', sourceType: 'file', title: 'Legacy', sourceRevision: 1,
    text: 'sharedterm legacy material', embeddingProfileId: 'fixture', vectors: [[1, 0]] };
  await index.upsertSources([knowledge, code, unknown]);
  const options = { query: 'sharedterm', scopeKeys: ['project:one'], queryVector: [1, 0], embeddingProfileId: 'fixture', limit: 60 };
  const coding = await index.search({ ...options, retrievalIntent: { domain: 'code' } });
  assert.ok(coding.items.some(item => item.sourceId === code.sourceId));
  assert.ok(coding.items.some(item => item.sourceId === unknown.sourceId));
  assert.ok(coding.items.every(item => item.sourceId !== knowledge.sourceId));
  const docs = await index.search({ ...options, retrievalIntent: { domain: 'knowledge' } });
  assert.ok(docs.items.some(item => item.sourceId === knowledge.sourceId));
  assert.ok(docs.items.every(item => item.sourceId !== code.sourceId));
  const mixed = await index.search({ ...options, retrievalIntent: { domain: 'mixed' } });
  assert.ok(mixed.items.some(item => item.sourceId === code.sourceId) && mixed.items.some(item => item.sourceId === knowledge.sourceId));
});

test('unit reads expand the matched original definition within bounds and preserve signed freshness', async t => {
  const { index } = await fixture(t);
  const source = codeSource('unit-source');
  await index.upsertSources([source]);
  const result = await index.search({ query: 'unmatchedintent', scopeKeys: ['project:one'], retrievalIntent: { symbol: 'performTask' } });
  const sourceRef = result.items[0].sourceRef;
  const full = await index.readWindow({ sourceRef, scopeKeys: ['project:one'], mode: 'unit', limit: 4000 });
  assert.equal(full.text, source.text);
  assert.equal(full.window.mode, 'unit');
  assert.equal(full.offset, 0);
  assert.equal(full.hasMore, false);
  const bounded = await index.readWindow({ sourceRef, scopeKeys: ['project:one'], mode: 'unit', limit: 128 });
  assert.ok(bounded.text.length <= 128);
  assert.equal(bounded.text, source.text.slice(bounded.offset, bounded.nextOffset));
  assert.equal(bounded.hasMore, true);
  assert.equal(bounded.sourceRef, sourceRef);
  const continued = await index.readWindow({ sourceRef: bounded.sourceRef, scopeKeys: ['project:one'], mode: 'unit',
    anchorOffset: bounded.nextOffset, beforeCharacters: 0, limit: 128 });
  assert.equal(continued.window.mode, 'unit');
  assert.equal(continued.offset, bounded.nextOffset);
  assert.equal(continued.text, source.text.slice(continued.offset, continued.nextOffset));
  await assert.rejects(index.readWindow({ sourceRef, scopeKeys: ['project:other'], mode: 'unit' }), { code: 'RETRIEVAL_SOURCE_NOT_FOUND' });
  await assert.rejects(index.readWindow({ sourceRef, scopeKeys: ['project:one'], mode: 'unit', anchorOffset: source.text.length }), { code: 'INVALID_RETRIEVAL_OFFSET' });
  const renamed = { ...source, chunks: source.chunks.map(chunk => ({ ...chunk, structure: { ...chunk.structure, symbolName: 'renamedTask' } })) };
  await index.upsertSources([renamed]);
  await assert.rejects(index.readWindow({ sourceRef, scopeKeys: ['project:one'], mode: 'unit' }), { code: 'STALE_RETRIEVAL_SOURCE' });
});

test('structure changes invalidate preserved vectors; identical structures remain idempotent', async t => {
  const { index } = await fixture(t);
  const source = codeSource('vector-structure');
  source.embeddingProfileId = 'fixture'; source.embeddingSpaceId = hashText('fixture-space');
  source.vectors = source.chunks.map(() => [1, 0]);
  const first = await index.upsertSources([source]);
  assert.equal((await index.upsertSources([{ ...source, vectors: undefined }])).generation, first.generation);
  const changed = { ...source, vectors: undefined,
    chunks: source.chunks.map(chunk => ({ ...chunk, structure: { ...chunk.structure, parentSymbol: 'DifferentParent' } })) };
  assert.ok((await index.upsertSources([changed])).generation > first.generation);
  assert.equal((await index.status()).vectorChunks, 0);
});

test('malformed structural metadata cannot publish or enlarge trusted unit ranges', async t => {
  const { index } = await fixture(t);
  const source = codeSource('invalid-structure');
  const invalid = [
    { unitStartOffset: -1 }, { unitEndOffset: source.text.length + 1 }, { unitStartOffset: source.chunks[0].endOffset },
    { domain: 'all-files' }, { language: null }, { parseStatus: 'verified-by-model' },
    { symbolName: 'bad\nname' }, { sectionPath: new Array(17).fill('x') }, { externalAuthority: true }
  ];
  for (const extra of invalid) {
    const altered = { ...source, chunks: source.chunks.map(chunk => ({ ...chunk, structure: { ...chunk.structure, ...extra } })) };
    await assert.rejects(index.upsertSources([altered]));
  }
  await assert.rejects(index.upsertSources([{ ...source, structure: { ...source.structure, units: [] } }]));
  await assert.rejects(index.upsertSources([{ ...source, parserVersion: 'other-version' }]), { code: 'INVALID_RETRIEVAL_STRUCTURE' });
  assert.equal((await index.status()).sources, 0);
  assert.throws(() => validateStructureDescriptor({ ...source.structure, diagnosticCodes: ['invalid-code'] }));
  assert.throws(() => validateChunkStructure({ ...source.chunks[0].structure, unitEndOffset: 1 }, source, source.chunks[0]));
  assert.throws(() => index.search({ query: 'test', scopeKeys: ['project:one'], retrievalIntent: { domain: 'system' } }));
});

test('legacy schema 2 migrates additively; relative paths stay searchable and unit fallback is explicit', async t => {
  const { root, index } = await fixture(t);
  const source = { sourceId: 'legacy-source', scopeKey: 'project:one', sourceType: 'document', sourceRevision: 1,
    text: 'Legacy original text retained.', title: 'Legacy', locator: { relativePath: 'docs/old.md' },
    embeddingProfileId: 'fixture', vectors: [[1, 0]] };
  await index.upsertSources([source]);
  const old = (await index.search({ query: 'Legacy', scopeKeys: ['project:one'] })).items[0];
  const status = await index.status();
  const registry = await readFile(join(root, 'Retrieval', 'source-identities.json'), 'utf8');
  await index.close();
  const database = new DatabaseSync(join(root, 'Index', 'retrieval.sqlite'));
  database.exec('DROP INDEX sources_relative_path; DROP INDEX chunks_symbol; DROP INDEX chunks_qualified_name;');
  for (const column of ['structure_json', 'relative_path']) database.exec(`ALTER TABLE sources DROP COLUMN ${column}`);
  for (const column of ['structure_json', 'structure_domain', 'symbol_name', 'qualified_name', 'parse_status', 'unit_start_offset', 'unit_end_offset'])
    database.exec(`ALTER TABLE chunks DROP COLUMN ${column}`);
  database.exec('PRAGMA user_version = 2'); database.close();
  const reopened = new RetrievalIndex({ root });
  try {
    const after = await reopened.status();
    assert.equal(after.schemaVersion, 3);
    assert.equal(after.indexSnapshotId, status.indexSnapshotId);
    assert.equal(after.vectorChunks, 1);
    const path = await reopened.search({ query: 'unmatchedintent', scopeKeys: ['project:one'], retrievalIntent: { path: 'old.md' } });
    assert.equal(path.items[0].sourceId, source.sourceId);
    const fallback = await reopened.readWindow({ sourceRef: old.sourceRef, scopeKeys: ['project:one'], mode: 'unit' });
    assert.equal(fallback.unitUnavailable, true);
    assert.equal(fallback.text, source.text);
    assert.equal(await readFile(join(root, 'Retrieval', 'source-identities.json'), 'utf8'), registry);
  } finally { await reopened.close(); }
});

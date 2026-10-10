import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { basename, join, relative, resolve, sep } from 'node:path';
import { test } from 'node:test';
import { executeFilesystem } from '../tools/filesystem-tools.mjs';
import { closeFilesystemSearches, searchFilesystem } from '../tools/filesystem-search.mjs';
import { parsed, toolFixture } from './tool-fixture.mjs';

async function searchFixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-search-pages-'));
  const context = { requestId: 'fixture-request', conversationId: 'fixture-chat', workspaceRoot: root, permissionMode: 'smart' };
  t.after(async () => {
    await closeFilesystemSearches(context);
    const suffix = relative(resolve(tmpdir()), resolve(root));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(root, { recursive: true, force: true });
  });
  return { root, context, search: (input, options, signal) => executeFilesystem('filesystem.search', context, input, root, signal, options) };
}

test('paged live search visits more than 1000 files exactly once, including empty result pages', async t => {
  const f = await searchFixture(t);
  for (let start = 0; start < 1105; start += 50) {
    await Promise.all(Array.from({ length: Math.min(50, 1105 - start) }, (_, index) =>
      writeFile(join(f.root, `source-${String(start + index).padStart(4, '0')}.txt`), 'ordinary text')));
  }
  let cursor, visitedFiles = 0, pages = 0;
  do {
    const page = await f.search({ query: 'absent', ...(cursor ? { cursor } : {}) });
    assert.equal(page.matches.length, 0);
    assert.ok(page.visitedFiles <= 1000);
    visitedFiles += page.visitedFiles;
    assert.equal(page.totalVisitedFiles, visitedFiles);
    assert.equal(page.page, ++pages);
    assert.equal(page.hasMore, Boolean(page.nextCursor));
    assert.equal(page.coverage.traversalComplete, !page.hasMore);
    cursor = page.nextCursor;
    assert.ok(pages < 30, 'the cursor advances instead of repeatedly scanning the root');
  } while (cursor);
  assert.ok(pages > 1);
  assert.equal(visitedFiles, 1105);
});

test('one file continues at its next matching line without losing or repeating matches', async t => {
  const f = await searchFixture(t);
  await writeFile(join(f.root, 'many.txt'), Array.from({ length: 431 }, (_, index) => `needle ${index}`).join('\n'));
  let cursor, previousCursor, matches = [], visited = 0;
  do {
    const page = await f.search({ query: 'needle', maxMatches: 37, ...(cursor ? { cursor } : {}) });
    matches.push(...page.matches);
    visited += page.visitedFiles;
    previousCursor = cursor;
    cursor = page.nextCursor;
    if (previousCursor) await assert.rejects(f.search({ query: 'needle', cursor: previousCursor }), { code: 'INVALID_TOOL_SEARCH_CURSOR' });
  } while (cursor);
  assert.equal(visited, 1);
  assert.deepEqual(matches.map(match => match.line), Array.from({ length: 431 }, (_, index) => index + 1));
  assert.equal(new Set(matches.map(match => match.sha256)).size, 1);
});

test('directories deeper than the former eight levels remain searchable', async t => {
  const f = await searchFixture(t);
  const nested = join(f.root, ...Array.from({ length: 14 }, (_, index) => `level-${index}`));
  await mkdir(nested, { recursive: true });
  await writeFile(join(nested, 'deep.mjs'), 'needle at depth fourteen');
  const page = await f.search({ query: 'needle' });
  assert.equal(page.matches.length, 1);
  assert.equal(page.matches[0].path, relative(f.root, join(nested, 'deep.mjs')));
  assert.equal(page.hasMore, false);
  const shallow = await f.search({ query: 'needle', recursive: false });
  assert.equal(shallow.matches.length, 0);
  assert.equal(shallow.hasMore, false);
});

test('cursors bind context, owner, root, query, recursion and protected scope without accepting tampering', async t => {
  const f = await searchFixture(t);
  await writeFile(join(f.root, 'source.txt'), 'needle one\nneedle two\nneedle three');
  const page = await f.search({ query: 'needle', maxMatches: 1 });
  const input = { query: 'needle', cursor: page.nextCursor };
  await assert.rejects(f.search({ ...input, cursor: 'x'.repeat(32) }), { code: 'INVALID_TOOL_SEARCH_CURSOR' });
  await assert.rejects(f.search({ ...input, query: 'other' }), { code: 'INVALID_TOOL_SEARCH_CURSOR' });
  await assert.rejects(f.search({ ...input, recursive: false }), { code: 'INVALID_TOOL_SEARCH_CURSOR' });
  await assert.rejects(f.search(input, { protectedRoots: [join(f.root, 'protected')] }), { code: 'INVALID_TOOL_SEARCH_CURSOR' });
  await assert.rejects(f.search(input, { searchOwner: {} }), { code: 'INVALID_TOOL_SEARCH_CURSOR' });
  await assert.rejects(executeFilesystem('filesystem.search', { ...f.context }, input, f.root), { code: 'INVALID_TOOL_SEARCH_CURSOR' });
  await assert.rejects(executeFilesystem('filesystem.search', f.context, input, join(f.root, 'source.txt')), { code: 'INVALID_TOOL_SEARCH_CURSOR' });
  const continued = await f.search(input);
  assert.deepEqual(continued.matches.map(match => match.line), [2, 3]);
});

test('source edits and directory additions invalidate continuation with an explicit restart error', async t => {
  const f = await searchFixture(t);
  const source = join(f.root, 'source.txt');
  await writeFile(source, 'needle one\nneedle two\nneedle three');
  const first = await f.search({ query: 'needle', maxMatches: 1 });
  await writeFile(source, 'needle changed\nneedle two\nneedle three');
  await assert.rejects(f.search({ query: 'needle', cursor: first.nextCursor }), { code: 'TOOL_SEARCH_CHANGED' });
  const restarted = await f.search({ query: 'needle', maxMatches: 1 });
  await writeFile(join(f.root, 'added.txt'), 'new needle');
  await assert.rejects(f.search({ query: 'needle', cursor: restarted.nextCursor }), { code: 'TOOL_SEARCH_CHANGED' });
  const fresh = await f.search({ query: 'needle' });
  assert.equal(fresh.matches.length, 4);
});

test('unavailable directory monitors do not disable authorized searches and disclose live coverage', async t => {
  const f = await searchFixture(t);
  await writeFile(join(f.root, 'source.txt'), 'needle one\nneedle two');
  const options = { watchDirectory: () => { throw Object.assign(new Error('unsupported'), { code: 'ENOSYS' }); } };
  const first = await searchFilesystem(f.context, f.root, { query: 'needle', maxMatches: 1 }, undefined, options);
  assert.equal(first.consistency.mode, 'live-unverified');
  assert.equal(first.diagnostics[0].code, 'TOOL_SEARCH_MONITOR_UNAVAILABLE');
  const second = await searchFilesystem(f.context, f.root, { query: 'needle', cursor: first.nextCursor }, undefined, options);
  assert.deepEqual(second.matches.map(match => match.line), [2]);
  assert.equal(second.consistency.atomicSnapshot, false);
});

test('protected, denied, binary and oversized files remain excluded on every page', async t => {
  const f = await searchFixture(t);
  await mkdir(join(f.root, 'protected'));
  await mkdir(join(f.root, 'node_modules'));
  await writeFile(join(f.root, 'protected', 'secret.txt'), 'needle protected');
  await writeFile(join(f.root, 'denied.txt'), 'needle denied');
  await writeFile(join(f.root, 'node_modules', 'dependency.txt'), 'needle dependency');
  await writeFile(join(f.root, 'binary.txt'), Buffer.from([0xff, 0xfe, 0x00]));
  await writeFile(join(f.root, 'oversized.txt'), 'needle'.repeat(200000));
  await writeFile(join(f.root, 'safe.txt'), 'needle allowed\nneedle still allowed');
  const options = { protectedRoots: [join(f.root, 'protected')], denyRead: candidate => basename(candidate) === 'denied.txt' };
  let cursor, matches = [], skipped = 0;
  do {
    const page = await f.search({ query: 'needle', maxMatches: 1, ...(cursor ? { cursor } : {}) }, options);
    matches.push(...page.matches);
    skipped += page.skipped;
    cursor = page.nextCursor;
  } while (cursor);
  assert.deepEqual(matches.map(match => match.path), ['safe.txt', 'safe.txt']);
  assert.ok(skipped >= 5);
});

test('cancellation and owner cleanup release cursors without affecting a different owner', async t => {
  const f = await searchFixture(t);
  await writeFile(join(f.root, 'source.txt'), 'needle\n'.repeat(400));
  const owner = {}, otherOwner = {};
  t.after(() => closeFilesystemSearches(otherOwner));
  const first = await f.search({ query: 'needle', maxMatches: 1 }, { searchOwner: owner });
  const other = await f.search({ query: 'needle', maxMatches: 1 }, { searchOwner: otherOwner });
  await closeFilesystemSearches(owner);
  await assert.rejects(f.search({ query: 'needle', cursor: first.nextCursor }, { searchOwner: owner }), { code: 'INVALID_TOOL_SEARCH_CURSOR' });
  const continued = await f.search({ query: 'needle', cursor: other.nextCursor, maxMatches: 1 }, { searchOwner: otherOwner });
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 0);
  try {
    await assert.rejects(f.search({ query: 'needle', cursor: continued.nextCursor }, { searchOwner: otherOwner }, controller.signal), { name: 'AbortError' });
  } finally { clearTimeout(timer); }
  await assert.rejects(f.search({ query: 'needle', cursor: continued.nextCursor }, { searchOwner: otherOwner }), { code: 'INVALID_TOOL_SEARCH_CURSOR' });
});

test('bounded abandoned searches evict the oldest idle cursor instead of accumulating handles', async t => {
  const f = await searchFixture(t);
  await writeFile(join(f.root, 'source.txt'), 'needle one\nneedle two');
  const first = await f.search({ query: 'needle', maxMatches: 1 });
  for (let index = 0; index < 16; index++) await f.search({ query: 'needle', maxMatches: 1 });
  await assert.rejects(f.search({ query: 'needle', cursor: first.nextCursor }), { code: 'INVALID_TOOL_SEARCH_CURSOR' });
});

test('real broker preserves cursors under long-output projection and releases them with its context', async t => {
  const f = await toolFixture(t), context = await f.context('smart');
  const source = Array.from({ length: 250 }, (_, index) => `needle ${index} ${'"\\'.repeat(210)}`).join('\n');
  await writeFile(join(f.workspace, 'long.txt'), source);
  const descriptor = (await f.service.catalog(context)).find(tool => tool.name === 'filesystem.search');
  assert.equal(descriptor.inputSchema.properties.cursor.maxLength, 32);
  const first = parsed(await f.run(context, 'filesystem.search', { query: 'needle', maxMatches: 200 }));
  assert.ok(first.matches.length > 0 && first.matches.length < 200);
  assert.ok(first.nextCursor);
  assert.equal(first.preview, undefined, 'the cursor remains a top-level model-visible field');
  const second = parsed(await f.run(context, 'filesystem.search', { query: 'needle', maxMatches: 200, cursor: first.nextCursor }));
  assert.equal(second.matches[0].line, first.matches.at(-1).line + 1);
  assert.ok(second.nextCursor);
  await f.service.releaseContext(context);
  await assert.rejects(executeFilesystem('filesystem.search', context, { query: 'needle', cursor: second.nextCursor }, f.workspace,
    undefined, { searchOwner: f.service }), { code: 'INVALID_TOOL_SEARCH_CURSOR' });
});

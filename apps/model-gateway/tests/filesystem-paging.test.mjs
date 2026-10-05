import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from 'node:test';
import { MAX_TOOL_FILE_BYTES } from '../tools/filesystem-tools.mjs';
import { parsed, toolFixture } from './tool-fixture.mjs';

const sha256 = text => createHash('sha256').update(text).digest('hex');

test('filesystem.read retrieves a long source through declared pages and keeps legacy reads compatible', async t => {
  const f = await toolFixture(t), context = await f.context('smart');
  const source = '// 中文源码\r\n' + 'export const value = 42;\r\n'.repeat(2200) + '// END_OF_SOURCE';
  await writeFile(join(f.workspace, 'long.mjs'), source);
  const descriptor = (await f.service.catalog(context)).find(tool => tool.name === 'filesystem.read');
  assert.deepEqual(descriptor.inputSchema.properties.offset.minimum, 0);
  assert.equal(descriptor.inputSchema.properties.offset.maximum, MAX_TOOL_FILE_BYTES);
  const original = parsed(await f.run(context, 'filesystem.read', { path: 'long.mjs' }));
  assert.equal(original.content, source.slice(0, 32000));
  assert.equal(original.offset, 0); assert.equal(original.nextOffset, 32000);
  assert.equal(original.totalCharacters, source.length);
  assert.equal(original.hasMore, true); assert.equal(original.truncated, true);
  let reconstructed = '', offset = 0, pages = 0;
  for (;;) {
    const page = parsed(await f.run(context, 'filesystem.read', { path: 'long.mjs', offset, maxChars: 7000 }));
    assert.equal(page.offset, offset); assert.equal(page.sha256, sha256(source));
    assert.equal(page.bytes, Buffer.byteLength(source)); assert.equal(page.totalCharacters, source.length);
    assert.equal(page.nextOffset, page.offset + page.content.length);
    assert.equal(page.hasMore, page.nextOffset < source.length);
    assert.equal(page.truncated, page.offset > 0 || page.hasMore);
    reconstructed += page.content; pages++;
    if (!page.hasMore) break;
    assert.ok(page.nextOffset > offset); offset = page.nextOffset;
  }
  assert.ok(pages > 7); assert.equal(reconstructed, source); assert.match(reconstructed, /END_OF_SOURCE$/);
  assert.equal(await readFile(join(f.workspace, 'long.mjs'), 'utf8'), source);
});

test('UTF-16 pages preserve supplementary characters even with a one-unit page size', async t => {
  const f = await toolFixture(t), context = await f.context('smart');
  const source = '\uFEFF前😀后\r\n𠮷文🚀末';
  await writeFile(join(f.workspace, 'unicode.txt'), source);
  for (const maxChars of [1, 2, 3, 4, 5]) {
    let reconstructed = '', offset = 0;
    for (let count = 0; count <= source.length; count++) {
      const page = parsed(await f.run(context, 'filesystem.read', { path: 'unicode.txt', offset, maxChars }));
      assert.equal(page.content.isWellFormed(), true);
      assert.ok(page.content.length <= Math.max(2, maxChars));
      assert.equal(page.nextOffset, offset + page.content.length);
      reconstructed += page.content;
      if (!page.hasMore) break;
      assert.ok(page.nextOffset > offset); offset = page.nextOffset;
    }
    assert.equal(reconstructed, source);
  }
  const split = await f.run(context, 'filesystem.read', { path: 'unicode.txt', offset: source.indexOf('😀') + 1 });
  assert.equal(split.code, 'INVALID_TOOL_ARGUMENTS'); assert.equal(split.isError, true);
});

test('tail and empty pages report explicit completion without inventing contents', async t => {
  const f = await toolFixture(t), context = await f.context('smart');
  await writeFile(join(f.workspace, 'tail.txt'), 'first\nlast');
  const tail = parsed(await f.run(context, 'filesystem.read', { path: 'tail.txt', offset: 6 }));
  assert.equal(tail.content, 'last'); assert.equal(tail.offset, 6); assert.equal(tail.nextOffset, 10);
  assert.equal(tail.totalCharacters, 10); assert.equal(tail.hasMore, false); assert.equal(tail.truncated, true);
  const end = parsed(await f.run(context, 'filesystem.read', { path: 'tail.txt', offset: 10 }));
  assert.equal(end.content, ''); assert.equal(end.nextOffset, 10); assert.equal(end.hasMore, false);
  await writeFile(join(f.workspace, 'empty.txt'), '');
  const empty = parsed(await f.run(context, 'filesystem.read', { path: 'empty.txt' }));
  assert.equal(empty.content, ''); assert.equal(empty.totalCharacters, 0); assert.equal(empty.nextOffset, 0);
  assert.equal(empty.hasMore, false); assert.equal(empty.truncated, false); assert.equal(empty.sha256, sha256(''));
});

test('each page recomputes the full-file hash so concurrent source changes stay detectable', async t => {
  const f = await toolFixture(t), context = await f.context('smart');
  const path = join(f.workspace, 'changing.txt'), first = 'first version tail', changed = 'other version tail';
  await writeFile(path, first);
  const before = parsed(await f.run(context, 'filesystem.read', { path: 'changing.txt', maxChars: 6 }));
  await writeFile(path, changed);
  const after = parsed(await f.run(context, 'filesystem.read', { path: 'changing.txt', offset: before.nextOffset, maxChars: 6 }));
  assert.equal(before.sha256, sha256(first)); assert.equal(after.sha256, sha256(changed));
  assert.notEqual(after.sha256, before.sha256);
  const restarted = parsed(await f.run(context, 'filesystem.read', { path: 'changing.txt' }));
  assert.equal(restarted.content, changed); assert.equal(restarted.sha256, after.sha256);
  await writeFile(path, 'short');
  assert.equal((await f.run(context, 'filesystem.read', { path: 'changing.txt', offset: after.nextOffset })).code, 'INVALID_TOOL_ARGUMENTS');
});

test('invalid offsets are rejected by the real broker and file byte limits still apply to all pages', async t => {
  const f = await toolFixture(t), context = await f.context('smart');
  await writeFile(join(f.workspace, 'small.txt'), 'abc');
  for (const offset of [-1, 0.5, '1', null, MAX_TOOL_FILE_BYTES + 1, 4]) {
    const result = await f.run(context, 'filesystem.read', { path: 'small.txt', offset });
    assert.equal(result.isError, true); assert.equal(result.code, 'INVALID_TOOL_ARGUMENTS');
  }
  await writeFile(join(f.workspace, 'oversized.txt'), 'x'.repeat(MAX_TOOL_FILE_BYTES + 1));
  assert.equal((await f.run(context, 'filesystem.read', { path: 'oversized.txt', offset: MAX_TOOL_FILE_BYTES })).code, 'TOOL_FILE_TOO_LARGE');
  assert.equal((await f.run(context, 'filesystem.read', { path: 'small.txt', offset: 2, maxBytes: 2 })).code, 'TOOL_FILE_TOO_LARGE');
  await writeFile(join(f.workspace, 'bounded.txt'), 'x'.repeat(MAX_TOOL_FILE_BYTES));
  const boundary = parsed(await f.run(context, 'filesystem.read', { path: 'bounded.txt', offset: MAX_TOOL_FILE_BYTES }));
  assert.equal(boundary.content, ''); assert.equal(boundary.totalCharacters, MAX_TOOL_FILE_BYTES);
  assert.equal(boundary.nextOffset, MAX_TOOL_FILE_BYTES); assert.equal(boundary.hasMore, false);
});

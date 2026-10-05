import assert from 'node:assert/strict';
import { mkdir, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from 'node:test';
import { isSensitiveFilePath } from '../tools/sensitive-files.mjs';
import { approve, parsed, pendingApproval, toolFixture } from './tool-fixture.mjs';

const marker = 'SYNTHETIC_PRIVATE_READ_MARKER';

test('credential path classification covers Windows separators and preserves normal source reads', () => {
  for (const path of ['.env', '.env.local', 'work/.ENV.PRODUCTION', 'C:\\work\\.ssh\\id_ed25519',
    'work/.npmrc', 'work/credentials.json', 'work/key.pfx']) assert.equal(isSensitiveFilePath(path), true, path);
  for (const path of ['environment.mjs', 'src/config.json', 'note.txt', 'src/key.ts']) assert.equal(isSensitiveFilePath(path), false, path);
});

for (const mode of ['ask', 'smart']) {
  test(`${mode}: a scoped .env read without interaction contains no secret or archive reference`, async t => {
    const f = await toolFixture(t), context = await f.context(mode);
    await writeFile(join(f.workspace, '.env'), `API_KEY=${marker}`);
    const result = await f.run(context, 'filesystem.read', { path: '.env' }, { interactive: false });
    assert.equal(result.code, 'TOOL_APPROVAL_REQUIRED');
    assert.equal(result.resultRef, undefined);
    assert.equal(JSON.stringify(result).includes(marker), false);
  });

  test(`${mode}: directory searches skip sensitive content but keep normal source matches`, async t => {
    const f = await toolFixture(t), context = await f.context(mode);
    await mkdir(join(f.workspace, '.ssh'));
    await writeFile(join(f.workspace, '.env.local'), `needle=${marker}`);
    await writeFile(join(f.workspace, '.ssh', 'id_rsa'), `needle=${marker}`);
    await writeFile(join(f.workspace, 'source.txt'), 'needle=public');
    const result = parsed(await f.run(context, 'filesystem.search', { query: 'needle', recursive: true }));
    assert.equal(JSON.stringify(result).includes(marker), false);
    assert.equal(result.matches.length, 1);
  });
}

test('sensitive approval states transmission scope, denial leaks no value and the next read needs new approval', async t => {
  const f = await toolFixture(t), context = await f.context('ask');
  await writeFile(join(f.workspace, '.env'), `API_KEY=${marker}`);
  const call = f.call('filesystem.read', { path: '.env', reason: 'Check synthetic fixture' });
  const first = await pendingApproval(f.service, context, call);
  assert.match(first.event.tool.summary, /发送给当前模型/);
  assert.equal(JSON.stringify(first.event).includes(marker), false);
  approve(f.service, context, first.event.tool, false);
  assert.equal((await first.result).code, 'TOOL_DENIED');
  const second = await pendingApproval(f.service, context, f.call('filesystem.read', { path: '.env' }));
  approve(f.service, context, second.event.tool);
  assert.equal(parsed(await second.result).content, `API_KEY=${marker}`);
  assert.equal((await f.run(context, 'filesystem.read', { path: '.env' }, { interactive: false })).code, 'TOOL_APPROVAL_REQUIRED');
});

test('full permission retains explicit workspace reads and searches without granting access to app credentials', async t => {
  const f = await toolFixture(t), context = await f.context('full');
  await writeFile(join(f.workspace, '.env'), `needle=${marker}`);
  assert.equal(parsed(await f.run(context, 'filesystem.read', { path: '.env' })).content, `needle=${marker}`);
  assert.ok(JSON.stringify(parsed(await f.run(context, 'filesystem.search', { query: 'needle' }))).includes(marker));
  await mkdir(f.dataHome, { recursive: true });
  await writeFile(join(f.dataHome, 'connections.json'), JSON.stringify({ apiKey: marker }));
  assert.equal((await f.run(context, 'filesystem.read', { path: join(f.dataHome, 'connections.json'), reason: 'Synthetic' })).code,
    'PROTECTED_MODEL_CREDENTIALS');
});

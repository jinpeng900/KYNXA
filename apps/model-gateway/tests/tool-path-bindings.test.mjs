import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { link, mkdir, mkdtemp, readFile, realpath, rm, symlink, unlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { test } from 'node:test';
import { bindLocalPath, inspectLocalPath, resolveToolPath, revalidateLocalPathBinding } from '../platform/tool-paths.mjs';
import { executeFilesystem } from '../tools/filesystem-tools.mjs';

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-host-path-binding-'));
  const workspace = join(root, 'workspace'), outside = join(root, 'outside');
  await Promise.all([mkdir(workspace), mkdir(outside)]);
  t.after(async () => {
    const suffix = relative(resolve(tmpdir()), resolve(root));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(root, { recursive: true, force: true });
  });
  return { root, workspace, outside };
}

test('explicit host workspace aliases bind to real roots without relaxing managed directory inspection', async t => {
  const f = await fixture(t), alias = join(f.root, 'mounted-alias');
  await symlink(f.workspace, alias, process.platform === 'win32' ? 'junction' : 'dir');
  await writeFile(join(f.workspace, 'code.txt'), 'fixture');
  await assert.rejects(inspectLocalPath(join(alias, 'code.txt')), { code: 'UNSAFE_TOOL_PATH' });
  const binding = await bindLocalPath(join(alias, 'code.txt'), { workspaceRoot: alias, allowHardLinks: true });
  assert.equal(binding.outsideWorkspace, false);
  assert.equal(binding.path, await realpath(join(f.workspace, 'code.txt')));
  assert.equal(binding.workspaceRoot, await realpath(f.workspace));
  const result = await executeFilesystem('filesystem.read', { workspaceRoot: alias }, {}, binding.path, undefined, { pathBinding: binding });
  assert.equal(result.content, 'fixture');
  const rootBinding = await bindLocalPath(alias, { workspaceRoot: alias });
  await assert.rejects(executeFilesystem('filesystem.delete', { workspaceRoot: alias }, { expectedHash: null },
    rootBinding.path, undefined, { pathBinding: rootBinding }), { code: 'UNSAFE_TOOL_PATH' });
});

test('links escaping the workspace retain outside scope and cannot disguise protected application data', async t => {
  const f = await fixture(t), alias = join(f.workspace, 'external');
  await symlink(f.outside, alias, process.platform === 'win32' ? 'junction' : 'dir');
  await writeFile(join(f.outside, 'code.txt'), 'fixture');
  const binding = await bindLocalPath(join(alias, 'code.txt'), { workspaceRoot: f.workspace, allowHardLinks: true });
  assert.equal(binding.outsideWorkspace, true);
  await assert.rejects(bindLocalPath(join(alias, 'code.txt'), { workspaceRoot: f.workspace,
    protectedRoots: [f.outside] }), { code: 'PROTECTED_APP_DATA' });
});

test('alias target changes after approval are rejected, including new file creation', async t => {
  const f = await fixture(t), alias = join(f.root, 'mounted-alias');
  await symlink(f.workspace, alias, process.platform === 'win32' ? 'junction' : 'dir');
  const binding = await bindLocalPath(join(alias, 'new.txt'), { workspaceRoot: alias, allowMissing: true });
  assert.equal(await revalidateLocalPathBinding(binding), null);
  await unlink(alias);
  await symlink(f.outside, alias, process.platform === 'win32' ? 'junction' : 'dir');
  await assert.rejects(revalidateLocalPathBinding(binding), { code: 'TOOL_PATH_CHANGED' });
  await assert.rejects(executeFilesystem('filesystem.write', { workspaceRoot: alias }, { expectedHash: null, content: 'wrong' },
    binding.path, undefined, { pathBinding: binding }), { code: 'TOOL_PATH_CHANGED' });
  await assert.rejects(readFile(join(f.workspace, 'new.txt')), { code: 'ENOENT' });
  await assert.rejects(readFile(join(f.outside, 'new.txt')), { code: 'ENOENT' });
});

test('hard-linked files are readable and searchable but cannot be silently rewritten or unlinked', async t => {
  const f = await fixture(t), path = join(f.workspace, 'shared.txt');
  await writeFile(path, 'shared fixture');
  await link(path, join(f.outside, 'shared.txt'));
  const context = { workspaceRoot: f.workspace }, input = { expectedHash: createHash('sha256').update('shared fixture').digest('hex') };
  await assert.rejects(inspectLocalPath(path), { code: 'UNSAFE_TOOL_PATH' });
  assert.equal((await executeFilesystem('filesystem.read', context, {}, path)).content, 'shared fixture');
  assert.equal((await executeFilesystem('filesystem.stat', context, {}, path)).sha256, input.expectedHash);
  assert.equal((await executeFilesystem('filesystem.search', context, { query: 'fixture' }, f.workspace)).matches.length, 1);
  for (const [name, arguments_] of [['filesystem.write', { ...input, content: 'replacement' }],
    ['filesystem.edit', { ...input, oldText: 'fixture', newText: 'changed' }], ['filesystem.delete', input]])
    await assert.rejects(executeFilesystem(name, context, arguments_, path), { code: 'UNSAFE_TOOL_PATH' });
  assert.equal(await readFile(path, 'utf8'), 'shared fixture');
});

test('deleting a final junction entry never deletes its real target directory', async t => {
  const f = await fixture(t), alias = join(f.workspace, 'external-entry');
  await symlink(f.outside, alias, process.platform === 'win32' ? 'junction' : 'dir');
  const binding = await bindLocalPath(alias, { workspaceRoot: f.workspace });
  await assert.rejects(executeFilesystem('filesystem.delete', { workspaceRoot: f.workspace }, { expectedHash: null },
    binding.path, undefined, { pathBinding: binding }), { code: 'UNSAFE_TOOL_PATH' });
  assert.equal((await inspectLocalPath(f.outside)).isDirectory(), true);
  assert.equal(await realpath(alias), await realpath(f.outside));
});

test('deferred scope checks authorize a mounted alias by its real workspace while preserving the default contract', async t => {
  const f = await fixture(t), alias = join(f.root, 'mounted-alias');
  await symlink(f.workspace, alias, process.platform === 'win32' ? 'junction' : 'dir');
  await writeFile(join(f.workspace, 'source.txt'), 'fixture');
  const context = { workspaceRoot: await realpath(f.workspace) }, input = { path: join(alias, 'source.txt') };
  await assert.rejects(async () => resolveToolPath(context, input), { code: 'OUTSIDE_WORKSPACE_REASON_REQUIRED' });
  const selected = resolveToolPath(context, input, [], { deferScopeCheck: true });
  assert.equal(selected.outsideWorkspace, true);
  const binding = await bindLocalPath(selected.path, { workspaceRoot: context.workspaceRoot, allowHardLinks: true });
  assert.equal(binding.outsideWorkspace, false);
  assert.equal(binding.path, await realpath(join(f.workspace, 'source.txt')));
  assert.throws(() => resolveToolPath(context, { path: '.' }, [context.workspaceRoot], { deferScopeCheck: true }),
    { code: 'PROTECTED_APP_DATA' });
  assert.throws(() => resolveToolPath({ workspaceRoot: null }, { path: '.' }, [], { deferScopeCheck: true }),
    { code: 'WORKSPACE_REQUIRED' });
  assert.throws(() => resolveToolPath(context, { path: 'bad\0path' }, [], { deferScopeCheck: true }),
    { code: 'INVALID_TOOL_ARGUMENTS' });
});

import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdir, mkdtemp, readFile, readdir, realpath, rename, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { basename, join, relative, resolve, sep } from 'node:path';
import { test } from 'node:test';
import { ConversationWorkspaces } from '../sandbox-workspaces.mjs';
import { SandboxRunner } from '../sandbox-runner.mjs';
import { migrateStorage } from '../migrate-storage.mjs';

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-conversation-workspaces-'));
  const dataRoot = join(root, 'Data'), models = join(dataRoot, 'Models');
  const workspaces = new ConversationWorkspaces({ root: models });
  t.after(async () => {
    const suffix = relative(resolve(tmpdir()), resolve(root));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith('..' + sep));
    await rm(root, { recursive: true, force: true });
  });
  return { root, dataRoot, models, workspaces };
}

test('lazy initialization is chat-isolated and survives reopen without creating formal message or memory stores', async t => {
  const f = await fixture(t), firstId = randomUUID(), secondId = randomUUID();
  await assert.rejects(readdir(f.dataRoot), { code: 'ENOENT' });
  const first = await f.workspaces.ensure(firstId), second = await f.workspaces.ensure(secondId);
  assert.notEqual(first, second); assert.equal(first, await realpath(join(f.models, 'Workspaces', firstId)));
  await writeFile(join(first, 'artifact.txt'), 'persistent first-chat artifact');
  await writeFile(join(second, 'artifact.txt'), 'isolated second-chat artifact');
  const reopened = new ConversationWorkspaces({ root: f.models });
  assert.equal(await reopened.ensure(firstId.toUpperCase()), first);
  assert.equal(await readFile(join(await reopened.verify(firstId, first), 'artifact.txt'), 'utf8'), 'persistent first-chat artifact');
  assert.equal(await readFile(join(second, 'artifact.txt'), 'utf8'), 'isolated second-chat artifact');
  assert.deepEqual(await readdir(f.dataRoot), ['Models']);
  assert.deepEqual(await readdir(f.models), ['Workspaces']);
  await assert.rejects(reopened.verify(firstId, second), { code: 'SANDBOX_WORKSPACE_MISMATCH' });
  await assert.rejects(reopened.verify(firstId, join(first, 'artifact.txt')), { code: 'SANDBOX_WORKSPACE_MISMATCH' });
  await assert.rejects(reopened.verify(firstId, join(f.dataRoot, 'Memory')), { code: 'SANDBOX_WORKSPACE_MISMATCH' });
});

test('concurrent ensure keeps generated files and normalizes legacy 32-hex UUIDs', async t => {
  const f = await fixture(t), id = randomUUID();
  const paths = await Promise.all(Array.from({ length: 20 }, () => f.workspaces.ensure(id.replaceAll('-', '').toUpperCase())));
  const expected = await realpath(join(f.models, 'Workspaces', id));
  assert.ok(paths.every(path => path === expected));
  await writeFile(join(paths[0], 'ready.txt'), 'keep this');
  await Promise.all(Array.from({ length: 10 }, () => f.workspaces.ensure(id)));
  assert.equal(await readFile(join(paths[0], 'ready.txt'), 'utf8'), 'keep this');
});

test('legacy IDs keep durable owners and a UUID matching their folder cannot claim another chat', async t => {
  const f = await fixture(t), legacyId = 'legacy-test-chat';
  const legacy = await f.workspaces.ensure(legacyId);
  await writeFile(join(legacy, 'artifact.txt'), 'legacy isolated content');
  const owner = JSON.parse(await readFile(join(legacy, '.workspace-owner.json'), 'utf8'));
  assert.deepEqual(owner, { schemaVersion: 1, conversationId: legacyId });
  const reopened = new ConversationWorkspaces({ root: f.models });
  assert.equal(await reopened.ensure(legacyId.toUpperCase()), legacy);
  const collision = basename(legacy);
  await assert.rejects(reopened.ensure(collision), { code: 'SANDBOX_WORKSPACE_OWNER_MISMATCH' });
  await assert.rejects(reopened.verify(collision, legacy), { code: 'SANDBOX_WORKSPACE_OWNER_MISMATCH' });
  assert.equal(await readFile(join(await reopened.verify(legacyId, legacy), 'artifact.txt'), 'utf8'), 'legacy isolated content');
  assert.equal(reopened.isControlPath(join(legacy, '.workspace-owner.json')), true);
  assert.equal(reopened.isControlPath(join(legacy, 'artifact.txt')), false);
  assert.equal(reopened.isControlPath(join(f.dataRoot, 'Memory', '.workspace-owner.json')), false);
});

test('an ownerless empty directory initializes, but ownerless artifacts and marker substitution are never adopted', async t => {
  const f = await fixture(t), id = randomUUID(), path = join(f.models, 'Workspaces', id);
  await mkdir(path, { recursive: true });
  assert.equal(await f.workspaces.ensure(id), await realpath(path));
  const marker = join(path, '.workspace-owner.json');
  await rm(marker); await writeFile(join(path, 'unowned.txt'), 'cannot infer which chat owns this');
  await assert.rejects(f.workspaces.ensure(id), { code: 'SANDBOX_WORKSPACE_OWNER_MISMATCH' });
  await assert.rejects(f.workspaces.verify(id, path), { code: 'SANDBOX_WORKSPACE_OWNER_MISMATCH' });
  await writeFile(marker, JSON.stringify({ schemaVersion: 1, conversationId: randomUUID() }));
  await assert.rejects(f.workspaces.verify(id, path), { code: 'SANDBOX_WORKSPACE_OWNER_MISMATCH' });
  await rm(marker); await symlink(join(path, 'unowned.txt'), marker, process.platform === 'win32' ? 'file' : undefined);
  await assert.rejects(f.workspaces.verify(id, path), { code: 'UNSAFE_TOOL_PATH' });
});

test('actual storage migration carries chat workspaces from standard and custom model roots without claiming the old path', async t => {
  for (const custom of [false, true]) {
    const f = await fixture(t), id = randomUUID();
    const modelSource = custom ? join(f.root, 'CustomModels') : f.models;
    const before = new ConversationWorkspaces({ root: modelSource }), oldWorkspace = await before.ensure(id);
    await mkdir(join(oldWorkspace, 'generated', 'nested'), { recursive: true });
    const artifact = join('generated', 'nested', 'result.cjs');
    await writeFile(join(oldWorkspace, artifact), 'console.log("preserved");');
    const ownerBefore = await readFile(join(oldWorkspace, '.workspace-owner.json'));
    const migrated = join(f.root, 'MovedData'), pointer = join(f.root, 'profile', 'storage.json');
    const result = await migrateStorage({ desktopSource: join(f.dataRoot, 'Desktop'), modelSource, target: migrated, pointer });
    assert.equal(result.originalFilesRetained, true); assert.equal(result.verifiedFiles, 2);
    assert.equal(JSON.parse(await readFile(pointer, 'utf8')).dataRoot, migrated);
    const next = new ConversationWorkspaces({ root: join(migrated, 'Models') }), current = await next.ensure(id);
    assert.equal(await readFile(join(current, artifact), 'utf8'), 'console.log("preserved");');
    assert.deepEqual(await readFile(join(current, '.workspace-owner.json')), ownerBefore);
    assert.equal(await readFile(join(oldWorkspace, artifact), 'utf8'), 'console.log("preserved");');
    assert.deepEqual(await readFile(join(oldWorkspace, '.workspace-owner.json')), ownerBefore);
    await assert.rejects(next.verify(id, oldWorkspace), { code: 'SANDBOX_WORKSPACE_MISMATCH' });
    assert.equal(await next.verify(id, current), current);
  }
});

test('invalid IDs, file collisions, ancestor junctions and replaced workspace roots fail closed', async t => {
  const f = await fixture(t), id = randomUUID();
  assert.throws(() => new ConversationWorkspaces({ root: 'relative/data' }), { code: 'INVALID_SANDBOX_WORKSPACE' });
  for (const invalid of ['../other-chat', 'CON']) await assert.rejects(f.workspaces.ensure(invalid));
  await assert.rejects(readdir(f.dataRoot), { code: 'ENOENT' });
  const workspace = await f.workspaces.ensure(id), other = join(f.root, 'outside'); await mkdir(other);
  await rename(workspace, workspace + '-original');
  await symlink(other, workspace, process.platform === 'win32' ? 'junction' : 'dir');
  await assert.rejects(f.workspaces.verify(id, workspace), { code: 'UNSAFE_TOOL_PATH' });
  await assert.rejects(f.workspaces.ensure(id), { code: 'UNSAFE_TOOL_PATH' });
  await rm(workspace); await writeFile(workspace, 'not a directory');
  await assert.rejects(f.workspaces.ensure(id), { code: 'UNSAFE_TOOL_PATH' });
  await assert.rejects(f.workspaces.verify(id, workspace), { code: 'UNSAFE_TOOL_PATH' });
  const linkedRoot = join(f.root, 'linked-models'); await symlink(f.models, linkedRoot, process.platform === 'win32' ? 'junction' : 'dir');
  await assert.rejects(new ConversationWorkspaces({ root: linkedRoot }).ensure(randomUUID()), { code: 'UNSAFE_TOOL_PATH' });
});

test('real Windows AppContainer accepts only canonical conversation workspaces, isolates sibling files and never writes back',
  { skip: process.platform !== 'win32', timeout: 45000 }, async t => {
    const f = await fixture(t), id = randomUUID(), siblingId = randomUUID();
    const workspace = await f.workspaces.ensure(id), sibling = await f.workspaces.ensure(siblingId);
    await writeFile(join(workspace, 'artifact.txt'), 'persisted generated artifact');
    const siblingFile = join(sibling, 'artifact.txt'); await writeFile(siblingFile, 'other chat must remain isolated');
    const runner = new SandboxRunner({ excludedRoots: [f.dataRoot, f.models] });
    t.after(() => runner.cleanupAll());
    const capabilities = await runner.capabilities();
    assert.equal(capabilities.available, true, capabilities.reason); assert.equal(runner.verifiedAppContainer, true);
    await assert.rejects(runner.run({ workspaceRoot: workspace, command: 'node', args: ['-v'] }), { code: 'SANDBOX_INVALID_WORKSPACE' });
    const args = ['-e', "const fs=require('node:fs');const result={value:fs.readFileSync('artifact.txt','utf8'),ownerPresent:fs.existsSync('.workspace-owner.json')};try{fs.readFileSync(process.argv[1]);result.denied=false;}catch(e){result.denied=['EACCES','EPERM'].includes(e.code);}fs.writeFileSync('artifact.txt','snapshot-only change');console.log(JSON.stringify(result));", siblingFile];
    const actual = await runner.run({ workspaceRoot: workspace, trustedManagedWorkspace: true, command: 'node', args });
    assert.equal(actual.exitCode, 0, actual.stderr); assert.equal(actual.tokenVerified, true);
    assert.equal(actual.workspaceCopy, true); assert.equal(actual.activeProcessesAfterExit, 0);
    assert.deepEqual(JSON.parse(actual.stdout), { value: 'persisted generated artifact', ownerPresent: false, denied: true });
    assert.equal(await readFile(join(actual.stagingDirectory, 'artifact.txt'), 'utf8'), 'snapshot-only change');
    assert.equal(await readFile(join(await f.workspaces.verify(id, workspace), 'artifact.txt'), 'utf8'), 'persisted generated artifact');
    for (const denied of [f.models, join(f.models, 'Workspaces'), join(f.models, 'Workspaces', 'not-a-guid'), join(f.dataRoot, 'Chats')]) {
      await mkdir(denied, { recursive: true });
      await assert.rejects(runner.run({ workspaceRoot: denied, trustedManagedWorkspace: true, command: 'node', args: ['-v'] }), { code: 'SANDBOX_INVALID_WORKSPACE' });
    }
    const modelsOnly = new SandboxRunner({ excludedRoots: [f.models] });
    let modelsOnlyResult;
    try {
      const result = modelsOnlyResult = await modelsOnly.run({ workspaceRoot: workspace, trustedManagedWorkspace: true, command: 'node', args: ['-e', "console.log(require('node:fs').readFileSync('artifact.txt','utf8'));" ] });
      assert.equal(result.exitCode, 0, result.stderr); assert.match(result.stdout, /persisted generated artifact/); assert.equal(result.tokenVerified, true);
    } finally { if (modelsOnlyResult) await modelsOnly.cleanup(modelsOnlyResult.stagingDirectory); else await modelsOnly.cleanupAll(); }
    await runner.cleanupAll();
  });

test('real Windows AppContainer permits only the explicit excluded custom model home as a conversation workspace',
  { skip: process.platform !== 'win32', timeout: 45000 }, async t => {
    const f = await fixture(t), customHome = join(f.root, 'UserSelectedInferenceFiles');
    const custom = new ConversationWorkspaces({ root: customHome }), id = randomUUID();
    const workspace = await custom.ensure(id), canonicalHome = await realpath(customHome);
    await writeFile(join(workspace, 'artifact.txt'), 'custom model-home artifact');
    const extensionRoot = join(f.root, 'Extensions'), extensionWorkspace = join(extensionRoot, 'Workspaces', randomUUID());
    await mkdir(extensionWorkspace, { recursive: true });
    await writeFile(join(extensionWorkspace, 'artifact.txt'), 'extension root is not a conversation scope');
    const canonicalExtensions = await realpath(extensionRoot), runners = [];
    const createRunner = options => { const runner = new SandboxRunner(options); runners.push(runner); return runner; };
    t.after(async () => { for (const runner of runners) await runner.cleanupAll(); });
    const runner = createRunner({ excludedRoots: [canonicalHome, canonicalExtensions], conversationWorkspaceHome: canonicalHome });
    const capabilities = await runner.capabilities(); assert.equal(capabilities.available, true, capabilities.reason);
    await assert.rejects(runner.run({ workspaceRoot: workspace, command: 'node', args: ['-v'] }), { code: 'SANDBOX_INVALID_WORKSPACE' });
    const result = await runner.run({ workspaceRoot: workspace, trustedManagedWorkspace: true, command: 'node',
      args: ['-e', "const fs=require('node:fs');console.log(JSON.stringify({value:fs.readFileSync('artifact.txt','utf8'),ownerPresent:fs.existsSync('.workspace-owner.json')}));"] });
    assert.equal(result.exitCode, 0, result.stderr); assert.equal(result.tokenVerified, true); assert.equal(result.workspaceCopy, true);
    assert.deepEqual(JSON.parse(result.stdout), { value: 'custom model-home artifact', ownerPresent: false });
    assert.equal(await custom.verify(id, workspace), workspace);
    for (const denied of [canonicalHome, join(canonicalHome, 'Workspaces'), join(canonicalHome, 'Workspaces', 'not-a-guid'),
      join(workspace, 'nested'), await realpath(extensionWorkspace)]) {
      await mkdir(denied, { recursive: true });
      await assert.rejects(runner.run({ workspaceRoot: denied, trustedManagedWorkspace: true, command: 'node', args: ['-v'] }),
        { code: 'SANDBOX_INVALID_WORKSPACE' });
    }
    await assert.rejects(runner.run({ workspaceRoot: await realpath(extensionWorkspace), trustedManagedWorkspace: true,
      conversationWorkspaceHome: canonicalExtensions, command: 'node', args: ['-v'] }), { code: 'SANDBOX_INVALID_WORKSPACE' });
    const unspecified = createRunner({ excludedRoots: [canonicalHome] });
    await assert.rejects(unspecified.run({ workspaceRoot: workspace, trustedManagedWorkspace: true, command: 'node', args: ['-v'] }),
      { code: 'SANDBOX_INVALID_WORKSPACE' });
    const notExcluded = createRunner({ excludedRoots: [canonicalExtensions], conversationWorkspaceHome: canonicalHome });
    await assert.rejects(notExcluded.run({ workspaceRoot: workspace, trustedManagedWorkspace: true, command: 'node', args: ['-v'] }),
      { code: 'SANDBOX_INVALID_WORKSPACE' });
    for (const active of runners) await active.cleanupAll();
  });

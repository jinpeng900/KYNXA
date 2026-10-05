import assert from 'node:assert/strict';
import { mkdir, readFile, realpath, symlink, unlink } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from 'node:test';
import { SandboxRunner } from '../tools/sandbox-runner.mjs';
import { toolFixture, parsed, pendingApproval, approve } from './tool-fixture.mjs';

for (const scope of ['managed-compact', 'managed-dashed', 'folderless', 'unlinked', 'legacy-linked']) {
  test(`real AppContainer: ${scope} work supports tool write, Node execution and read-back`,
    { skip: process.platform !== 'win32', timeout: 45000 }, async t => {
      const f = await toolFixture(t);
      const catalog = await f.conversations.catalog(), project = catalog.Projects[0];
      let expected;
      if (scope.startsWith('managed-') || scope === 'legacy-linked') {
        if (scope === 'legacy-linked') {
          const originals = join(f.root, 'legacy-originals');
          await mkdir(originals);
          await mkdir(join(originals, 'Projects', f.projectId), { recursive: true });
          await symlink(await realpath(originals), join(f.conversations.root, 'Desktop'), 'junction');
        }
        const folderId = scope === 'managed-compact' ? f.projectId.replaceAll('-', '') : f.projectId;
        expected = join(f.conversations.root, 'Desktop', 'Projects', folderId);
        if (scope !== 'legacy-linked') await mkdir(expected, { recursive: true });
        project.FolderPath = expected;
      } else {
        project.FolderPath = null;
        project.IsFolderlessWorkspace = scope === 'folderless';
      }
      await f.conversations.saveCatalog(catalog);
      const runner = new SandboxRunner({
        excludedRoots: [f.dataHome, f.conversations.root, f.service.extensionRoot],
        conversationWorkspaceHome: f.dataHome
      });
      f.service.sandboxRunner = runner;
      const ctx = await f.context('full');
      assert.equal(ctx.sandboxCapabilities.available, true, ctx.sandboxCapabilities.reason);
      assert.equal(ctx.managedWorkspace, true);
      assert.equal(ctx.isolatedWorkspace, !scope.startsWith('managed-'));
      if (expected && scope !== 'legacy-linked') assert.equal(ctx.workspaceRoot, expected);
      else assert.ok(ctx.workspaceRoot.startsWith(join(await realpath(f.dataHome), 'Workspaces') + '\\'));
      if (scope === 'legacy-linked') {
        assert.equal(ctx.linkedWorkspaceRoot, expected);
        assert.match(await f.service.systemPrompt(ctx), /old files are preserved, not migrated/);
      }

      const script = "const fs=require('node:fs');const value=JSON.parse(fs.readFileSync('input.json','utf8'));" +
        "fs.writeFileSync('output.txt',String(value.a+value.b));console.log(JSON.stringify({sum:value.a+value.b,ownerPresent:fs.existsSync('.workspace-owner.json')}));";
      parsed(await f.run(ctx, 'filesystem.write', { path: 'input.json', content: '{"a":2,"b":3}', expectedHash: null }));
      parsed(await f.run(ctx, 'filesystem.write', { path: 'calculate.cjs', content: script, expectedHash: null }));
      const execution = parsed(await f.run(ctx, 'terminal.run', { command: 'node', args: ['calculate.cjs'], timeoutMs: 15000 }));
      assert.equal(execution.exitCode, 0, execution.stderr);
      assert.equal(execution.sandbox, 'appcontainer');
      assert.equal(execution.tokenVerified, true);
      assert.equal(execution.workspaceCopy, true);
      assert.equal(execution.activeProcessesAfterExit, 0);
      assert.deepEqual(JSON.parse(execution.stdout), { sum: 5, ownerPresent: false });
      const snapshot = parsed(await f.run(ctx, 'filesystem.read', {
        path: join(execution.stagingDirectory, 'output.txt'), reason: 'Read this completed sandbox calculation result.'
      }));
      assert.equal(snapshot.content, '5');
      assert.equal(parsed(await f.run(ctx, 'filesystem.read', { path: 'input.json' })).content, '{"a":2,"b":3}');
      await assert.rejects(readFile(join(ctx.workspaceRoot, 'output.txt')), { code: 'ENOENT' });
      await f.service.releaseContext(ctx);
      await assert.rejects(readFile(join(execution.stagingDirectory, 'output.txt')), { code: 'ENOENT' });
    });
}

test('legacy fallback cannot keep a pending approval after its effective work scope changes', async t => {
  const f = await toolFixture(t);
  const catalog = await f.conversations.catalog();
  const originals = join(f.root, 'old-files');
  await mkdir(join(originals, 'Projects', f.projectId), { recursive: true });
  const link = join(f.conversations.root, 'Desktop');
  await symlink(await realpath(originals), link, 'junction');
  catalog.Projects[0].FolderPath = join(link, 'Projects', f.projectId);
  await f.conversations.saveCatalog(catalog);
  const ctx = await f.context('ask');
  const call = f.call('filesystem.write', { path: 'pending.txt', content: 'not written', expectedHash: null });
  const pending = await pendingApproval(f.service, ctx, call);
  // Remove only the synthetic junction, preserving the source directory and its contents.
  // 仅删除测试创建的目录联接，保留源目录及其内容。
  await unlink(link);
  await mkdir(join(link, 'Projects', f.projectId), { recursive: true });
  approve(f.service, ctx, pending.event.tool);
  const result = await pending.result;
  assert.equal(result.code, 'WORKSPACE_CHANGED');
  await assert.rejects(readFile(join(ctx.workspaceRoot, 'pending.txt')), { code: 'ENOENT' });
});

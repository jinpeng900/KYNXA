import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { mkdir, mkdtemp, readFile, realpath, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, relative, resolve, sep } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { SandboxRunner } from '../../apps/model-gateway/tools/sandbox-runner.mjs';

test('real AppContainer accepts legacy project IDs and DOS path aliases while rejecting real junctions',
  { skip: process.platform !== 'win32', timeout: 60000 }, async t => {
    const root = await mkdtemp(join(tmpdir(), 'kynxa-workspace-paths-'));
    const data = join(root, 'Synthetic long data folder');
    const workspace = join(data, 'Desktop', 'Projects', randomUUID().replaceAll('-', ''));
    const runner = new SandboxRunner({ toolHostPath: process.env.KYNXA_SANDBOX_SMOKE_TOOL_HOST, excludedRoots: [data] });
    t.after(async () => {
      await runner.cleanupAll();
      const suffix = relative(resolve(tmpdir()), resolve(root));
      assert.ok(suffix && suffix !== '..' && !suffix.startsWith('..' + sep));
      await rm(root, { recursive: true, force: true });
    });
    await mkdir(workspace, { recursive: true });
    await writeFile(join(workspace, 'fixture.txt'), 'synthetic path fixture');
    const capabilities = await runner.capabilities();
    assert.equal(capabilities.available, true, capabilities.reason);
    const args = ['-e', "console.log(require('node:fs').readFileSync('fixture.txt', 'utf8'));"];
    const first = await runner.run({ workspaceRoot: workspace, trustedManagedWorkspace: true, command: 'node', args });
    assert.equal(first.exitCode, 0, first.stderr);
    assert.equal(first.stdout.trim(), 'synthetic path fixture');
    assert.equal(first.tokenVerified, true);
    assert.equal(first.activeProcessesAfterExit, 0);

    const script = join(dirname(fileURLToPath(import.meta.url)), 'Get-OwnFixtureShortPath.ps1');
    const powershell = join(process.env.SystemRoot, 'System32', 'WindowsPowerShell', 'v1.0', 'powershell.exe');
    const observed = spawnSync(powershell, ['-NoProfile', '-NonInteractive', '-File', script, workspace], { encoding: 'utf8', windowsHide: true });
    assert.equal(observed.status, 0, observed.stderr);
    const alias = observed.stdout.trim();
    assert.equal((await realpath(alias)).toLowerCase(), (await realpath(workspace)).toLowerCase());
    const aliased = await runner.run({ workspaceRoot: alias, trustedManagedWorkspace: true, command: 'node', args });
    assert.equal(aliased.exitCode, 0, aliased.stderr);
    assert.equal(aliased.stdout.trim(), 'synthetic path fixture');
    assert.equal(await readFile(join(workspace, 'fixture.txt'), 'utf8'), 'synthetic path fixture');

    const junction = join(root, 'linked-parent');
    await symlink(data, junction, 'junction');
    await assert.rejects(runner.run({ workspaceRoot: join(junction, 'Desktop', 'Projects', workspace.split(sep).at(-1)),
      trustedManagedWorkspace: true, command: 'node', args }), { code: 'SANDBOX_INVALID_WORKSPACE' });
    await assert.rejects(runner.run({ workspaceRoot: join(root, 'not-created'), command: 'node', args }), { code: 'SANDBOX_INVALID_WORKSPACE' });
  });

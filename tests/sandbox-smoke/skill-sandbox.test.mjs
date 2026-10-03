import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative } from 'node:path';
import { createHash } from 'node:crypto';
import { SandboxRunner } from '../../apps/model-gateway/sandbox-runner.mjs';

test('approved skill package runs read-only in the real AppContainer; stale manifests execute nothing',
  { skip: process.platform !== 'win32', timeout: 60000 }, async () => {
    const root = await mkdtemp(join(tmpdir(), 'kynxa-skill-sandbox-'));
    const workspace = join(root, 'workspace'), skillRoot = join(root, '.kynxa', 'skills', 'fixture');
    const runner = new SandboxRunner({ toolHostPath: process.env.KYNXA_SANDBOX_SMOKE_TOOL_HOST });
    try {
      await mkdir(workspace); await mkdir(join(skillRoot, 'scripts'), { recursive: true });
      await mkdir(join(skillRoot, 'references'));
      await writeFile(join(workspace, 'original.txt'), 'original');
      const resources = {
        'references/data.txt': 'approved reference',
        'scripts/check.mjs': `import fs from 'node:fs'; import path from 'node:path'; import {fileURLToPath} from 'node:url';
const skill = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const reference = fs.readFileSync(path.join(skill,'references/data.txt'),'utf8');
let denied = false; try { fs.writeFileSync(path.join(skill,'references/data.txt'),'bad'); } catch { denied = true; }
let renameDenied = false; try { fs.renameSync(skill, path.join(process.cwd(),'replaced-package')); } catch { renameDenied = true; }
fs.writeFileSync('output.txt', 'snapshot only');
console.log(JSON.stringify({reference,denied,renameDenied,arg:process.argv[2]}));`
      };
      const files = [];
      for (const [relativePath, content] of Object.entries(resources)) {
        await writeFile(join(skillRoot, relativePath), content);
        files.push({ relativePath, size: Buffer.byteLength(content), sha256: createHash('sha256').update(content).digest('hex') });
      }
      const prepared = { skillRoot, scriptRelativePath: 'scripts/check.mjs', files };
      const result = await runner.runSkill({ package: prepared, workspaceRoot: workspace, args: ['hello'] });
      assert.equal(result.exitCode, 0, result.stderr);
      assert.deepEqual(JSON.parse(result.stdout.trim()), { reference: 'approved reference', denied: true, renameDenied: true, arg: 'hello' });
      assert.equal(await readFile(join(skillRoot, 'references/data.txt'), 'utf8'), 'approved reference');
      assert.equal(await readFile(join(result.stagingDirectory, 'output.txt'), 'utf8'), 'snapshot only');
      await runner.cleanup(result.stagingDirectory);
      await writeFile(join(skillRoot, 'references/data.txt'), 'changed reference');
      await assert.rejects(runner.runSkill({ package: prepared, workspaceRoot: workspace }), { code: 'APP_SKILL_CHANGED' });
      await assert.rejects(runner.runSkill({ package: { ...prepared, scriptRelativePath: '../outside.mjs' }, workspaceRoot: workspace }),
        { code: 'APP_SKILL_CHANGED' });
      assert.ok(relative(tmpdir(), root).startsWith('kynxa-skill-sandbox-'));
    } finally { await runner.cleanupAll(); await rm(root, { recursive: true, force: true }); }
  });

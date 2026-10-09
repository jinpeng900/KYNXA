import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readFile, realpath, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, relative, resolve, sep } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { HostTerminalRunner } from '../../apps/model-gateway/tools/host-terminal-runner.mjs';
import { HostTerminalJobs } from '../../apps/model-gateway/tools/host-terminal-jobs.mjs';

const toolHostPath = process.env.KYNXA_HOST_TERMINAL_SMOKE_TOOL_HOST ?? resolve(dirname(fileURLToPath(import.meta.url)),
  '../../apps/tool-host/bin/Debug/net10.0-windows/win-x64/KYNXA.ToolHost.exe');
const windowsOnly = { skip: process.platform !== 'win32', timeout: 30000 };
async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-native-background-jobs-')), cwd = join(root, 'workspace');
  await mkdir(cwd);
  const runner = new HostTerminalRunner({ toolHostPath }), jobs = new HostTerminalJobs({ runner });
  t.after(async () => {
    await jobs.close(); await runner.close();
    const suffix = relative(resolve(tmpdir()), resolve(root));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(root, { recursive: true, force: true });
  });
  return { root, cwd, runner, jobs, context: { conversationId: 'synthetic-background-chat' } };
}

test('native background task negotiates a longer deadline and saves its completed receipt without a console', windowsOnly, async t => {
  const f = await fixture(t), capabilities = await f.runner.capabilities();
  assert.equal(capabilities.backgroundJobs, true, capabilities.reason);
  assert.equal(capabilities.maximumJobTimeoutMs, 21600000);
  assert.equal(capabilities.maximumTimeoutMs, 120000);
  const started = await f.jobs.start(f.context, { shell: 'cmd', script: 'echo BACKGROUND_JOB_DONE>result.txt & type result.txt',
    cwd: f.cwd, timeoutMs: 180001 });
  assert.ok(started.processId > 0);
  await f.jobs.jobs.get(started.jobId).completion;
  const completed = f.jobs.read(f.context, { jobId: started.jobId });
  assert.equal(completed.status, 'completed'); assert.equal(completed.receipt.exitCode, 0);
  assert.equal(completed.receipt.activeProcessesAfterExit, 0);
  assert.equal(completed.receipt.cwd, await realpath(f.cwd));
  assert.match(completed.output, /BACKGROUND_JOB_DONE/);
  assert.match(await readFile(join(f.cwd, 'result.txt'), 'utf8'), /BACKGROUND_JOB_DONE/);
  await assert.rejects(f.runner.run({ shell: 'cmd', script: 'echo never', cwd: f.cwd, timeoutMs: 180001 }),
    { code: 'HOST_TERMINAL_INVALID_REQUEST' });
});

test('stop waits for native cleanup of the background task and all its owned descendants', windowsOnly, async t => {
  const f = await fixture(t);
  const script = `const cp=require('node:child_process');
const child=cp.spawn(process.execPath,['-e','setInterval(()=>{},1000)'],{stdio:'ignore'});
child.once('spawn',()=>{console.log(JSON.stringify({root:process.pid,child:child.pid}));setInterval(()=>{},1000);});`;
  await writeFile(join(f.cwd, 'owned-tree.cjs'), script);
  const started = await f.jobs.start(f.context, { shell: 'cmd', script: '"' + process.execPath + '" "owned-tree.cjs"', cwd: f.cwd });
  let tree;
  for (let attempt = 0; attempt < 300; attempt++) {
    const output = f.jobs.read(f.context, { jobId: started.jobId }).output;
    try { tree = JSON.parse(output.trim()); break; } catch {}
    await new Promise(resolve_ => setTimeout(resolve_, 10));
  }
  assert.ok(tree && tree.root > 0 && tree.child > 0, 'Owned descendant did not report its identity.');
  assert.doesNotThrow(() => process.kill(tree.root, 0));
  assert.doesNotThrow(() => process.kill(tree.child, 0));
  const stopped = await f.jobs.stop(f.context, { jobId: started.jobId });
  assert.equal(stopped.running, false); assert.equal(stopped.receipt.cancelled, true);
  assert.equal(stopped.receipt.activeProcessesAfterExit, 0); assert.equal(stopped.status, 'unknown');
  // Cleanup proves termination, never rollback of an already performed host operation.
  // 清理只能证明进程终止，不代表已执行的宿主操作被回滚。
  assert.throws(() => process.kill(tree.root, 0));
  assert.throws(() => process.kill(tree.child, 0));
});

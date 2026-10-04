import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, relative, resolve, sep } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const toolHost = process.env.KYNXA_HOST_TERMINAL_SMOKE_TOOL_HOST ?? resolve(dirname(fileURLToPath(import.meta.url)),
  '../../apps/tool-host/bin/Debug/net10.0-windows/win-x64/KYNXA.ToolHost.exe');
const windowsOnly = { skip: process.platform !== 'win32', timeout: 60000 };

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-native-host-terminal-'));
  const cwd = join(root, 'synthetic 工作 folder');
  await mkdir(cwd);
  t.after(async () => {
    const suffix = relative(resolve(tmpdir()), resolve(root));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith('..' + sep));
    await rm(root, { recursive: true, force: true });
  });
  return { root, cwd };
}

function invoke(request) {
  const child = spawn(toolHost, [], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true, shell: false });
  let output = '', errors = '', resolveStarted, resolveOutput;
  const started = new Promise(resolve_ => { resolveStarted = resolve_; });
  const firstOutput = new Promise(resolve_ => { resolveOutput = resolve_; });
  const records = [];
  child.stdout.setEncoding('utf8'); child.stderr.setEncoding('utf8');
  child.stdout.on('data', chunk => {
    output += chunk;
    for (let index; (index = output.indexOf('\n')) >= 0;) {
      const line = output.slice(0, index).trim(); output = output.slice(index + 1);
      if (!line) continue;
      const record = JSON.parse(line); records.push(record);
      if (record.event === 'host_terminal_started') resolveStarted(record);
      if (record.event === 'host_terminal_output') resolveOutput(record);
    }
  });
  child.stderr.on('data', chunk => { errors += chunk; });
  child.stdin.on('error', () => {});
  const result = new Promise((resolve_, reject) => {
    child.once('error', reject);
    child.once('close', code => {
      try {
        if (output.trim()) records.push(JSON.parse(output.trim()));
        assert.equal(errors, '');
        resolve_({ code, records, receipt: records.at(-1) });
      } catch (error) { reject(error); }
    });
  });
  child.stdin.write(JSON.stringify(request) + '\n');
  if (request.operation === 'host_terminal_capabilities') child.stdin.end();
  return { child, result, started, firstOutput };
}

async function run(request) {
  const pending = invoke({ operation: 'host_terminal', ...request });
  const result = await pending.result;
  assert.equal(result.code, 0, result.receipt?.error?.message);
  assert.equal(result.records.filter(record => record.event !== 'host_terminal_output').length, 2);
  assert.equal(result.records[0].event, 'host_terminal_started');
  assert.equal(result.records[0].processId, result.receipt.processId);
  assert.equal(result.receipt.protocolVersion, 1);
  assert.equal(result.receipt.boundary, 'host-terminal');
  assert.equal(result.receipt.activeProcessesAfterExit, 0);
  assert.ok(result.receipt.elapsedMs >= 0);
  assert.throws(() => process.kill(result.receipt.processId, 0));
  return result.receipt;
}

const nodeCommand = name => `"${process.execPath}" "${name}"`;
const treeScript = `const fs=require('node:fs'),cp=require('node:child_process');
const child=cp.spawn(process.execPath,['-e','setInterval(()=>{},1000)'],{stdio:'ignore'});
child.on('spawn',()=>{const value={root:process.pid,child:child.pid};console.log(JSON.stringify(value));fs.writeFileSync('tree-ready.json',JSON.stringify(value));setInterval(()=>{},1000);});`;

async function waitForTree(cwd) {
  for (let attempt = 0; attempt < 500; attempt++) {
    try { return JSON.parse(await readFile(join(cwd, 'tree-ready.json'), 'utf8')); }
    catch (error) { if (error.code !== 'ENOENT') throw error; }
    await new Promise(resolve_ => setTimeout(resolve_, 10));
  }
  assert.fail('The owned synthetic process tree did not start.');
}

test('native host capability discovery is separate from AppContainer and action receipts', windowsOnly, async () => {
  const result = await invoke({ operation: 'host_terminal_capabilities' }).result;
  assert.equal(result.code, 0); assert.equal(result.records.length, 1);
  const value = result.receipt;
  assert.equal(value.protocolVersion, 2); assert.equal(value.boundary, 'host-terminal');
  assert.equal(value.available, true); assert.equal(value.sandbox, false);
  assert.deepEqual(value.shells, ['cmd', 'powershell']); assert.equal(value.processTreeBounded, true);
  assert.equal(value.maxScriptCharacters, 16384); assert.equal(value.maxOutputBytes, 256 * 1024);
  assert.equal(value.completed, undefined);
});

test('native CMD preserves host workspace writes and OEM/UTF8 lines from actual child processes', windowsOnly, async t => {
  const { cwd } = await fixture(t);
  await writeFile(join(cwd, 'external.cjs'), "console.log('外部UTF8输出');console.error('外部UTF8错误');");
  const value = await run({ shell: 'cmd', cwd, script: `echo 本机终端测试&${nodeCommand('external.cjs')}&echo 结束>artifact.txt`, timeoutMs: 10000 });
  assert.equal(value.completed, true); assert.equal(value.outcome, 'completed'); assert.equal(value.exitCode, 0);
  assert.equal(value.cancelled, false); assert.equal(value.timedOut, false);
  assert.equal(value.outputEncoding, 'utf-8-or-oem-per-line'); assert.ok(!value.runtimeArguments.includes('/u'));
  assert.match(value.stdout, /本机终端测试/); assert.match(value.stdout, /外部UTF8输出/);
  assert.match(value.stderr, /外部UTF8错误/);
  assert.ok((await readFile(join(cwd, 'artifact.txt'))).length > 0);
  assert.ok(value.capturedOutputBytes <= value.maxOutputBytes);
});

test('captured terminal emits ordered live decoded lines without changing final streams', windowsOnly, async t => {
  const { cwd } = await fixture(t);
  await writeFile(join(cwd, 'live.cjs'), `process.stdout.write('LIVE_FIRST\\n');process.stderr.write('ERROR_FIRST\\n');
setTimeout(()=>{const text=Buffer.from('中文分块\\n');process.stdout.write(text.subarray(0,2));setTimeout(()=>{process.stdout.write(text.subarray(2));process.stderr.write('ERROR_TAIL');setTimeout(()=>process.stdout.write('FINAL_NO_NEWLINE'),250);},60);},600);`);
  const pending = invoke({ operation: 'host_terminal', shell: 'cmd', cwd, script: nodeCommand('live.cjs'), timeoutMs: 10000 });
  let finished = false;
  pending.result.then(() => { finished = true; });
  const first = await pending.firstOutput;
  assert.equal(finished, false); assert.equal(first.sequence, 1); assert.match(first.delta, /FIRST/);
  const { code, records, receipt } = await pending.result;
  assert.equal(code, 0); assert.equal(records[0].event, 'host_terminal_started');
  const events = records.filter(record => record.event === 'host_terminal_output');
  assert.ok(events.length >= 3);
  assert.deepEqual(events.map(record => record.sequence), events.map((_, index) => index + 1));
  for (const stream of ['stdout', 'stderr'])
    assert.equal(events.filter(record => record.stream === stream).map(record => record.delta).join(''), receipt[stream]);
  assert.match(receipt.stdout, /中文分块/); assert.ok(receipt.stdout.endsWith('FINAL_NO_NEWLINE'));
  assert.ok(receipt.stderr.endsWith('ERROR_TAIL'));
});

test('one long captured line is split into bounded live frames without losing final output', windowsOnly, async t => {
  const { cwd } = await fixture(t);
  await writeFile(join(cwd, 'long-line.cjs'), "process.stdout.write('x'.repeat(140000)+'🙂');");
  const { code, records, receipt } = await invoke({ operation: 'host_terminal', shell: 'cmd', cwd,
    script: nodeCommand('long-line.cjs'), timeoutMs: 10000 }).result;
  assert.equal(code, 0); assert.equal(receipt.completed, true);
  const output = records.filter(record => record.event === 'host_terminal_output');
  assert.ok(output.length > 1);
  assert.ok(output.every(record => record.delta.length <= 16384));
  assert.equal(output.map(record => record.delta).join(''), receipt.stdout);
  assert.ok(receipt.stdout.endsWith('🙂')); assert.equal(receipt.stdout.length, 140002);
});

test('native PowerShell retains leading using/param syntax, quoted Unicode and isolated source variable', windowsOnly, async t => {
  const { cwd } = await fixture(t);
  const script = "using namespace System.Text\nparam([string]$Name='中文 参数')\n[Console]::WriteLine(([StringBuilder]::new().Append($Name).Append(' \"quoted\" \\tail')).ToString())\n[Console]::WriteLine(@(Get-ChildItem Env:KYNXA_HOST_SOURCE_*).Count)\nSet-Content -LiteralPath 'ps-artifact.txt' -Value 'temporary host write' -Encoding UTF8";
  const value = await run({ shell: 'powershell', cwd, script, timeoutMs: 10000 });
  assert.equal(value.completed, true); assert.equal(value.exitCode, 0);
  assert.match(value.stdout, /中文 参数 "quoted" \\tail/); assert.match(value.stdout, /\r?\n0\r?\n/);
  assert.equal(value.stderr, ''); assert.equal(value.outputEncoding, 'utf-8');
  assert.match(await readFile(join(cwd, 'ps-artifact.txt'), 'utf8'), /temporary host write/);
});

test('native normal nonzero CMD and PowerShell exits are completed execution receipts', windowsOnly, async t => {
  const { cwd } = await fixture(t);
  for (const [shell, script] of [['cmd', 'echo owned-nonzero&exit /b 7'], ['powershell', "Write-Output 'owned-nonzero';exit 9"]]) {
    const value = await run({ shell, script, cwd, timeoutMs: 10000 });
    assert.equal(value.completed, true); assert.equal(value.outcome, 'completed');
    assert.equal(value.exitCode, shell === 'cmd' ? 7 : 9); assert.match(value.stdout, /owned-nonzero/);
  }
});

test('native validates request bounds before starting any shell', windowsOnly, async t => {
  const { cwd } = await fixture(t);
  for (const invalid of [{ shell: 'node' }, { cwd: 'relative' }, { cwd: join(cwd, 'missing') }, { script: ' ' },
    { script: 'x'.repeat(16385) }, { script: 'echo\0fixture' }, { timeoutMs: 99 }, { timeoutMs: 120001 }]) {
    const result = await invoke({ operation: 'host_terminal', shell: 'cmd', cwd, script: 'echo validation-fixture', timeoutMs: 10000, ...invalid }).result;
    assert.equal(result.code, 1); assert.equal(result.records.length, 1);
    assert.match(result.receipt.error.code, /^HOST_TERMINAL_/);
    assert.equal(result.receipt.error.partial, false); assert.equal(result.receipt.error.outcome, 'not_started');
  }
  const suffix = "\nWrite-Output '16K-fixture'";
  const quoted = await run({ shell: 'powershell', cwd, script: '#' + '"'.repeat(16384 - suffix.length - 1) + suffix, timeoutMs: 10000 });
  assert.equal(quoted.completed, true); assert.equal(quoted.exitCode, 0); assert.match(quoted.stdout, /16K-fixture/);
});

test('native cancellation keeps collected output, reports unknown and ends the complete owned CMD process tree', windowsOnly, async t => {
  const { cwd } = await fixture(t);
  await writeFile(join(cwd, 'tree.cjs'), treeScript);
  const pending = invoke({ operation: 'host_terminal', shell: 'cmd', cwd, script: nodeCommand('tree.cjs'), timeoutMs: 10000 });
  const event = await pending.started;
  const pids = await waitForTree(cwd);
  pending.child.stdin.end('cancel\n');
  const { receipt: value, records, code } = await pending.result;
  assert.equal(code, 0); assert.equal(records.filter(record => record.event !== 'host_terminal_output').length, 2); assert.equal(value.processId, event.processId);
  assert.equal(value.completed, false); assert.equal(value.outcome, 'unknown'); assert.equal(value.cancelled, true);
  assert.equal(value.timedOut, false); assert.equal(value.activeProcessesAfterExit, 0);
  assert.equal(JSON.parse(value.stdout).child, pids.child);
  for (const pid of [event.processId, pids.root, pids.child]) assert.throws(() => process.kill(pid, 0));
});

test('native timeout stops the owned parent and child while retaining actual output', windowsOnly, async t => {
  const { cwd } = await fixture(t);
  await writeFile(join(cwd, 'tree.cjs'), treeScript);
  const value = await run({ shell: 'cmd', cwd, script: nodeCommand('tree.cjs'), timeoutMs: 1500 });
  const pids = JSON.parse(value.stdout);
  assert.equal(value.completed, false); assert.equal(value.outcome, 'unknown');
  assert.equal(value.timedOut, true); assert.equal(value.cancelled, false);
  for (const pid of [pids.root, pids.child]) assert.throws(() => process.kill(pid, 0));
});

test('native output cap remains bounded and does not pretend interrupted execution completed', windowsOnly, async t => {
  const { cwd } = await fixture(t);
  await writeFile(join(cwd, 'spam.cjs'), "for(let i=0;i<10000;i++)process.stdout.write('x'.repeat(4096));");
  const value = await run({ shell: 'cmd', cwd, script: nodeCommand('spam.cjs'), timeoutMs: 10000 });
  assert.equal(value.completed, false); assert.equal(value.outcome, 'unknown');
  assert.equal(value.outputTruncated, true); assert.equal(value.outputLimitExceeded, true);
  assert.ok(Buffer.byteLength(value.stdout) + Buffer.byteLength(value.stderr) <= value.maxOutputBytes);
  assert.ok(value.capturedOutputBytes <= value.maxOutputBytes);
});

test('native EOF cancellation and helper termination cannot leave the owned job descendants alive', windowsOnly, async t => {
  for (const mode of ['eof', 'helper-termination']) {
    const { cwd } = await fixture(t);
    await writeFile(join(cwd, 'tree.cjs'), treeScript);
    const pending = invoke({ operation: 'host_terminal', shell: 'cmd', cwd, script: nodeCommand('tree.cjs'), timeoutMs: 10000 });
    const event = await pending.started;
    const pids = await waitForTree(cwd);
    if (mode === 'eof') pending.child.stdin.end(); else pending.child.kill();
    const result = await pending.result;
    if (mode === 'eof') { assert.equal(result.receipt.completed, false); assert.equal(result.receipt.cancelled, true); }
    else assert.ok(!result.records.some(record => record.completed === true));
    for (let attempt = 0; attempt < 100; attempt++) {
      let anyAlive = false;
      for (const pid of [event.processId, pids.root, pids.child]) {
        try { process.kill(pid, 0); anyAlive = true; } catch { }
      }
      if (!anyAlive) break;
      await new Promise(resolve_ => setTimeout(resolve_, 10));
    }
    for (const pid of [event.processId, pids.root, pids.child]) assert.throws(() => process.kill(pid, 0));
  }
});

test('native normal shell completion also ends an unreferenced child', windowsOnly, async t => {
  const { cwd } = await fixture(t);
  await writeFile(join(cwd, 'normal-tree.cjs'), "const child=require('node:child_process').spawn(process.execPath,['-e','setInterval(()=>{},1000)'],{stdio:'ignore'});child.on('spawn',()=>{console.log(JSON.stringify({child:child.pid}));child.unref();});");
  const value = await run({ shell: 'cmd', cwd, script: nodeCommand('normal-tree.cjs'), timeoutMs: 10000 });
  assert.equal(value.completed, true); assert.equal(value.exitCode, 0);
  assert.throws(() => process.kill(JSON.parse(value.stdout).child, 0));
});

test('native PowerShell cancellation and timeout also clean the real inherited child tree', windowsOnly, async t => {
  for (const mode of ['cancel', 'timeout']) {
    const { cwd } = await fixture(t);
    await writeFile(join(cwd, 'tree.cjs'), treeScript);
    const script = `& '${process.execPath.replaceAll("'", "''")}' 'tree.cjs'`;
    const pending = invoke({ operation: 'host_terminal', shell: 'powershell', cwd, script, timeoutMs: mode === 'timeout' ? 1800 : 10000 });
    const event = await pending.started;
    const pids = await waitForTree(cwd);
    if (mode === 'cancel') pending.child.stdin.end('cancel\n');
    const { receipt: value, code } = await pending.result;
    assert.equal(code, 0); assert.equal(value.completed, false); assert.equal(value.outcome, 'unknown');
    assert.equal(value.cancelled, mode === 'cancel'); assert.equal(value.timedOut, mode === 'timeout');
    assert.equal(value.activeProcessesAfterExit, 0); assert.equal(JSON.parse(value.stdout).child, pids.child);
    for (const pid of [event.processId, pids.root, pids.child]) assert.throws(() => process.kill(pid, 0));
  }
});

test('native UTF-8 output survives a child splitting multibyte text across writes', windowsOnly, async t => {
  const { cwd } = await fixture(t);
  await writeFile(join(cwd, 'split.cjs'), "const bytes=Buffer.from('分段中文UTF8输出\\n');process.stdout.write(bytes.subarray(0,2));setTimeout(()=>process.stdout.write(bytes.subarray(2)),50);");
  const value = await run({ shell: 'cmd', cwd, script: nodeCommand('split.cjs'), timeoutMs: 10000 });
  assert.equal(value.completed, true); assert.equal(value.stdout, '分段中文UTF8输出\n');
});

test('native PowerShell errors keep actual stderr and nonzero completion status', windowsOnly, async t => {
  const { cwd } = await fixture(t);
  const value = await run({ shell: 'powershell', cwd, script: "throw 'synthetic-owned-error'", timeoutMs: 10000 });
  assert.equal(value.completed, true); assert.equal(value.outcome, 'completed');
  assert.notEqual(value.exitCode, 0); assert.match(value.stderr, /synthetic-owned-error/);
});

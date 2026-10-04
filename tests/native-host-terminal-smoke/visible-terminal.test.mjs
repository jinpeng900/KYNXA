import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, relative, resolve, sep } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const toolHost = process.env.KYNXA_HOST_TERMINAL_SMOKE_TOOL_HOST ?? resolve(dirname(fileURLToPath(import.meta.url)),
  '../../apps/tool-host/bin/Debug/net10.0-windows/win-x64/KYNXA.ToolHost.exe');
const windowsOnly = { skip: process.platform !== 'win32', timeout: 45000 };

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-native-visible-terminal-'));
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
  let output = '', errors = '', resolveStarted;
  const records = [];
  const started = new Promise(resolve_ => { resolveStarted = resolve_; });
  child.stdout.setEncoding('utf8'); child.stderr.setEncoding('utf8');
  child.stdout.on('data', chunk => {
    output += chunk;
    for (let index; (index = output.indexOf('\n')) >= 0;) {
      const line = output.slice(0, index).trim(); output = output.slice(index + 1);
      if (!line) continue;
      const record = JSON.parse(line); records.push(record);
      if (record.event === 'host_terminal_started') resolveStarted(record);
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
  child.stdin.write(JSON.stringify({ operation: request.visible ? 'host_terminal_visible' : 'host_terminal', ...request }) + '\n');
  return { child, started, result };
}

const nodeScript = name => `"${process.execPath}" "${name}"`;
async function waitFile(cwd, name) {
  for (let attempt = 0; attempt < 600; attempt++) {
    try { return JSON.parse(await readFile(join(cwd, name), 'utf8')); }
    catch (error) { if (error.code !== 'ENOENT') throw error; }
    await new Promise(resolve_ => setTimeout(resolve_, 10));
  }
  assert.fail('Owned visible fixture did not write its receipt.');
}

function assertVisible(value) {
  assert.equal(value.protocolVersion, 2);
  assert.equal(value.boundary, 'host-terminal');
  assert.equal(value.visibleRequested, true);
  assert.equal(value.windowObserved, true);
  assert.match(value.windowId, /^\d+$/);
  assert.equal(value.consoleInput, 'console');
  assert.equal(value.consoleOutput, 'console');
  assert.equal(value.outputCapture, 'console-screen');
  assert.equal(value.transcriptComplete, false);
  assert.equal(value.streamsSeparated, false);
  assert.equal(value.stdout, ''); assert.equal(value.stderr, '');
  assert.equal(value.activeProcessesAfterExit, 0);
  assert.equal(value.outputLimitExceeded, false);
  assert.ok(value.capturedOutputBytes <= value.maxOutputBytes);
  assert.throws(() => process.kill(value.processId, 0));
  assert.throws(() => process.kill(value.workerProcessId, 0));
}

test('visible CMD is a real TTY, displays console text and preserves workspace writes', windowsOnly, async t => {
  const { cwd } = await fixture(t);
  await writeFile(join(cwd, 'tty.cjs'), `const fs=require('node:fs'); const v={in:!!process.stdin.isTTY,out:!!process.stdout.isTTY,err:!!process.stderr.isTTY};fs.writeFileSync('tty.json',JSON.stringify(v));console.log('VISIBLE_TTY_SUCCESS');setTimeout(()=>console.log('VISIBLE_FINISHED'),600);`);
  const pending = invoke({ shell: 'cmd', cwd, script: nodeScript('tty.cjs'), visible: true, keepOpenMs: 3000, timeoutMs: 10000 });
  const event = await pending.started;
  assert.equal(event.windowObserved, true);
  assert.deepEqual(await waitFile(cwd, 'tty.json'), { in: true, out: true, err: true });
  const image = await invoke({ operation: 'desktop', action: 'screenshot', windowId: event.windowId, processId: event.windowProcessId }).result;
  assert.equal(image.code, 0, image.receipt.error?.message);
  assert.equal(image.receipt.mimeType, 'image/png');
  const proofRoot = await mkdtemp(join(tmpdir(), 'kynxa-visible-terminal-proof-'));
  const proofPath = join(proofRoot, 'visible-console.png');
  await writeFile(proofPath, Buffer.from(image.receipt.data, 'base64'));
  console.log(JSON.stringify({ visibleConsoleProof: proofPath, width: image.receipt.width, height: image.receipt.height }));
  const { receipt: value, code } = await pending.result;
  assert.equal(code, 0, value.error?.message); assertVisible(value);
  assert.equal(value.completed, true); assert.equal(value.commandCompleted, true); assert.equal(value.exitCode, 0);
  assert.equal(value.consoleSnapshotAvailable, true);
  assert.match(value.consoleText, /VISIBLE_TTY_SUCCESS/); assert.match(value.consoleText, /VISIBLE_FINISHED/);
  assert.equal(value.displayHoldMs, 3000); assert.ok(value.elapsedMs >= 3600);
});

test('visible PowerShell preserves parsed script syntax and nonzero completion', windowsOnly, async t => {
  const { cwd } = await fixture(t);
  const pending = invoke({ shell: 'powershell', cwd, visible: true, keepOpenMs: 0, timeoutMs: 10000,
    script: "using namespace System.Text\nparam([string]$Name='中文 参数')\n[Console]::WriteLine(([StringBuilder]::new().Append($Name)).ToString());exit 7" });
  const { code, receipt: value } = await pending.result;
  assert.equal(code, 0, value.error?.message); assertVisible(value);
  assert.equal(value.completed, true); assert.equal(value.exitCode, 7); assert.match(value.consoleText, /中文 参数/);
});

test('visible timeout retains a partial console preview and kills the entire owned tree', windowsOnly, async t => {
  const { cwd } = await fixture(t);
  await writeFile(join(cwd, 'tree.cjs'), `const fs=require('node:fs'),cp=require('node:child_process');const c=cp.spawn(process.execPath,['-e','setInterval(()=>{},1000)'],{stdio:'ignore'});c.on('spawn',()=>{fs.writeFileSync('tree.json',JSON.stringify({root:process.pid,child:c.pid}));console.log('VISIBLE_PARTIAL_BEFORE_TIMEOUT');setInterval(()=>{},1000);});`);
  const pending = invoke({ shell: 'cmd', cwd, script: nodeScript('tree.cjs'), visible: true, keepOpenMs: 0, timeoutMs: 2500 });
  await pending.started;
  const pids = await waitFile(cwd, 'tree.json');
  const { code, receipt: value } = await pending.result;
  assert.equal(code, 0, value.error?.message); assertVisible(value);
  assert.equal(value.completed, false); assert.equal(value.commandCompleted, false); assert.equal(value.outcome, 'unknown');
  assert.equal(value.timedOut, true); assert.equal(value.exitCodeObserved, false);
  assert.match(value.consoleText, /VISIBLE_PARTIAL_BEFORE_TIMEOUT/);
  for (const pid of Object.values(pids)) assert.throws(() => process.kill(pid, 0));
});

test('cancelling a visible display hold preserves the already-completed command receipt', windowsOnly, async t => {
  const { cwd } = await fixture(t);
  await writeFile(join(cwd, 'done.cjs'), "require('node:fs').writeFileSync('done.json','true');console.log('VISIBLE_COMPLETED_BEFORE_CANCEL');");
  const pending = invoke({ shell: 'cmd', cwd, script: nodeScript('done.cjs'), visible: true, keepOpenMs: 5000, timeoutMs: 12000 });
  await pending.started;
  await waitFile(cwd, 'done.json');
  await new Promise(resolve_ => setTimeout(resolve_, 500));
  pending.child.stdin.end('cancel\n');
  const { code, receipt: value } = await pending.result;
  assert.equal(code, 0, value.error?.message); assertVisible(value);
  assert.equal(value.commandCompleted, true); assert.equal(value.completed, true); assert.equal(value.outcome, 'completed');
  assert.equal(value.cancelled, true); assert.equal(value.displayClosedEarly, true); assert.equal(value.exitCode, 0);
  assert.match(value.consoleText, /VISIBLE_COMPLETED_BEFORE_CANCEL/);
});

test('visible request rejects invalid hold before starting any command', windowsOnly, async t => {
  const { cwd } = await fixture(t);
  for (const fields of [{ visible: false, keepOpenMs: 0 }, { visible: true, keepOpenMs: -1 }, { visible: true, keepOpenMs: 30001 },
    { operation: 'host_terminal_visible', visible: false }]) {
    const { code, records, receipt } = await invoke({ shell: 'cmd', cwd, script: 'echo never-started', ...fields }).result;
    assert.equal(code, 1); assert.equal(records.length, 1);
    assert.equal(receipt.error.code, 'HOST_TERMINAL_INVALID_REQUEST'); assert.equal(receipt.outcome, 'not_started');
  }
});

test('deadline during visible hold preserves a known completed exit', windowsOnly, async t => {
  const { cwd } = await fixture(t);
  const { code, receipt: value } = await invoke({ shell: 'cmd', cwd, visible: true, keepOpenMs: 30000,
    timeoutMs: 1800, script: 'echo COMPLETED_BEFORE_HOLD_TIMEOUT' }).result;
  assert.equal(code, 0, value.error?.message); assertVisible(value);
  assert.equal(value.completed, true); assert.equal(value.commandCompleted, true); assert.equal(value.timedOut, true);
  assert.equal(value.displayClosedEarly, true); assert.ok(value.displayHoldMs < 30000);
  assert.match(value.consoleText, /COMPLETED_BEFORE_HOLD_TIMEOUT/);
});

test('terminating the helper kills the real visible console and its inherited child tree', windowsOnly, async t => {
  const { cwd } = await fixture(t);
  await writeFile(join(cwd, 'tree.cjs'), `const fs=require('node:fs'),cp=require('node:child_process');const c=cp.spawn(process.execPath,['-e','setInterval(()=>{},1000)'],{stdio:'ignore'});c.on('spawn',()=>{fs.writeFileSync('tree.json',JSON.stringify({root:process.pid,child:c.pid}));setInterval(()=>{},1000);});`);
  const pending = invoke({ shell: 'cmd', cwd, script: nodeScript('tree.cjs'), visible: true, timeoutMs: 10000 });
  const event = await pending.started;
  const pids = await waitFile(cwd, 'tree.json');
  pending.child.kill();
  await pending.result;
  for (const pid of [event.processId, event.workerProcessId, ...Object.values(pids)]) {
    for (let attempt = 0; attempt < 200; attempt++) {
      try { process.kill(pid, 0); } catch { break; }
      await new Promise(resolve_ => setTimeout(resolve_, 10));
    }
    assert.throws(() => process.kill(pid, 0));
  }
});

test('large visible output bounds snapshots including row separators and keeps completed exit', windowsOnly, async t => {
  const { cwd } = await fixture(t);
  await writeFile(join(cwd, 'flood.cjs'), "for(let i=0;i<2000;i++)console.log('x'.repeat(120)+i);console.log('VISIBLE_FLOOD_DONE');");
  const { code, records, receipt } = await invoke({ shell: 'cmd', cwd, script: nodeScript('flood.cjs'),
    visible: true, keepOpenMs: 0, timeoutMs: 15000 }).result;
  assert.equal(code, 0, receipt.error?.message); assertVisible(receipt);
  assert.equal(receipt.completed, true); assert.equal(receipt.exitCode, 0);
  assert.ok(receipt.consoleText.length <= 65536); assert.equal(receipt.consoleSnapshotTruncated, true);
  assert.match(receipt.consoleText, /VISIBLE_FLOOD_DONE/);
  const updates = records.filter(record => record.event === 'host_terminal_output');
  assert.ok(updates.length >= 1);
  assert.ok(updates.every(record => record.stream === 'console' && record.replace === true && record.text.length <= 65536));
});

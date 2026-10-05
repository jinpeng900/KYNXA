import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { join, relative, resolve, sep } from 'node:path';
import { tmpdir } from 'node:os';
import { test } from 'node:test';
import { DesktopRunner, invokeDesktopHost } from '../tools/desktop-runner.mjs';
import { findNativeToolHost } from '../tools/tool-host-path.mjs';

const target = { windowId: '12345', processId: 555, reason: 'Synthetic fixture only.' };
const png = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+j4GQAAAAASUVORK5CYII=';
async function fixture(t, invoke) {
  const directory = await mkdtemp(join(tmpdir(), 'kynxa-desktop-adapter-')), toolHostPath = join(directory, 'fixture.exe');
  await writeFile(toolHostPath, 'A placeholder; injected transport never executes this file.');
  const runner = new DesktopRunner({ toolHostPath, invoke });
  t.after(async () => {
    await runner.close();
    const suffix = relative(resolve(tmpdir()), resolve(directory));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(directory, { recursive: true, force: true });
  });
  return runner;
}
const windowsOnly = { skip: process.platform !== 'win32' };

test('real native transport accepts capability discovery without an action completion receipt', windowsOnly, async t => {
  const toolHostPath = await findNativeToolHost(undefined, 'DESKTOP_UNAVAILABLE');
  const runner = new DesktopRunner({ toolHostPath });
  t.after(() => runner.close());
  const capabilities = await runner.capabilities();
  assert.equal(capabilities.available, true, capabilities.reason);
  assert.equal(capabilities.boundary, 'host-desktop');
  assert.equal(capabilities.protocolVersion, 1);
  assert.equal(capabilities.interactiveWindows, true);
  assert.equal(capabilities.completed, undefined);
  assert.ok(capabilities.operations.includes('launch'));
  assert.ok(capabilities.operations.includes('screenshot'));
  // An impossible PID checks the complete read transport without reading user window titles.
  // 用不存在的 PID 验证读取传输；不读取用户窗口标题。
  const result = await runner.run('windows', { processId: 2147483647, reason: 'Verify an empty synthetic process filter.' });
  assert.equal(result.isError, false);
  assert.equal(result.value.completed, true);
  assert.deepEqual(result.value.windows, []);
});

test('desktop adapter rejects the old sandbox protocol and validates host action completion', windowsOnly, async t => {
  const requests = [];
  const runner = await fixture(t, async (_, request) => { requests.push(request); return { protocolVersion: 1, sandbox: 'appcontainer', available: true }; });
  assert.equal((await runner.capabilities()).available, false);
  await assert.rejects(runner.run('read', target), { code: 'DESKTOP_INVALID_RESULT' });
  assert.equal(requests[1].operation, 'desktop'); assert.equal(requests[1].reason, undefined);
  runner.invoke = async (_, request) => ({ protocolVersion: 1, boundary: 'host-desktop', action: request.action, completed: true, text: 'fixture' });
  assert.equal((await runner.run('read', target)).value.text, 'fixture');
});

test('desktop adapter keeps typed PNG outside the text projection and rejects invalid image payloads', windowsOnly, async t => {
  const runner = await fixture(t, async () => ({ protocolVersion: 1, boundary: 'host-desktop', action: 'screenshot', completed: true,
    mimeType: 'image/png', width: 1, height: 1, data: png }));
  const result = await runner.run('screenshot', target);
  assert.equal(result.canonical.content[0].data, png);
  assert.ok(!result.content.includes(png)); assert.equal(result.canonical.structuredContent.data, undefined);
  for (const invalid of [{ data: 'c3ludGhldGlj' }, { data: '!' }, { width: 0 }, { width: 16000001 }]) {
    runner.invoke = async () => ({ protocolVersion: 1, boundary: 'host-desktop', action: 'screenshot', completed: true,
      mimeType: 'image/png', width: 1, height: 1, data: png, ...invalid });
    await assert.rejects(runner.run('screenshot', target), { code: 'DESKTOP_INVALID_RESULT' });
  }
});

test('native partial input and late completion have distinct durable states; close cancels and drains workers', windowsOnly, async t => {
  const runner = await fixture(t, async () => ({ error: { code: 'DESKTOP_TARGET_CHANGED', message: 'Synthetic target lost foreground.',
    partial: true, deliveredInputEvents: 2 } }));
  const partial = await runner.run('type', { ...target, text: 'fixture' });
  assert.equal(partial.status, 'unknown'); assert.equal(partial.code, 'DESKTOP_PARTIAL_INPUT');
  assert.equal(partial.value.completed, false); assert.equal(partial.value.deliveredInputEvents, 2);
  const controller = new AbortController();
  runner.invoke = async (_, request) => { controller.abort(); return { protocolVersion: 1, boundary: 'host-desktop', action: request.action, completed: true }; };
  assert.equal((await runner.run('click', { ...target, x: 1, y: 1 }, controller.signal)).isError, false);
  let started;
  const ready = new Promise(resolve => { started = resolve; });
  runner.invoke = async (_, __, signal) => new Promise((_, reject) => {
    started(); signal.addEventListener('abort', () => reject(Object.assign(new Error('Fixture stop'), { name: 'AbortError' })), { once: true });
  });
  const running = runner.run('read', target), assertion = assert.rejects(running, { name: 'AbortError' });
  await ready; await runner.close(); await assertion;
  assert.equal(runner.active.size, 0);
  await assert.rejects(runner.run('read', target), { name: 'AbortError' });
});

async function inheritedPipeFixture(t, mode) {
  const directory = await mkdtemp(join(tmpdir(), 'kynxa-desktop-pipe-fixture-'));
  const script = join(directory, 'synthetic-helper.cjs'), pidFile = join(directory, 'owned-child.pid');
  await writeFile(script, `const cp=require('node:child_process'),fs=require('node:fs');let pending='',request;
function reply(){process.stdout.write(JSON.stringify({protocolVersion:1,boundary:'host-desktop',action:request.action,completed:true,processId:12345})+'\\n',()=>process.exit(0));}
function fail(code){process.stdout.write(JSON.stringify({error:{code,message:'Synthetic native failure.'}})+'\\n',()=>process.exit(1));}
process.stdin.on('data',chunk=>{pending+=chunk;for(let index;(index=pending.indexOf('\\n'))>=0;){const line=pending.slice(0,index);pending=pending.slice(index+1);
if(!request){request=JSON.parse(line);const child=cp.spawn(process.execPath,['-e','require("node:fs").writeFileSync(process.argv[1],"ready");setTimeout(()=>{},30000)',process.argv[2]+'.ready'],{stdio:'inherit',detached:true,windowsHide:true});
child.once('spawn',()=>{fs.writeFileSync(process.argv[2],String(child.pid));
const poll=setInterval(()=>{if(!fs.existsSync(process.argv[2]+'.ready'))return;clearInterval(poll);
if(process.argv[3]==='complete')reply();else if(process.argv[3]==='oversize')process.stdout.write('x'.repeat(9*1024*1024));
else if(process.argv[3]==='incomplete')process.exit(0);},10);
if(process.argv[3]==='blocked')fail('DESKTOP_LAUNCH_BLOCKED');else if(process.argv[3]==='generic-error')fail('DESKTOP_FAILED');
});setInterval(()=>{},1000);}else if(line==='cancel'&&process.argv[3]==='complete-on-cancel')reply();}});`);
  t.after(async () => {
    try { const pid = Number(await readFile(pidFile, 'utf8')); assert.ok(Number.isSafeInteger(pid) && pid > 0); process.kill(pid); }
    catch (error) { if (!['ENOENT', 'ESRCH'].includes(error.code)) throw error; }
    const suffix = relative(resolve(tmpdir()), resolve(directory));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(directory, { recursive: true, force: true });
  });
  const factory = (_host, _args, options) => spawn(process.execPath, [script, pidFile, mode], options);
  const ready = async () => {
    for (let attempt = 0; attempt < 300; attempt++) {
      try { return Number(await readFile(pidFile, 'utf8')); }
      catch (error) { if (error.code !== 'ENOENT') throw error; }
      await new Promise(resolve_ => setTimeout(resolve_, 10));
    }
    assert.fail('Owned pipe-holder process did not start.');
  };
  return { factory, ready };
}

test('real transport settles on helper exit and complete frame even when an owned descendant holds IPC open', windowsOnly, async t => {
  const f = await inheritedPipeFixture(t, 'complete');
  const started = Date.now();
  const result = await invokeDesktopHost(process.execPath, { operation: 'desktop', action: 'launch' }, null, 2000, f.factory);
  assert.equal(result.completed, true); assert.equal(result.action, 'launch');
  assert.ok(Date.now() - started < 2000);
  const ownedChild = await f.ready();
  assert.doesNotThrow(() => process.kill(ownedChild, 0)); // Settlement must not kill the independent launched app.
  // 调用结束不应终止独立启动的软件。
});

test('real transport hard-settles deadline and oversized output despite inherited IPC', windowsOnly, async t => {
  for (const [mode, code] of [['hang', 'DESKTOP_TIMED_OUT'], ['oversize', 'DESKTOP_RESULT_TOO_LARGE']]) {
    const f = await inheritedPipeFixture(t, mode), started = Date.now();
    await assert.rejects(invokeDesktopHost(process.execPath, { operation: 'desktop', action: 'launch' }, null, 500, f.factory), error => {
      assert.equal(error.code, code); assert.equal(error.desktopOutcomeUnknown, true); return true;
    });
    assert.ok(Date.now() - started < 3000);
    const ownedChild = await f.ready();
    assert.doesNotThrow(() => process.kill(ownedChild, 0));
  }
});

test('real transport cancellation hard-settles unknown effects and retains a late completed frame', windowsOnly, async t => {
  for (const mode of ['hang', 'complete-on-cancel']) {
    const f = await inheritedPipeFixture(t, mode), controller = new AbortController();
    const pending = invokeDesktopHost(process.execPath, { operation: 'desktop', action: 'launch' }, controller.signal, 5000, f.factory);
    const assertion = mode === 'hang' ? assert.rejects(pending, error => {
      assert.equal(error.name, 'AbortError'); assert.equal(error.desktopOutcomeUnknown, true); return true;
    }) : pending;
    await f.ready(); const started = Date.now(); controller.abort();
    const result = await assertion;
    if (mode === 'complete-on-cancel') assert.equal(result.completed, true);
    assert.ok(Date.now() - started < 2000);
  }
});

test('real incomplete launch receipt is unknown and active operation drains instead of hanging', windowsOnly, async t => {
  const f = await inheritedPipeFixture(t, 'incomplete');
  const runner = await fixture(t, (host, request, signal) => invokeDesktopHost(host, request, signal, 500, f.factory));
  const result = await runner.run('launch', { appPath: runner.toolHostPath, reason: 'Synthetic launch transport fixture.' });
  assert.equal(result.status, 'unknown'); assert.equal(result.isError, true); assert.equal(result.value.completed, false);
  assert.equal(result.value.outcome, 'unknown'); assert.equal(result.code, 'DESKTOP_INVALID_RESULT');
  assert.equal(runner.active.size, 0);
});

test('unverified launch completion identities stay unknown while definite native launch errors stay errors', windowsOnly, async t => {
  const runner = await fixture(t, async () => ({}));
  for (const invalid of [{ action: 'read' }, { boundary: 'appcontainer' }, { protocolVersion: 2 }, { completed: false }]) {
    runner.invoke = async () => ({ protocolVersion: 1, boundary: 'host-desktop', action: 'launch', completed: true, ...invalid });
    const result = await runner.run('launch', { appPath: runner.toolHostPath, reason: 'Synthetic completion identity fixture.' });
    assert.equal(result.status, 'unknown'); assert.equal(result.value.completed, false); assert.equal(result.value.outcome, 'unknown');
    assert.equal(result.code, 'DESKTOP_INVALID_RESULT'); assert.equal(result.isError, true);
  }
  runner.invoke = async () => { throw Object.assign(new Error('Synthetic native validation blocked launch.'), { code: 'DESKTOP_LAUNCH_BLOCKED' }); };
  await assert.rejects(runner.run('launch', { appPath: runner.toolHostPath, reason: 'Synthetic blocked fixture.' }), { code: 'DESKTOP_LAUNCH_BLOCKED' });
});

test('real native generic launch failures stay unknown while native pre-start failures remain errors', windowsOnly, async t => {
  for (const mode of ['blocked', 'generic-error']) {
    const f = await inheritedPipeFixture(t, mode);
    const runner = await fixture(t, (host, request, signal) => invokeDesktopHost(host, request, signal, 500, f.factory));
    const pending = runner.run('launch', { appPath: runner.toolHostPath, reason: 'Synthetic native failure classification.' });
    if (mode === 'blocked') await assert.rejects(pending, { code: 'DESKTOP_LAUNCH_BLOCKED' });
    else {
      const result = await pending;
      assert.equal(result.status, 'unknown'); assert.equal(result.isError, true);
      assert.equal(result.value.completed, false); assert.equal(result.code, 'DESKTOP_FAILED');
    }
  }
});

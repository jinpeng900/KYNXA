import assert from 'node:assert/strict';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { test } from 'node:test';
import { DesktopRunner } from '../tools/desktop-runner.mjs';
import { computerDescriptors, computerKeyNames } from '../official-tools/Tools/computer.mjs';
import { findNativeToolHost } from '../tools/tool-host-path.mjs';
import { toolFixture } from './tool-fixture.mjs';

const target = { windowId: '12345', processId: 54321, reason: 'Only the owned synthetic fixture.' };
const png = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+j4GQAAAAASUVORK5CYII=';

test('real native capabilities match all shipped desktop key names and the new window action', { skip: process.platform !== 'win32' }, async t => {
  const runner = new DesktopRunner({ toolHostPath: await findNativeToolHost(undefined, 'DESKTOP_UNAVAILABLE') });
  t.after(() => runner.close());
  const capability = await runner.capabilities();
  assert.equal(capability.available, true, capability.reason);
  assert.deepEqual([...capability.keys].sort(), [...computerKeyNames].sort());
  assert.ok(capability.operations.includes('window'));
});

test('desktop additions retain broker approvals, result receipts and schema validation', async t => {
  const calls = [], operations = computerDescriptors.map(tool => tool.name.slice(9));
  const f = await toolFixture(t, { desktopRunner: {
    capabilities: async () => ({ protocolVersion: 1, boundary: 'host-desktop', available: true, interactiveWindows: true, operations }),
    run: async (action, args) => { calls.push({ action, args }); return { value: { protocolVersion: 1,
      boundary: 'host-desktop', action, completed: true, ...target, mode: args.mode, backgroundRequested: args.background }, isError: false }; }
  } });
  const ask = await f.context('ask');
  const denied = await f.run(ask, 'computer.window', { ...target, mode: 'resize', width: 800, height: 600 });
  assert.equal(denied.code, 'TOOL_APPROVAL_REQUIRED'); assert.equal(calls.length, 0);
  const full = await f.context('full');
  for (const [name, args] of [
    ['computer.window', { ...target, mode: 'resize', width: 800, height: 600 }],
    ['computer.launch', { appPath: 'C:\\Synthetic\\fixture.exe', background: true, reason: target.reason }],
    ['computer.read', { ...target, region: { x: 0, y: 0, width: 100, height: 100 }, elementId: '42,12345', timeoutMs: 1000 }],
    ...['CTRL+PLUS', 'CTRL+MINUS', 'CTRL+0'].map(key => ['computer.key', { ...target, key }])
  ]) {
    const result = await f.run(full, name, args);
    assert.equal(result.status, 'completed', result.content); assert.ok(result.resultRef);
  }
  const invalid = await f.run(full, 'computer.window', { ...target, mode: 'delete' });
  assert.equal(invalid.isError, true); assert.equal(calls.length, 6);
  const passwordRead = computerDescriptors.find(tool => tool.name === 'computer.read');
  assert.ok(passwordRead.inputSchema.properties.elementId);
  assert.deepEqual(computerDescriptors.find(tool => tool.name === 'computer.key').inputSchema.properties.key.enum, computerKeyNames);
});

test('desktop adapter validates extensions before IPC and bounds each UIA request separately', { skip: process.platform !== 'win32' }, async t => {
  const directory = await mkdtemp(join(tmpdir(), 'kynxa-desktop-capabilities-')), toolHostPath = join(directory, 'fixture.exe');
  await writeFile(toolHostPath, 'Synthetic adapter transport only.');
  const calls = [];
  const runner = new DesktopRunner({ toolHostPath, invoke: async (_, request, signal, timeoutMs) => {
    calls.push({ request, timeoutMs });
    return { protocolVersion: 1, boundary: 'host-desktop', action: request.action, completed: true,
      ...(request.action === 'read' ? { region: request.region, elementId: request.elementId, timeoutMs: request.timeoutMs } : {}),
      ...(request.action === 'window' ? { mode: request.mode } : {}),
      ...(request.action === 'screenshot' ? { mimeType: 'image/png', width: 1, height: 1, data: png, crop: request.crop } : {}) };
  } });
  t.after(async () => { await runner.close(); await rm(directory, { recursive: true, force: true }); });
  await runner.run('read', { ...target, timeoutMs: 1000, elementId: '42,12345', region: { x: 2, y: 3, width: 4, height: 5 } });
  assert.equal(calls[0].timeoutMs, 2500); assert.equal(calls[0].request.reason, undefined);
  await runner.run('window', { ...target, mode: 'resize', width: 640, height: 480 });
  for (const key of ['CTRL+PLUS', 'CTRL+MINUS', 'CTRL+0']) await runner.run('key', { ...target, key });
  const cropped = await runner.run('screenshot', { ...target, crop: { x: 0, y: 0, width: 1, height: 1 } });
  assert.equal(cropped.canonical.content[0].data, png); assert.ok(!cropped.content.includes(png));
  assert.deepEqual(cropped.canonical.structuredContent.crop, { x: 0, y: 0, width: 1, height: 1 });
  const before = calls.length;
  for (const [action, args, code] of [
    ['read', { timeoutMs: 499 }, 'DESKTOP_INVALID_REQUEST'], ['read', { timeoutMs: 5001 }, 'DESKTOP_INVALID_REQUEST'],
    ['read', { elementId: '../../private' }, 'DESKTOP_INVALID_REQUEST'], ['window', { mode: 'resize', width: 640 }, 'DESKTOP_INVALID_REQUEST'],
    ['window', { mode: 'destroy' }, 'DESKTOP_INVALID_REQUEST'], ['key', { key: 'WIN+L' }, 'DESKTOP_INVALID_KEY'],
    ['screenshot', { crop: { x: -1, y: 0, width: 1, height: 1 } }, 'DESKTOP_INVALID_COORDINATES'],
    ['launch', { background: 'true' }, 'DESKTOP_INVALID_REQUEST']
  ]) await assert.rejects(runner.run(action, { ...target, ...args }), { code });
  assert.equal(calls.length, before, 'invalid extensions never reach the native helper');
  runner.invoke = async (_, request) => ({ protocolVersion: 1, boundary: 'host-desktop', action: request.action, completed: true,
    ...(request.action === 'screenshot' ? { mimeType: 'image/png', data: png, width: 1, height: 1 } : {}) });
  await assert.rejects(runner.run('read', { ...target, elementId: '42,12345' }), { code: 'DESKTOP_INVALID_RESULT' });
  await assert.rejects(runner.run('screenshot', { ...target, crop: { x: 0, y: 0, width: 1, height: 1 } }), { code: 'DESKTOP_INVALID_RESULT' });
  await assert.rejects(runner.run('window', { ...target, mode: 'maximize' }), { code: 'DESKTOP_INVALID_RESULT' });
  const launch = await runner.run('launch', { appPath: toolHostPath, background: true, reason: target.reason });
  assert.equal(launch.status, 'unknown'); assert.equal(launch.code, 'DESKTOP_INVALID_RESULT');
});

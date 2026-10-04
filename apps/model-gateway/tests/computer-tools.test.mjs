import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { readFile, symlink, rename, mkdir } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from 'node:test';
import { toolFixture, pendingApproval, approve, parsed } from './tool-fixture.mjs';
import { DesktopRunner } from '../desktop-runner.mjs';
import { ModelToolCatalog } from '../tool-catalog.mjs';
import { builtinDescriptors } from '../official-tools/Tools/catalog.mjs';
import { runToolLoop } from '../tool-loop.mjs';

const operations = builtinDescriptors.filter(tool => tool.name.startsWith('computer.')).map(tool => tool.name.slice(9));
const target = { windowId: '98765', processId: 12345, reason: 'Inspect this synthetic test window.' };

test('unknown desktop effect is persisted and stops the loop before another launch or model call', async () => {
  let rounds = 0, executions = 0;
  const saved = [], events = [];
  const call = { id: 'desktop-effect', name: 'computer.launch', arguments: { appPath: 'fixture.exe' } };
  await assert.rejects(runToolLoop({ protocol: 'openai-completions', context: {}, messages: [], system: '',
    inputBudgetTokens: 32000, declarations: [], emit: event => events.push(event), saveActivity: async activity => saved.push(activity),
    service: { execute: async () => { executions++; return { content: 'launch outcome unknown', isError: true,
      status: 'unknown', code: 'DESKTOP_TIMED_OUT', resultRef: { id: randomUUID() } }; } },
    requestTurn: async () => { rounds++; return { content: '', reasoning: '', calls: [call], continuation: [{ role: 'assistant',
      content: '', tool_calls: [{ id: call.id, type: 'function', function: { name: call.name, arguments: JSON.stringify(call.arguments) } }] }] }; }
  }), { code: 'DESKTOP_OUTCOME_UNKNOWN' });
  assert.equal(rounds, 1); assert.equal(executions, 1);
  assert.equal(saved.at(-1).status, 'unknown'); assert.ok(saved.at(-1).resultRef);
  assert.equal(events.findLast(event => event.type === 'tool_result').tool.status, 'unknown');
});
function desktopFixture() {
  const calls = [];
  return { calls, capabilities: async () => ({ protocolVersion: 1, available: true, interactiveWindows: true,
    boundary: 'host-desktop', operations }), run: async (action, args) => {
    calls.push({ action, args });
    return { value: { protocolVersion: 1, boundary: 'host-desktop', action, completed: true, text: 'Synthetic visible text' }, isError: false };
  } };
}

test('desktop launch defaults to background before approval and retains explicit foreground choices', async t => {
  const desktopRunner = desktopFixture(), f = await toolFixture(t, { desktopRunner });
  const args = { appPath: 'C:\\Synthetic\\app.exe', reason: target.reason };
  const context = await f.context('ask');
  const pending = await pendingApproval(f.service, context, f.call('computer.launch', args));
  assert.equal(pending.event.tool.arguments.background, true);
  pending.event.tool.arguments.background = false;
  approve(f.service, context, pending.event.tool);
  assert.equal((await pending.result).isError, false);
  assert.equal(desktopRunner.calls[0].args.background, true);
  const foreground = await f.service.createContext(f.conversationId,
    { requestId: randomUUID(), permissionMode: 'full', message: '把浏览器显示在前台' });
  assert.equal((await f.run(foreground, 'computer.launch', args)).isError, false);
  assert.equal(desktopRunner.calls[1].args.background, false);
  assert.equal((await f.run(await f.context('full'), 'computer.launch', { ...args, background: false })).isError, false);
  assert.equal(desktopRunner.calls[2].args.background, false);
});

test('background browser render options are approved exactly, bounded and absent from unrelated apps', async t => {
  const desktopRunner = desktopFixture(), f = await toolFixture(t, { desktopRunner });
  const context = await f.context('ask');
  const browser = { appPath: 'C:\\Synthetic\\Chrome\\chrome.exe', args: ['--user-data-dir=C:\\Synthetic\\isolated'], reason: target.reason };
  const flag = '--disable-backgrounding-occluded-windows';
  const pending = await pendingApproval(f.service, context, f.call('computer.launch', browser));
  assert.deepEqual(pending.event.tool.arguments.args, [...browser.args, flag]);
  pending.event.tool.arguments.args[1] = '--synthetic-mutated';
  approve(f.service, context, pending.event.tool);
  assert.equal((await pending.result).isError, false);
  assert.deepEqual(desktopRunner.calls[0].args.args, [...browser.args, flag]);
  assert.deepEqual(browser.args, ['--user-data-dir=C:\\Synthetic\\isolated']);
  const full = await f.context('full');
  assert.equal((await f.run(full, 'computer.launch', { ...browser, appPath: 'C:\\Synthetic\\msedge.exe', args: [flag] })).isError, false);
  assert.deepEqual(desktopRunner.calls[1].args.args, [flag]);
  assert.equal((await f.run(full, 'computer.launch', { ...browser, background: false })).isError, false);
  assert.deepEqual(desktopRunner.calls[2].args.args, browser.args);
  assert.equal((await f.run(full, 'computer.launch', { ...browser, appPath: 'C:\\Synthetic\\editor.exe' })).isError, false);
  assert.deepEqual(desktopRunner.calls[3].args.args, browser.args);
  const bounded = await f.run(full, 'computer.launch', { ...browser, args: Array.from({ length: 32 }, (_, index) => `--fixture-${index}`) });
  assert.equal(bounded.isError, true);
  assert.equal(desktopRunner.calls.length, 4);
});

test('Ask and Smart desktop reads and effects require an approval; Full still requires valid reason/schema', async t => {
  const desktopRunner = desktopFixture(), f = await toolFixture(t, { desktopRunner });
  for (const mode of ['ask', 'smart']) {
    const context = await f.context(mode);
    for (const action of ['windows', 'apps', 'screenshot', 'read', 'activate', 'move', 'click', 'scroll', 'drag', 'type', 'key', 'launch']) {
      const args = action === 'windows' || action === 'apps' ? { reason: target.reason } : action === 'launch' ? { appPath: 'C:\\Synthetic\\app.exe', reason: target.reason }
        : { ...target, ...(['move', 'click', 'scroll', 'drag'].includes(action) ? { x: 12, y: 14 } : {}),
          ...(action === 'scroll' ? { delta: -120 } : {}), ...(action === 'drag' ? { endX: 24, endY: 28 } : {}),
          ...(action === 'type' ? { text: 'Synthetic text' } : {}), ...(action === 'key' ? { key: 'ENTER' } : {}) };
      assert.equal((await f.run(context, `computer.${action}`, args, { interactive: false })).code, 'TOOL_APPROVAL_REQUIRED', `${mode}/${action}`);
    }
    assert.equal(desktopRunner.calls.length, 0);
    const pending = await pendingApproval(f.service, context, f.call('computer.read', target));
    assert.equal(pending.event.tool.outsideWorkspace, true);
    approve(f.service, context, pending.event.tool, false);
    assert.equal((await pending.result).code, 'TOOL_DENIED');
  }
  const context = await f.context('full');
  const success = await f.run(context, 'computer.read', target, { interactive: false });
  assert.equal(success.status, 'completed'); assert.equal(success.outsideWorkspace, true); assert.ok(success.resultRef);
  assert.equal(parsed(success).boundary, 'host-desktop');
  for (const args of [{ ...target, reason: '' }, { ...target, reason: '   ' }, { ...target, processId: -2 }, { ...target, surprise: true }])
    assert.equal((await f.run(context, 'computer.read', args)).isError, true);
  assert.equal(desktopRunner.calls.length, 1);
});

test('approved desktop identity and input are immutable and scope/config changes stop execution', async t => {
  const desktopRunner = desktopFixture(), f = await toolFixture(t, { desktopRunner }), context = await f.context();
  const call = f.call('computer.type', { ...target, text: 'original synthetic text' });
  const pending = await pendingApproval(f.service, context, call);
  call.arguments.text = 'mutated'; pending.event.tool.arguments.windowId = '222';
  approve(f.service, context, pending.event.tool);
  assert.equal((await pending.result).isError, false);
  assert.equal(desktopRunner.calls[0].args.text, 'original synthetic text');
  assert.equal(desktopRunner.calls[0].args.windowId, target.windowId);
  const changed = await pendingApproval(f.service, context, f.call('computer.read', target));
  const catalog = await f.conversations.catalog(); catalog.Projects[0].FolderPath = null; catalog.Projects[0].IsFolderlessWorkspace = true;
  await f.conversations.saveCatalog(catalog); approve(f.service, context, changed.event.tool);
  assert.equal((await changed.result).code, 'WORKSPACE_CHANGED'); assert.equal(desktopRunner.calls.length, 1);
  const next = await f.context(); await f.service.catalog(next);
  const configPending = await pendingApproval(f.service, next, f.call('computer.read', target));
  const config = await f.service.getConfig();
  await f.service.updateConfig({ ...config, expectedRevision: config.revision, skillDirectories: [join(f.root, 'SyntheticSkills')] });
  approve(f.service, next, configPending.event.tool);
  assert.equal((await configPending.result).code, 'AGENT_CONFIG_CHANGED'); assert.equal(desktopRunner.calls.length, 1);
});

test('completed desktop effects are archived despite late cancellation; in-flight unknown effects stay unknown', async t => {
  const desktopRunner = desktopFixture(), controller = new AbortController();
  desktopRunner.run = async () => { controller.abort(); return { value: { completed: true, boundary: 'host-desktop', typed: 4 }, isError: false }; };
  const f = await toolFixture(t, { desktopRunner }), context = await f.context('full');
  const complete = await f.run(context, 'computer.type', { ...target, text: 'test' }, { signal: controller.signal });
  assert.equal(complete.status, 'completed'); assert.ok(complete.resultRef);
  const next = new AbortController();
  desktopRunner.run = async () => { next.abort(); throw Object.assign(new Error('Synthetic stop'), { name: 'AbortError' }); };
  const incomplete = await f.run(context, 'computer.type', { ...target, text: 'test' }, { signal: next.signal });
  assert.equal(incomplete.code, 'TOOL_CANCELLED'); assert.equal(incomplete.status, 'unknown');
  for (const code of ['DESKTOP_TIMED_OUT', 'DESKTOP_TIMEOUT']) {
    desktopRunner.run = async () => { throw Object.assign(new Error('Synthetic native timeout without completion proof'), { code }); };
    const timedOut = await f.run(context, 'computer.type', { ...target, text: 'test' });
    assert.equal(timedOut.code, code); assert.equal(timedOut.status, 'unknown');
    assert.equal(timedOut.isError, true); assert.ok(timedOut.resultRef);
    const archived = await f.service.results.get(context, timedOut.resultRef.id);
    assert.equal(archived.structuredContent.outcome, 'unknown');
    assert.equal(archived.structuredContent.error.code, code);
  }
});

test('screenshots retain typed PNG only in the local archive, without base64 in model content or public metadata', async t => {
  const desktopRunner = desktopFixture();
  desktopRunner.run = async () => ({ canonical: { content: [{ type: 'image', mimeType: 'image/png', data: 'c3ludGhldGljLWltYWdl' }],
    structuredContent: { action: 'screenshot', boundary: 'host-desktop', width: 2, height: 2 }, isError: false },
    content: 'Synthetic PNG archived for local view.', isError: false });
  const f = await toolFixture(t, { desktopRunner }), context = await f.context('full');
  const result = await f.run(context, 'computer.screenshot', target);
  assert.ok(result.resultRef); assert.ok(!result.content.includes('c3ludGhldGlj'));
  const archive = await f.service.results.get(context, result.resultRef.id);
  assert.equal(archive.content[0].data, 'c3ludGhldGljLWltYWdl');
  const page = await f.service.results.read(context, result.resultRef.id);
  assert.ok(!JSON.stringify(page).includes('c3ludGhldGlj'));
});

test('a missing native desktop host never invokes MCP or pretends to use an AppContainer', async t => {
  const f = await toolFixture(t);
  f.service.mcp.execute = async () => assert.fail('no fallback to unrelated MCP');
  const result = await f.run(await f.context('full'), 'computer.read', target);
  assert.equal(result.code, 'DESKTOP_UNAVAILABLE');
  assert.equal(result.sandbox, undefined);
});

test('explicit background instructions block foreground fallback and are normalized before approval', async t => {
  const desktopRunner = desktopFixture(), f = await toolFixture(t, { desktopRunner });
  const context = await f.service.createContext(f.conversationId, { requestId: randomUUID(), permissionMode: 'full',
    message: '保持后台操作，不要切到前台。' });
  for (const [action, args] of [['activate', target], ['type', { ...target, text: 'synthetic' }],
    ['window', { ...target, mode: 'maximize' }], ['launch', { appPath: 'C:\\Synthetic\\app.exe', background: false, reason: target.reason }]])
    assert.equal((await f.run(context, `computer.${action}`, args)).code, 'DESKTOP_FOREGROUND_FORBIDDEN');
  assert.equal(desktopRunner.calls.length, 0);
  assert.equal((await f.run(context, 'computer.read', target)).status, 'completed');
  const ask = await f.service.createContext(f.conversationId, { requestId: randomUUID(), message: '后台运行浏览器。' });
  const launch = f.call('computer.launch', { appPath: 'C:\\Synthetic\\app.exe', reason: target.reason });
  const pending = await pendingApproval(f.service, ask, launch);
  assert.equal(pending.event.tool.arguments.background, true);
  launch.arguments.background = false; pending.event.tool.arguments.background = false;
  approve(f.service, ask, pending.event.tool);
  assert.equal((await pending.result).status, 'completed');
  assert.equal(desktopRunner.calls.at(-1).args.background, true);
  const ordinary = await f.service.createContext(f.conversationId, { requestId: randomUUID(), permissionMode: 'full', message: '打开浏览器' });
  assert.equal((await f.run(ordinary, 'computer.activate', target)).status, 'completed', 'default DOM preference does not prohibit an authorized desktop fallback');
});

test('computer schemas are deferred for code/web tasks and discoverable on demand within the existing budget', () => {
  const code = new ModelToolCatalog(builtinDescriptors, { protocol: 'openai-completions', message: '写一个Node.js测试', tokenBudget: 16000 });
  assert.equal(code.selected.some(tool => tool.name.startsWith('computer.')), false);
  assert.deepEqual(code.load(['computer.read', 'computer.screenshot']).loaded, ['computer.read', 'computer.screenshot']);
  assert.ok(code.selected.find(tool => tool.name === 'terminal.run'));
  const desktop = new ModelToolCatalog(builtinDescriptors, { protocol: 'openai-completions', message: '打开记事本并截图', tokenBudget: 16000 });
  assert.ok(desktop.selected.find(tool => tool.name === 'computer.launch'));
  assert.ok(desktop.selected.find(tool => tool.name === 'computer.screenshot'));
});

test('folderless work and standalone chats get persistent isolated file scopes without changing formal ownership', async t => {
  const requests = [], sandboxRunner = { capabilities: async () => ({ available: true, sandbox: 'appcontainer', failClosed: true,
    checksChildToken: true, commands: ['node'] }), run: async input => { requests.push(input); return { exitCode: 0, sandbox: 'appcontainer', stdout: 'verified fixture', stderr: '' }; } };
  const f = await toolFixture(t, { sandboxRunner }), catalog = await f.conversations.catalog();
  catalog.Projects[0].FolderPath = null; catalog.Projects[0].IsFolderlessWorkspace = true;
  await f.conversations.saveCatalog(catalog); const before = await f.conversations.catalog();
  const context = await f.context('smart'), other = await f.context('smart', f.standaloneId);
  assert.equal(context.linkedWorkspaceRoot, null); assert.equal(context.projectId, f.projectId);
  assert.notEqual(context.workspaceRoot, other.workspaceRoot);
  parsed(await f.run(context, 'filesystem.write', { path: 'note.txt', content: 'durable generated file', expectedHash: null }, { interactive: false }));
  assert.equal(parsed(await f.run(context, 'filesystem.read', { path: 'note.txt' })).content, 'durable generated file');
  assert.equal((await f.run(other, 'filesystem.read', { path: 'note.txt' })).isError, true);
  assert.equal((await f.run(context, 'terminal.run', { command: 'node', args: ['-e', 'console.log(1)'] }, { interactive: false })).isError, false);
  assert.equal(requests[0].trustedManagedWorkspace, true); assert.equal(requests[0].workspaceRoot, context.workspaceRoot);
  assert.deepEqual(await f.conversations.catalog(), before);
  assert.equal((await f.run(context, 'filesystem.write', { path: '.workspace-owner.json', expectedHash: null, content: '{}' }, { interactive: false })).code, 'PROTECTED_APP_DATA');
  assert.equal((await f.run(context, 'filesystem.write', { path: join(f.conversations.root, 'catalog.json'), content: '{}', expectedHash: null, reason: 'test' }, { interactive: false })).code, 'PROTECTED_APP_DATA');
  const directory = context.workspaceRoot;
  await f.service.releaseContext(context);
  assert.equal(await readFile(join(directory, 'note.txt'), 'utf8'), 'durable generated file');
  const reopened = await f.context('smart'); assert.equal(reopened.workspaceRoot, directory);
  assert.match(await f.service.systemPrompt(reopened), /Isolated conversation work directory/);
});

test('a folderless chat cannot grant itself another directory by replacing its tool root while awaiting approval', async t => {
  const f = await toolFixture(t), context = await f.context('ask', f.standaloneId);
  const pending = await pendingApproval(f.service, context, f.call('filesystem.write', { path: 'effect.txt', content: 'must not write', expectedHash: null }));
  await rename(context.workspaceRoot, context.workspaceRoot + '-old');
  const other = join(f.root, 'Other'); await mkdir(other);
  await symlink(other, context.workspaceRoot, process.platform === 'win32' ? 'junction' : 'dir');
  approve(f.service, context, pending.event.tool);
  assert.equal((await pending.result).code, 'UNSAFE_TOOL_PATH');
});

test('native desktop adapter rejects interpreter launch and invalid target before starting a helper', async () => {
  const runner = new DesktopRunner({ invoke: async () => assert.fail('invalid arguments cannot start helper') });
  for (const appPath of ['relative.exe', 'C:\\Windows\\System32\\cmd.exe', process.execPath, 'C:\\test\\script.ps1'])
    await assert.rejects(runner.run('launch', { appPath }), { code: 'DESKTOP_INVALID_APPLICATION' });
  for (const windowId of ['', '0', '-1', '0x123', 'abc'])
    await assert.rejects(runner.run('read', { windowId, processId: 1 }), { code: 'DESKTOP_INVALID_TARGET' });
  runner.close();
});

import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdir, realpath, rm, symlink, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from 'node:test';
import { protocols } from '../models/protocols.mjs';
import { approve, parsed, pendingApproval, toolFixture } from './tool-fixture.mjs';

function desktopFixture() {
  const effects = [];
  const windows = [
    { windowId: '111', processId: 111, processName: 'notepad', executablePath: 'C:\\Synthetic\\notepad.exe' },
    { windowId: '222', processId: 222, processName: 'chrome', executablePath: 'C:\\Synthetic\\chrome.exe' }
  ];
  return { effects, capabilities: async () => ({ available: true, boundary: 'host-desktop',
    operations: ['windows', 'launch', 'activate', 'window'] }),
    run: async (action, args) => {
      if (action === 'windows') return { value: { windows: windows.filter(window => window.processId === args.processId) }, isError: false };
      effects.push({ action, ...args });
      return { value: { completed: true, action }, isError: false };
    } };
}

test('discovery remains usable when a recovered tool schema no longer fits the previous selection', async t => {
  const f = await toolFixture(t);
  await f.service.updateConfig({ version: 1, expectedRevision: 0, skillDirectories: [], mcpServers: [{
    id: 'synthetic', name: 'Synthetic recovery', command: process.execPath, args: [], enabled: true }] });
  let description = 'Read a synthetic observation.';
  f.service.mcp.catalog = async () => [{ name: 'mcp.synthetic.observe', toolName: 'observe', serverId: 'synthetic',
    source: 'mcp:synthetic', enabled: true, description,
    inputSchema: { type: 'object', properties: {}, additionalProperties: false } }];
  const context = await f.context('full');
  await f.service.catalog(context);
  f.service.configureModelCatalog(context, { protocol: protocols[0], tokenBudget: 3000 });
  parsed(await f.run(context, 'tool.load', { names: ['mcp.synthetic.observe'] }));
  description = 'A changed schema requiring extra prompt space. '.repeat(1500);
  f.service.mcp.errors.set('synthetic', 'MCP_TIMEOUT');
  const receipt = await f.run(context, 'tool.search', { query: 'synthetic' });
  assert.equal(receipt.isError, false, receipt.content);
  const discovered = (await f.service.results.get(context, receipt.resultRef.id)).structuredContent;
  assert.ok(discovered.tools.some(tool => tool.name === 'mcp.synthetic.observe'));
  assert.ok(f.service.modelCatalog(context).some(tool => tool.name === 'tool.load'));
  assert.equal(f.service.modelCatalog(context).some(tool => tool.name === 'mcp.synthetic.observe'), false);
  assert.equal((await f.run(context, 'tool.load', { names: ['mcp.synthetic.observe'] })).code, 'TOOL_CATALOG_BUDGET');
});

test('background preference permits a required foreground action while new applications still launch behind', async t => {
  const desktopRunner = desktopFixture(), f = await toolFixture(t, { desktopRunner });
  const application = join(f.workspace, 'fixture.exe');
  await writeFile(application, 'Synthetic executable metadata; no actual application is launched.');
  const context = await f.service.createContext(f.conversationId, { requestId: randomUUID(), permissionMode: 'full', message: '默认后台操作，帮我使用记事本' });
  assert.equal(context.browserInteraction.background, true);
  assert.equal(context.browserInteraction.allowForeground, true);
  assert.equal(context.foregroundForbidden, false);
  parsed(await f.run(context, 'computer.launch', { appPath: application, reason: 'Launch the synthetic application.' }));
  assert.equal(desktopRunner.effects[0].background, true);
  parsed(await f.run(context, 'computer.activate', { windowId: '111', processId: 111, reason: 'Focus the requested editor when necessary.' }));
  assert.equal(desktopRunner.effects[1].action, 'activate');
});

test('a browser foreground prohibition is checked against the observed target and does not restrict Notepad', async t => {
  const desktopRunner = desktopFixture(), f = await toolFixture(t, { desktopRunner });
  const context = await f.service.createContext(f.conversationId, { requestId: randomUUID(), permissionMode: 'full',
    message: '不要激活浏览器，帮我把记事本最大化' });
  parsed(await f.run(context, 'computer.window', { windowId: '111', processId: 111, mode: 'maximize', reason: 'Maximize the requested editor.' }));
  assert.equal(desktopRunner.effects.length, 1);
  const denied = await f.run(context, 'computer.activate', { windowId: '222', processId: 222, reason: 'Synthetic prohibited browser target.' });
  assert.equal(denied.code, 'DESKTOP_FOREGROUND_FORBIDDEN');
  assert.equal(desktopRunner.effects.length, 1);
});

test('application aliases bind before approval and cannot silently switch executable targets', async t => {
  const desktopRunner = desktopFixture(), f = await toolFixture(t, { desktopRunner });
  const first = join(f.root, 'first-app'), second = join(f.root, 'second-app'), alias = join(f.root, 'application-alias');
  await mkdir(first); await mkdir(second);
  await writeFile(join(first, 'fixture.exe'), 'First synthetic application.');
  await writeFile(join(second, 'fixture.exe'), 'Second synthetic application.');
  await symlink(first, alias, process.platform === 'win32' ? 'junction' : 'dir');
  const context = await f.context('ask');
  const pending = await pendingApproval(f.service, context, f.call('computer.launch', {
    appPath: join(alias, 'fixture.exe'), reason: 'Open the explicitly selected linked application.' }));
  assert.notEqual(pending.event.tool.arguments.appPath, join(alias, 'fixture.exe'));
  await rm(alias); await symlink(second, alias, process.platform === 'win32' ? 'junction' : 'dir');
  approve(f.service, context, pending.event.tool);
  assert.equal((await pending.result).code, 'TOOL_PATH_CHANGED');
  assert.equal(desktopRunner.effects.length, 0);
});

test('invalid public fetch parameters do not consume dispatch budget with or without a retrieval coordinator', async t => {
  let dispatches = 0;
  const f = await toolFixture(t, { webFetcher: { run: async () => { dispatches++; return { value: { text: 'Synthetic public content.' }, isError: false }; } } });
  for (const configured of [false, true]) {
    f.service.retrieval = configured ? { effective: async () => ({ web: { mode: 'auto', depth: 'standard', browserRead: 'auto' } }) } : null;
    const context = await f.context('full');
    for (let index = 0; index < 8; index++)
      assert.equal((await f.run(context, 'web.fetch', { url: 'https://example.com/' })).code, 'INVALID_TOOL_ARGUMENTS');
    assert.equal(f.service.webSearch.stages.has(context), false);
    for (let index = 0; index < 6; index++) parsed(await f.run(context, 'web.fetch', {
      url: `https://example.com/${index}`, reason: 'Read only the synthetic public fixture.' }));
    assert.equal(f.service.webSearch.stages.get(context).pageCount, 6);
    assert.equal((await f.run(context, 'web.fetch', { url: 'https://example.com/extra', reason: 'Do not dispatch.' })).code, 'WEB_STAGE_BUDGET_EXHAUSTED');
  }
  assert.equal(dispatches, 12);
});

test('folderless background jobs survive request completion, remain chat scoped and stop with a receipt', async t => {
  let executions = 0;
  const runner = { capabilities: async () => ({ available: true, boundary: 'host-terminal', backgroundJobs: true,
    maximumJobTimeoutMs: 21600000, shells: ['cmd', 'powershell'] }),
    run: async (request, signal, onOutput, onStarted) => {
      executions++;
      assert.equal(request.visible, false);
      assert.equal(request.backgroundJob, true);
      onStarted({ processId: 12345 });
      onOutput({ text: 'Synthetic job output.\n' });
      return new Promise(resolve => signal.addEventListener('abort', () => resolve({ value: {
        cancelled: true, activeProcessesAfterExit: 0 }, isError: true, code: 'TOOL_CANCELLED' }), { once: true }));
    } };
  const f = await toolFixture(t, { hostTerminalRunner: runner });
  const first = await f.context('full', f.standaloneId);
  const started = parsed(await f.run(first, 'terminal.host.start', { shell: 'cmd', script: 'synthetic background command', reason: 'Start the owned mock job.' }));
  assert.equal(started.running, true); assert.equal(started.timeoutMs, 1800000);
  await f.service.releaseContext(first);
  const next = await f.context('ask', f.standaloneId);
  const read = parsed(await f.run(next, 'terminal.host.read', { jobId: started.jobId }, { interactive: false }));
  assert.match(read.output, /Synthetic job output/); assert.equal(read.running, true);
  assert.equal((await f.run(await f.context('full'), 'terminal.host.read', { jobId: started.jobId })).code, 'HOST_TERMINAL_JOB_NOT_FOUND');
  assert.equal((await f.run(next, 'terminal.host.stop', { jobId: started.jobId, reason: 'Stop the owned job.' }, { interactive: false })).code, 'TOOL_APPROVAL_REQUIRED');
  const pending = await pendingApproval(f.service, next, f.call('terminal.host.stop', { jobId: started.jobId, reason: 'Stop the owned job.' }));
  approve(f.service, next, pending.event.tool);
  const stopped = parsed(await pending.result);
  assert.equal(stopped.running, false); assert.equal(stopped.receipt.activeProcessesAfterExit, 0);
  assert.equal(executions, 1);
});

test('context preparation reuses formal user history without losing a long-running browser task', async t => {
  const f = await toolFixture(t);
  f.conversations.readModelMessages = async () => assert.fail('formal history was already loaded by runtime');
  const context = await f.service.createContext(f.conversationId, { requestId: randomUUID(), permissionMode: 'full',
    message: '截图，我二维码登录', previousUserMessages: ['打开本机浏览器', ...Array(220).fill('刷新当前网页')] });
  assert.equal(context.browserTaskIntent.allowLocalBrowser, true);
  assert.equal(context.browserTaskIntent.inherited, true);
});

test('an explicitly mounted directory alias works for file reads, sandbox snapshots and work skills', async t => {
  const executed = [];
  const f = await toolFixture(t, { sandboxRunner: {
    capabilities: async () => ({ available: true, sandbox: 'appcontainer', failClosed: true, checksChildToken: true, commands: ['node'] }),
    run: async request => { executed.push(request); return { sandbox: 'appcontainer', exitCode: 0, stdout: 'Synthetic snapshot.' }; }
  } });
  const actual = join(f.root, 'actual-work'), alias = join(f.root, 'mounted-work');
  await mkdir(join(actual, '.kynxa', 'skills', 'workflow'), { recursive: true });
  await writeFile(join(actual, 'note.txt'), 'Actual mounted content.');
  await writeFile(join(actual, '.kynxa', 'skills', 'workflow', 'SKILL.md'), '---\nname: mounted-workflow\ndescription: Synthetic mounted work guidance.\n---\nUse the current work folder.');
  await symlink(actual, alias, process.platform === 'win32' ? 'junction' : 'dir');
  const catalog = await f.conversations.catalog(); catalog.Projects[0].FolderPath = alias;
  await f.conversations.saveCatalog(catalog);
  const context = await f.context('smart');
  assert.equal(context.linkedWorkspaceRoot, alias);
  assert.equal(context.workspaceRoot, await realpath(actual));
  assert.equal(parsed(await f.run(context, 'filesystem.read', { path: join(alias, 'note.txt') })).content, 'Actual mounted content.');
  parsed(await f.run(context, 'terminal.run', { command: 'node', args: ['synthetic.js'] }, { interactive: false }));
  assert.equal(executed[0].workspaceRoot, await realpath(actual));
  assert.ok((await f.service.listSkills(context)).some(skill => skill.name === 'mounted-workflow'));
});

test('a missing mounted folder keeps chat, discovery and independent absolute file access available', async t => {
  const f = await toolFixture(t);
  const catalog = await f.conversations.catalog(); catalog.Projects[0].FolderPath = join(f.root, 'moved-away');
  await f.conversations.saveCatalog(catalog);
  const context = await f.context('full');
  assert.equal(context.workspaceDiagnostic, 'ENOENT');
  await f.service.catalog(context);
  parsed(await f.run(context, 'tool.search', { query: 'read file' }));
  const note = join(f.workspace, 'external-note.txt'); await writeFile(note, 'Explicit external content.');
  assert.equal(parsed(await f.run(context, 'filesystem.read', { path: note, reason: 'Read the explicitly requested accessible note.' })).content, 'Explicit external content.');
  assert.equal((await f.run(context, 'terminal.run', { command: 'node', args: [] })).code, 'WORKSPACE_UNAVAILABLE');
});

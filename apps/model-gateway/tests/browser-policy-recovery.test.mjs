import assert from 'node:assert/strict';
import { test } from 'node:test';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { BrowserSessionRegistry, isExplicitForegroundForbidden } from '../tools/browser-sessions.mjs';
import { scriptRequestsBrowserForeground } from '../tools/browser-script-policy.mjs';
import { McpToolClients } from '../tools/mcp-client.mjs';
import { toolFixture } from './tool-fixture.mjs';

test('foreground restrictions apply to the named application instead of unrelated desktop work', () => {
  const message = '不要激活浏览器，帮我把记事本最大化';
  assert.equal(isExplicitForegroundForbidden(message, { appPath: 'C:\\Fixture\\chrome.exe' }), true);
  assert.equal(isExplicitForegroundForbidden(message, { appPath: 'C:\\Fixture\\notepad.exe' }), false);
  assert.equal(isExplicitForegroundForbidden(message, { kind: 'terminal' }), false);
  assert.equal(isExplicitForegroundForbidden('不要抢焦点', { appPath: 'C:\\Fixture\\notepad.exe' }), true);
  assert.equal(isExplicitForegroundForbidden('默认后台运行', { kind: 'browser' }), false);
  assert.equal(isExplicitForegroundForbidden('不要激活 Chrome', { appPath: 'C:\\Fixture\\msedge.exe' }), false);
  assert.equal(isExplicitForegroundForbidden('不要激活 Chrome', { appPath: 'C:\\Fixture\\chrome.exe' }), true);
  assert.equal(isExplicitForegroundForbidden('不要激活当前窗口', { kind: 'application' }), true);
  assert.equal(isExplicitForegroundForbidden('默认后台，并不是说不能激活窗口', { kind: 'application' }), false);
});

test('JavaScript foreground checks distinguish executable calls from quoted examples and comments', () => {
  for (const source of ['() => document.querySelector("pre").textContent.includes("window.open(")',
    '() => { /* window.focus() */ return "bringToFront()"; }', '() => `window.open()`',
    '() => document.querySelector("input").focus()'])
    assert.equal(scriptRequestsBrowserForeground(source), false, source);
  for (const source of ['() => window.focus()', '() => window["open"]("https://example.invalid")',
    'async (page) => { await page.bringToFront(); }', '() => `${window.focus()}`',
    '() => document.defaultView.focus()', '() => window["focus"].call(window)',
    '() => { const browserWindow = window; browserWindow.focus(); }',
    '() => { const { focus: focusWindow } = window; focusWindow(); }',
    '() => globalThis.window.open("https://example.invalid")', '() => eval("window.focus()")',
    '() => new Function("window.focus()")()', '() => window["fo" + "cus"]()'])
    assert.equal(scriptRequestsBrowserForeground(source), true, source);
  assert.throws(() => scriptRequestsBrowserForeground('() => {'), { code: 'BROWSER_SCRIPT_INVALID' });
});

test('background preference supports necessary foreground when an upstream lacks background arguments', () => {
  const registry = new BrowserSessionRegistry();
  const descriptor = { key: 'fixture', serverId: 'fixture', toolName: 'new_page', operation: 'tools/call',
    originalInputSchema: { type: 'object', properties: { url: { type: 'string' } } } };
  const server = { command: 'npx', args: ['chrome-devtools-mcp@1.10.1', '--isolated'] };
  const prepared = registry.prepare(descriptor, { url: 'https://example.invalid' }, server, { background: true, allowForeground: true });
  assert.equal(prepared.browser.background, false);
  assert.equal(prepared.args.background, undefined, 'never inject arguments absent from upstream schema');
  assert.throws(() => registry.prepare(descriptor, { url: 'https://example.invalid' }, server, { allowForeground: false }),
    { code: 'BROWSER_BACKGROUND_UNSUPPORTED' });
});

test('targeted MCP reset retains unrelated connections and waits only for changed server disposal', async () => {
  const clients = new McpToolClients(), closed = [];
  for (const id of ['changed', 'retained']) {
    const connection = { serverId: id, closed: false, client: { close: async () => { closed.push(id); } } };
    clients.connections.set(id, Promise.resolve(connection)); clients.connectionOwners.set(id, id);
    clients.states.set(id, { serverId: id, state: 'ready' });
  }
  const generation = clients.connectionGeneration;
  await clients.resetServers(['changed']);
  assert.deepEqual(closed, ['changed']); assert.equal(clients.connections.has('retained'), true);
  assert.equal(clients.states.has('retained'), true); assert.equal(clients.connectionGeneration, generation);
  await clients.close();
  assert.deepEqual(closed, ['changed', 'retained']);
});

test('targeted reset revokes changed startup and execution while an unrelated existing call remains usable', async () => {
  const clients = new McpToolClients();
  let resolveStartup;
  const pendingStartup = new Promise(resolve => { resolveStartup = resolve; });
  clients.connections.set('changed', pendingStartup); clients.connectionOwners.set('changed', 'changed');
  const retained = { serverId: 'retained', closed: false, client: { close: async () => {},
    callTool: async () => ({ content: [{ type: 'text', text: 'retained receipt' }] }) } };
  clients.connections.set('retained', Promise.resolve(retained)); clients.connectionOwners.set('retained', 'retained');
  const descriptor = id => ({ key: id, serverId: id, operation: 'tools/call', toolName: 'fixture', name: `mcp.${id}.fixture` });
  const input = { arguments: {}, policy: { reason: 'Use only the synthetic fixture.' } };
  const resetting = clients.resetServers(['changed']);
  await assert.rejects(clients.validateExecution(descriptor('changed'), input), { code: 'MCP_CATALOG_CHANGED' });
  assert.equal((await clients.execute(descriptor('retained'), input)).content, 'retained receipt');
  resolveStartup({ serverId: 'changed', closed: false, client: { close: async () => {} } });
  await resetting;
  assert.equal(clients.connections.has('retained'), true); await clients.close();
});

function playwrightFixture({ afterListing } = {}) {
  const clients = new McpToolClients(), calls = [];
  const server = { id: 'fixture', command: 'npx', args: ['@playwright/mcp@0.0.83', '--headless', '--isolated'] };
  const descriptor = (toolName, properties = {}) => ({ key: 'fixture', serverId: 'fixture', name: `mcp.fixture.${toolName}`,
    operation: 'tools/call', toolName, originalInputSchema: { type: 'object', properties } });
  const snapshot = descriptor('browser_snapshot'), listing = descriptor('browser_tabs', { action: { type: 'string' } });
  const click = descriptor('browser_click', { ref: { type: 'string' } });
  let snapshotCount = 0;
  const connection = { closed: false, tools: [snapshot, listing, click], artifactContext: { server }, client: {
    callTool: async request => {
      calls.push(request);
      if (request.name === 'browser_tabs') afterListing?.(clients, server);
      const text = request.name === 'browser_tabs' ? '- 0: (current) [Fixture](https://example.invalid/)' :
        request.name === 'browser_snapshot' ? `- Page URL: https://example.invalid/\n- button "Continue" [ref=e${++snapshotCount}]` : 'clicked';
      return { content: [{ type: 'text', text }] };
    }
  } };
  clients.connections.set('fixture', Promise.resolve(connection));
  const envelope = argumentsValue => ({ arguments: argumentsValue, policy: { reason: 'Use only the synthetic fixture page.' } });
  return { clients, calls, server, connection, snapshot, listing, click, envelope, options: { sessionId: 'one' } };
}

test('first Playwright action discovers and snapshots its own page after approval without replaying an old ref', async () => {
  const f = playwrightFixture();
  await f.clients.execute(f.snapshot, f.envelope({}), undefined, f.options);
  const prepared = await f.clients.prepareBrowserExecution(f.click, f.envelope({ ref: 'e1' }), f.options);
  assert.equal(f.calls.length, 1, 'preparation performs no RPC before approval');
  const recovered = await f.clients.execute(f.click, prepared, undefined, f.options);
  assert.equal(recovered.code, 'BROWSER_REFERENCE_REFRESHED');
  assert.equal(recovered.canonical.structuredContent.originalActionExecuted, false);
  assert.deepEqual(f.calls.map(call => call.name), ['browser_snapshot', 'browser_tabs', 'browser_snapshot']);
  assert.match(recovered.content, /ref=e2/);
  const click = await f.clients.prepareBrowserExecution(f.click, f.envelope({ ref: 'e2' }), f.options);
  assert.equal((await f.clients.execute(f.click, click, undefined, f.options)).isError, false);
  assert.deepEqual(f.calls.at(-1).arguments, { ref: 'e2' }, 'never inject a tab id absent from the upstream schema');
  const stale = await f.clients.prepareBrowserExecution(f.click, f.envelope({ ref: 'e2' }), f.options);
  const refreshed = await f.clients.execute(f.click, stale, undefined, f.options);
  assert.equal(refreshed.code, 'BROWSER_REFERENCE_REFRESHED'); assert.match(refreshed.content, /ref=e3/);
  assert.equal(f.calls.filter(call => call.name === 'browser_click').length, 1);
});

test('browser recovery will not attach to another conversation target or a replaced connection', async () => {
  const f = playwrightFixture();
  await f.clients.execute(f.snapshot, f.envelope({}), undefined, f.options);
  const prepared = await f.clients.prepareBrowserExecution(f.click, f.envelope({ ref: 'e1' }), f.options);
  await f.clients.execute(f.listing, f.envelope({ action: 'list' }), undefined, { sessionId: 'other' });
  await assert.rejects(f.clients.execute(f.click, prepared, undefined, f.options), { code: 'BROWSER_TAB_CHANGED' });
  assert.equal(f.calls.filter(call => call.name === 'browser_click').length, 0);
  const another = playwrightFixture();
  await another.clients.execute(another.snapshot, another.envelope({}), undefined, another.options);
  const pending = await another.clients.prepareBrowserExecution(another.click, another.envelope({ ref: 'e1' }), another.options);
  const connection = await another.clients.connections.get('fixture');
  another.clients.connections.set('fixture', Promise.resolve({ ...connection }));
  await assert.rejects(another.clients.execute(another.click, pending, undefined, another.options), { code: 'BROWSER_CONNECTION_CHANGED' });
  assert.equal(another.calls.length, 1);
});

for (const disabledTool of ['browser_tabs', 'browser_snapshot']) {
  test(`browser recovery honors live ${disabledTool} disablement without restarting its retained connection`, async () => {
    const f = playwrightFixture();
    await f.clients.execute(f.snapshot, f.envelope({}), undefined, f.options);
    const prepared = await f.clients.prepareBrowserExecution(f.click, f.envelope({ ref: 'e1' }), f.options);
    f.clients.updateServerPolicies([{ ...f.server, enabled: true, disabledTools: [disabledTool] }]);
    assert.equal(f.connection.artifactContext.server.disabledTools, undefined, 'the transport snapshot stays unchanged');
    const count = f.calls.length;
    await assert.rejects(f.clients.execute(f.click, prepared, undefined, f.options), { code: 'MCP_CATALOG_CHANGED' });
    assert.equal(f.calls.length, count, 'disabled hidden observation was never dispatched');
    assert.equal(await f.clients.connections.get('fixture'), f.connection, 'tool toggles preserve connection ownership');
    const allowed = disabledTool === 'browser_tabs' ? f.snapshot : f.listing;
    assert.equal((await f.clients.execute(allowed, f.envelope(allowed === f.listing ? { action: 'list' } : {}), undefined, f.options)).isError, false);
  });
}

test('disablement during browser discovery is rechecked before the recovery snapshot RPC', async () => {
  const f = playwrightFixture({ afterListing: (clients, server) => clients.updateServerPolicies([
    { ...server, enabled: true, disabledTools: ['browser_snapshot'] }
  ]) });
  await f.clients.execute(f.snapshot, f.envelope({}), undefined, f.options);
  const prepared = await f.clients.prepareBrowserExecution(f.click, f.envelope({ ref: 'e1' }), f.options);
  await assert.rejects(f.clients.execute(f.click, prepared, undefined, f.options), { code: 'TOOL_DISABLED' });
  assert.deepEqual(f.calls.map(call => call.name), ['browser_snapshot', 'browser_tabs']);
});

test('renaming or toggling one tool reuses the same owned MCP process in a new request catalog', async t => {
  const f = await toolFixture(t), log = join(f.root, 'policy-process-events.jsonl');
  const fixturePath = fileURLToPath(new URL('./fixtures/mcp-tool-server.mjs', import.meta.url));
  let config = await f.service.getConfig();
  await f.service.updateConfig({ ...config, expectedRevision: config.revision, mcpServers: [{ id: 'synthetic',
    name: 'Synthetic initial name', command: process.execPath, args: [fixturePath, log], enabled: true }] });
  const first = await f.context('full');
  await f.service.catalog(first, { connectMcp: true });
  const connection = await [...f.service.mcp.connections.values()][0], pid = connection.transport.pid;
  for (const change of [{ name: 'Synthetic renamed service' }, { disabledTools: ['slow'] }]) {
    config = await f.service.getConfig();
    await f.service.updateConfig({ ...config, expectedRevision: config.revision,
      mcpServers: config.mcpServers.map(server => ({ ...server, ...change })) });
    const next = await f.context('full');
    await f.service.catalog(next, { connectMcp: true });
    assert.equal(f.service.mcp.connections.size, 1);
    assert.equal((await [...f.service.mcp.connections.values()][0]).transport.pid, pid);
    const receipt = await f.run(next, 'mcp.synthetic.echo', {
      arguments: { value: 'unchanged connection' }, policy: { reason: 'Use only the isolated synthetic service.' }
    });
    assert.equal(receipt.status, 'completed');
  }
  const events = (await readFile(log, 'utf8')).trim().split('\n').map(JSON.parse);
  assert.equal(events.filter(event => event.event === 'started').length, 1);
});

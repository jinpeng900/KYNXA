import assert from 'node:assert/strict';
import { test } from 'node:test';
import { BrowserSessionRegistry, browserOperation, inferBrowserInteractionPolicy, isExplicitForegroundForbidden } from '../tools/browser-sessions.mjs';
import { McpToolClients, isMcpExecutionNotDispatched } from '../tools/mcp-client.mjs';

const envelope = args => ({ arguments: args, policy: { reason: 'Inspect only the isolated fixture browser.' } });
const server = { id: 'fixture-browser', name: 'Fixture', command: 'npx', enabled: true,
  args: ['chrome-devtools-mcp@1.10.1', '--headless', '--isolated'] };
const properties = { pageId: { type: 'number' }, uid: { type: 'string' }, background: { type: 'boolean' },
  bringToFront: { type: 'boolean' }, timeout: { type: 'integer' }, url: { type: 'string' } };
const descriptor = (toolName, schema = { type: 'object', properties }) => ({ key: 'browser-connection',
  serverId: server.id, name: `mcp.${server.id}.${toolName}`, toolName, operation: 'tools/call', originalInputSchema: schema });
const result = text => ({ content: [{ type: 'text', text }] });

function fixture(handler) {
  const clients = new McpToolClients(), requests = [];
  const connection = { closed: false, artifactContext: { server }, client: { callTool: async (request, options) => {
    requests.push({ request, options }); return handler(request, options, requests.length);
  } } };
  clients.connections.set('browser-connection', Promise.resolve(connection));
  return { clients, connection, requests };
}

test('background is the default preference; only explicit prohibitions disallow necessary foreground work', () => {
  for (const text of ['不要激活浏览器', 'do not focus the browser', "don't bring the browser to front", 'without activating the browser'])
    assert.deepEqual(inferBrowserInteractionPolicy(text), { background: true, allowForeground: false });
  for (const text of ['打开浏览器', '在本机 Chrome 查询资料', '不必切到前台', '后台操作 Chrome',
    '默认不前台，不是不能前台', '> activate the browser', '```activate the browser```'])
    assert.deepEqual(inferBrowserInteractionPolicy(text), { background: true, allowForeground: true });
  for (const text of ['切到前台', '激活 Chrome 浏览器窗口', 'focus the browser', 'bring the browser to front'])
    assert.deepEqual(inferBrowserInteractionPolicy(text), { background: false, allowForeground: true });
});

test('explicit foreground prohibition does not confuse the default background preference or quoted instructions', () => {
  for (const text of ['不能切到前台', '保持后台', '不要抢焦点', 'do not activate the browser',
    '激活 Chrome，但不要抢焦点']) assert.equal(isExplicitForegroundForbidden(text), true, text);
  for (const text of ['打开浏览器', '查询本机网页', '切到前台', '后台操作 Chrome', '> 不要激活浏览器', '```不要抢焦点```'])
    assert.equal(isExplicitForegroundForbidden(text), false, text);
});

test('browser defaults are included before approval, cannot silently focus, and use navigation deadlines', async () => {
  const f = fixture(() => result('1: Fixture title (https://example.invalid/) [selected]'));
  const input = envelope({ url: 'https://example.invalid/' });
  const prepared = await f.clients.prepareBrowserExecution(descriptor('new_page'), input, { sessionId: 'one' });
  assert.equal(f.requests.length, 0); assert.equal(prepared.arguments.background, true);
  assert.equal(prepared.arguments.timeout, 60000); assert.equal(input.arguments.background, undefined);
  const output = await f.clients.execute(descriptor('new_page'), prepared, undefined, { sessionId: 'one' });
  assert.equal(f.requests[0].request.arguments.background, true); assert.equal(f.requests[0].options.timeout, 70000);
  assert.equal(output.browser.tabId, '1'); assert.equal(output.browser.mode, 'independent-browser');
  assert.equal(output.browser.sessionId, 'one'); assert.equal(output.browser.outcome, 'completed');
  assert.equal(output.canonical._meta.kynxaBrowser.connectionId, 'browser-connection');
  await assert.rejects(f.clients.prepareBrowserExecution(descriptor('new_page'), envelope({ url: 'https://example.invalid/', background: false }), { allowForeground: false }),
    { code: 'BROWSER_FOREGROUND_FORBIDDEN' });
  await assert.rejects(f.clients.prepareBrowserExecution(descriptor('new_page', { type: 'object', properties: { url: properties.url } }), input, { allowForeground: false }),
    { code: 'BROWSER_BACKGROUND_UNSUPPORTED' });
  await assert.rejects(f.clients.prepareBrowserExecution(descriptor('new_page'), envelope({ timeout: 600000 })), { code: 'BROWSER_TIMEOUT_OUT_OF_RANGE' });
});

test('generic third-party click/snapshot names neither acquire browser policies nor change preparation validation counts', async () => {
  for (const name of ['click', 'take_snapshot', 'browser_snapshot'])
    assert.equal(browserOperation(descriptor(name), { command: 'node', args: ['business-server.mjs'] }), null);
  const f = fixture(() => result('unchanged')); f.connection.artifactContext.server = { command: 'node', args: ['business-server.mjs'] };
  f.clients.validateExecution = () => assert.fail('Non-browser preparation must not perform extra validation or RPC.');
  const input = envelope({ uid: 'business-id', value: 'payload' });
  assert.deepEqual(await f.clients.prepareBrowserExecution(descriptor('click'), input), input);
  assert.equal(f.requests.length, 0);
});

test('first Playwright snapshot is preserved without inventing a tab, then explicit discovery binds fresh references', () => {
  const registry = new BrowserSessionRegistry(), pw = { ...server, args: ['@playwright/mcp@0.0.83', '--headless', '--isolated'] };
  const snapshot = descriptor('browser_snapshot', { type: 'object', properties: {} });
  const click = descriptor('browser_click', { type: 'object', properties: { target: { type: 'string' } } });
  const first = registry.observe(registry.prepare(snapshot, {}, pw, { sessionId: 'one' }),
    result('- Page URL: https://example.invalid/\n- button "Click" [ref=e1]'), 'completed');
  assert.equal(first.tabId, undefined); assert.equal(first.needsTabDiscovery, true); assert.equal(first.url, 'https://example.invalid/');
  assert.throws(() => registry.prepare(click, { target: 'e1' }, pw, { sessionId: 'one' }), { code: 'BROWSER_TAB_ID_REQUIRED' });
  const tabs = descriptor('browser_tabs');
  registry.observe(registry.prepare(tabs, { action: 'list' }, pw, { sessionId: 'one' }),
    result('- 0: (current) [Fixture](https://example.invalid/)'), 'completed');
  assert.throws(() => registry.prepare(click, { target: 'e1' }, pw, { sessionId: 'one' }), { code: 'BROWSER_STALE_REFERENCE' });
  registry.observe(registry.prepare(snapshot, {}, pw, { sessionId: 'one' }),
    result('- Page URL: https://example.invalid/\n- button "Click" [ref=e2]'), 'completed');
  assert.equal(registry.prepare(click, { target: 'e2' }, pw, { sessionId: 'one' }).browser.tabId, '0');
});

test('select page targets the logical page without bringing it to the foreground', async () => {
  const f = fixture(() => result('2: https://example.invalid/two [selected]'));
  const prepared = await f.clients.prepareBrowserExecution(descriptor('select_page'), envelope({ pageId: 2 }));
  assert.equal(prepared.arguments.bringToFront, false);
  await f.clients.execute(descriptor('select_page'), prepared);
  assert.equal(f.requests[0].request.arguments.bringToFront, false);
  const front = await f.clients.prepareBrowserExecution(descriptor('select_page'), envelope({ pageId: 2 }), { background: false, allowForeground: true });
  assert.equal(front.arguments.bringToFront, true);
  await assert.rejects(f.clients.prepareBrowserExecution(descriptor('evaluate_script'), envelope({ function: '() => window.focus()' }), { allowForeground: false }),
    { code: 'BROWSER_FOREGROUND_FORBIDDEN' });
});

test('fresh snapshot references including iframe nodes are scoped to the exact tab and conversation', async () => {
  const f = fixture(request => request.name === 'take_snapshot' ? result('uid=5_0 RootWebArea "Fixture" url="https://example.invalid/"\nuid=5_1 iframe\n uid=5_2 button "Frame button"')
    : result('clicked'));
  await f.clients.execute(descriptor('take_snapshot'), envelope({ pageId: 5 }), undefined, { sessionId: 'one' });
  await assert.rejects(f.clients.execute(descriptor('click'), envelope({ pageId: 5, uid: '5_2' }), undefined, { sessionId: 'two' }),
    { code: 'BROWSER_STALE_REFERENCE' });
  await assert.rejects(f.clients.execute(descriptor('click'), envelope({ pageId: 6, uid: '5_2' }), undefined, { sessionId: 'one' }),
    { code: 'BROWSER_STALE_REFERENCE' });
  const clicked = await f.clients.execute(descriptor('click'), envelope({ uid: '5_2' }), undefined, { sessionId: 'one' });
  assert.equal(clicked.browser.tabId, '5'); assert.equal(f.requests.at(-1).request.arguments.pageId, 5);
  await assert.rejects(f.clients.execute(descriptor('click'), envelope({ pageId: 5, uid: '5_2' }), undefined, { sessionId: 'one' }),
    { code: 'BROWSER_STALE_REFERENCE' });
});

test('selected-tab APIs reject cross-conversation stale targets and visible Playwright cannot secretly focus tabs', () => {
  const registry = new BrowserSessionRegistry(), pwServer = { ...server, args: ['@playwright/mcp@0.0.83', '--extension'] };
  const tabs = descriptor('browser_tabs', { type: 'object', properties: { action: { type: 'string' }, index: { type: 'number' } } });
  registry.observe(registry.prepare(tabs, { action: 'list' }, pwServer, { sessionId: 'one' }),
    result('- 0: (current) [One](https://example.invalid/one)'), 'completed');
  assert.throws(() => registry.prepare(tabs, { action: 'select', index: 1 }, pwServer, { allowForeground: false }), { code: 'BROWSER_BACKGROUND_UNSUPPORTED' });
  const snapshot = descriptor('browser_snapshot', { type: 'object', properties: {} });
  registry.observe(registry.prepare(snapshot, {}, pwServer, { sessionId: 'two' }),
    result('- 1: (current) [Two](https://example.invalid/two)\n- button "Two" [ref=e1]'), 'completed');
  assert.throws(() => registry.prepare(snapshot, {}, pwServer, { sessionId: 'one' }), { code: 'BROWSER_TAB_CHANGED' });
});

test('effect timeout returns a paired unknown receipt, retains identity, and requires a fresh observation', async () => {
  let fail = true;
  const f = fixture(request => {
    if (fail && request.name === 'navigate_page') throw Object.assign(new Error('private endpoint must not propagate'), { code: 'REQUEST_TIMEOUT' });
    return result('uid=2_0 RootWebArea "Fixture" url="https://example.invalid/page"\n uid=2_1 button "Continue"');
  });
  const output = await f.clients.execute(descriptor('navigate_page'), envelope({ pageId: 2, url: 'https://user:secret@example.invalid/page?token=DO_NOT_PRINT&q=public' }),
    undefined, { sessionId: 'one' });
  assert.equal(output.status, 'unknown'); assert.equal(output.code, 'MCP_TIMEOUT'); assert.equal(output.browser.tabId, '2');
  assert.equal(output.canonical.structuredContent.status, 'unknown'); assert.doesNotMatch(JSON.stringify(output), /DO_NOT_PRINT|user:secret|private endpoint/);
  assert.equal(f.requests.length, 1);
  await assert.rejects(f.clients.execute(descriptor('navigate_page'), envelope({ pageId: 2, url: 'https://example.invalid/' }),
    undefined, { sessionId: 'one' }), { code: 'BROWSER_OBSERVATION_REQUIRED' });
  fail = false;
  await f.clients.execute(descriptor('take_snapshot'), envelope({ pageId: 2 }), undefined, { sessionId: 'one' });
  assert.equal(f.clients.browserDiagnostics('one')[0].snapshotGeneration > 0, true);
});

test('read-only timeout is an error; evaluate remains a possible effect irrespective of server hints', async () => {
  const f = fixture(() => { throw Object.assign(new Error('timeout'), { code: 'ETIMEDOUT' }); });
  const snapshot = await f.clients.execute(descriptor('take_snapshot'), envelope({ pageId: 2 }));
  assert.equal(snapshot.status, 'error'); assert.equal(snapshot.code, 'MCP_TIMEOUT'); assert.equal(snapshot.browser.readOnly, true);
  const script = await f.clients.execute(descriptor('evaluate_script'), envelope({ pageId: 2 }));
  assert.equal(script.status, 'unknown'); assert.equal(script.browser.readOnly, false);
});

test('caught navigation timeout text is not reported as a completed operation', async () => {
  const f = fixture(() => result('Unable to navigate in the selected page: Navigation timeout of 60000 ms exceeded.'));
  const output = await f.clients.execute(descriptor('navigate_page'), envelope({ pageId: 2 }));
  assert.equal(output.status, 'unknown'); assert.equal(output.code, 'MCP_TIMEOUT'); assert.equal(output.browser.outcome, 'unknown');
});

test('browser calls serialize their selected-page state and do not replay after cancellation', async () => {
  const order = []; let unblock;
  const first = new Promise(resolve => { unblock = resolve; });
  const f = fixture(async (request, options, index) => { order.push('start' + index); if (index === 1) await first; order.push('end' + index); return result(''); });
  const one = f.clients.execute(descriptor('list_pages'), envelope({}), undefined, { sessionId: 'one' });
  const controller = new AbortController();
  const two = f.clients.execute(descriptor('list_pages'), envelope({}), controller.signal, { sessionId: 'two' });
  await new Promise(resolve => setTimeout(resolve, 10)); assert.deepEqual(order, ['start1']);
  controller.abort(); unblock(); await one; await assert.rejects(two, { name: 'AbortError' });
  assert.deepEqual(order, ['start1', 'end1']);
});

for (const name of ['take_snapshot', 'navigate_page'])
  test(`queued ${name} cancellation finishes without waiting for another call and is never dispatched`, { timeout: 2000 }, async () => {
    let unblock;
    const blocked = new Promise(resolve => { unblock = resolve; });
    const f = fixture(async () => { await blocked; return result(''); });
    const occupying = f.clients.execute(descriptor('list_pages'), envelope({}));
    const controller = new AbortController();
    const cancelled = f.clients.execute(descriptor(name), envelope({ pageId: 1 }), controller.signal);
    await new Promise(resolve => setTimeout(resolve, 5)); controller.abort();
    try {
      await assert.rejects(cancelled, error => error.name === 'AbortError' && isMcpExecutionNotDispatched(error) &&
        error !== controller.signal.reason && error.cause === controller.signal.reason);
      assert.equal(isMcpExecutionNotDispatched(controller.signal.reason), false);
      assert.equal(f.requests.length, 1);
    } finally { unblock(); await occupying; await f.connection.browserQueue; }
    assert.equal(isMcpExecutionNotDispatched(controller.signal.reason), false);
  });

test('server-supplied not-dispatched fields and post-dispatch errors cannot forge the adapter marker', async () => {
  for (const error of [{ operationDispatched: false }, { executionNotDispatched: true }, new Error('ordinary')])
    assert.equal(isMcpExecutionNotDispatched(error), false);
  const f = fixture(() => { throw Object.assign(new Error('RPC failed after dispatch'), { code: 'ETIMEDOUT', operationDispatched: false }); });
  const response = await f.clients.execute(descriptor('navigate_page'), envelope({ pageId: 1 }));
  assert.equal(response.status, 'unknown'); assert.equal(isMcpExecutionNotDispatched(response), false);
  assert.equal(f.requests.length, 1);
});

test('late cancellation after dispatch retains a returned completion instead of racing away its receipt', async () => {
  const controller = new AbortController();
  const f = fixture(() => { controller.abort(); return result('2: Fixture (https://example.invalid/) [selected]'); });
  const response = await f.clients.execute(descriptor('new_page'), envelope({ url: 'https://example.invalid/' }), controller.signal);
  assert.equal(response.isError, false); assert.equal(response.browser.outcome, 'completed');
  assert.equal(response.browser.tabId, '2'); assert.equal(f.requests.length, 1);
});

test('an implicit page target is frozen before approval even when another conversation selects a tab', async () => {
  const f = fixture(request => result(`${request.arguments.pageId ?? 1}: https://example.invalid/ [selected]`));
  const implicit = descriptor('navigate_page', { type: 'object', properties: { url: properties.url, timeout: properties.timeout } });
  await f.clients.execute(descriptor('list_pages'), envelope({}), undefined, { sessionId: 'other' });
  const approved = await f.clients.prepareBrowserExecution(implicit, envelope({ url: 'https://example.invalid/approved' }), { sessionId: 'pending' });
  assert.equal(approved.arguments.pageId, undefined);
  await f.clients.execute(descriptor('select_page'), envelope({ pageId: 2 }), undefined, { sessionId: 'other' });
  const count = f.requests.length;
  await assert.rejects(f.clients.execute(implicit, approved, undefined, { sessionId: 'pending' }), { code: 'BROWSER_TAB_CHANGED' });
  assert.equal(f.requests.length, count);
});

test('approval cannot acquire an unknown target later or attach to a replaced connection', async () => {
  const f = fixture(() => result('1: https://example.invalid/ [selected]'));
  const implicit = descriptor('take_snapshot', { type: 'object', properties: {} });
  const unbound = await f.clients.prepareBrowserExecution(implicit, envelope({}), { sessionId: 'pending' });
  await f.clients.execute(descriptor('list_pages'), envelope({}), undefined, { sessionId: 'other' });
  await assert.rejects(f.clients.execute(implicit, unbound, undefined, { sessionId: 'pending' }), { code: 'BROWSER_TAB_CHANGED' });
  const approved = await f.clients.prepareBrowserExecution(descriptor('new_page'), envelope({ url: 'https://example.invalid/' }), { sessionId: 'pending' });
  f.clients.connections.set('browser-connection', Promise.resolve({ ...f.connection }));
  await assert.rejects(f.clients.execute(descriptor('new_page'), approved, undefined, { sessionId: 'pending' }), { code: 'BROWSER_CONNECTION_CHANGED' });
  assert.equal(f.requests.length, 1);
});

test('approval snapshot references cannot silently acquire a later snapshot generation', async () => {
  const f = fixture(() => result('uid=4_1 button "Fixture"'));
  await f.clients.execute(descriptor('take_snapshot'), envelope({ pageId: 4 }), undefined, { sessionId: 'one' });
  const approved = await f.clients.prepareBrowserExecution(descriptor('click'), envelope({ pageId: 4, uid: '4_1' }), { sessionId: 'one' });
  await f.clients.execute(descriptor('take_snapshot'), envelope({ pageId: 4 }), undefined, { sessionId: 'one' });
  await assert.rejects(f.clients.execute(descriptor('click'), approved, undefined, { sessionId: 'one' }), { code: 'BROWSER_STALE_REFERENCE' });
  assert.equal(f.requests.length, 2);
});

test('catalog and connection invalidation remove old references and captured diagnostics stay immutable', () => {
  const registry = new BrowserSessionRegistry(), snap = descriptor('take_snapshot'), click = descriptor('click');
  registry.observe(registry.prepare(snap, { pageId: 4 }, server), result('uid=4_0 RootWebArea "Fixture" url="https://example.invalid/"'), 'completed');
  const diagnostics = registry.diagnostics(); diagnostics[0].url = 'tampered';
  assert.notEqual(registry.diagnostics()[0].url, 'tampered');
  registry.invalidate('browser-connection');
  assert.throws(() => registry.prepare(click, { pageId: 4, uid: '4_0' }, server), { code: 'BROWSER_STALE_REFERENCE' });
  registry.remove('browser-connection'); assert.deepEqual(registry.diagnostics(), []);
});

test('closing a tab releases its implicit target instead of injecting a closed page into the next read', () => {
  const registry = new BrowserSessionRegistry();
  registry.observe(registry.prepare(descriptor('take_snapshot'), { pageId: 4 }, server, { sessionId: 'one' }),
    result('uid=4_0 RootWebArea "Fixture" url="https://example.invalid/"'), 'completed');
  registry.observe(registry.prepare(descriptor('close_page'), { pageId: 4 }, server, { sessionId: 'one' }), result('closed'), 'completed');
  assert.equal(registry.prepare(descriptor('take_snapshot'), {}, server, { sessionId: 'one' }).args.pageId, undefined);
});

import assert from 'node:assert/strict';
import { test } from 'node:test';
import { runToolLoop } from '../orchestration/tool-loop.mjs';
import { ToolReadFailureGuard } from '../tools/tool-observations.mjs';
import { toolFixture, pendingApproval, approve } from './tool-fixture.mjs';

const target = { windowId: '98765', processId: 12345, reason: 'Read the synthetic page.' };
function turn(protocol, calls = [], content = '') {
  const continuation = protocol === 'anthropic-messages'
    ? [{ role: 'assistant', content: calls.map(call => ({ type: 'tool_use', id: call.id, name: call.name, input: call.arguments })) }]
    : protocol === 'openai-responses'
      ? calls.map(call => ({ type: 'function_call', call_id: call.id, name: call.name, arguments: JSON.stringify(call.arguments) }))
      : [{ role: 'assistant', content, tool_calls: calls.map(call => ({ id: call.id, type: 'function',
        function: { name: call.name, arguments: JSON.stringify(call.arguments) } })) }];
  return { content, reasoning: '', calls, continuation };
}
function toolObservations(protocol, messages) {
  if (protocol === 'anthropic-messages') return messages.flatMap(message => Array.isArray(message.content) ? message.content : [])
    .filter(block => block.type === 'tool_result').map(block => ({ id: block.tool_use_id, content: block.content }));
  return messages.filter(message => message.role === 'tool' || message.type === 'function_call_output')
    .map(message => ({ id: message.tool_call_id ?? message.call_id, content: message.content ?? message.output }));
}

test('native read timeout is archived as failed and can continue with a different observation', async t => {
  const runner = { capabilities: async () => ({ available: true, boundary: 'host-desktop', operations: ['read', 'windows'] }),
    run: async action => {
      if (action === 'read') throw Object.assign(new Error('Synthetic UIA timeout; use the page DOM.'), { code: 'DESKTOP_TIMEOUT' });
      return { value: { windows: [{ windowId: target.windowId, processId: target.processId, isResponding: true }] }, isError: false };
    } };
  const f = await toolFixture(t, { desktopRunner: runner }), context = await f.context('full');
  const first = f.call('computer.read', target), next = f.call('computer.windows', { reason: target.reason });
  const saved = [], histories = [];
  let round = 0;
  const result = await runToolLoop({ protocol: 'openai-completions', context, messages: [], system: '', declarations: [],
    inputBudgetTokens: 32000, service: f.service, emit: () => {}, saveActivity: async item => saved.push(item),
    requestTurn: async messages => { histories.push(messages); round++; return round === 1 ? turn('openai-completions', [first])
      : round === 2 ? turn('openai-completions', [next]) : turn('openai-completions', [], 'Verified alternative observation.'); } });
  assert.equal(round, 3); assert.equal(result.content, 'Verified alternative observation.');
  const failure = saved.find(item => item.toolCallId === first.id && item.status === 'error');
  assert.equal(failure.code, 'DESKTOP_TIMEOUT'); assert.ok(failure.resultRef);
  const archived = await f.service.results.get(context, failure.resultRef.id);
  assert.equal(archived.structuredContent.outcome, 'failed');
  assert.equal(archived.structuredContent.error.code, 'DESKTOP_TIMEOUT');
  const pairs = toolObservations('openai-completions', histories[2]);
  assert.deepEqual(pairs.map(pair => pair.id), [first.id, next.id]);
  assert.match(pairs[0].content, /DESKTOP_TIMEOUT/);
  assert.equal(saved.at(-1).status, 'completed');
});

for (const protocol of ['openai-completions', 'openai-responses', 'anthropic-messages'])
  test(`${protocol}: failed read history stays paired and repeated failures reach a bounded final response`, async () => {
    const saved = [], history = [], declarations = []; let round = 0;
    const result = await runToolLoop({ protocol, context: {}, messages: [], system: '', inputBudgetTokens: 32000,
      declarations: [{ type: 'function', name: 'synthetic' }], emit: () => {}, saveActivity: async item => saved.push(item),
      service: { execute: async () => ({ content: 'DESKTOP_TIMEOUT: synthetic page could not be read.', isError: true,
        status: 'unknown', code: 'DESKTOP_TIMEOUT' }) },
      requestTurn: async (messages, tools) => {
        history.push(messages); declarations.push(tools); round++;
        return round <= 3 ? turn(protocol, [{ id: `read-${round}`, name: 'computer.read',
          arguments: { ...target, maxElements: 100 - round, reason: `Synthetic attempt ${round}` } }])
          : turn(protocol, [], 'The page is unresponsive; verified history is preserved.');
      } });
    assert.equal(round, 4); assert.equal(result.content, 'The page is unresponsive; verified history is preserved.');
    assert.equal(declarations.at(-1).length, 0);
    assert.match(history.at(-1).at(-1).content, /KYNXA_READ_FAILURE_FINAL/);
    assert.deepEqual(toolObservations(protocol, history.at(-1)).map(pair => pair.id), ['read-1', 'read-2', 'read-3']);
    assert.deepEqual(saved.filter(item => item.status !== 'running').map(item => item.status), ['error', 'error', 'error']);
  });

test('unknown MCP effects are recorded and stop before another model turn or effect', async () => {
  const saved = [], phases = []; let rounds = 0, executions = 0;
  await assert.rejects(runToolLoop({ protocol: 'openai-completions', context: {}, messages: [], system: '', declarations: [],
    inputBudgetTokens: 32000, emit: () => {}, saveActivity: async item => saved.push(item), saveRunState: async state => phases.push(state),
    service: { execute: async () => { executions++; return { content: 'Navigation outcome unknown.', isError: true,
      status: 'unknown', code: 'MCP_TIMEOUT', resultRef: { id: 'synthetic-receipt' } }; } },
    requestTurn: async () => { rounds++; return turn('openai-completions', [{ id: 'navigation-1', name: 'mcp.browser.navigate_page',
      arguments: { arguments: { url: 'https://example.invalid' }, policy: { reason: 'Open a synthetic page.' } } }]); } }),
  { code: 'MCP_OUTCOME_UNKNOWN' });
  assert.equal(rounds, 1); assert.equal(executions, 1);
  assert.equal(saved.at(-1).status, 'unknown'); assert.ok(saved.at(-1).resultRef);
  assert.equal(phases.at(-1).phase, 'interrupted'); assert.equal(phases.at(-1).code, 'MCP_OUTCOME_UNKNOWN');
});

test('failure guard resets after a changed target or a successful observation and never reclassifies effects', () => {
  const guard = new ToolReadFailureGuard();
  const pair = (processId, extra = {}) => ({ call: { name: 'computer.read', arguments: { ...target, processId } },
    result: { isError: true, code: 'DESKTOP_NOT_RESPONDING', ...extra } });
  assert.equal(guard.observeRound([pair(1)]).warning, false);
  assert.equal(guard.observeRound([pair(1)]).warning, true);
  assert.equal(guard.observeRound([pair(2)]).warning, false);
  assert.equal(guard.observeRound([pair(2, { isError: false, status: 'completed' })]).finalize, false);
  assert.equal(guard.observeRound([pair(2)]).warning, false);
  assert.equal(guard.observeRound([{ call: { name: 'computer.type', arguments: target },
    result: { isError: true, status: 'unknown', code: 'DESKTOP_TIMEOUT' } }]).repeated, false);
});

test('cancelling a read stays cancelled, persists its receipt and never advances the model', async t => {
  const controller = new AbortController();
  const runner = { capabilities: async () => ({ available: true, boundary: 'host-desktop', operations: ['read'] }),
    run: async () => { controller.abort(); throw Object.assign(new Error('Synthetic read cancelled.'), { name: 'AbortError' }); } };
  const f = await toolFixture(t, { desktopRunner: runner }), context = await f.context('full');
  let rounds = 0; const saved = [];
  await assert.rejects(runToolLoop({ protocol: 'openai-completions', context, messages: [], system: '', declarations: [],
    inputBudgetTokens: 32000, service: f.service, signal: controller.signal, emit: () => {}, saveActivity: async item => saved.push(item),
    requestTurn: async () => { rounds++; return turn('openai-completions', [f.call('computer.read', target)]); } }),
  { name: 'AbortError' });
  assert.equal(rounds, 1); assert.equal(saved.at(-1).status, 'cancelled'); assert.ok(saved.at(-1).resultRef);
});

async function browserBrokerFixture(t) {
  const f = await toolFixture(t), requests = [];
  const server = { id: 'fixture-browser', command: 'npx', args: ['chrome-devtools-mcp@1.10.1', '--headless', '--isolated'] };
  const key = 'synthetic-browser-connection';
  const schema = { type: 'object', properties: { pageId: { type: 'integer' }, url: { type: 'string' },
    timeout: { type: 'integer' }, background: { type: 'boolean' } } };
  const descriptor = toolName => ({ name: `mcp.${server.id}.${toolName}`, source: `mcp:${server.id}`,
    serverId: server.id, key, operation: 'tools/call', toolName, originalInputSchema: schema });
  const connection = { closed: false, artifactContext: { server }, client: { close: async () => {},
    callTool: async request => { requests.push(request); return { content: [{ type: 'text',
      text: '1: Synthetic page (https://example.invalid/) [selected]' }] }; } } };
  f.service.mcp.connections.set(key, Promise.resolve(connection));
  f.service.mcp.validateExecution = async (_, input) => ({ connection, args: structuredClone(input.arguments) });
  const context = async (permissionMode = 'ask') => {
    const ctx = await f.context(permissionMode);
    f.service.catalogs.set(ctx, { generation: f.service.configGeneration,
      descriptors: new Map(['new_page', 'take_snapshot'].map(name => { const item = descriptor(name); return [item.name, item]; })) });
    return ctx;
  };
  return { ...f, requests, context };
}

test('browser approval includes normalized background parameters and approved identity cannot mutate', async t => {
  const f = await browserBrokerFixture(t), context = await f.context();
  const call = f.call('mcp.fixture-browser.new_page', { arguments: { url: 'https://example.invalid/' }, policy: { reason: 'Open only the fixture.' } });
  const pending = await pendingApproval(f.service, context, call);
  assert.equal(f.requests.length, 0);
  assert.equal(pending.event.tool.arguments.arguments.background, true);
  assert.equal(pending.event.tool.arguments.arguments.timeout, 60000);
  call.arguments.arguments.url = 'https://changed.invalid'; pending.event.tool.arguments.arguments.background = false;
  approve(f.service, context, pending.event.tool);
  const completed = await pending.result;
  assert.equal(completed.status, 'completed'); assert.ok(completed.resultRef);
  assert.equal(f.requests.length, 1); assert.equal(f.requests[0].arguments.background, true);
  assert.equal(f.requests[0].arguments.url, 'https://example.invalid/');
  const archive = await f.service.results.get(context, completed.resultRef.id);
  assert.equal(completed.browser.sessionId, context.conversationId);
  assert.match(archive.content[0].text, /Synthetic page/);
});

test('browser read timeout keeps its typed archive and reaches a later model response', async t => {
  const f = await browserBrokerFixture(t), context = await f.context('full'), saved = []; let round = 0;
  const connection = await f.service.mcp.connections.get('synthetic-browser-connection');
  connection.client.callTool = async () => { throw Object.assign(new Error('Fixture read timeout.'), { code: 'ETIMEDOUT' }); };
  const call = f.call('mcp.fixture-browser.take_snapshot', { arguments: { pageId: 1 }, policy: { reason: 'Read only the fixture.' } });
  const result = await runToolLoop({ protocol: 'openai-completions', context, messages: [], system: '', declarations: [],
    inputBudgetTokens: 32000, service: f.service, emit: () => {}, saveActivity: async activity => saved.push(activity),
    requestTurn: async messages => { round++; if (round === 1) return turn('openai-completions', [call]);
      assert.match(toolObservations('openai-completions', messages)[0].content, /MCP_TIMEOUT/);
      return turn('openai-completions', [], 'The page could not be read.'); } });
  assert.equal(round, 2); assert.equal(result.content, 'The page could not be read.');
  assert.equal(saved.at(-1).status, 'error'); assert.equal(saved.at(-1).code, 'MCP_TIMEOUT');
  const archive = await f.service.results.get(context, saved.at(-1).resultRef.id);
  assert.equal(archive.structuredContent.browser.readOnly, true); assert.equal(archive.structuredContent.status, 'error');
});

test('approving an implicit-page action after another chat selects a tab never acts on the new target', async t => {
  const f = await browserBrokerFixture(t), context = await f.context(), requests = f.requests;
  const connection = await f.service.mcp.connections.get('synthetic-browser-connection');
  let selected = 1;
  connection.client.callTool = async request => { requests.push(request); return { content: [{ type: 'text',
    text: `${selected}: Synthetic page (https://example.invalid/${selected}) [selected]` }] }; };
  const base = { source: 'mcp:fixture-browser', serverId: 'fixture-browser', key: 'synthetic-browser-connection', operation: 'tools/call' };
  const descriptor = (toolName, properties = {}) => ({ ...base, toolName, name: `mcp.fixture-browser.${toolName}`,
    originalInputSchema: { type: 'object', properties } });
  const envelope = arguments_ => ({ arguments: arguments_, policy: { reason: 'Only the fixture browser.' } });
  await f.service.mcp.execute(descriptor('list_pages'), envelope({}), undefined, { sessionId: 'other-chat' });
  const navigate = descriptor('navigate_page', { url: { type: 'string' }, timeout: { type: 'integer' } });
  f.service.catalogs.get(context).descriptors.set(navigate.name, navigate);
  const pending = await pendingApproval(f.service, context, f.call(navigate.name, envelope({ url: 'https://example.invalid/destination' })));
  assert.equal(requests.length, 1);
  selected = 2;
  await f.service.mcp.execute(descriptor('select_page', { pageId: { type: 'integer' }, bringToFront: { type: 'boolean' } }),
    envelope({ pageId: 2 }), undefined, { sessionId: 'other-chat' });
  approve(f.service, context, pending.event.tool);
  const rejected = await pending.result;
  assert.equal(rejected.code, 'BROWSER_TAB_CHANGED'); assert.equal(rejected.status, 'error');
  assert.equal(requests.length, 2, 'no navigation RPC was issued after the approved target changed');
});

for (const toolName of ['take_snapshot', 'new_page'])
  test(`${toolName}: cancelling while queued is archived as cancelled without dispatch or an unknown effect`, async t => {
    const f = await browserBrokerFixture(t), context = await f.context('full'), controller = new AbortController();
    const connection = await f.service.mcp.connections.get('synthetic-browser-connection');
    let release;
    const waiting = new Promise(resolve => { release = resolve; });
    connection.browserQueue = waiting;
    const call = f.call(`mcp.fixture-browser.${toolName}`, { arguments: toolName === 'new_page'
      ? { url: 'https://example.invalid/' } : { pageId: 1 }, policy: { reason: 'Inspect only this synthetic page.' } });
    const pending = f.service.execute(context, call, { signal: controller.signal });
    for (let i = 0; connection.browserQueue === waiting && i < 200; i++) await new Promise(resolve => setTimeout(resolve, 5));
    assert.notEqual(connection.browserQueue, waiting, 'the actual broker call is waiting behind another browser operation');
    controller.abort();
    let deadline, cancelled;
    try { cancelled = await Promise.race([pending, new Promise((_, reject) => {
      deadline = setTimeout(() => reject(new Error('A queued cancellation waited for the occupied browser.')), 2000);
    })]); }
    finally { clearTimeout(deadline); release(); }
    assert.equal(f.requests.length, 0); assert.equal(cancelled.status, 'cancelled'); assert.equal(cancelled.code, 'TOOL_CANCELLED');
    assert.ok(cancelled.resultRef);
    const archived = await f.service.results.get(context, cancelled.resultRef.id);
    assert.equal(archived.structuredContent.outcome, 'cancelled');
  });

test('unclassified third-party MCP timeout or disconnect cannot turn an uncertain effect into a retryable error', async t => {
  const f = await toolFixture(t), context = await f.context('full');
  const name = 'mcp.synthetic.apply_change', descriptor = { name, source: 'mcp:synthetic',
    serverId: 'synthetic', key: 'synthetic-change', toolName: 'apply_change', operation: 'tools/call', originalInputSchema: { type: 'object' } };
  f.service.catalogs.set(context, { generation: f.service.configGeneration, descriptors: new Map([[name, descriptor]]) });
  for (const code of ['MCP_TIMEOUT', 'MCP_CONNECTION_LOST']) {
    f.service.mcp.execute = async () => { throw Object.assign(new Error('Synthetic external outcome unavailable.'), { code }); };
    const uncertain = await f.run(context, name, { arguments: {}, policy: { reason: 'Only a fixture change.' } });
    assert.equal(uncertain.status, 'unknown'); assert.equal(uncertain.code, code); assert.ok(uncertain.resultRef);
    const archive = await f.service.results.get(context, uncertain.resultRef.id);
    assert.equal(archive.structuredContent.outcome, 'unknown');
  }
});

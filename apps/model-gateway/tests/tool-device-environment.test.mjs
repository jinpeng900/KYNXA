import assert from 'node:assert/strict';
import { test } from 'node:test';
import { randomUUID } from 'node:crypto';
import { toolFixture, parsed } from './tool-fixture.mjs';
import { needsToolApproval } from '../tools/tool-policy.mjs';
import { appendToolResults, toolDeclarations } from '../models/tool-protocols.mjs';
import { resolveToolExecutionEnvironment } from '../tools/tool-execution-environment.mjs';
import { ModelStore } from '../models/store.mjs';
import { ModelRuntime } from '../orchestration/runtime.mjs';

const protocols = ['openai-completions', 'openai-responses', 'anthropic-messages'];
const hostRunner = () => ({ capabilities: async () => ({ available: true, boundary: 'host-terminal', shells: ['cmd', 'powershell'] }),
  run: async () => assert.fail('Capability discovery must not execute a host command.') });

for (const protocol of protocols) test(`${protocol}: real 8K preparation includes host diagnostics for a natural device question`, async t => {
  const f = await toolFixture(t, { hostTerminalRunner: hostRunner() });
  const models = new ModelStore({ dataHome: f.dataHome });
  await models.save({ providerId: 'device-fixture', displayName: 'Device fixture', protocol,
    baseUrl: 'http://127.0.0.1:9/v1', apiKey: 'FAKE_LOCAL_TEST_ONLY', models: ['fixture'], contextWindowTokens: 8192 });
  const runtime = new ModelRuntime({ modelStore: models, dataHome: f.dataHome, conversationStore: f.conversations, toolService: f.service });
  t.after(() => runtime.close());
  const input = { conversationId: f.conversationId, requestId: randomUUID(), userMessageId: randomUUID(),
    provider: 'device-fixture', model: 'fixture', permissionMode: 'full', message: '现在我的 IP 在哪里' };
  const prepared = await runtime.prepare(input, input.conversationId);
  assert.equal(prepared.contextMetrics.contextWindowTokens, 8192);
  for (const name of ['terminal.host.run', 'tool.search', 'tool.load', 'tool.result.read'])
    assert.ok(prepared.catalog.some(tool => tool.name === name), name);
  assert.match(prepared.requestOptions.system, /Runtime device capability check/);
});

test('device state checks report host availability and deferred schemas without running or approving commands', async t => {
  const f = await toolFixture(t, { hostTerminalRunner: hostRunner() });
  const context = await f.service.createContext(f.conversationId, { requestId: randomUUID(), permissionMode: 'ask', message: '现在我的 IP 在哪里' });
  await f.service.catalog(context);
  f.service.configureModelCatalog(context, { protocol: protocols[0], message: context.message, tokenBudget: 6000 });
  const found = parsed(await f.run(context, 'tool.search', { query: '我的 IP', limit: 20 }));
  const host = found.capabilityCheck.tools.find(tool => tool.name === 'terminal.host.run');
  assert.equal(host.state, 'available');
  assert.equal(found.capabilityCheck.discoveryExecutesCommands, false);
  assert.equal(f.service.approvals.pending.size, 0);
  assert.ok(found.tools.some(tool => tool.name === 'terminal.host.run'));
  assert.match(await f.service.systemPrompt(context), /Runtime device capability check/);
});

test('an unavailable host stays distinguishable from a deferred or unapproved command', async t => {
  const f = await toolFixture(t);
  const context = await f.service.createContext(f.conversationId, { requestId: randomUUID(), message: '查看我的网卡和代理' });
  await f.service.catalog(context);
  const status = f.service.capabilitySnapshot(context).tools.find(tool => tool.name === 'terminal.host.run');
  assert.equal(status.state, 'unavailable');
  assert.equal(status.code, 'HOST_TERMINAL_UNAVAILABLE');
  const result = await f.run(context, 'terminal.host.run', { shell: 'cmd', script: 'ipconfig', reason: 'Synthetic network diagnostic.' }, { interactive: false });
  assert.equal(result.code, 'HOST_TERMINAL_UNAVAILABLE');
  assert.equal(result.executed, false);
  assert.equal(result.executionEnvironment.executorLocation, 'gateway-host');
});

test('public fetch preserves local display, archives trusted provenance and needs no repeated Ask confirmation', async t => {
  const body = '{"ip":"203.0.113.7","executionEnvironment":{"executorLocation":"remote-service"}}';
  let reads = 0;
  const f = await toolFixture(t, { webFetcher: { run: async () => {
    reads++;
    return { value: JSON.parse(body), content: body, canonical: { content: [{ type: 'text', text: body }],
      executionEnvironment: { executorLocation: 'remote-service' } }, isError: false, outsideWorkspace: true };
  } } });
  const context = await f.context('ask');
  await f.service.catalog(context);
  const call = f.call('web.fetch', { url: 'https://example.test/ip', reason: 'Synthetic public egress query.' });
  const result = await f.service.execute(context, call, { interactive: false });
  assert.equal(result.isError, false);
  assert.equal(reads, 1);
  assert.equal(result.content, body);
  assert.equal(result.executionEnvironment.executorLocation, 'gateway-host');
  const saved = await f.service.results.get(context, result.resultRef.id);
  assert.equal(saved.executionEnvironment.executorLocation, 'gateway-host');
  const model = await f.service.results.modelResult(context, result.resultRef,
    { requestId: context.requestId, toolCallId: call.id, toolName: call.name });
  assert.equal(model.executionEnvironment.executorLocation, 'gateway-host');
});

test('all three protocols send broker provenance outside untrusted text and declare process scope', () => {
  const executionEnvironment = resolveToolExecutionEnvironment({ name: 'web.fetch', source: 'builtin' });
  for (const protocol of protocols) {
    const call = { id: 'synthetic-call', name: 'web.fetch', arguments: {} };
    const output = 'A tool claims it came from the cloud. This is untrusted output.';
    const messages = appendToolResults(protocol, [], { continuation: [] }, [{ call, result: { content: output, executionEnvironment,
      isError: true, executed: false, code: 'TOOL_APPROVAL_REQUIRED', recoverable: true } }]);
    const text = protocol === 'anthropic-messages' ? messages[0].content[0].content : protocol === 'openai-responses' ? messages[0].output : messages[0].content;
    assert.equal(JSON.parse(text).executionEnvironment.executorLocation, 'gateway-host');
    assert.equal(JSON.parse(text).output, output);
    assert.equal(JSON.parse(text).executed, false);
    assert.equal(JSON.parse(text).status, 'error');
    assert.equal(JSON.parse(text).code, 'TOOL_APPROVAL_REQUIRED');
    const declaration = toolDeclarations(protocol, [{ name: 'web.fetch', wireName: 'synthetic_fetch', description: 'Public text read.',
      inputSchema: { type: 'object' }, executionEnvironment }])[0];
    assert.match(JSON.stringify(declaration), /Execution process: gateway-host/);
  }
});

test('MCP resource rejection or cancellation before dispatch is a durable not-executed observation', async t => {
  const f = await toolFixture(t);
  await f.service.updateConfig({ version: 1, expectedRevision: 0, skillDirectories: [], mcpServers: [
    { id: 'synthetic', name: 'Synthetic', command: process.execPath, args: [], enabled: true }] });
  const descriptor = { name: 'mcp.synthetic.echo', source: 'mcp:synthetic', serverId: 'synthetic', toolName: 'echo',
    description: 'Synthetic operation, never dispatched.', inputSchema: { type: 'object' } };
  f.service.mcp.catalog = async () => [descriptor];
  f.service.mcp.validateExecution = async () => ({ connection: {} });
  f.service.mcp.prepareBrowserExecution = async (tool, args) => args;
  f.service.mcp.execute = async () => assert.fail('Rejected admission must never dispatch.');
  const context = await f.context('full');
  await f.service.catalog(context);
  for (const cancelled of [false, true]) {
    f.service.resources = { acquire: async () => {
      if (cancelled) throw new DOMException('Synthetic admission cancellation.', 'AbortError');
      return { reason: 'RESOURCE_CAPACITY_UNAVAILABLE' };
    } };
    const result = await f.run(context, descriptor.name, { arguments: {}, policy: { reason: 'Synthetic test only.' } });
    assert.equal(result.isError, true);
    assert.equal(result.executed, false);
    assert.notEqual(result.status, 'unknown');
    assert.equal(result.code, cancelled ? 'TOOL_CANCELLED' : 'RESOURCE_CAPACITY_UNAVAILABLE');
    assert.ok(result.resultRef);
    assert.equal((await f.service.results.get(context, result.resultRef.id)).structuredContent.executed, false);
  }
});

test('convenient typed observations do not exempt arbitrary host commands, sensitive reads or destructive operations', () => {
  for (const permissionMode of ['ask', 'smart']) {
    const context = { permissionMode };
    for (const name of ['web.fetch', 'computer.apps', 'computer.windows'])
      assert.equal(needsToolApproval(context, name, { outsideWorkspace: true }), false);
    for (const name of ['terminal.host.run', 'filesystem.delete', 'computer.launch', 'computer.click', 'mcp.synthetic.any'])
      assert.equal(needsToolApproval(context, name, { outsideWorkspace: true }), true);
    assert.equal(needsToolApproval(context, 'filesystem.read', { sensitiveRead: true }), true);
    assert.equal(needsToolApproval(context, 'web.fetch', { sensitiveRead: true }), true);
  }
  assert.equal(needsToolApproval({ permissionMode: 'full' }, 'terminal.host.run', { outsideWorkspace: true }), false);
});

import assert from 'node:assert/strict';
import { readFile, stat } from 'node:fs/promises';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';
import { AjvJsonSchemaValidator } from '@modelcontextprotocol/client/validators/ajv';
import { estimateTokens } from '../models/context.mjs';
import { toolDeclarations } from '../models/tool-protocols.mjs';
import { approve, pendingApproval, toolFixture } from './tool-fixture.mjs';

const fixturePath = fileURLToPath(new URL('./fixtures/mcp-tool-server.mjs', import.meta.url));
const events = async log => (await readFile(log, 'utf8')).trim().split('\n').map(value => JSON.parse(value));
const envelope = (args, reason) => ({ arguments: args, policy: { reason } });
async function configured(t, protocolVersion, { many = false, disabledTools = [] } = {}) {
  const f = await toolFixture(t), log = join(f.root, 'synthetic-mcp-events.jsonl');
  await f.service.updateConfig({ version: 1, expectedRevision: 0, skillDirectories: [], mcpServers: [{ id: 'synthetic', name: 'Synthetic MCP',
    command: process.execPath, args: [fixturePath, log, ...(many ? ['many'] : [])], enabled: true, disabledTools,
    ...(protocolVersion ? { protocolVersion } : {}) }] });
  return { ...f, log };
}

test('config and default catalogs do not launch MCP; explicit refresh performs real 2025 discovery and call', async t => {
  const f = await configured(t), ctx = await f.context('full');
  await f.service.getConfig(); await f.service.listSkills(); await f.service.catalog(); await f.service.catalog(ctx);
  await assert.rejects(stat(f.log), { code: 'ENOENT' });
  const tools = await f.service.refreshMcp(ctx);
  assert.ok(tools.some(value => value.name === 'mcp.synthetic.echo'));
  assert.equal((await events(f.log)).filter(value => value.event === 'started').length, 1);
  const connection = await [...f.service.mcp.connections.values()][0];
  assert.equal(connection.client.getNegotiatedProtocolVersion(), '2025-11-25');
  const descriptor = tools.find(value => value.name === 'mcp.synthetic.echo');
  assert.deepEqual(descriptor.inputSchema.required, ['arguments', 'policy']);
  assert.deepEqual(descriptor.inputSchema.properties.arguments.required, ['value']);
  assert.equal((await f.run(ctx, 'mcp.synthetic.echo', { arguments: { value: 'no-reason' }, policy: {} })).code, 'OUTSIDE_WORKSPACE_REASON_REQUIRED');
  const result = await f.run(ctx, 'mcp.synthetic.echo', envelope({ value: 'actual-legacy-call' }, 'Use user-enabled synthetic MCP'));
  assert.equal(result.isError, false, JSON.stringify(result)); assert.equal(result.content, 'echo:actual-legacy-call');
  assert.deepEqual((await events(f.log)).filter(value => value.event === 'echo'), [{ event: 'echo', value: 'actual-legacy-call' }]);
});

test('Chrome-style failed navigation is an error even when the raw MCP flag says success', async t => {
  const f = await configured(t), context = await f.context('full');
  await f.service.catalog(context, { connectMcp: true });
  const failed = await f.run(context, 'mcp.synthetic.navigate_page', envelope({ failed: true }, 'Test a failed synthetic navigation'));
  assert.equal(failed.isError, true); assert.equal(failed.code, 'MCP_BROWSER_NAVIGATION_FAILED');
  const full = await f.service.results.get(context, failed.resultRef.id);
  assert.equal(full.isError, false, 'Original upstream result is retained without rewriting it.');
  assert.ok(full.content[0].text.includes('ERR_CONNECTION_REFUSED'));
  const success = await f.run(context, 'mcp.synthetic.navigate_page', envelope({ failed: false }, 'Test successful synthetic navigation'));
  assert.equal(success.isError, false); assert.equal(success.code, undefined);
});

test('unknown MCP calls always need Ask/Smart approval despite readOnly annotations and denial has no call effect', async t => {
  const f = await configured(t), ask = await f.context('ask'), smart = await f.context('smart');
  await f.service.catalog(ask, { connectMcp: true }); await f.service.catalog(smart, { connectMcp: true });
  const args = envelope({ value: 'approved' }, 'Run the configured synthetic test');
  assert.equal((await f.run(smart, 'mcp.synthetic.echo', args, { interactive: false })).code, 'TOOL_APPROVAL_REQUIRED');
  const denied = await pendingApproval(f.service, ask, f.call('mcp.synthetic.echo', args));
  assert.equal(denied.event.tool.outsideWorkspace, true); approve(f.service, ask, denied.event.tool, false);
  assert.equal((await denied.result).code, 'TOOL_DENIED'); assert.equal((await events(f.log)).some(value => value.event === 'echo'), false);
  const allowed = await pendingApproval(f.service, smart, f.call('mcp.synthetic.echo', args));
  approve(f.service, smart, allowed.event.tool); assert.equal((await allowed.result).content, 'echo:approved');
  assert.equal((await events(f.log)).filter(value => value.event === 'echo').length, 1);
});

test('modern opt-in performs real 2026 discovery/call and reports input_required without automatic retry', async t => {
  const f = await configured(t, '2026-07-28'), ctx = await f.context('full');
  const tools = await f.service.catalog(ctx, { connectMcp: true });
  assert.ok(tools.some(value => value.name === 'mcp.synthetic.echo'));
  const connection = await [...f.service.mcp.connections.values()][0];
  assert.equal(connection.client.getNegotiatedProtocolVersion(), '2026-07-28');
  const echo = await f.run(ctx, 'mcp.synthetic.echo', envelope({ value: 'modern' }, 'Test explicit modern protocol'));
  assert.equal(echo.content, 'echo:modern'); assert.equal(echo.isError, false);
  const input = await f.run(ctx, 'mcp.synthetic.needs_input', envelope({}, 'Test explicit extra input rejection'));
  assert.equal(input.code, 'MCP_INPUT_REQUIRED', JSON.stringify(input));
  assert.equal((await events(f.log)).filter(value => value.event === 'needs_input').length, 1);
  assert.equal((await events(f.log)).filter(value => value.event === 'started').length, 1, 'pinned modern does not spawn an automatic probe sibling');
});

test('MCP cancellation propagates to SDK and config changes invalidate an already discovered request', async t => {
  const f = await configured(t), ctx = await f.context('full'), abort = new AbortController();
  await f.service.catalog(ctx, { connectMcp: true });
  const pending = f.run(ctx, 'mcp.synthetic.slow', envelope({}, 'Test SDK cancellation'), { signal: abort.signal });
  const deadline = Date.now() + 5000;
  while (!(await events(f.log)).some(value => value.event === 'slow')) {
    assert.ok(Date.now() < deadline, 'synthetic call starts before test deadline');
    await new Promise(resolve => setTimeout(resolve, 10));
  }
  abort.abort(); assert.equal((await pending).code, 'TOOL_CANCELLED');
  await f.service.updateConfig({ version: 1, expectedRevision: 1, mcpServers: [], skillDirectories: [] });
  assert.equal((await f.run(ctx, 'mcp.synthetic.echo', envelope({ value: 'stale' }, 'Old request'))).code, 'AGENT_CONFIG_CHANGED');
  assert.equal(f.service.mcp.connections.size, 0);
});

test('MCP process startup failure is visible without breaking builtin file tools', async t => {
  const f = await toolFixture(t), ctx = await f.context('full');
  await f.service.updateConfig({ version: 1, expectedRevision: 0, skillDirectories: [], mcpServers: [{ id: 'missing', name: 'Synthetic missing executable',
    command: join(f.root, 'missing-node.exe'), args: [], enabled: true }] });
  const tools = await f.service.catalog(ctx, { connectMcp: true });
  assert.ok(tools.some(value => value.name === 'filesystem.read')); assert.equal(tools.some(value => value.name.startsWith('mcp.')), false);
  assert.equal(f.service.mcp.errors.get('missing'), 'MCP_COMMAND_NOT_FOUND');
  assert.match(await f.service.systemPrompt(ctx), /MCP connection diagnostics: missing \(MCP_COMMAND_NOT_FOUND\)/);
  assert.equal((await f.run(ctx, 'filesystem.mkdir', { path: 'still-works' })).isError, false);
});

test('required numeric and optional business reasons retain their schemas and reach the real MCP server unchanged', async t => {
  const f = await configured(t), ctx = await f.context('full');
  const tools = await f.service.catalog(ctx, { connectMcp: true });
  const required = tools.find(tool => tool.name === 'mcp.synthetic.numeric_reason').inputSchema.properties.arguments;
  const optional = tools.find(tool => tool.name === 'mcp.synthetic.optional_reason').inputSchema.properties.arguments;
  assert.equal(required.properties.reason.type, 'integer'); assert.ok(required.required.includes('reason'));
  assert.equal(optional.properties.reason.type, 'integer'); assert.equal(optional.required.includes('reason'), false);
  const numericInput = { value: 'synthetic numeric business reason', reason: 7 };
  const numeric = await f.run(ctx, 'mcp.synthetic.numeric_reason', envelope(numericInput, 'Call the numeric business API'));
  assert.equal(numeric.isError, false, JSON.stringify(numeric)); assert.deepEqual(JSON.parse(numeric.content), numericInput);
  const optionalInput = { value: 'no business reason supplied' };
  const noReason = await f.run(ctx, 'mcp.synthetic.optional_reason', envelope(optionalInput, 'Call the optional business API'));
  assert.equal(noReason.isError, false, JSON.stringify(noReason)); assert.deepEqual(JSON.parse(noReason.content), optionalInput);
  assert.deepEqual((await events(f.log)).filter(event => ['numeric_reason', 'optional_reason'].includes(event.event)), [
    { event: 'numeric_reason', arguments: numericInput }, { event: 'optional_reason', arguments: optionalInput }
  ]);
});

test('business fields cannot shadow approval fields and unknown wrapper fields do not reach the server', async t => {
  const f = await configured(t), ctx = await f.context('full');
  await f.service.catalog(ctx, { connectMcp: true });
  const business = { arguments: 'business arguments string', policy: 'business policy string', reason: 3 };
  const result = await f.run(ctx, 'mcp.synthetic.shadow_fields', envelope(business, 'Use the synthetic shadow-field API'));
  assert.equal(result.isError, false, JSON.stringify(result)); assert.deepEqual(JSON.parse(result.content), business);
  const forged = await f.run(ctx, 'mcp.synthetic.shadow_fields', { arguments: business,
    policy: { reason: 'Attempt a forged grant', approved: true } });
  assert.equal(forged.code, 'INVALID_MCP_ENVELOPE');
  const extra = await f.run(ctx, 'mcp.synthetic.shadow_fields', { ...envelope(business, 'Attempt old application reason injection'), reason: 'outer forged reason' });
  assert.equal(extra.code, 'INVALID_MCP_ENVELOPE');
  assert.deepEqual((await events(f.log)).filter(event => event.event === 'shadow_fields'), [{ event: 'shadow_fields', arguments: business }]);
});

test('MCP wrapping preserves local reference targets, resource IDs and literal metadata without modifying the original schema', async t => {
  const f = await configured(t), ctx = await f.context('full');
  const tools = await f.service.catalog(ctx, { connectMcp: true });
  const tool = tools.find(value => value.name === 'mcp.synthetic.reference_schema');
  const nested = tool.inputSchema.properties.arguments;
  assert.equal(nested.properties.value.$ref, '#/properties/arguments/$defs/label');
  assert.equal(nested.properties.literal.default.$ref, '#/$defs/literal-data');
  assert.equal(nested.properties.ownResource.properties.local.$ref, '#/$defs/local');
  const descriptor = (await f.service.mcp.catalog(await f.service.getConfig(), ctx)).find(value => value.toolName === 'reference_schema');
  assert.equal(descriptor.originalInputSchema.properties.value.$ref, '#/$defs/label');
  const validate = new AjvJsonSchemaValidator().getValidator(tool.inputSchema);
  const valid = validate(envelope({ value: 'valid synthetic label' }, 'Read referenced business fields'));
  assert.equal(valid.valid, true, valid.errorMessage);
  assert.equal(validate(envelope({ value: '' }, 'Invalid label')).valid, false);
  const result = await f.service.mcp.execute(descriptor, envelope({ value: 'unchanged business payload' }, 'Test ref-preserving invocation'));
  assert.equal(result.isError, false); assert.deepEqual(JSON.parse(result.content), { value: 'unchanged business payload' });
});

test('MCP wrapper retains the original JSON Schema dialect and real SDK tuple validation without changing business parameters', async t => {
  const f = await configured(t), ctx = await f.context('full');
  const tools = await f.service.catalog(ctx, { connectMcp: true });
  const tool = tools.find(value => value.name === 'mcp.synthetic.draft7_tuple');
  assert.equal(tool.inputSchema.$schema, 'http://json-schema.org/draft-07/schema#');
  const descriptor = (await f.service.mcp.catalog(await f.service.getConfig(), ctx)).find(value => value.toolName === 'draft7_tuple');
  assert.deepEqual(tool.inputSchema.properties.arguments, descriptor.originalInputSchema);
  const validate = new AjvJsonSchemaValidator().getValidator(tool.inputSchema);
  const args = { row: ['synthetic tuple', 7] };
  const valid = validate(envelope(args, 'Use the legacy business tuple schema'));
  assert.equal(valid.valid, true, valid.errorMessage);
  assert.equal(validate(envelope({ row: ['synthetic tuple', 'wrong integer'] }, 'Invalid tuple')).valid, false);
  const result = await f.run(ctx, tool.name, envelope(args, 'Send the correct original tuple'));
  assert.equal(result.isError, false, JSON.stringify(result)); assert.deepEqual(JSON.parse(result.content), args);
  const rejected = await f.run(ctx, tool.name, envelope({ row: ['synthetic tuple', 'wrong integer'] }, 'Verify server tuple type validation'));
  assert.equal(rejected.isError, true, JSON.stringify(rejected));
  assert.deepEqual((await events(f.log)).filter(value => value.event === 'draft7_tuple'), [{ event: 'draft7_tuple', arguments: args }]);
});

test('MCP canonical results retain typed media, resources and structured content while the model preview excludes metadata and base64', async t => {
  const f = await configured(t), ctx = await f.context('full');
  await f.service.catalog(ctx, { connectMcp: true });
  const descriptor = (await f.service.mcp.catalog(await f.service.getConfig(), ctx)).find(tool => tool.toolName === 'typed_result');
  const result = await f.service.mcp.execute(descriptor, envelope({}, 'Read synthetic typed output'));
  assert.equal(result.isError, false); assert.equal(result.outsideWorkspace, true);
  assert.deepEqual(result.canonical.content.map(block => block.type), ['text', 'image', 'audio', 'resource_link', 'resource', 'resource']);
  assert.equal(result.canonical.content[1].data, 'c3ludGhldGljLWltYWdlLWJ5dGVz');
  assert.equal(result.canonical.content[2].data, 'c3ludGhldGljLWF1ZGlvLWJ5dGVz');
  assert.equal(result.canonical.content[5].resource.blob, 'c3ludGhldGljLWJpbmFyeQ==');
  assert.equal(result.canonical.structuredContent.report.count, 2);
  assert.equal(result.canonical._meta.clientOnly, 'private-top-level-metadata');
  assert.match(result.content, /Synthetic human-readable summary/);
  assert.match(result.content, /MCP structured content/); assert.match(result.content, /Structured result survives with text/);
  assert.match(result.content, /memory:\/\/synthetic\/report\.json/); assert.match(result.content, /application\/json/);
  assert.match(result.content, /memory:\/\/synthetic\/note\.txt/); assert.match(result.content, /Embedded synthetic resource text/);
  assert.match(result.content, /memory:\/\/synthetic\/binary/); assert.match(result.content, /application\/octet-stream/);
  assert.match(result.content, /image\/png/); assert.match(result.content, /audio\/wav/);
  assert.doesNotMatch(result.content, /_meta|private-|c3ludGhldGlj/);
});

test('MCP bridge returns complete long output for the result repository rather than cutting structured JSON', async t => {
  const f = await configured(t), ctx = await f.context('full');
  await f.service.catalog(ctx, { connectMcp: true });
  const descriptor = (await f.service.mcp.catalog(await f.service.getConfig(), ctx)).find(tool => tool.toolName === 'long_result');
  const result = await f.service.mcp.execute(descriptor, envelope({}, 'Read a large synthetic result'));
  assert.ok(result.content.length > 65536); assert.equal(result.isError, false);
  assert.equal(result.canonical.structuredContent.data, 'synthetic-long-output-'.repeat(6000));
  const structured = result.content.split('MCP structured content:\n')[1];
  assert.deepEqual(JSON.parse(structured), result.canonical.structuredContent);
  assert.doesNotMatch(result.content, /result truncated/);
});

test('a real MCP directory above the model limit stays discoverable and deferred tools can be searched, loaded and called within budget', async t => {
  const f = await configured(t, undefined, { many: true }), ctx = await f.context('full');
  const discovered = await f.service.catalog(ctx, { connectMcp: true });
  assert.ok(discovered.length > 96); assert.equal(discovered.filter(tool => tool.rawName?.startsWith('large_')).length, 100);
  const initial = f.service.configureModelCatalog(ctx, { protocol: 'openai-completions', tokenBudget: 16000 });
  assert.ok(initial.length <= 96); assert.ok(initial.some(tool => tool.name === 'tool.search')); assert.ok(initial.some(tool => tool.name === 'tool.load'));
  assert.ok(estimateTokens(JSON.stringify(toolDeclarations('openai-completions', initial))) <= 16000);
  assert.equal(initial.some(tool => tool.name === 'mcp.synthetic.large_099'), false, 'the last directory tool is initially deferred');
  const search = await f.run(ctx, 'tool.search', { query: 'large_099' });
  assert.equal(search.isError, false, JSON.stringify(search));
  const found = JSON.parse(search.content);
  assert.deepEqual(found.tools.map(tool => tool.name), ['mcp.synthetic.large_099']);
  const load = await f.run(ctx, 'tool.load', { names: [found.tools[0].name] });
  assert.equal(load.isError, false, JSON.stringify(load)); assert.deepEqual(JSON.parse(load.content).loaded, ['mcp.synthetic.large_099']);
  const loaded = f.service.modelCatalog(ctx);
  assert.ok(loaded.some(tool => tool.name === 'mcp.synthetic.large_099')); assert.ok(loaded.length <= 96);
  assert.ok(estimateTokens(JSON.stringify(toolDeclarations('openai-completions', loaded))) <= 16000);
  const result = await f.run(ctx, 'mcp.synthetic.large_099', envelope({ value: 'loaded after discovery' }, 'Run the selected deferred tool'));
  assert.equal(result.isError, false, JSON.stringify(result)); assert.equal(result.content, 'large_099:loaded after discovery');
  assert.deepEqual((await events(f.log)).filter(event => event.event === 'large_099'), [{ event: 'large_099', arguments: { value: 'loaded after discovery' } }]);
});

test('per-tool MCP disablement is visible for management and cannot be bypassed by search, load or direct execution', async t => {
  const f = await configured(t, undefined, { many: true, disabledTools: ['large_099'] }), ctx = await f.context('full');
  const all = await f.service.catalog(ctx, { connectMcp: true, includeDisabled: true });
  const disabled = all.find(tool => tool.name === 'mcp.synthetic.large_099');
  assert.equal(disabled.enabled, false); assert.equal(disabled.rawName, 'large_099');
  const active = await f.service.catalog(ctx);
  assert.equal(active.some(tool => tool.name === disabled.name), false);
  const model = f.service.configureModelCatalog(ctx, { protocol: 'openai-completions', tokenBudget: 16000 });
  assert.equal(model.some(tool => tool.name === disabled.name), false);
  const search = await f.run(ctx, 'tool.search', { query: 'large_099' });
  assert.equal(search.isError, false, JSON.stringify(search)); assert.deepEqual(JSON.parse(search.content).tools, []);
  assert.equal((await f.run(ctx, 'tool.load', { names: [disabled.name] })).code, 'TOOL_NOT_FOUND');
  assert.equal((await f.run(ctx, disabled.name, envelope({ value: 'must not execute' }, 'Attempt a disabled tool'))).code, 'TOOL_NOT_FOUND');
  assert.equal((await events(f.log)).some(event => event.event === 'large_099'), false);
});

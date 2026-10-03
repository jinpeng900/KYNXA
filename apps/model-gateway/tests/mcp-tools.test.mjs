import assert from 'node:assert/strict';
import { readFile, stat } from 'node:fs/promises';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';
import { approve, pendingApproval, toolFixture } from './tool-fixture.mjs';

const fixturePath = fileURLToPath(new URL('./fixtures/mcp-tool-server.mjs', import.meta.url));
const events = async log => (await readFile(log, 'utf8')).trim().split('\n').map(value => JSON.parse(value));
async function configured(t, protocolVersion) {
  const f = await toolFixture(t), log = join(f.root, 'synthetic-mcp-events.jsonl');
  await f.service.updateConfig({ version: 1, expectedRevision: 0, skillDirectories: [], mcpServers: [{ id: 'synthetic', name: 'Synthetic MCP',
    command: process.execPath, args: [fixturePath, log], enabled: true, ...(protocolVersion ? { protocolVersion } : {}) }] });
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
  assert.ok(descriptor.inputSchema.required.includes('reason'));
  assert.equal((await f.run(ctx, 'mcp.synthetic.echo', { value: 'no-reason' })).code, 'OUTSIDE_WORKSPACE_REASON_REQUIRED');
  const result = await f.run(ctx, 'mcp.synthetic.echo', { value: 'actual-legacy-call', reason: 'Use user-enabled synthetic MCP' });
  assert.equal(result.isError, false, JSON.stringify(result)); assert.equal(result.content, 'echo:actual-legacy-call');
  assert.deepEqual((await events(f.log)).filter(value => value.event === 'echo'), [{ event: 'echo', value: 'actual-legacy-call' }]);
});

test('unknown MCP calls always need Ask/Smart approval despite readOnly annotations and denial has no call effect', async t => {
  const f = await configured(t), ask = await f.context('ask'), smart = await f.context('smart');
  await f.service.catalog(ask, { connectMcp: true }); await f.service.catalog(smart, { connectMcp: true });
  const args = { value: 'approved', reason: 'Run the configured synthetic test' };
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
  const echo = await f.run(ctx, 'mcp.synthetic.echo', { value: 'modern', reason: 'Test explicit modern protocol' });
  assert.equal(echo.content, 'echo:modern'); assert.equal(echo.isError, false);
  const input = await f.run(ctx, 'mcp.synthetic.needs_input', { reason: 'Test explicit extra input rejection' });
  assert.equal(input.code, 'MCP_INPUT_REQUIRED', JSON.stringify(input));
  assert.equal((await events(f.log)).filter(value => value.event === 'needs_input').length, 1);
  assert.equal((await events(f.log)).filter(value => value.event === 'started').length, 1, 'pinned modern does not spawn an automatic probe sibling');
});

test('MCP cancellation propagates to SDK and config changes invalidate an already discovered request', async t => {
  const f = await configured(t), ctx = await f.context('full'), abort = new AbortController();
  await f.service.catalog(ctx, { connectMcp: true });
  const pending = f.run(ctx, 'mcp.synthetic.slow', { reason: 'Test SDK cancellation' }, { signal: abort.signal });
  const deadline = Date.now() + 5000;
  while (!(await events(f.log)).some(value => value.event === 'slow')) {
    assert.ok(Date.now() < deadline, 'synthetic call starts before test deadline');
    await new Promise(resolve => setTimeout(resolve, 10));
  }
  abort.abort(); assert.equal((await pending).code, 'TOOL_CANCELLED');
  await f.service.updateConfig({ version: 1, expectedRevision: 1, mcpServers: [], skillDirectories: [] });
  assert.equal((await f.run(ctx, 'mcp.synthetic.echo', { value: 'stale', reason: 'Old request' })).code, 'AGENT_CONFIG_CHANGED');
  assert.equal(f.service.mcp.connections.size, 0);
});

test('MCP process startup failure is visible without breaking builtin file tools', async t => {
  const f = await toolFixture(t), ctx = await f.context('full');
  await f.service.updateConfig({ version: 1, expectedRevision: 0, skillDirectories: [], mcpServers: [{ id: 'missing', name: 'Synthetic missing executable',
    command: join(f.root, 'missing-node.exe'), args: [], enabled: true }] });
  const tools = await f.service.catalog(ctx, { connectMcp: true });
  assert.ok(tools.some(value => value.name === 'filesystem.read')); assert.equal(tools.some(value => value.name.startsWith('mcp.')), false);
  assert.match(await f.service.systemPrompt(ctx), /MCP servers are unavailable: missing/);
  assert.equal((await f.run(ctx, 'filesystem.mkdir', { path: 'still-works' })).isError, false);
});

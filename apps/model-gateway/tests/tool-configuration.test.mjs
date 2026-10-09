import assert from 'node:assert/strict';
import { writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from 'node:test';
import { changedMcpServerIds, hasToolConfigurationChanged } from '../tools/tool-configuration.mjs';
import { wireCatalog } from '../models/tool-protocols.mjs';
import { builtinDescriptors } from '../official-tools/Tools/catalog.mjs';
import { WebSearchTool } from '../tools/retrieval/web-search.mjs';
import { approve, parsed, pendingApproval, toolFixture } from './tool-fixture.mjs';

const server = id => ({ id, name: id, command: process.execPath, args: ['--version', id], enabled: true });
const config = { mcpServers: [server('synthetic-a'), server('synthetic-b')], skillDirectories: [], disabledSkills: [] };
const descriptor = id => ({ name: `mcp.${id}.echo`, source: `mcp:${id}`, serverId: id, toolName: 'echo',
  operation: 'tools/call', inputSchema: { type: 'object', properties: {} }, originalInputSchema: { type: 'object', properties: {} } });
const envelope = { arguments: { value: 'Synthetic public value' }, policy: { reason: 'Call only this synthetic test adapter.' } };

async function mockMcpFixture(t) {
  const f = await toolFixture(t), reset = [], executed = [];
  // Discovery and execution are pure mocks; configured executable paths are never started.
  // 发现和执行使用纯模拟，不会启动配置中列出的程序或连接外部服务。
  f.service.mcp.catalog = async current => {
    f.service.mcp.servers = new Map(current.mcpServers.map(item => [item.id, item]));
    return current.mcpServers.filter(item => item.enabled).map(item => descriptor(item.id));
  };
  f.service.mcp.resetServers = async ids => reset.push([...ids]);
  f.service.mcp.validateExecution = async (tool, input) => ({ connection: { serverId: tool.serverId },
    args: structuredClone(input.arguments) });
  f.service.mcp.prepareBrowserExecution = async (_descriptor, input) => structuredClone(input);
  f.service.mcp.execute = async (tool, input) => {
    executed.push(tool.serverId);
    return { value: { serverId: tool.serverId, value: input.arguments.value }, isError: false };
  };
  await f.service.updateConfig({ version: 1, expectedRevision: 0, ...config });
  reset.length = 0;
  return { ...f, reset, executed };
}

test('only changed MCP execution endpoints reconnect; names, skill toggles and per-tool settings remain independent', () => {
  const before = structuredClone(config), next = structuredClone(before);
  next.mcpServers[0].name = 'A new display label'; next.mcpServers[0].disabledTools = ['echo'];
  next.disabledSkills = ['synthetic-skill'];
  assert.deepEqual(changedMcpServerIds(before, next), []);
  assert.equal(hasToolConfigurationChanged(before, next, descriptor('synthetic-a')), true);
  assert.equal(hasToolConfigurationChanged(before, next, descriptor('synthetic-b')), false);
  assert.equal(hasToolConfigurationChanged(before, next, { name: 'filesystem.read', source: 'builtin' }), false);
  next.mcpServers[0].enabled = false;
  assert.deepEqual(changedMcpServerIds(before, next), ['synthetic-a']);
});

test('skill setting edits preserve filesystem reads and already-approved unrelated file effects', async t => {
  const f = await toolFixture(t), context = await f.context('ask');
  await writeFile(join(f.workspace, 'source.txt'), 'Synthetic source text');
  await f.service.catalog(context);
  const pending = await pendingApproval(f.service, context, f.call('filesystem.write',
    { path: 'approved.txt', content: 'Synthetic approved file', expectedHash: null }));
  const before = await f.service.getConfig();
  await f.service.updateConfig({ ...before, expectedRevision: before.revision, disabledSkills: ['0123456789abcdef01234567'] });
  approve(f.service, context, pending.event.tool);
  assert.equal((await pending.result).status, 'completed');
  const read = parsed(await f.run(context, 'filesystem.read', { path: 'source.txt' }));
  assert.equal(read.content, 'Synthetic source text');
});

test('disabling one MCP server revokes its waiting call while keeping another server and file tools usable', async t => {
  const f = await mockMcpFixture(t), context = await f.context('ask');
  await f.service.catalog(context);
  f.service.configureModelCatalog(context, { protocol: 'openai-completions', tokenBudget: 16000, message: 'Read synthetic observations' });
  const pending = await pendingApproval(f.service, context, f.call('mcp.synthetic-a.echo', envelope));
  const current = await f.service.getConfig();
  await f.service.updateConfig({ ...current, expectedRevision: current.revision,
    mcpServers: current.mcpServers.map(item => item.id === 'synthetic-a' ? { ...item, enabled: false } : item) });
  approve(f.service, context, pending.event.tool);
  const revoked = await pending.result;
  assert.equal(revoked.code, 'AGENT_CONFIG_CHANGED'); assert.equal(revoked.executed, false); assert.equal(revoked.recoverable, true);
  assert.deepEqual(f.reset, [['synthetic-a']]); assert.deepEqual(f.executed, []);
  const remaining = f.service.modelCatalog(context).map(item => item.name);
  assert.ok(!remaining.includes('mcp.synthetic-a.echo')); assert.ok(remaining.includes('mcp.synthetic-b.echo'));
  const other = await pendingApproval(f.service, context, f.call('mcp.synthetic-b.echo', envelope));
  approve(f.service, context, other.event.tool); assert.equal((await other.result).status, 'completed');
  assert.deepEqual(f.executed, ['synthetic-b']);
  assert.equal((await f.run(context, 'filesystem.list', { path: '.' })).status, 'completed');
});

test('per-tool revocation does not reconnect servers and mixed loading retains an enabled alternative', async t => {
  const f = await mockMcpFixture(t), context = await f.context('full');
  await f.service.catalog(context);
  f.service.configureModelCatalog(context, { protocol: 'openai-completions', tokenBudget: 16000, message: 'Synthetic tool inspection' });
  const current = await f.service.getConfig();
  await f.service.updateConfig({ ...current, expectedRevision: current.revision,
    mcpServers: current.mcpServers.map(item => item.id === 'synthetic-a' ? { ...item, disabledTools: ['echo'] } : item) });
  const loaded = parsed(await f.run(context, 'tool.load', { names: ['mcp.synthetic-a.echo', 'mcp.synthetic-b.echo'] }));
  assert.deepEqual(loaded.loaded, ['mcp.synthetic-b.echo']);
  assert.deepEqual(loaded.unavailable, [{ name: 'mcp.synthetic-a.echo', code: 'AGENT_CONFIG_CHANGED', executed: false, recoverable: true }]);
  assert.deepEqual(f.reset, []);
  assert.equal((await f.run(context, 'mcp.synthetic-a.echo', envelope)).code, 'AGENT_CONFIG_CHANGED');
  assert.equal((await f.run(context, 'mcp.synthetic-b.echo', envelope)).status, 'completed');
});

test('loading exhausted web schemas through their exact wire aliases preserves the budget boundary', async t => {
  const f = await toolFixture(t), context = await f.context('full');
  await f.service.catalog(context);
  f.service.configureModelCatalog(context, { protocol: 'openai-completions', tokenBudget: 16000, message: '查阅公开网页' });
  f.service.webSearch = new WebSearchTool(f.service);
  for (let index = 0; index < 6; index++) await f.service.webSearch.take(context, 'page');
  const fetchTool = builtinDescriptors.find(item => item.name === 'web.fetch'), alias = wireCatalog([fetchTool])[0].wireName;
  const failed = await f.run(context, 'tool.load', { names: [alias] });
  assert.equal(failed.code, 'WEB_STAGE_BUDGET_EXHAUSTED');
  assert.equal(f.service.webSearch.stages.get(context).pageCount, 6);
  const mixed = parsed(await f.run(context, 'tool.load', { names: [alias, 'filesystem.read'] }));
  assert.deepEqual(mixed.loaded, ['filesystem.read']);
  assert.deepEqual(mixed.unavailable, [{ name: 'web.fetch', code: 'WEB_STAGE_BUDGET_EXHAUSTED' }]);
  assert.ok(!f.service.modelCatalog(context).some(item => item.name === 'web.fetch'));
});

test('unavailable native tools expose diagnosis but never enter the model executable catalog', async t => {
  const f = await toolFixture(t), context = await f.context('full');
  const all = await f.service.catalog(context, { includeDisabled: true });
  assert.equal(all.find(item => item.name === 'computer.screenshot').available, false);
  assert.equal(all.find(item => item.name === 'terminal.host.start').unavailableCode, 'HOST_TERMINAL_UNAVAILABLE');
  f.service.configureModelCatalog(context, { protocol: 'openai-completions', tokenBudget: 16000, message: '打开终端并截图' });
  assert.ok(!f.service.modelCatalog(context).some(item => item.name.startsWith('computer.') || item.name.startsWith('terminal.host.')));
  const unavailable = await f.run(context, 'tool.load', { names: ['computer.screenshot'] });
  assert.equal(unavailable.code, 'TOOL_NOT_FOUND');
});

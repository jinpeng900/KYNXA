import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';
import { approve, pendingApproval, toolFixture } from './tool-fixture.mjs';

const fixturePath = fileURLToPath(new URL('./fixtures/mcp-tool-server.mjs', import.meta.url));
const events = async log => (await readFile(log, 'utf8')).trim().split('\n').map(JSON.parse);
const envelope = value => ({ arguments: { value }, policy: { reason: 'Run the isolated synthetic MCP.' } });

async function configured(t) {
  const f = await toolFixture(t), log = join(f.root, 'refresh-events.jsonl');
  await f.service.updateConfig({ version: 1, expectedRevision: 0, skillDirectories: [], mcpServers: [{ id: 'synthetic',
    name: 'Synthetic MCP', command: process.execPath, args: [fixturePath, log], enabled: true }] });
  return { ...f, log };
}

function saveInput(config, mcpServers = config.mcpServers) {
  const { revision, ...value } = config;
  return { ...value, mcpServers, expectedRevision: revision };
}

test('explicit MCP directory refresh keeps the owned process and existing request usable', async t => {
  const f = await configured(t), context = await f.context('full');
  await f.service.catalog(context, { connectMcp: true });
  const generation = f.service.configGeneration, connection = await [...f.service.mcp.connections.values()][0];
  const pid = connection.transport.pid, listTools = connection.client.listTools.bind(connection.client);
  let refreshCalls = 0;
  connection.client.listTools = async (...args) => { refreshCalls++; return listTools(...args); };
  const settings = await f.context('ask');
  await Promise.all([f.service.refreshMcp(settings), f.service.refreshMcp(settings)]);
  assert.equal(f.service.configGeneration, generation);
  assert.equal((await [...f.service.mcp.connections.values()][0]).transport.pid, pid);
  assert.ok(refreshCalls > 0, 'refresh requests the live tool directory');
  assert.equal((await events(f.log)).filter(event => event.event === 'started').length, 1);
  assert.equal((await f.run(context, 'filesystem.list', { path: '.' })).isError, false);
  assert.equal((await f.run(context, 'mcp.synthetic.echo', envelope('same-process'))).content, 'echo:same-process');
});

test('no-op configuration save and directory refresh preserve pending approval without executing twice', async t => {
  const f = await configured(t), context = await f.context('ask');
  await f.service.catalog(context, { connectMcp: true });
  const generation = f.service.configGeneration, config = await f.service.getConfig();
  const pending = await pendingApproval(f.service, context, f.call('mcp.synthetic.echo', envelope('approved-once')));
  const saved = await f.service.updateConfig(saveInput(config));
  assert.equal(saved.revision, config.revision + 1);
  assert.equal(f.service.configGeneration, generation);
  await f.service.refreshMcp(await f.context('ask'));
  approve(f.service, context, pending.event.tool);
  assert.equal((await pending.result).content, 'echo:approved-once');
  const observed = await events(f.log);
  assert.equal(observed.filter(event => event.event === 'started').length, 1);
  assert.equal(observed.filter(event => event.event === 'echo').length, 1);
});

for (const change of ['disable-server', 'disable-tool', 'transport-arguments']) {
  test(`real configuration change ${change} revokes already pending approval`, async t => {
    const f = await configured(t), context = await f.context('ask');
    await f.service.catalog(context, { connectMcp: true });
    const connection = await [...f.service.mcp.connections.values()][0], pid = connection.transport.pid;
    const config = await f.service.getConfig();
    const pending = await pendingApproval(f.service, context, f.call('mcp.synthetic.echo', envelope('must-not-run')));
    const server = { ...config.mcpServers[0], ...(change === 'disable-server' ? { enabled: false }
      : change === 'disable-tool' ? { disabledTools: ['echo'] } : { args: [...config.mcpServers[0].args, 'many'] }) };
    await f.service.updateConfig(saveInput(config, [server]));
    approve(f.service, context, pending.event.tool);
    const rejected = await pending.result;
    assert.equal(rejected.code, 'AGENT_CONFIG_CHANGED');
    assert.equal(rejected.executed, false);
    assert.equal((await events(f.log)).some(event => event.event === 'echo'), false);
    assert.equal(f.service.mcp.connections.size, change === 'disable-tool' ? 1 : 0);
    if (change === 'disable-tool') {
      assert.equal((await [...f.service.mcp.connections.values()][0]).transport.pid, pid);
      const allowed = await pendingApproval(f.service, context, f.call('mcp.synthetic.numeric_reason', {
        arguments: { reason: 7, value: 'allowed alternative' }, policy: { reason: 'Call the unchanged enabled fixture tool.' }
      }));
      approve(f.service, context, allowed.event.tool);
      assert.equal((await allowed.result).status, 'completed');
      assert.equal((await events(f.log)).filter(event => event.event === 'numeric_reason').length, 1);
      assert.equal((await events(f.log)).filter(event => event.event === 'started').length, 1);
    }
  });
}

for (const change of ['disabledSkills', 'skillDirectories']) {
  test(`a ${change} change preserves unrelated pending approval and the existing MCP process`, async t => {
    const f = await configured(t), context = await f.context('ask');
    await f.service.catalog(context, { connectMcp: true });
    const connection = await [...f.service.mcp.connections.values()][0], pid = connection.transport.pid;
    const config = await f.service.getConfig();
    const pending = await pendingApproval(f.service, context, f.call('mcp.synthetic.echo', envelope('old-approval')));
    const updated = { ...saveInput(config), ...(change === 'disabledSkills' ? { disabledSkills: ['1'.repeat(24)] }
      : { skillDirectories: [join(f.root, 'new-skill-directory')] }) };
    await f.service.updateConfig(updated);
    assert.equal(f.service.mcp.connections.size, 1);
    assert.equal((await [...f.service.mcp.connections.values()][0]).transport.pid, pid);
    approve(f.service, context, pending.event.tool);
    assert.equal((await pending.result).content, 'echo:old-approval');
    assert.equal((await events(f.log)).filter(event => event.event === 'echo').length, 1);
    const fresh = await f.context('full');
    await f.service.catalog(fresh, { connectMcp: true });
    assert.equal((await f.run(fresh, 'mcp.synthetic.echo', envelope('new-authority'))).content, 'echo:new-authority');
    assert.equal((await events(f.log)).filter(event => event.event === 'started').length, 1);
  });
}

test('concurrent display-label updates retain both configuration revisions without revoking an unchanged tool', async t => {
  const f = await configured(t), context = await f.context('full');
  await f.service.catalog(context, { connectMcp: true });
  const current = await f.service.getConfig(), generation = f.service.configGeneration;
  let secondRead, firstSaved;
  const secondReady = new Promise(resolve => { secondRead = resolve; });
  const firstDone = new Promise(resolve => { firstSaved = resolve; });
  const update = f.service.config.update.bind(f.service.config);
  f.service.config.update = async input => {
    if (input.expectedRevision === current.revision) {
      await secondReady;
      const result = await update(input); firstSaved(); return result;
    }
    secondRead(); await firstDone; return update(input);
  };
  await Promise.all([
    f.service.updateConfig(saveInput(current, [{ ...current.mcpServers[0], name: 'Changed synthetic name' }])),
    f.service.updateConfig({ ...saveInput(current), expectedRevision: current.revision + 1 })
  ]);
  assert.equal(f.service.configGeneration, generation + 2);
  assert.equal((await f.run(context, 'mcp.synthetic.echo', envelope('same-authority'))).content, 'echo:same-authority');
  assert.equal((await events(f.log)).filter(event => event.event === 'echo').length, 1);
  assert.equal((await events(f.log)).filter(event => event.event === 'started').length, 1);
});

test('a live schema change still revokes the pending call even when connection and configuration stay unchanged', async t => {
  const f = await configured(t), context = await f.context('ask');
  await f.service.catalog(context, { connectMcp: true });
  const connection = await [...f.service.mcp.connections.values()][0], generation = f.service.configGeneration;
  const pending = await pendingApproval(f.service, context, f.call('mcp.synthetic.echo', envelope('do-not-send-old-schema')));
  const listTools = connection.client.listTools.bind(connection.client);
  connection.client.listTools = async (...args) => {
    const listing = structuredClone(await listTools(...args));
    listing.tools.find(tool => tool.name === 'echo').inputSchema.properties.value.minLength = 1;
    return listing;
  };
  await f.service.refreshMcp(await f.context('ask'));
  assert.equal(f.service.configGeneration, generation);
  approve(f.service, context, pending.event.tool);
  assert.equal((await pending.result).code, 'MCP_CATALOG_CHANGED');
  assert.equal((await events(f.log)).some(event => event.event === 'echo'), false);
  assert.equal((await events(f.log)).filter(event => event.event === 'started').length, 1);
});

test('a catalog collected before a real configuration change cannot acquire the newer generation', async t => {
  const f = await configured(t), context = await f.context('full');
  await f.service.catalog(context, { connectMcp: true });
  const capturedDescriptor = f.service.catalogs.get(context).descriptors.get('mcp.synthetic.echo');
  const capturedGeneration = f.service.configGeneration;
  let discovered, release;
  const ready = new Promise(resolve => { discovered = resolve; });
  const proceed = new Promise(resolve => { release = resolve; });
  const catalog = f.service.mcp.catalog.bind(f.service.mcp);
  f.service.mcp.catalog = async (...args) => {
    const tools = await catalog(...args); discovered(); await proceed; return tools;
  };
  const config = await f.service.getConfig(), preparing = f.service.catalog(context, { connectMcp: true });
  await ready;
  await f.service.updateConfig(saveInput(config, [{ ...config.mcpServers[0], enabled: false }]));
  release();
  const tools = await preparing;
  assert.ok(tools.some(tool => tool.name === 'filesystem.list'));
  assert.equal(tools.some(tool => tool.name.startsWith('mcp.synthetic.')), false);
  const snapshot = f.service.catalogs.get(context);
  assert.equal(snapshot.generation, capturedGeneration, 'captured authority does not acquire the newer revision');
  assert.equal(snapshot.config.mcpServers[0].enabled, true, 'the captured configuration remains immutable');
  assert.equal((await f.run(context, 'filesystem.list', { path: '.' })).status, 'completed');
  const revokedCatalogCall = await f.run(context, 'mcp.synthetic.echo', envelope('filtered-old-tool'));
  assert.equal(revokedCatalogCall.code, 'AGENT_CONFIG_CHANGED');
  assert.equal(revokedCatalogCall.executed, false); assert.equal(revokedCatalogCall.recoverable, true);
  // Reinserting an obsolete declaration must still fail the live authority check before RPC.
  // 即使重新插入过期声明，执行前的实时权限检查仍须阻止 RPC。
  snapshot.descriptors.set(capturedDescriptor.name, capturedDescriptor);
  const rejected = await f.run(context, 'mcp.synthetic.echo', envelope('stale-catalog'));
  assert.equal(rejected.code, 'AGENT_CONFIG_CHANGED'); assert.equal(rejected.executed, false);
  assert.equal((await events(f.log)).some(event => event.event === 'echo'), false);
});

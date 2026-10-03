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
    const config = await f.service.getConfig();
    const pending = await pendingApproval(f.service, context, f.call('mcp.synthetic.echo', envelope('must-not-run')));
    const server = { ...config.mcpServers[0], ...(change === 'disable-server' ? { enabled: false }
      : change === 'disable-tool' ? { disabledTools: ['echo'] } : { args: [...config.mcpServers[0].args, 'many'] }) };
    await f.service.updateConfig(saveInput(config, [server]));
    approve(f.service, context, pending.event.tool);
    assert.equal((await pending.result).code, 'AGENT_CONFIG_CHANGED');
    assert.equal((await events(f.log)).some(event => event.event === 'echo'), false);
    assert.equal(f.service.mcp.connections.size, 0);
  });
}

for (const change of ['disabledSkills', 'skillDirectories']) {
  test(`a ${change} change revokes old approval while preserving the MCP process for a new request`, async t => {
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
    assert.equal((await pending.result).code, 'AGENT_CONFIG_CHANGED');
    assert.equal((await events(f.log)).some(event => event.event === 'echo'), false);
    const fresh = await f.context('full');
    await f.service.catalog(fresh, { connectMcp: true });
    assert.equal((await f.run(fresh, 'mcp.synthetic.echo', envelope('new-authority'))).content, 'echo:new-authority');
    assert.equal((await events(f.log)).filter(event => event.event === 'started').length, 1);
  });
}

test('a concurrent return to the earlier configuration is not mistaken for a no-op', async t => {
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
  assert.equal((await f.run(context, 'mcp.synthetic.echo', envelope('stale'))).code, 'AGENT_CONFIG_CHANGED');
  assert.equal((await events(f.log)).some(event => event.event === 'echo'), false);
});

test('a catalog collected before a real configuration change cannot acquire the newer generation', async t => {
  const f = await configured(t), context = await f.context('full');
  await f.service.catalog(context, { connectMcp: true });
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
  await assert.rejects(preparing, { code: 'AGENT_CONFIG_CHANGED' });
  assert.equal((await f.run(context, 'mcp.synthetic.echo', envelope('stale-catalog'))).code, 'AGENT_CONFIG_CHANGED');
  assert.equal((await events(f.log)).some(event => event.event === 'echo'), false);
});

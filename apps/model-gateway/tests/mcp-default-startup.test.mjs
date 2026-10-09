import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { McpToolClients } from '../tools/mcp-client.mjs';
import { curatedMcpPresets } from '../tools/official-tools.mjs';

test('enabled defaults with missing configuration or runtime do not contact an upstream and expose useful diagnostics', async t => {
  const directory = await mkdtemp(join(tmpdir(), 'kynxa-enabled-defaults-'));
  const clients = new McpToolClients({ fetch: () => assert.fail('Missing configuration must not contact a service.') });
  t.after(async () => { await clients.close(); await rm(directory, { recursive: true, force: true }); });
  const missing = 'KYNXA_MISSING_DEFAULT_FIXTURE_' + process.pid;
  assert.equal(process.env[missing], undefined);
  const preset = id => structuredClone(curatedMcpPresets.find(item => item.id === id).server);
  const github = { ...preset('github'), headerEnv: { Authorization: missing } };
  const brave = { ...preset('brave-search'), envRefs: { BRAVE_API_KEY: missing } };
  const database = preset('dbhub');
  const absent = { id: 'absent', name: 'Owned absent runtime', command: join(directory, 'absent-runtime.exe'), args: [], enabled: true };
  const tools = await clients.catalog({ mcpServers: [github, brave, database, absent] }, { workspaceRoot: directory }, { connect: true });
  assert.deepEqual(tools, []);
  assert.equal(clients.connections.size, 0);
  const diagnostics = new Map(clients.diagnostics().map(value => [value.serverId, value]));
  for (const id of ['github', 'brave-search']) assert.equal(diagnostics.get(id).code, 'MCP_ENV_MISSING');
  assert.equal(diagnostics.get('dbhub').code, 'MCP_CONFIG_REQUIRED');
  assert.equal(diagnostics.get('absent').code, 'MCP_COMMAND_NOT_FOUND');
  assert.ok([...diagnostics.values()].every(value => value.state === 'error' && value.toolCount === 0));
  assert.ok(!JSON.stringify(diagnostics).includes(directory), 'Diagnostics omit private command locations.');
});

test('reading an enabled official catalog starts no process, transport or network request', async t => {
  const clients = new McpToolClients({ fetch: () => assert.fail('A settings read must not connect.') });
  t.after(() => clients.close());
  clients._connect = () => assert.fail('A settings read must not start any enabled service.');
  assert.deepEqual(await clients.catalog({ mcpServers: curatedMcpPresets.map(preset => preset.server) }, {}, { connect: false }), []);
  assert.equal(clients.connections.size, 0);
  assert.equal(clients.diagnostics().length, 11);
  assert.ok(clients.diagnostics().every(state => state.state === 'disconnected' && state.toolCount === 0));
});

test('passive discovery does not await an unrelated handshake already owned by another request', async t => {
  const clients = new McpToolClients(), server = { id: 'slow-startup', name: 'Slow unrelated service', enabled: true };
  const key = clients._key(server, {});
  clients.connections.set(key, new Promise(() => {}));
  clients.states.set(server.id, { serverId: server.id, state: 'connecting', toolCount: 0 });
  t.after(async () => { clients.connections.delete(key); await clients.close(); });
  clients._connect = () => assert.fail('Passive discovery cannot join the unrequested startup.');
  assert.deepEqual(await clients.catalog({ mcpServers: [server] }, {}, { connect: false }), []);
  const headers = clients.discovery({ mcpServers: [server] }, {});
  assert.equal(headers[0].name, 'mcp.slow-startup');
  assert.equal(headers[0].state, 'connecting');
  assert.deepEqual(headers[0].tools, []);
});

test('independent startup is limited to four at a time and discovery retains configured ordering', async t => {
  const clients = new McpToolClients();
  t.after(() => clients.close());
  const pending = new Map();
  let active = 0, maximum = 0;
  clients._connect = server => {
    active++; maximum = Math.max(maximum, active);
    return new Promise(resolve => pending.set(server.id, () => {
      active--; pending.delete(server.id);
      resolve({ closed: false, tools: [{ name: 'mcp.' + server.id + '.fixture' }] });
    }));
  };
  const servers = Array.from({ length: 9 }, (_, index) => ({ id: 'fixture-' + index, enabled: true }));
  const catalog = clients.catalog({ mcpServers: servers }, {}, { connect: true });
  assert.deepEqual([...pending.keys()], servers.slice(0, 4).map(server => server.id));
  for (const id of [...pending.keys()].reverse()) pending.get(id)();
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual([...pending.keys()], servers.slice(4, 8).map(server => server.id));
  for (const id of [...pending.keys()].reverse()) pending.get(id)();
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual([...pending.keys()], ['fixture-8']);
  pending.get('fixture-8')();
  assert.deepEqual((await catalog).map(tool => tool.name), servers.map(server => 'mcp.' + server.id + '.fixture'));
  assert.equal(maximum, 4); assert.equal(active, 0);
});

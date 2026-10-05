import assert from 'node:assert/strict';
import test from 'node:test';
import { McpToolClients } from '../tools/mcp-client.mjs';

function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

const nextTurn = () => new Promise(resolve => setImmediate(resolve));
const fixtureServer = id => ({ id, name: id, enabled: true });

/** Simulate owned startup/close receipts; never create a transport or process.
 * 模拟自有启动和关闭回执；不创建真实传输或进程。
 */
function controlledClients() {
  const clients = new McpToolClients();
  const started = [], records = [];
  clients._connect = (server, context) => {
    const key = clients._key(server, context);
    if (clients.connections.has(key)) return clients.connections.get(key);
    const startup = deferred(), closing = deferred();
    const record = { serverId: server.id, key, startup, closing, closeCount: 0 };
    const state = { serverId: server.id, state: 'connecting', toolCount: 0 };
    const connection = { serverId: server.id, closed: false,
      tools: [{ name: `mcp.${server.id}.fixture`, serverId: server.id }],
      client: { close: async () => { record.closeCount++; await closing.promise; } } };
    record.connection = connection;
    record.ready = () => { state.state = 'ready'; state.toolCount = 1; startup.resolve(connection); };
    started.push(server.id); records.push(record);
    clients.states.set(server.id, state);
    clients.connections.set(key, startup.promise);
    return startup.promise;
  };
  return { clients, started, records };
}

test('reset owns pending startup and revokes later discovery batches without orphaning processes', async () => {
  const f = controlledClients();
  const servers = Array.from({ length: 9 }, (_, index) => fixtureServer(`fixture-${index}`));
  const discovering = f.clients.catalog({ mcpServers: servers }, {}, { connect: true });
  const revoked = assert.rejects(discovering, { code: 'AGENT_CONFIG_CHANGED' });
  assert.deepEqual(f.started, servers.slice(0, 4).map(server => server.id));
  const resetting = f.clients.reset();
  let resetFinished = false;
  resetting.then(() => { resetFinished = true; });
  for (const record of f.records) record.ready();
  await revoked;
  await nextTurn();
  assert.deepEqual(f.started, servers.slice(0, 4).map(server => server.id), 'reset must prevent the second batch from starting');
  assert.equal(resetFinished, false, 'reset waits for disposal of every owned startup receipt');
  assert.ok(f.records.every(record => record.closeCount === 1 && record.connection.closed));
  for (const record of f.records) record.closing.resolve();
  await resetting;
  assert.equal(f.clients.connections.size, 0);
  assert.equal(f.clients.resetOperation, null);
  assert.deepEqual(f.clients.diagnostics(), []);
  await f.clients.close();
});

test('overlapping resets share cleanup and reject new connect before touching its configuration', async () => {
  const clients = new McpToolClients();
  const closing = deferred();
  let closeCount = 0;
  const connection = { serverId: 'owned', closed: false, client: { close: async () => { closeCount++; await closing.promise; } } };
  clients.connections.set('owned-key', Promise.resolve(connection));
  const first = clients.reset(), second = clients.reset();
  assert.equal(first, second, 'one owner performs overlapping resets');
  const forbiddenConfig = new Proxy({}, { get() { assert.fail('Reset must reject before configuring or spawning a new server.'); } });
  await assert.rejects(clients._connect(forbiddenConfig, {}), { code: 'AGENT_CONFIG_CHANGED' });
  await assert.rejects(clients.catalog({ mcpServers: [] }, {}, { connect: false }), { code: 'AGENT_CONFIG_CHANGED' });
  await nextTurn();
  assert.equal(closeCount, 1);
  closing.resolve();
  await Promise.all([first, second]);
  assert.equal(clients.connections.size, 0);
  assert.deepEqual(await clients.catalog({ mcpServers: [] }, {}, { connect: false }), []);
  await clients.close();
});

test('reset crossing an existing startup prevents a revoked catalog from issuing its refresh RPC', async () => {
  const clients = new McpToolClients(), startup = deferred(), server = fixtureServer('fixture');
  let refreshCount = 0, closeCount = 0;
  const connection = { serverId: server.id, closed: false, tools: [],
    refreshCatalog: async () => { refreshCount++; }, client: { close: async () => { closeCount++; } } };
  clients.connections.set(clients._key(server, {}), startup.promise);
  const discovering = clients.catalog({ mcpServers: [server] }, {}, { connect: true, refresh: true });
  const revoked = assert.rejects(discovering, { code: 'AGENT_CONFIG_CHANGED' });
  const resetting = clients.reset();
  startup.resolve(connection);
  await Promise.all([revoked, resetting]);
  assert.equal(refreshCount, 0, 'old discovery must not send a refresh after its generation is revoked');
  assert.equal(closeCount, 1);
  assert.equal(clients.connections.size, 0);
  await clients.close();
});

test('fresh discovery and explicit reconnect resume after reset without reusing a disposed connection', async () => {
  const f = controlledClients(), server = fixtureServer('fixture');
  const first = f.clients.catalog({ mcpServers: [server] }, {}, { connect: true });
  f.records[0].ready();
  assert.equal((await first).length, 1);
  const resetting = f.clients.reset();
  f.records[0].closing.resolve();
  await resetting;
  const fresh = f.clients.catalog({ mcpServers: [server] }, {}, { connect: true });
  assert.equal(f.records.length, 2);
  f.records[1].ready();
  assert.equal((await fresh)[0].name, 'mcp.fixture.fixture');
  assert.equal(f.records[0].closeCount, 1);
  assert.equal(f.records[1].connection.closed, false);
  f.records[1].closing.resolve();
  const reconnecting = f.clients.reconnect(server, {});
  await nextTurn();
  assert.equal(f.records.length, 3);
  f.records[2].ready();
  assert.equal((await reconnecting).state, 'ready');
  assert.equal(f.records[1].closeCount, 1);
  assert.equal(f.records[2].connection.closed, false);
  f.records[2].closing.resolve();
  await f.clients.close();
  assert.equal(f.records[2].closeCount, 1);
});

test('close during reset waits for the same cleanup and permanently refuses subsequent startup', async () => {
  const clients = new McpToolClients(), closing = deferred();
  let closeCount = 0;
  clients.connections.set('owned-key', Promise.resolve({ serverId: 'owned', closed: false,
    client: { close: async () => { closeCount++; await closing.promise; } } }));
  const resetting = clients.reset();
  const closingClient = clients.close();
  let closeFinished = false;
  closingClient.then(() => { closeFinished = true; });
  const forbiddenConfig = new Proxy({}, { get() { assert.fail('A closed client must not inspect new server configuration.'); } });
  await assert.rejects(clients._connect(forbiddenConfig, {}), { code: 'TOOL_SERVICE_CLOSED' });
  await nextTurn();
  assert.equal(closeCount, 1);
  assert.equal(closeFinished, false);
  closing.resolve();
  await Promise.all([resetting, closingClient]);
  assert.equal(clients.connections.size, 0);
  await assert.rejects(clients.catalog({ mcpServers: [] }, {}, { connect: true }), { code: 'TOOL_SERVICE_CLOSED' });
});

test('a reset crossing explicit reconnect disposes the old connection without starting its replacement', async () => {
  const f = controlledClients(), server = fixtureServer('fixture');
  const initial = f.clients.catalog({ mcpServers: [server] }, {}, { connect: true });
  f.records[0].ready(); await initial;
  const reconnecting = f.clients.reconnect(server, {});
  const revoked = assert.rejects(reconnecting, { code: 'AGENT_CONFIG_CHANGED' });
  await nextTurn();
  const resetting = f.clients.reset();
  f.records[0].closing.resolve();
  await Promise.all([revoked, resetting]);
  assert.equal(f.records.length, 1);
  assert.equal(f.records[0].closeCount, 1);
  assert.equal(f.clients.connections.size, 0);
  await f.clients.close();
});

test('shared failed cleanup retains ownership and prevents further discovery instead of forgetting the process', async () => {
  const clients = new McpToolClients(), closing = deferred();
  let closeCount = 0;
  const connection = { serverId: 'owned', closed: false, client: { close: async () => { closeCount++; await closing.promise; } } };
  clients.connections.set('owned-key', Promise.resolve(connection));
  const first = clients.reset(), second = clients.reset();
  const failure = assert.rejects(first, { code: 'MCP_PROCESS_CLEANUP_FAILED' });
  const sameFailure = assert.rejects(second, { code: 'MCP_PROCESS_CLEANUP_FAILED' });
  assert.equal(first, second);
  await nextTurn(); closing.reject(new Error('Synthetic owned cleanup failed.'));
  await Promise.all([failure, sameFailure]);
  assert.equal(closeCount, 1);
  assert.equal(clients.failedClosures.get('owned-key'), connection);
  assert.equal(clients.connections.size, 1);
  await assert.rejects(clients._connect(fixtureServer('next'), {}), { code: 'MCP_PROCESS_CLEANUP_FAILED' });
  await assert.rejects(clients.close(), { code: 'MCP_PROCESS_CLEANUP_FAILED' });
  assert.equal(closeCount, 1, 'failed ownership stays fenced instead of silently retrying teardown');
});

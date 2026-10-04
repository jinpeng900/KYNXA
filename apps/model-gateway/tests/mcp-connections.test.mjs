import assert from 'node:assert/strict';
import { mkdtemp, writeFile, readFile, rm } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { Readable } from 'node:stream';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';
import { McpServer, createMcpHandler } from '@modelcontextprotocol/server';
import { z } from 'zod';
import { AgentConfigRepository, sameMcpEndpoint } from '../agent-config.mjs';
import { McpToolClients } from '../mcp-client.mjs';
import { createMcpTransport } from '../mcp-transport.mjs';

const fixture = fileURLToPath(new URL('./fixtures/mcp-tool-server.mjs', import.meta.url));
const envelope = args => ({ arguments: args, policy: { reason: 'Use the isolated synthetic MCP server.' } });
const serverConfig = options => ({ id: 'fixture', name: 'Synthetic fixture', command: process.execPath, args: [], enabled: true, ...options });
async function temporary(t) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-mcp-connection-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  return root;
}
async function until(check) {
  const end = Date.now() + 5000;
  while (!await check()) { if (Date.now() > end) assert.fail('Synthetic MCP condition timed out.'); await new Promise(r => setTimeout(r, 30)); }
}

test('connection configuration keeps old stdio, env references, skill disablement and distinct instance identities', async t => {
  const root = await temporary(t), repository = new AgentConfigRepository(root);
  const first = serverConfig({ args: ['one'], cwd: root, env: { MODE: 'test' }, envRefs: { API_TOKEN: 'KYNXA_TEST_TOKEN' } });
  const second = serverConfig({ id: 'fixture-two', args: ['two'] });
  const value = await repository.update({ version: 1, expectedRevision: 0, mcpServers: [first, second], skillDirectories: [], disabledSkills: ['a'.repeat(24)] });
  assert.equal(value.mcpServers[0].transport, 'stdio'); assert.deepEqual(value.disabledSkills, ['a'.repeat(24)]);
  assert.equal(sameMcpEndpoint(first, { ...first, id: 'other', name: 'Other' }), true);
  assert.equal(sameMcpEndpoint(first, { ...first, args: ['two'] }), false);
  assert.equal(sameMcpEndpoint(first, { ...first, cwd: join(root, 'different') }), false);
  assert.equal(sameMcpEndpoint(first, { ...first, startupTimeoutMs: 60000 }), true);
  await assert.rejects(repository.update({ ...value, expectedRevision: 1, mcpServers: [{ ...first, startupTimeoutMs: 120001 }] }), { code: 'INVALID_AGENT_CONFIG' });
  await assert.rejects(repository.update({ ...value, expectedRevision: 1, mcpServers: [first, { ...first, id: 'other' }] }), { code: 'DUPLICATE_MCP_ENDPOINT' });
  await assert.rejects(repository.update({ ...value, expectedRevision: 1, mcpServers: [{ ...first, env: { API_KEY: 'not-persisted' } }] }), { code: 'INVALID_AGENT_CONFIG' });
  assert.equal((await readFile(repository.file, 'utf8')).includes('not-persisted'), false);
  await assert.rejects(repository.update({ ...value, expectedRevision: 1, disabledSkills: ['arbitrary/path'] }), { code: 'INVALID_AGENT_CONFIG' });
  // A configuration produced by the old version is never called corrupt solely
  // because that version permitted duplicate connections.
  // 旧版本曾允许重复连接；不能仅因此将旧版生成的配置判为损坏。
  await writeFile(repository.file, JSON.stringify({ ...value, mcpServers: [first, { ...first, id: 'old-duplicate' }] }));
  assert.equal((await repository.read()).mcpServers.length, 2);
});

test('Windows cmd-backed startup timeout and disconnect reap the exact owned process tree', { skip: process.platform !== 'win32', timeout: 30000 }, async t => {
  const root = await temporary(t), clients = new McpToolClients();
  const command = join(root, 'synthetic-npx.cmd');
  await writeFile(command, `@echo off\r\n"${process.execPath}" %*\r\n`);
  try {
    for (const mode of ['timeout', 'connected']) {
      const pids = join(root, mode + '-pids.json'), log = join(root, mode + '-events.jsonl'), script = join(root, mode + '.mjs');
      await writeFile(script, `import {spawn} from 'node:child_process'; import {writeFile} from 'node:fs/promises';
const child=spawn(process.execPath,['-e','setInterval(()=>{},1000)'],{stdio:'ignore',windowsHide:true});
await writeFile(${JSON.stringify(pids)},JSON.stringify([process.ppid,process.pid,child.pid]));setInterval(()=>{},1000);
${mode === 'connected' ? `await import(${JSON.stringify(new URL('./fixtures/mcp-tool-server.mjs', import.meta.url).href)});` : ''}`);
      const server = serverConfig({ id: mode, command, args: [script, log], cwd: root, startupTimeoutMs: mode === 'timeout' ? 1000 : 15000 });
      const tools = await clients.catalog({ mcpServers: [server] }, {}, { connect: true });
      if (mode === 'connected') { assert.ok(tools.some(tool => tool.toolName === 'echo')); await clients.disconnect(server.id); }
      else { assert.deepEqual(tools, []); assert.equal(clients.diagnostics().find(item => item.serverId === mode).code, 'MCP_TIMEOUT'); }
      const owned = JSON.parse(await readFile(pids, 'utf8'));
      await until(() => owned.every(pid => { try { process.kill(pid, 0); return false; } catch (error) { return error.code === 'ESRCH'; } }));
    }
  } finally { await clients.close(); }
});

test('HTTP URL and header validation reject credentials, remote cleartext and duplicate endpoints', async t => {
  const root = await temporary(t), repository = new AgentConfigRepository(root);
  const update = (servers, revision = 0) => repository.update({ version: 1, expectedRevision: revision, skillDirectories: [], mcpServers: servers });
  for (const url of ['http://example.com/mcp', 'https://user:secret@example.com/mcp', 'https://example.com/mcp?key=secret', 'https://example.com/mcp#secret'])
    await assert.rejects(update([serverConfig({ transport: 'streamable-http', url })]), { code: 'INVALID_AGENT_CONFIG' });
  await assert.rejects(update([serverConfig({ transport: 'streamable-http', url: 'https://example.com/mcp', headerEnv: { Host: 'HOST_VALUE' } })]), { code: 'INVALID_AGENT_CONFIG' });
  const server = serverConfig({ transport: 'streamable-http', url: 'http://127.0.0.1:9999/mcp', headerEnv: { Authorization: 'TOKEN_HEADER' } });
  const saved = await update([server]); assert.equal(saved.mcpServers[0].command, '');
  await assert.rejects(update([server, { ...server, id: 'other' }], 1), { code: 'DUPLICATE_MCP_ENDPOINT' });
});

test('nullable fields from the desktop JSON DTO retain stdio and HTTP connection defaults', async t => {
  const root = await temporary(t), repository = new AgentConfigRepository(root);
  const stdio = serverConfig({ transport: 'stdio', cwd: null, env: null, envRefs: null, url: null, headerEnv: null, auth: null });
  const http = serverConfig({ id: 'desktop-http', transport: 'streamable-http', url: 'https://resource.example/mcp',
    cwd: null, env: null, envRefs: null, headerEnv: null, auth: { type: 'oauth-client-credentials',
      clientId: 'synthetic-client', clientSecretEnv: 'KYNXA_TEST_OAUTH_SECRET', issuer: 'https://issuer.example', scope: null } });
  const config = await repository.update({ version: 1, expectedRevision: 0, skillDirectories: [], mcpServers: [stdio, http] });
  assert.deepEqual(config.mcpServers[0].env, {}); assert.deepEqual(config.mcpServers[0].envRefs, {});
  assert.deepEqual(config.mcpServers[1].headerEnv, {}); assert.equal(Object.hasOwn(config.mcpServers[1].auth, 'scope'), false);
});

test('real SDK stdio applies configured cwd/env references and supports Windows cmd executable resolution', async t => {
  const root = await temporary(t), log = join(root, 'started.jsonl'), marker = `KYNXA_MCP_REF_${process.pid}`;
  process.env[marker] = 'synthetic-private-value'; t.after(() => { delete process.env[marker]; });
  const script = join(root, 'environment.mjs');
  await writeFile(script, `import {writeFile} from 'node:fs/promises'; await writeFile(${JSON.stringify(join(root, 'environment.json'))},JSON.stringify({cwd:process.cwd(),mode:process.env.MODE,credential:process.env.API_TOKEN})); await import(${JSON.stringify(new URL('./fixtures/mcp-tool-server.mjs', import.meta.url).href)});`);
  const server = serverConfig({ args: [script, log], cwd: root, env: { MODE: 'synthetic' }, envRefs: { API_TOKEN: marker } });
  if (process.platform === 'win32') {
    const command = join(root, 'npx.cmd');
    await writeFile(command, `@echo off\r\n"${process.execPath}" %*\r\n`);
    server.command = command;
  }
  const clients = new McpToolClients(); t.after(() => clients.close());
  try {
    const tools = await clients.catalog({ mcpServers: [server] }, { workspaceRoot: join(root, 'nonexistent-workspace') }, { connect: true });
    assert.ok(tools.some(tool => tool.toolName === 'echo'), JSON.stringify(clients.diagnostics()));
    assert.deepEqual(JSON.parse(await readFile(join(root, 'environment.json'), 'utf8')), { cwd: root, mode: 'synthetic', credential: 'synthetic-private-value' });
    assert.equal(JSON.stringify(clients.diagnostics()).includes('synthetic-private-value'), false);
    assert.equal(JSON.stringify(clients.diagnostics()).includes(marker), false);
  } finally { await clients.close(); }
});

async function httpFixture(t, { rejectCalls = false, resourceTtlMs = 0 } = {}) {
  let extra = false, schemaChanged = false, resourceText = 'synthetic-resource', calls = 0;
  const seen = [];
  const handler = createMcpHandler(() => {
    const server = new McpServer({ name: 'synthetic-http', version: '1.0.0' });
    server.registerTool('echo', { inputSchema: z.object({ value: schemaChanged ? z.number() : z.string() }) }, async ({ value }) => {
      calls++; return { content: [{ type: 'text', text: String(value) }] };
    });
    if (extra) server.registerTool('added', { inputSchema: z.object({}) }, async () => ({ content: [{ type: 'text', text: 'added' }] }));
    server.registerResource('note', 'synthetic://note', { mimeType: 'text/plain' }, async uri => ({
      contents: [{ uri: uri.href, text: resourceText, mimeType: 'text/plain' }], _meta: { private: 'synthetic-hidden' },
      ...(resourceTtlMs ? { ttlMs: resourceTtlMs, cacheScope: 'private' } : {})
    }));
    return server;
  }, { keepAliveMs: 0 });
  const socket = createServer(async (request, response) => {
    const controller = new AbortController(); response.on('close', () => controller.abort());
    const chunks = []; for await (const chunk of request) chunks.push(chunk);
    const body = Buffer.concat(chunks), parsed = body.length ? JSON.parse(body.toString()) : null;
    seen.push({ method: parsed?.method, headers: request.headers });
    if (rejectCalls && ['tools/call', 'server/call'].includes(parsed?.method)) { calls++; response.writeHead(401); response.end('synthetic-only'); return; }
    const source = new Request(`http://${request.headers.host}${request.url}`, { method: request.method, headers: request.headers,
      ...(body.length ? { body } : {}), signal: controller.signal });
    try {
      const result = await handler.fetch(source);
      response.writeHead(result.status, Object.fromEntries(result.headers));
      if (result.body) Readable.fromWeb(result.body).pipe(response); else response.end();
    } catch { if (!response.headersSent) response.writeHead(500); response.end(); }
  });
  await new Promise(resolve => socket.listen(0, '127.0.0.1', resolve));
  t.after(async () => { await handler.close(); socket.closeAllConnections(); await new Promise(resolve => socket.close(resolve)); });
  return { url: `http://127.0.0.1:${socket.address().port}/mcp`, seen, handler, add: () => { extra = true; handler.notify.toolsChanged(); },
    changeSchema: () => { schemaChanged = true; }, setResourceText: value => { resourceText = value; }, calls: () => calls };
}

test('explicit live directory refresh discovers schema changes and blocks an old prepared call', async t => {
  const fixture = await httpFixture(t), clients = new McpToolClients();
  t.after(() => clients.close());
  const config = { mcpServers: [serverConfig({ transport: 'streamable-http', command: '', url: fixture.url, protocolVersion: '2026-07-28' })] };
  const before = (await clients.catalog(config, {}, { connect: true })).find(tool => tool.toolName === 'echo');
  fixture.changeSchema();
  const after = (await clients.catalog(config, {}, { connect: true, refresh: true })).find(tool => tool.toolName === 'echo');
  assert.equal(before.originalInputSchema.properties.value.type, 'string');
  assert.equal(after.originalInputSchema.properties.value.type, 'number');
  await assert.rejects(clients.execute(before, envelope({ value: 'old-approved-argument' })), { code: 'MCP_CATALOG_CHANGED' });
  assert.equal(fixture.calls(), 0);
  assert.equal((await clients.execute(after, envelope({ value: 7 }))).content, '7');
  assert.equal(fixture.calls(), 1);
});

test('explicit resource reads refresh a cached same-URI SDK result and return updated content', async t => {
  const fixture = await httpFixture(t, { resourceTtlMs: 60000 }), clients = new McpToolClients();
  t.after(() => clients.close());
  const config = { mcpServers: [serverConfig({ transport: 'streamable-http', command: '', url: fixture.url, protocolVersion: '2026-07-28' })] };
  const descriptor = (await clients.catalog(config, {}, { connect: true })).find(tool => tool.operation === 'resources/read');
  const uri = 'synthetic://note', reads = () => fixture.seen.filter(item => item.method === 'resources/read').length;
  assert.equal((await clients.execute(descriptor, envelope({ uri }))).canonical.content[0].resource.text, 'synthetic-resource');
  assert.equal(reads(), 1);
  fixture.setResourceText('resource-updated-without-notification');
  const connection = await [...clients.connections.values()][0];
  assert.equal((await connection.client.readResource({ uri })).contents[0].text, 'synthetic-resource',
    'the real SDK cache is still fresh, so this regression exercises a cache hit');
  assert.equal(reads(), 1);
  assert.equal((await clients.execute(descriptor, envelope({ uri }))).canonical.content[0].resource.text, 'resource-updated-without-notification');
  assert.equal(reads(), 2, 'the broker forces a fresh network read despite the server TTL');
  fixture.setResourceText('resource-updated-again');
  assert.equal((await clients.execute(descriptor, envelope({ uri }))).canonical.content[0].resource.text, 'resource-updated-again');
  assert.equal(reads(), 3);
});

test('real loopback HTTP uses env headers, discovers resource wrappers, refreshes notifications and reconnects explicitly', async t => {
  const fixture = await httpFixture(t), variable = `KYNXA_MCP_HEADER_${process.pid}`;
  process.env[variable] = 'synthetic-header-secret'; t.after(() => { delete process.env[variable]; });
  const server = serverConfig({ transport: 'streamable-http', url: fixture.url, headerEnv: { 'X-Test-Key': variable }, protocolVersion: '2026-07-28' });
  const clients = new McpToolClients(); t.after(() => clients.close()); const config = { mcpServers: [server] };
  assert.deepEqual(await clients.catalog(config, {}, {}), []); assert.equal(fixture.seen.length, 0);
  const tools = await clients.catalog(config, {}, { connect: true }); assert.ok(tools.some(tool => tool.toolName === 'echo'), JSON.stringify(clients.diagnostics()));
  const list = tools.find(tool => tool.operation === 'resources/list'), read = tools.find(tool => tool.operation === 'resources/read');
  assert.ok(list); assert.ok(read);
  await assert.rejects(clients.execute({ ...list, operation: 'tools/call' }, envelope({})), { code: 'MCP_CATALOG_CHANGED' });
  await assert.rejects(clients.execute({ ...list, toolName: 'echo' }, envelope({})), { code: 'MCP_CATALOG_CHANGED' });
  const listing = await clients.execute(list, envelope({})); assert.equal(listing.canonical.structuredContent.resources[0].uri, 'synthetic://note');
  const result = await clients.execute(read, envelope({ uri: 'synthetic://note' }));
  assert.equal(result.canonical.content[0].resource.text, 'synthetic-resource'); assert.equal(result.content.includes('synthetic-hidden'), false);
  assert.ok(fixture.seen.every(request => request.headers['x-test-key'] === 'synthetic-header-secret'));
  fixture.add(); await until(async () => (await clients.catalog(config, {})).some(tool => tool.toolName === 'added'));
  assert.ok(clients.diagnostics()[0].generation > 1);
  assert.equal(JSON.stringify(clients.diagnostics()).includes('synthetic-header-secret'), false);
  const old = tools.find(tool => tool.toolName === 'echo'); await clients.disconnect(server.id);
  assert.deepEqual(await clients.catalog(config, {}), []); await assert.rejects(clients.execute(old, envelope({ value: 'not-replayed' })), { code: 'MCP_NOT_CONNECTED' });
  await clients.reconnect(server, {}); const fresh = (await clients.catalog(config, {})).find(tool => tool.toolName === 'echo');
  assert.equal((await clients.execute(fresh, envelope({ value: 'once' }))).content, 'once'); assert.equal(fixture.calls(), 1);
});

test('a stopped stdio process removes stale tools and passive discovery never restarts it', async t => {
  const root = await temporary(t), log = join(root, 'started.jsonl');
  const clients = new McpToolClients(), server = serverConfig({ args: [fixture, log] }), config = { mcpServers: [server] };
  try {
    const tools = await clients.catalog(config, {}, { connect: true });
    const connection = await [...clients.connections.values()][0]; process.kill(connection.transport.pid);
    await until(() => clients.connections.size === 0);
    assert.equal(clients.diagnostics()[0].state, 'disconnected');
    assert.equal(clients.diagnostics()[0].code, 'MCP_CONNECTION_LOST');
    assert.deepEqual(await clients.catalog(config, {}), []);
    await assert.rejects(clients.execute(tools[0], envelope({ value: 'not-replayed' })), { code: 'MCP_NOT_CONNECTED' });
    let events = (await readFile(log, 'utf8')).trim().split('\n').map(JSON.parse);
    assert.equal(events.filter(event => event.event === 'started').length, 1);
    await clients.reconnect(server, {});
    events = (await readFile(log, 'utf8')).trim().split('\n').map(JSON.parse);
    assert.equal(events.filter(event => event.event === 'started').length, 2);
  } finally { await clients.close(); }
});

test('HTTP unauthorized calls are not replayed, and auth diagnostics never expose response text or credentials', async t => {
  const fixture = await httpFixture(t, { rejectCalls: true }), variable = `KYNXA_MCP_BEARER_${process.pid}`;
  process.env[variable] = 'synthetic-bearer-secret'; t.after(() => { delete process.env[variable]; });
  const server = serverConfig({ transport: 'streamable-http', url: fixture.url, auth: { type: 'bearer-env', tokenEnv: variable } });
  const clients = new McpToolClients(); t.after(() => clients.close());
  const tool = (await clients.catalog({ mcpServers: [server] }, {}, { connect: true })).find(tool => tool.toolName === 'echo');
  await assert.rejects(clients.execute(tool, envelope({ value: 'call-once' })), { code: 'MCP_AUTH_REQUIRED' });
  assert.equal(fixture.calls(), 1); assert.equal(clients.diagnostics()[0].state, 'auth-required');
  assert.equal(JSON.stringify(clients.diagnostics()).includes('synthetic-bearer-secret'), false);
  assert.equal(JSON.stringify(clients.diagnostics()).includes('synthetic-only'), false);
});

test('missing env references and HTTP redirects fail with bounded codes without leaking configured values', async t => {
  const clients = new McpToolClients(); t.after(() => clients.close());
  const missing = serverConfig({ transport: 'streamable-http', url: 'http://127.0.0.1:9/mcp', auth: { type: 'bearer-env', tokenEnv: 'KYNXA_TEST_NONEXISTENT_TOKEN' } });
  assert.deepEqual(await clients.catalog({ mcpServers: [missing] }, {}, { connect: true }), []);
  assert.equal(clients.diagnostics()[0].code, 'MCP_ENV_MISSING');
  let targetCalls = 0;
  const target = createServer((_request, response) => { targetCalls++; response.end(); });
  await new Promise(resolve => target.listen(0, '127.0.0.1', resolve));
  const redirect = createServer((_request, response) => { response.writeHead(307, { Location: `http://127.0.0.1:${target.address().port}/mcp` }); response.end(); });
  await new Promise(resolve => redirect.listen(0, '127.0.0.1', resolve));
  t.after(() => { redirect.closeAllConnections(); target.closeAllConnections(); redirect.close(); target.close(); });
  const value = serverConfig({ id: 'redirect', transport: 'streamable-http', url: `http://127.0.0.1:${redirect.address().port}/mcp` });
  assert.deepEqual(await clients.catalog({ mcpServers: [value] }, {}, { connect: true }), []); assert.equal(targetCalls, 0);
});

test('SDK client-credentials auth uses configured issuer binding, env secret and resource indicator without interactive flow', async () => {
  const variable = `KYNXA_MCP_OAUTH_${process.pid}`, resource = 'https://resource.example/mcp', issuer = 'https://issuer.example';
  process.env[variable] = 'synthetic-oauth-secret';
  const requests = [];
  const fetch = async (input, init = {}) => {
    const url = String(input), headers = new Headers(init.headers); requests.push({ url, init });
    if (url.startsWith(resource) || url.includes('oauth-protected-resource'))
      return Response.json({ resource, authorization_servers: [issuer] });
    if (url.includes('oauth-authorization-server') || url.includes('openid-configuration'))
      return Response.json({ issuer, authorization_endpoint: issuer + '/authorize', token_endpoint: issuer + '/token', token_endpoint_auth_methods_supported: ['client_secret_basic'], grant_types_supported: ['client_credentials'], response_types_supported: ['code'] });
    if (url === issuer + '/token') {
      const body = new URLSearchParams(init.body); assert.equal(body.get('grant_type'), 'client_credentials'); assert.equal(body.get('resource'), resource);
      assert.equal(headers.get('authorization'), 'Basic ' + Buffer.from('synthetic-client:synthetic-oauth-secret').toString('base64'));
      return Response.json({ access_token: 'synthetic-access-token', token_type: 'Bearer', expires_in: 3600 });
    }
    assert.fail('Unexpected auth URL: ' + url);
  };
  try {
    const transport = await createMcpTransport(serverConfig({ transport: 'streamable-http', url: resource,
      auth: { type: 'oauth-client-credentials', clientId: 'synthetic-client', clientSecretEnv: variable, issuer } }), {}, { fetch });
    assert.ok(transport); assert.ok(requests.some(request => request.url === issuer + '/token')); await transport.close();
  } finally { delete process.env[variable]; }
});

test('SDK OAuth discovery cannot send a client secret to a mismatched issuer or an unconfigured origin', async () => {
  const variable = `KYNXA_MCP_OAUTH_BINDING_${process.pid}`, resource = 'https://resource.example/mcp', issuer = 'https://issuer.example';
  process.env[variable] = 'synthetic-never-sent-secret';
  let tokens = 0;
  try {
    for (const malicious of ['https://issuer.example/other', 'https://unconfigured.example']) {
      const fetch = async (input, init = {}) => {
        const url = String(input);
        if (url.includes('oauth-protected-resource')) return Response.json({ resource, authorization_servers: [malicious] });
        if (url.includes('oauth-authorization-server') || url.includes('openid-configuration'))
          return Response.json({ issuer: malicious, authorization_endpoint: malicious + '/authorize', token_endpoint: malicious + '/token',
            token_endpoint_auth_methods_supported: ['client_secret_basic'], grant_types_supported: ['client_credentials'], response_types_supported: ['code'] });
        if (String(init.body).includes('client_credentials')) tokens++;
        return Response.json({ access_token: 'should-not-occur', token_type: 'Bearer' });
      };
      await assert.rejects(createMcpTransport(serverConfig({ transport: 'streamable-http', url: resource,
        auth: { type: 'oauth-client-credentials', clientId: 'synthetic', clientSecretEnv: variable, issuer } }), {}, { fetch }));
    }
    assert.equal(tokens, 0);
  } finally { delete process.env[variable]; }
});

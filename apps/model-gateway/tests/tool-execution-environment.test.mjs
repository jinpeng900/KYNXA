import assert from 'node:assert/strict';
import { test } from 'node:test';
import { resolveToolExecutionEnvironment } from '../tools/tool-execution-environment.mjs';
import { normalizeToolExecutionEnvironment } from '../platform/tool-execution-environment.mjs';

const builtin = name => ({ name, source: 'builtin' });
const external = id => ({ name: `mcp.${id}.fetch`, source: `mcp:${id}`, serverId: id, operation: 'tools/call' });

test('builtin fetch, host terminal and desktop run on the gateway host without claiming the user device or proxy', () => {
  for (const [name, kind, requestOrigin] of [['web.fetch', 'builtin-http', 'gateway-host'],
    ['terminal.host.run', 'host-terminal', 'tool-defined-unknown'], ['computer.launch', 'desktop-tool-host', 'unknown']]) {
    const value = resolveToolExecutionEnvironment(builtin(name));
    assert.equal(value.executorKind, kind);
    assert.equal(value.executorLocation, 'gateway-host');
    assert.equal(value.operationLocation, 'gateway-host');
    assert.equal(value.userDeviceRelationship, 'unverified');
    assert.equal(value.network.requestOrigin, requestOrigin);
    assert.equal(value.network.proxy, 'unknown');
    assert.equal(value.network.egress, 'unknown');
    assert.equal(value.grantsPermission, false);
    assert.equal(value.network.sameEgressDoesNotProveSameMachine, true);
  }
});

test('AppContainer provenance requires trusted runtime capability evidence and never becomes host execution', () => {
  const capabilities = { available: true, sandbox: 'appcontainer', failClosed: true, checksChildToken: true, network: false };
  for (const name of ['terminal.run', 'skill.run']) {
    const value = resolveToolExecutionEnvironment(builtin(name), { context: { sandboxCapabilities: capabilities } });
    assert.equal(value.isolation, 'windows-appcontainer');
    assert.equal(value.sandboxVerified, true);
    assert.equal(value.network.requestOrigin, 'unavailable');
    assert.equal(value.executorKind, 'sandbox-terminal');
    const unverified = resolveToolExecutionEnvironment({ ...builtin(name), metadata: { sandboxVerified: true } });
    assert.equal(unverified.sandboxVerified, false);
    assert.equal(unverified.network.requestOrigin, 'unknown');
  }
});

test('stdio provenance marks only the spawned transport process even when a configured wrapper launches SSH', () => {
  const server = { id: 'fetch', enabled: true, transport: 'stdio', command: 'C:\\Private\\ssh.exe',
    args: ['private-host', 'private-script'], env: { PRIVATE_VALUE: 'sensitive-value' } };
  const value = resolveToolExecutionEnvironment({ ...external('fetch'), description: 'This is on the user computer.' }, { servers: [server] });
  assert.equal(value.executorLocation, 'gateway-host');
  assert.equal(value.locationScope, 'configured-transport-process');
  assert.equal(value.operationLocation, 'unknown');
  assert.equal(value.network.requestOrigin, 'tool-defined-unknown');
  const serialized = JSON.stringify(value);
  for (const secret of ['Private', 'private-host', 'private-script', 'sensitive-value']) assert.ok(!serialized.includes(secret));
});

test('HTTP metadata identifies the service endpoint without exposing the URL or claiming its downstream origin', () => {
  const server = { id: 'fetch', transport: 'streamable-http', enabled: true,
    url: 'https://private.example/mcp?token=private-token', headerEnv: { Authorization: 'PRIVATE_AUTH_VAR' } };
  const value = resolveToolExecutionEnvironment(external('fetch'), { servers: new Map([['fetch', server]]) });
  assert.equal(value.executorLocation, 'remote-service');
  assert.equal(value.locationScope, 'configured-service-endpoint');
  assert.equal(value.serviceMachineIdentity, 'unknown');
  assert.equal(value.operationLocation, 'unknown');
  const serialized = JSON.stringify(value);
  for (const secret of ['private.example', 'private-token', 'PRIVATE_AUTH_VAR', 'Authorization']) assert.ok(!serialized.includes(secret));
});

test('tool names, metadata and mismatched server identities cannot manufacture configured ownership', () => {
  const server = { id: 'fetch', enabled: true, transport: 'stdio', command: 'fetch-server', args: [] };
  for (const descriptor of [{ ...external('fetch'), source: 'mcp:other' },
    { ...external('fetch'), name: 'web.fetch' },
    { ...external('missing'), metadata: { executorLocation: 'gateway-host' } },
    { name: 'web.fetch', description: 'Built in on the gateway', metadata: { source: 'builtin' } }]) {
    assert.equal(resolveToolExecutionEnvironment(descriptor, { servers: [server] }).executorLocation, 'unknown');
  }
  assert.equal(resolveToolExecutionEnvironment(external('fetch'), { servers: [{ ...server, enabled: false }] }).executorLocation, 'unknown');
});

test('browser targets use trusted connection settings without turning a local debug endpoint into a cloud browser', () => {
  const server = { id: 'browser', transport: 'stdio', enabled: true, command: 'npx',
    args: ['chrome-devtools-mcp@1.10.1', '--headless', '--isolated'] };
  const descriptor = external('browser');
  const local = resolveToolExecutionEnvironment(descriptor, { servers: [server] });
  assert.equal(local.browserTarget.location, 'gateway-host');
  assert.equal(local.browserTarget.visibility, 'headless');
  const endpoint = resolveToolExecutionEnvironment(descriptor, { servers: [{ ...server,
    args: ['chrome-devtools-mcp@1.10.1', '--browserUrl=http://127.0.0.1:9222/private-token'] }] });
  assert.equal(endpoint.browserTarget.location, 'unknown');
  assert.equal(endpoint.browserTarget.mode, 'remote-browser');
  assert.ok(!JSON.stringify(endpoint).includes('9222'));
  const custom = resolveToolExecutionEnvironment(descriptor, { servers: [{ ...server,
    args: ['chrome-devtools-mcp@1.10.1', '--config=C:\\Private\\config.json'] }] });
  assert.equal(custom.browserTarget.location, 'unknown');
});

test('explicit verified browser target binding is separate from untrusted descriptor metadata', () => {
  const server = { id: 'browser', transport: 'streamable-http', enabled: true, url: 'https://private.example/mcp' };
  const descriptor = { ...external('browser'), metadata: { browserTarget: { location: 'gateway-host' } } };
  const unbound = resolveToolExecutionEnvironment(descriptor, { servers: [server] });
  assert.equal(unbound.browserTarget, undefined);
  const bound = resolveToolExecutionEnvironment(descriptor, { servers: [server],
    trustedBrowserTargets: new Map([['browser', { location: 'remote-browser', secret: 'do-not-export' }]]) });
  assert.equal(bound.browserTarget.location, 'remote-browser');
  assert.equal(bound.browserTarget.evidence, 'application-verified-browser-target');
  assert.ok(!JSON.stringify(bound).includes('do-not-export'));
  assert.equal(Object.isFrozen(bound), true);
  assert.equal(Object.isFrozen(bound.network), true);
  assert.equal(Object.isFrozen(bound.browserTarget), true);
});

test('normalization copies only fixed provenance fields and drops extra keys without accepting authorization', () => {
  const original = resolveToolExecutionEnvironment(builtin('web.fetch'));
  const normalized = normalizeToolExecutionEnvironment({ ...original, endpoint: 'https://private.example/token',
    env: { PRIVATE_KEY: 'sensitive-key' }, currentAuthorization: 'approved',
    network: { ...original.network, proxyUrl: 'http://private-proxy', observedIp: 'private-address' },
    browserTarget: { location: 'unknown', evidence: 'trusted-browser-configuration',
      endpoint: 'wss://private.example/cdp?token=secret' } });
  assert.equal(normalized.grantsPermission, false);
  assert.equal(normalized.network.egress, 'unknown');
  assert.equal(normalized.endpoint, undefined);
  assert.equal(normalized.currentAuthorization, undefined);
  assert.equal(normalized.network.observedIp, undefined);
  assert.equal(normalized.browserTarget.endpoint, undefined);
  assert.ok(Buffer.byteLength(JSON.stringify(normalized), 'utf8') < 1024);
  assert.ok(!JSON.stringify(normalized).includes('private'));
  assert.notEqual(normalized.network, original.network);
});

test('invalid enums, credentials in known fields and a permission grant cannot survive normalization', () => {
  const original = resolveToolExecutionEnvironment(builtin('web.fetch'));
  for (const input of [null, [], { ...original, schemaVersion: 2 }, { ...original, grantsPermission: true },
    { ...original, executorLocation: 'C:\\Private\\machine' },
    { ...original, executorKind: 'private-model-description' },
    { ...original, network: { ...original.network, egress: 'private-ip' } },
    { ...original, network: { ...original.network, proxy: 'http://private-proxy' } },
    { ...original, browserTarget: { location: 'unknown', evidence: 'server-says-trusted' } },
    { ...original, sandboxVerified: true }]) {
    assert.equal(normalizeToolExecutionEnvironment(input), undefined);
  }
});

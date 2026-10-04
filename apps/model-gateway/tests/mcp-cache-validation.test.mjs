import assert from 'node:assert/strict';
import { test } from 'node:test';
import { McpToolClients } from '../mcp-client.mjs';

const envelope = argumentsValue => ({ arguments: argumentsValue, policy: { reason: 'Inspect the isolated public observation.' } });

function fixture() {
  let calls = 0;
  const requests = [];
  const clients = new McpToolClients({ fetch: () => assert.fail('Revalidation must not connect or fetch.') });
  const descriptor = { key: 'synthetic-connected', name: 'mcp.synthetic.search', serverId: 'synthetic',
    operation: 'tools/call', toolName: 'search', originalInputSchema: { type: 'object', properties: { query: { type: 'string' } } } };
  const result = { content: [{ type: 'text', text: 'A public source.' },
    { type: 'image', mimeType: 'image/png', data: 'AA==' }],
    structuredContent: { sources: ['https://example.invalid/source'] }, _meta: { private: 'synthetic-only-private' } };
  const connection = { closed: false, tools: [structuredClone(descriptor)], client: {
    callTool: async (request, options) => { calls++; requests.push({ request, options }); return result; }
  } };
  clients.connections.set(descriptor.key, Promise.resolve(connection));
  return { clients, descriptor, connection, result, requests, calls: () => calls };
}

test('MCP cache revalidation performs no RPC and normal execution uses the same check once', async () => {
  const f = fixture(), input = envelope({ query: 'dated evidence', reason: 7 });
  const prepared = await f.clients.validateExecution(f.descriptor, input);
  assert.equal(prepared.connection, f.connection);
  assert.deepEqual(prepared.args, input.arguments);
  prepared.args.query = 'Changed only inside the prepared copy';
  assert.equal(input.arguments.query, 'dated evidence');
  assert.equal(f.calls(), 0);
  const validate = f.clients.validateExecution.bind(f.clients);
  let validations = 0;
  f.clients.validateExecution = (...args) => { validations++; return validate(...args); };
  const controller = new AbortController();
  const output = await f.clients.execute(f.descriptor, input, controller.signal);
  assert.equal(validations, 1);
  assert.equal(f.calls(), 1);
  assert.deepEqual(f.requests[0].request, { name: 'search', arguments: input.arguments });
  assert.equal(f.requests[0].options.signal, controller.signal);
  assert.equal(f.requests[0].options.timeout, 30000);
  assert.equal(f.requests[0].options.maxTotalTimeout, 30000);
  assert.equal(f.requests[0].options.cacheMode, 'refresh');
  assert.equal(f.requests[0].options.allowInputRequired, true);
  assert.deepEqual(output.canonical, f.result, 'Typed results and private metadata remain owned by the existing result pipeline.');
  assert.equal(output.content.includes('synthetic-only-private'), false);
});

test('MCP cache revalidation rejects malformed envelopes before an observation may be reused', async () => {
  const f = fixture();
  for (const [input, code] of [
    [{ ...envelope({ query: 'source' }), reason: 'business fields cannot escape the envelope' }, 'INVALID_MCP_ENVELOPE'],
    [{ arguments: { query: 'source' }, policy: { reason: '' } }, 'OUTSIDE_WORKSPACE_REASON_REQUIRED'],
    [{ arguments: { query: 'source' }, policy: { reason: 'valid', grant: true } }, 'INVALID_MCP_ENVELOPE']
  ]) {
    await assert.rejects(f.clients.validateExecution(f.descriptor, input), { code });
    await assert.rejects(f.clients.execute(f.descriptor, input), { code });
  }
  assert.equal(f.calls(), 0);
});

test('MCP cache revalidation rejects changed schemas, operations, names and removed tools without reconnecting', async () => {
  const f = fixture(), input = envelope({ query: 'source' });
  for (const current of [
    { ...f.descriptor, originalInputSchema: { type: 'object', properties: { query: { type: 'number' } } } },
    { ...f.descriptor, operation: 'resources/read' },
    { ...f.descriptor, toolName: 'replacement' },
    null
  ]) {
    f.connection.tools = current ? [current] : [];
    await assert.rejects(f.clients.validateExecution(f.descriptor, input), { code: 'MCP_CATALOG_CHANGED' });
    await assert.rejects(f.clients.execute(f.descriptor, input), { code: 'MCP_CATALOG_CHANGED' });
  }
  assert.equal(f.calls(), 0);
});

test('MCP cache revalidation rejects disconnected and closed connections rather than restarting them', async () => {
  const f = fixture(), input = envelope({ query: 'source' });
  f.connection.closed = true;
  await assert.rejects(f.clients.validateExecution(f.descriptor, input), { code: 'MCP_NOT_CONNECTED' });
  await assert.rejects(f.clients.execute(f.descriptor, input), { code: 'MCP_NOT_CONNECTED' });
  f.clients.connections.clear();
  await assert.rejects(f.clients.validateExecution(f.descriptor, input), { code: 'MCP_NOT_CONNECTED' });
  assert.equal(f.clients.connections.size, 0);
  assert.equal(f.calls(), 0);
});

test('MCP cleanup failure blocks both cached observation revalidation and actual calls', async () => {
  const f = fixture(), input = envelope({ query: 'source' });
  f.clients.failedClosures.set('synthetic-cleanup', {});
  await assert.rejects(f.clients.validateExecution(f.descriptor, input), { code: 'MCP_PROCESS_CLEANUP_FAILED' });
  await assert.rejects(f.clients.execute(f.descriptor, input), { code: 'MCP_PROCESS_CLEANUP_FAILED' });
  assert.equal(f.calls(), 0);
});

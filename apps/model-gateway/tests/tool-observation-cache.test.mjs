import assert from 'node:assert/strict';
import { mkdir, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from 'node:test';
import { RequestObservationCache } from '../tools/tool-observations.mjs';
import { approve, pendingApproval, toolFixture } from './tool-fixture.mjs';

const searchName = 'mcp.exa.web_search_exa';
const envelope = (query = 'dated official evidence', reason = 'Read a public source.', extra = {}) =>
  ({ arguments: { query, ...extra }, policy: { reason } });
const observation = (text = 'https://example.invalid/source', error = false) => ({
  canonical: { content: [{ type: 'text', text }], structuredContent: { sources: [text] },
    isError: error, _meta: { privateFixture: 'synthetic-private-observation' } },
  content: text, isError: error, outsideWorkspace: true
});

async function fixture(t) {
  const f = await toolFixture(t);
  let calls = 0, validations = 0, validationCode = null;
  let connection = { closed: false };
  let produce = () => observation();
  const descriptor = { name: searchName, source: 'mcp:exa', key: 'synthetic-search', toolName: 'web_search_exa',
    operation: 'tools/call', inputSchema: { type: 'object' }, originalInputSchema: { type: 'object' } };
  const install = context => {
    f.service.catalogs.set(context, { generation: f.service.configGeneration,
      descriptors: new Map([[searchName, descriptor], ['mcp.synthetic.custom', { ...descriptor, name: 'mcp.synthetic.custom', source: 'mcp:synthetic' }]]) });
    return context;
  };
  f.service.mcp.execute = async (...args) => { calls++; return produce(...args); };
  f.service.mcp.validateExecution = async () => {
    validations++;
    if (validationCode) throw Object.assign(new Error('Synthetic stale connection or catalog.'), { code: validationCode });
    return { connection, args: {} };
  };
  const context = async (permissionMode = 'full', conversationId) => install(await f.context(permissionMode, conversationId));
  return { ...f, context, install, calls: () => calls, validations: () => validations,
    failValidation: code => { validationCode = code; }, produce: value => { produce = value; },
    connection: () => connection, reconnect: () => { connection = { closed: false }; } };
}

test('request-scoped observation reuse still binds each complete result to its own actual call', async t => {
  const f = await fixture(t), context = await f.context();
  const firstCall = f.call(searchName, envelope());
  const secondCall = f.call(searchName, envelope('dated official evidence', 'Confirm this already observed source.'));
  const first = await f.service.execute(context, firstCall);
  const second = await f.service.execute(context, secondCall);
  assert.equal(first.isError, false, first.code + ': ' + first.content); assert.equal(second.isError, false);
  assert.equal(f.calls(), 1); assert.equal(f.validations(), 2);
  assert.equal(second.reused, true);
  assert.match(second.content, /^\[KYNXA_OBSERVATION_REUSED\]/);
  assert.equal(second.content.includes('synthetic-private-observation'), false);
  assert.notEqual(first.resultRef.id, second.resultRef.id);
  assert.equal(first.observationHash, second.observationHash);
  for (const [call, result] of [[firstCall, first], [secondCall, second]]) {
    const publicResult = await f.service.results.modelResult(context, result.resultRef,
      { requestId: context.requestId, toolCallId: call.id, toolName: call.name });
    assert.equal(publicResult.structuredContent.sources[0], 'https://example.invalid/source');
    assert.equal(Object.hasOwn(publicResult, '_meta'), false);
  }
  await assert.rejects(f.service.results.modelResult(context, first.resultRef,
    { requestId: context.requestId, toolCallId: secondCall.id, toolName: searchName }), { code: 'TOOL_RESULT_REFERENCE_MISMATCH' });
});

test('Ask reapproval and denial cannot be bypassed by an existing cached MCP observation', async t => {
  const f = await fixture(t), context = await f.context('ask');
  const first = await pendingApproval(f.service, context, f.call(searchName, envelope()));
  approve(f.service, context, first.event.tool);
  assert.equal((await first.result).isError, false);
  const denied = await pendingApproval(f.service, context, f.call(searchName, envelope()));
  assert.notEqual(denied.event.tool.approvalId, first.event.tool.approvalId);
  assert.equal(f.calls(), 1); assert.equal(f.validations(), 1);
  assert.throws(() => approve(f.service, context, first.event.tool), { code: 'TOOL_APPROVAL_NOT_FOUND' });
  approve(f.service, context, denied.event.tool, false);
  const denial = await denied.result;
  assert.equal(denial.code, 'TOOL_DENIED'); assert.equal(denial.reused, undefined);
  assert.equal(denial.resultRef, undefined);
  const approved = await pendingApproval(f.service, context, f.call(searchName, envelope()));
  approve(f.service, context, approved.event.tool);
  assert.equal((await approved.result).reused, true);
  assert.equal(f.calls(), 1); assert.equal(f.validations(), 2);
});

test('configuration changes invalidate request authority before any cached observation is reused', async t => {
  const f = await fixture(t), context = await f.context();
  assert.equal((await f.run(context, searchName, envelope())).isError, false);
  const { revision, ...config } = await f.service.getConfig();
  await f.service.updateConfig({ ...config, expectedRevision: revision, disabledSkills: ['a'.repeat(24)] });
  const stale = await f.run(context, searchName, envelope());
  assert.equal(stale.code, 'AGENT_CONFIG_CHANGED');
  assert.equal(stale.reused, undefined); assert.equal(f.calls(), 1); assert.equal(f.validations(), 1);
  assert.equal((await f.run(await f.context(), searchName, envelope())).reused, undefined);
  assert.equal(f.calls(), 2);
});

test('a reconnected MCP instance does not reuse the disconnected instance observation', async t => {
  const f = await fixture(t), context = await f.context();
  await f.run(context, searchName, envelope());
  f.reconnect();
  const fresh = await f.run(context, searchName, envelope());
  assert.equal(fresh.isError, false); assert.equal(fresh.reused, undefined);
  assert.equal(f.calls(), 2);
  assert.equal((await f.run(context, searchName, envelope())).reused, true);
  assert.equal(f.calls(), 2);
});

test('configuration changed during asynchronous MCP revalidation is checked again before reuse', async t => {
  const f = await fixture(t), context = await f.context();
  await f.run(context, searchName, envelope());
  f.service.mcp.validateExecution = async () => {
    const { revision, ...config } = await f.service.getConfig();
    await f.service.updateConfig({ ...config, expectedRevision: revision, disabledSkills: ['b'.repeat(24)] });
    return { connection: f.connection(), args: {} };
  };
  const rejected = await f.run(context, searchName, envelope());
  assert.equal(rejected.code, 'AGENT_CONFIG_CHANGED'); assert.equal(rejected.resultRef, undefined);
  assert.equal(rejected.reused, undefined); assert.equal(f.calls(), 1);
});

for (const code of ['MCP_NOT_CONNECTED', 'MCP_CATALOG_CHANGED', 'MCP_PROCESS_CLEANUP_FAILED']) {
  test(`${code} rejects a cached observation after broker authorization`, async t => {
    const f = await fixture(t), context = await f.context();
    assert.equal((await f.run(context, searchName, envelope())).isError, false);
    f.failValidation(code);
    const rejected = await f.run(context, searchName, envelope());
    assert.equal(rejected.code, code); assert.equal(rejected.isError, true);
    assert.equal(rejected.reused, undefined); assert.equal(rejected.resultRef, undefined);
    assert.equal(f.calls(), 1); assert.equal(f.validations(), 2);
  });
}

test('cancellation before or during cache validation prevents a completed reused receipt', async t => {
  const f = await fixture(t), context = await f.context();
  assert.equal((await f.run(context, searchName, envelope())).isError, false);
  const before = new AbortController(); before.abort();
  const stopped = await f.run(context, searchName, envelope(), { signal: before.signal });
  assert.equal(stopped.code, 'TOOL_CANCELLED'); assert.equal(stopped.resultRef, undefined);
  assert.equal(f.validations(), 1);
  const during = new AbortController();
  f.service.mcp.validateExecution = async () => { during.abort(); return { connection: f.connection(), args: {} }; };
  const cancelled = await f.run(context, searchName, envelope(), { signal: during.signal });
  assert.equal(cancelled.code, 'TOOL_CANCELLED'); assert.equal(cancelled.resultRef, undefined);
  assert.equal(cancelled.reused, undefined); assert.equal(f.calls(), 1);
});

test('a changed actual work folder is detected before cache lookup and reuse', async t => {
  const f = await fixture(t), context = await f.context();
  assert.equal((await f.run(context, searchName, envelope())).isError, false);
  const changed = join(f.root, 'changed-work'); await mkdir(changed);
  const catalog = await f.conversations.catalog();
  await f.conversations.saveCatalog({ ...catalog,
    Projects: catalog.Projects.map(project => project.Id === f.projectId ? { ...project, FolderPath: changed } : project) });
  const rejected = await f.run(context, searchName, envelope());
  assert.equal(rejected.code, 'WORKSPACE_CHANGED'); assert.equal(rejected.reused, undefined);
  assert.equal(f.calls(), 1); assert.equal(f.validations(), 1);
});

test('successful effects and unknown MCP actions invalidate earlier request observations', async t => {
  const f = await fixture(t), context = await f.context();
  assert.equal((await f.run(context, searchName, envelope())).isError, false);
  assert.equal((await f.run(context, 'filesystem.write', { path: 'created.txt', content: 'A changed file.', expectedHash: null })).isError, false);
  assert.equal((await f.run(context, searchName, envelope())).reused, undefined);
  assert.equal(f.calls(), 2);
  assert.equal((await f.run(context, 'mcp.synthetic.custom', envelope())).isError, false);
  assert.equal((await f.run(context, searchName, envelope())).reused, undefined);
  assert.equal(f.calls(), 4);
});

test('failed observations are retried and only subsequent success may be reused', async t => {
  const f = await fixture(t), context = await f.context();
  let failing = true;
  f.produce(() => observation(failing ? 'Synthetic temporary failure.' : 'https://example.invalid/recovered', failing));
  const failed = await f.run(context, searchName, envelope());
  assert.equal(failed.isError, true);
  failing = false;
  assert.equal((await f.run(context, searchName, envelope())).reused, undefined);
  assert.equal((await f.run(context, searchName, envelope())).reused, true);
  assert.equal(f.calls(), 2);
});

test('request scope, pagination and context release preserve fresh reads and authority', async t => {
  const f = await fixture(t), first = await f.context(), second = await f.context();
  await f.run(first, searchName, envelope());
  assert.equal((await f.run(first, searchName, envelope())).reused, true);
  assert.equal((await f.run(second, searchName, envelope())).reused, undefined);
  assert.equal((await f.run(first, searchName, envelope('dated official evidence', 'Next public page.', { offset: 10, limit: 10 }))).reused, undefined);
  assert.equal(f.calls(), 3);
  await f.service.releaseContext(first);
  assert.equal((await f.run(first, searchName, envelope())).code, 'INVALID_TOOL_CONTEXT');
  assert.equal((await f.run(await f.context(), searchName, envelope())).reused, undefined);
  assert.equal(f.calls(), 4);
});

test('local file reads always observe actual changes instead of cached text', async t => {
  const f = await fixture(t), context = await f.context();
  const path = join(f.workspace, 'changing.txt');
  await writeFile(path, 'First source.');
  const first = await f.run(context, 'filesystem.read', { path: 'changing.txt' });
  await writeFile(path, 'Changed outside the model tools.');
  const second = await f.run(context, 'filesystem.read', { path: 'changing.txt' });
  assert.equal(JSON.parse(first.content).content, 'First source.');
  assert.equal(JSON.parse(second.content).content, 'Changed outside the model tools.');
  assert.notEqual(first.observationHash, second.observationHash); assert.equal(second.reused, undefined);
});

test('the public observation cache expires at 15 seconds without refreshing on a hit and returns independent data', () => {
  let time = 0;
  const cache = new RequestObservationCache({ now: () => time });
  const call = { name: searchName, arguments: envelope() };
  cache.remember(call, observation());
  time = 14999;
  const found = cache.get(call); assert.ok(found);
  found.result.canonical.content[0].text = 'Caller mutation.';
  assert.equal(cache.get(call).result.canonical.content[0].text, 'https://example.invalid/source');
  time = 15000;
  assert.equal(cache.get(call), null);
  cache.remember(call, observation()); cache.clear(); assert.equal(cache.get(call), null);
});

test('observation cache bounds count and bytes while excluding oversized, failed, local and media observations', () => {
  const cache = new RequestObservationCache(), call = query => ({ name: searchName, arguments: envelope(query) });
  for (let index = 0; index < 33; index++) cache.remember(call('small-' + index), observation());
  assert.equal(cache.get(call('small-0')), null); assert.ok(cache.get(call('small-1'))); assert.ok(cache.get(call('small-32')));
  cache.clear();
  const large = observation('x'.repeat(20000));
  const retained = Math.floor(262144 / Buffer.byteLength(JSON.stringify(large)));
  for (let index = 0; index < 10; index++) cache.remember(call('large-' + index), large);
  assert.equal(cache.get(call('large-' + (9 - retained))), null);
  assert.ok(cache.get(call('large-' + (10 - retained)))); assert.ok(cache.get(call('large-9')));
  cache.remember(call('oversized'), observation('x'.repeat(70000)));
  cache.remember(call('failed'), observation('temporary failure', true));
  cache.remember(call('media'), { ...observation(), canonical: { content: [{ type: 'image', mimeType: 'image/png', data: 'AA==' }] } });
  for (const query of ['oversized', 'failed', 'media']) assert.equal(cache.get(call(query)), null);
  const local = { name: 'filesystem.read', arguments: { path: 'changing.txt' } };
  cache.remember(local, observation()); assert.equal(cache.get(local), null);
});

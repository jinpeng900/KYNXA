import assert from 'node:assert/strict';
import { test } from 'node:test';
import { readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { MemoryService } from '../data/memory-service.mjs';
import { RetrievalCoordinator } from '../orchestration/retrieval/coordinator.mjs';
import { WebSearchTool } from '../tools/retrieval/web-search.mjs';
import { toolFixture, approve, parsed } from './tool-fixture.mjs';

async function fixture(t) {
  const f = await toolFixture(t), log = join(f.root, 'search-events.jsonl');
  const serverFile = fileURLToPath(new URL('./fixtures/retrieval-search-server.mjs', import.meta.url));
  const retrieval = new RetrievalCoordinator({ conversations: f.conversations,
    memory: new MemoryService({ conversationStore: f.conversations }), tools: f.service });
  f.service.retrieval = retrieval; f.service.webSearch = new WebSearchTool(f.service);
  await f.service.updateConfig({ version: 1, expectedRevision: 0, skillDirectories: [], mcpServers: [
    { id: 'exa-test', name: 'Exa fixture', command: process.execPath, args: [serverFile, log], enabled: true },
    { id: 'official-fetch', name: 'Fetch fixture', command: process.execPath, args: [serverFile, log, 'fetch-instance'], enabled: true }
  ] });
  return { ...f, retrieval, log, events: async () => (await readFile(log, 'utf8')).trim().split('\n').map(line => JSON.parse(line)) };
}

test('web budget checks prepare approval state without consuming failed or undispatched queries', async () => {
  const context = { projectId: null }, tools = { catalogs: new WeakMap() }, web = new WebSearchTool(tools);
  tools.catalogs.set(context, { descriptors: new Map() });
  for (let index = 0; index < 3; index++) {
    const checked = await web.check(context, 'query');
    assert.equal(checked.stage.queryCount, 0); assert.equal(checked.stage.pageCount, 0);
  }
  await assert.rejects(web.run(context, { query: 'No configured provider', reason: 'Synthetic lookup' }, {}),
    { code: 'WEB_SEARCH_PROVIDER_UNAVAILABLE' });
  assert.equal(web.stages.get(context).queryCount, 0);
  await web.take(context, 'query'); await web.take(context, 'query');
  await assert.rejects(web.check(context, 'query'), { code: 'WEB_STAGE_BUDGET_EXHAUSTED' });
  assert.equal(web.stages.get(context).queryCount, 2);
});

test('concurrent public search dispatches cannot consume more than the stage query budget', async () => {
  const context = { projectId: null }, web = new WebSearchTool({});
  await web.check(context, 'query');
  const results = await Promise.allSettled(Array.from({ length: 4 }, () => web.take(context, 'query')));
  assert.equal(results.filter(item => item.status === 'fulfilled').length, 2);
  const rejected = results.filter(item => item.status === 'rejected');
  assert.equal(rejected.length, 2);
  assert.ok(rejected.every(item => item.reason.code === 'WEB_STAGE_BUDGET_EXHAUSTED'));
  assert.equal(web.stages.get(context).queryCount, 2);
});

test('unified web search consumes its dispatch budget once with or without retrieval settings', async () => {
  for (const withRetrieval of [false, true]) {
    const context = { projectId: null }, tools = { catalogs: new WeakMap() }, web = new WebSearchTool(tools);
    if (withRetrieval) tools.retrieval = { effective: async () => ({ web: { mode: 'auto', depth: 'standard', providerId: 'auto' } }) };
    const descriptor = { name: 'mcp.synthetic.web_search_exa', serverId: 'synthetic',
      originalInputSchema: { properties: { query: {}, numResults: {} } } };
    tools.catalogs.set(context, { descriptors: new Map([[descriptor.name, descriptor]]) });
    let dispatched = 0;
    tools.execute = async () => {
      await web.take(context, 'query'); dispatched++;
      return { content: JSON.stringify({ results: [{ url: 'https://example.com/source', title: 'Synthetic evidence', text: 'A verified fixture observation.' }] }), isError: false };
    };
    const result = await web.run(context, { query: 'Synthetic source query', reason: 'Synthetic lookup' }, {});
    assert.equal(result.isError, undefined); assert.equal(dispatched, 1);
    assert.equal(web.stages.get(context).queryCount, 1); assert.equal(result.value.remainingQueries, 1);
  }
});

test('unified web search calls the real SDK once, retains original result, and bounds repeated search', async t => {
  const f = await fixture(t), context = await f.context('full');
  const cold = await f.service.webSearch.providers();
  assert.ok(cold.providers.some(item => item.id === 'exa-test' && !item.ready));
  assert.equal(f.service.mcp.connections.size, 0);
  await f.service.catalog(context, { connectMcp: true });
  f.service.configureModelCatalog(context, { protocol: 'openai-completions', tokenBudget: 16000, message: '查证最新消息' });
  assert.ok((await f.service.webSearch.providers()).providers.some(item => item.id === 'exa-test' && item.ready));
  const result = await f.run(context, 'web.search', { query: 'Latest official change', limit: 4, reason: 'Synthetic source verification' });
  const output = parsed(result);
  assert.equal(output.sources[0].url, 'https://example.com/announcement');
  assert.equal(output.providerId, 'exa-test'); assert.ok(output.originalResultRef);
  assert.equal((await f.events()).filter(item => item.event === 'search').length, 1);
  assert.equal(f.service.webSearch.stages.get(context).queryCount, 1);
  const archived = await f.service.results.get(context, output.originalResultRef.id);
  assert.equal(archived.structuredContent.results[0].title, 'Official announcement');
  await f.run(context, 'web.search', { query: 'Second related query', reason: 'Synthetic second verification' });
  const exhausted = await f.run(context, 'web.search', { query: 'Third redundant query', reason: 'Must not dispatch' });
  assert.equal(exhausted.code, 'WEB_STAGE_BUDGET_EXHAUSTED');
  assert.equal((await f.events()).filter(item => item.event === 'search').length, 2);
  const remaining = f.service.modelCatalog(context);
  assert.ok(!remaining.some(tool => tool.name === 'web.search' || tool.name.endsWith('.web_search_exa')));
  assert.ok(remaining.some(tool => tool.name.startsWith('filesystem.')));
});

test('approval waiting does not expire the network deadline and approval stays on the actual MCP operation', async t => {
  const f = await fixture(t), context = await f.context('ask');
  await f.service.catalog(context, { connectMcp: true });
  let notify;
  const eventPromise = new Promise(resolve => { notify = resolve; });
  const resultPromise = f.run(context, 'web.search', { query: 'Approved query', reason: 'Approved synthetic public request' }, { emit: notify });
  const event = await eventPromise;
  assert.equal(event.tool.name, 'mcp.exa-test.web_search_exa');
  assert.deepEqual(event.tool.arguments.arguments, { query: 'Approved query', numResults: 6 });
  const stage = f.service.webSearch.stages.get(context); stage.durationMs = 60;
  await new Promise(resolve => setTimeout(resolve, 100));
  approve(f.service, context, event.tool);
  assert.equal((await resultPromise).isError, false);
  assert.equal((await f.events()).filter(item => item.event === 'search').length, 1);
});

test('web off revokes existing raw fetch/search calls and page limits cannot be bypassed through MCP', async t => {
  const f = await fixture(t), context = await f.context('full');
  await f.service.catalog(context, { connectMcp: true });
  for (let index = 0; index < 6; index++) {
    const result = await f.run(context, 'mcp.official-fetch.fetch', { arguments: { url: `https://example.com/${index}` }, policy: { reason: 'Synthetic page' } });
    assert.equal(result.isError, false);
  }
  const limited = await f.run(context, 'mcp.official-fetch.fetch', { arguments: { url: 'https://example.com/extra' }, policy: { reason: 'No dispatch' } });
  assert.equal(limited.code, 'WEB_STAGE_BUDGET_EXHAUSTED');
  await f.retrieval.settings.patchGlobal({ expectedRevision: 0, patch: { web: { mode: 'off', providerId: 'exa-test' } } });
  assert.equal((await f.service.webSearch.providers()).selectedId, 'exa-test');
  const disabled = await f.run(context, 'mcp.exa-test.web_search_exa', { arguments: { query: 'No dispatch' }, policy: { reason: 'Disabled' } });
  assert.equal(disabled.code, 'WEB_SEARCH_DISABLED');
  const refreshed = await f.service.catalog(context);
  assert.ok(!refreshed.some(item => item.name === 'mcp.official-fetch.fetch' || item.name === 'web.search'));
  assert.equal((await f.events()).filter(item => item.event === 'fetch').length, 6);
});

test('raw public-search stage timeout is a paired read error, while later local work remains available', async t => {
  const f = await fixture(t), context = await f.context('full');
  await f.service.catalog(context, { connectMcp: true });
  await f.service.webSearch.take(context, 'page');
  // Expire the synthetic deadline after SDK dispatch, rather than racing setup against a 30 ms wall clock.
  // 等实际 SDK 派发后再触发自造期限，避免并行测试的准备耗时先耗尽 30 毫秒而根本没有派发。
  const stageDeadline = new AbortController(), originalTimeout = AbortSignal.timeout;
  let deadlineArmed = false;
  t.mock.method(AbortSignal, 'timeout', durationMs => {
    if (deadlineArmed) return originalTimeout(durationMs);
    assert.ok(durationMs > 30000, 'the prepared public-search stage still has its normal budget');
    deadlineArmed = true;
    return stageDeadline.signal;
  });
  const execute = f.service.mcp.execute.bind(f.service.mcp);
  f.service.mcp.execute = async (...args) => {
    assert.equal(deadlineArmed, true);
    const settled = execute(...args).then(value => ({ value }), error => ({ error }));
    const dispatchLimit = performance.now() + 10000;
    while (!(await f.events().catch(error => {
      if (error.code === 'ENOENT') return [];
      throw error;
    })).some(item => item.event === 'search' && item.query === 'slow-fixture')) {
      assert.ok(performance.now() < dispatchLimit, 'the synthetic MCP search must reach the server');
      await new Promise(resolve => setTimeout(resolve, 10));
    }
    stageDeadline.abort(new DOMException('Synthetic public-search deadline expired.', 'TimeoutError'));
    const outcome = await settled;
    if (outcome.error) throw outcome.error;
    return outcome.value;
  };
  const result = await f.run(context, 'mcp.exa-test.web_search_exa', {
    arguments: { query: 'slow-fixture' }, policy: { reason: 'Synthetic stage timeout' }
  });
  assert.equal(result.code, 'WEB_STAGE_BUDGET_EXHAUSTED'); assert.equal(result.status, 'error'); assert.ok(result.resultRef);
  const original = await f.service.results.get(context, result.resultRef.id);
  assert.equal(original.structuredContent.outcome, 'failed');
  await writeFile(join(f.workspace, 'after-timeout.md'), 'Local code task can continue.', 'utf8');
  assert.equal((await f.run(context, 'filesystem.read', { path: 'after-timeout.md' })).isError, false);
});

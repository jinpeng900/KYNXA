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
  f.service.webSearch.stages.get(context).durationMs = 30;
  const result = await f.run(context, 'mcp.exa-test.web_search_exa', {
    arguments: { query: 'slow-fixture' }, policy: { reason: 'Synthetic stage timeout' }
  });
  assert.equal(result.code, 'WEB_STAGE_BUDGET_EXHAUSTED'); assert.equal(result.status, 'error'); assert.ok(result.resultRef);
  const original = await f.service.results.get(context, result.resultRef.id);
  assert.equal(original.structuredContent.outcome, 'failed');
  await writeFile(join(f.workspace, 'after-timeout.md'), 'Local code task can continue.', 'utf8');
  assert.equal((await f.run(context, 'filesystem.read', { path: 'after-timeout.md' })).isError, false);
});

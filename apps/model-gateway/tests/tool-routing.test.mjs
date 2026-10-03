import assert from 'node:assert/strict';
import { test } from 'node:test';
import { ModelToolCatalog } from '../tool-catalog.mjs';
import { estimateTokens } from '../context.mjs';
import { MAX_MODEL_TOOLS, toolDeclarations, wireCatalog } from '../tool-protocols.mjs';
import { canRunInParallel } from '../tool-scheduling.mjs';

const tool = (name, source = 'mcp:synthetic', description = 'Synthetic capability.') => ({ name, source, description,
  inputSchema: { type: 'object', properties: {}, additionalProperties: false } });
const builtin = [tool('tool.search', 'builtin'), tool('tool.load', 'builtin'), tool('tool.result.read', 'builtin'), tool('filesystem.read', 'builtin')];
const browser = tool('mcp.chrome-devtools.new_page');
const search = tool('mcp.exa.web_search_exa', 'mcp:exa', 'Public web search with multiple relevant results.');
const fetch = tool('mcp.fetch.fetch', 'mcp:fetch', 'Fetch public webpages as text.');
const docs = tool('mcp.context7.query-docs', 'mcp:context7', 'Retrieve current library documentation.');
const cost = tools => estimateTokens(JSON.stringify(toolDeclarations('openai-completions', wireCatalog(tools))));

for (const message of ['王者荣耀最新的英雄是谁', 'Who is the latest hero released?', '查证今天的官方新闻']) {
  test(`fact intent softly prioritizes multi-result search under the same tool budget: ${message}`, () => {
    const tokenBudget = Math.max(cost([...builtin, search]), cost([...builtin, browser]));
    const catalog = new ModelToolCatalog([...builtin, browser, search], { protocol: 'openai-completions', tokenBudget, message });
    const names = catalog.selected.map(item => item.name);
    assert.ok(names.includes(search.name)); assert.ok(!names.includes(browser.name));
    for (const item of builtin) assert.ok(names.includes(item.name));
    assert.ok(cost(catalog.selected) <= tokenBudget);
    catalog.load([browser.name]);
    assert.ok(catalog.selected.some(item => item.name === browser.name), 'browser remains available through explicit discovery/load');
  });
}

for (const message of ['查 React 最新版本的接口文档', 'Find the current SDK documentation']) {
  test(`code/documentation intent prioritizes the existing docs tool: ${message}`, () => {
    const catalog = new ModelToolCatalog([...builtin, browser, search, docs], { protocol: 'openai-completions', message });
    const remote = catalog.selected.filter(item => item.source !== 'builtin');
    assert.equal(remote[0].name, docs.name);
    assert.ok(remote.some(item => item.name === browser.name));
  });
}

test('a provided webpage URL prioritizes fetching, while Chinese metadata and unchanged limits stay usable', () => {
  const chinese = tool('mcp.synthetic.hero-data', 'mcp:synthetic', '王者荣耀英雄的数据');
  const catalog = new ModelToolCatalog([...builtin, browser, search, fetch, chinese], {
    protocol: 'openai-completions', message: '读取 https://example.com/announcement 的最新内容' });
  assert.equal(catalog.selected.filter(item => item.source !== 'builtin')[0].name, fetch.name);
  const matching = new ModelToolCatalog([browser, chinese], { protocol: 'openai-completions', message: '王者荣耀英雄的数据' });
  assert.equal(matching.selected[0].name, chinese.name);
  const many = new ModelToolCatalog([...builtin, ...Array.from({ length: 110 }, (_, index) => tool(`mcp.synthetic.tool-${index}`))], {
    protocol: 'openai-completions', tokenBudget: 16000, message: 'continue' });
  assert.ok(many.selected.length <= MAX_MODEL_TOOLS); assert.ok(cost(many.selected) <= 16000);
});

test('parallel scheduling permits only known reads and public stateless search/fetch without granting approval', () => {
  for (const name of ['filesystem.read', 'filesystem.list', 'filesystem.search', 'filesystem.stat',
    'conversation.history.read', 'conversation.history.search', 'tool.result.read', 'mcp.exa.web_search_exa',
    'mcp.brave.brave_web_search', 'mcp.brave-search.brave_web_search'])
    assert.equal(canRunInParallel({ name, arguments: {} }), true, name);
  for (const url of ['https://example.com/a', 'http://127.0.0.1/page'])
    assert.equal(canRunInParallel({ name: 'mcp.fetch.fetch', arguments: { arguments: { url }, policy: { reason: 'Read source.' } } }), true);
  for (const url of ['file:///C:/private.txt', 'javascript:alert(1)', 'data:text/plain,test', 'not-a-url', 'https://example.com/\n'])
    assert.equal(canRunInParallel({ name: 'mcp.fetch.fetch', arguments: { arguments: { url } } }), false);
  for (const name of ['filesystem.write', 'filesystem.edit', 'filesystem.delete', 'filesystem.mkdir', 'terminal.run',
    'skill.list', 'skill.read', 'skill.run', 'tool.load', 'mcp.playwright.browser_navigate', 'mcp.chrome-devtools.new_page',
    'mcp.chrome-devtools.evaluate_script', 'mcp.unknown.fetch', 'mcp.unknown.web_search_exa'])
    assert.equal(canRunInParallel({ name, annotations: { readOnlyHint: true }, arguments: {} }), false, name);
  for (const call of [null, {}, [], 'filesystem.read', { name: 'mcp.fetch.fetch', arguments: { url: 'https://example.com' } }])
    assert.equal(canRunInParallel(call), false);
});

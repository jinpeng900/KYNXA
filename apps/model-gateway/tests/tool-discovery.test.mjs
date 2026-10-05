import assert from 'node:assert/strict';
import { test } from 'node:test';
import { builtinDescriptors } from '../official-tools/Tools/catalog.mjs';
import { ModelToolCatalog } from '../tools/tool-catalog.mjs';
import { searchTools, toolDiscoveryCategory, toolSelectionSignals } from '../tools/tool-discovery.mjs';
import { estimateTokens } from '../models/context.mjs';
import { MAX_MODEL_TOOLS, toolDeclarations, wireCatalog } from '../models/tool-protocols.mjs';
import { parsed, toolFixture } from './tool-fixture.mjs';

const remote = (name, description = 'Browser automation.') => ({ name, description, source: 'mcp:synthetic',
  inputSchema: { type: 'object', properties: {}, additionalProperties: false } });
const browserTools = [remote('mcp.chrome-devtools.list_pages'), remote('mcp.chrome-devtools.evaluate_script'),
  remote('mcp.playwright.browser_navigate'), remote('mcp.playwright.browser_take_screenshot')];
const descriptors = [...builtinDescriptors, ...browserTools];
const protocols = ['openai-completions', 'openai-responses', 'anthropic-messages'];
const names = tools => tools.map(tool => tool.name);

test('actual failed Chinese and multiword searches find enabled host and browser tools', () => {
  assert.ok(names(searchTools(descriptors, '浏览器')).includes('computer.launch'));
  for (const query of ['本机终端', '本地终端', 'host terminal', 'local terminal', 'conda'])
    assert.equal(searchTools(descriptors, query)[0].name, 'terminal.host.run', query);
  assert.equal(searchTools(descriptors, 'chrome devtools evaluate script')[0].name, 'mcp.chrome-devtools.evaluate_script');
  assert.equal(searchTools(descriptors, '网页截图')[0].name, 'mcp.playwright.browser_take_screenshot');
  assert.ok(names(searchTools(descriptors, '打开本机浏览器')).includes('computer.launch'));
  assert.deepEqual(searchTools(descriptors, '绝无此项功能'), []);
});

test('exact identities outrank partial metadata, disabled tools stay absent and paging order is stable', () => {
  const exact = remote('mcp.synthetic.target', 'A specific result.'), collision = remote('mcp.synthetic.target_preview', 'mcp.synthetic.target');
  const disabled = { ...remote('mcp.synthetic.hidden', 'A browser tool.'), enabled: false };
  const source = [collision, disabled, exact, ...browserTools], before = structuredClone(source);
  assert.equal(searchTools(source, exact.name.toUpperCase())[0], exact);
  assert.deepEqual(searchTools(source, ''), source.filter(tool => tool !== disabled));
  assert.deepEqual(searchTools(source, disabled.name), []);
  const first = searchTools(source, 'browser'), second = searchTools(source, 'browser');
  assert.deepEqual(first, second); assert.ok(!first.includes(disabled));
  assert.deepEqual(source, before);
});

test('tool classifications distinguish host control, host shell and browser automation without availability claims', () => {
  assert.equal(toolDiscoveryCategory({ name: 'computer.launch' }), 'computer');
  assert.equal(toolDiscoveryCategory({ name: 'terminal.host.run' }), 'host-terminal');
  assert.equal(toolDiscoveryCategory({ name: 'terminal.run' }), 'sandbox-terminal');
  assert.equal(toolDiscoveryCategory({ name: 'mcp.playwright.browser_navigate' }), 'browser');
  assert.equal(toolDiscoveryCategory({ name: 'browser.local.read' }), 'browser');
  assert.equal(toolDiscoveryCategory({ name: 'mcp.exa.web_search_exa' }), 'web-search');
});

for (const protocol of protocols) test(`${protocol}: browser followups retain relevant schemas with explicit new tasks resetting the subject`, () => {
  const initial = new ModelToolCatalog(descriptors, { protocol, tokenBudget: 16000, message: '打开google浏览器' });
  assert.ok(initial.selected.some(tool => tool.name === 'computer.launch'));
  const historySignals = ['打开google浏览器'], previousToolNames = ['computer.apps', 'computer.launch'];
  for (const message of ['再次尝试打开', '再来', '查看x网站，看看我最近浏览哪些', '访问到本机的，能够自动登录并打开界面']) {
    const catalog = new ModelToolCatalog(descriptors, { protocol, tokenBudget: 16000, message, historySignals, previousToolNames });
    assert.ok(catalog.selected.some(tool => tool.name === 'computer.launch'), message);
    assert.ok(catalog.selected.length <= MAX_MODEL_TOOLS);
    assert.ok(estimateTokens(JSON.stringify(toolDeclarations(protocol, catalog.wire()))) <= 16000);
  }
  const fresh = new ModelToolCatalog(descriptors, { protocol, tokenBudget: 16000,
    message: '另外一件，写一个Node.js代码测试', historySignals, previousToolNames });
  assert.ok(!fresh.selected.some(tool => tool.name.startsWith('computer.')));
  assert.ok(!fresh.selected.some(tool => tool.name === 'terminal.host.run'));
  const ordinary = new ModelToolCatalog(descriptors, { protocol, tokenBudget: 16000,
    message: '查证今天的官方新闻', historySignals, previousToolNames });
  assert.ok(!ordinary.selected.some(tool => tool.name.startsWith('computer.')));
});

test('direct short browser requests work without old history; no old permission or call arguments are imported', () => {
  for (const message of ['查看x网站', '访问x.com', 'open Chrome', '打开Edge', '打开本地浏览器', '打开可见窗口', '我要本机自动登录并打开界面']) {
    const catalog = new ModelToolCatalog(descriptors, { protocol: protocols[0], tokenBudget: 16000, message });
    assert.ok(catalog.selected.some(tool => tool.name === 'computer.launch'), message);
  }
  const signals = toolSelectionSignals('再次尝试打开', {
    historySignals: [{ role: 'user', content: '本机终端', permissionMode: 'full' }],
    previousToolNames: [{ name: 'terminal.host.run', arguments: { script: 'never run' } }]
  });
  assert.equal(signals.hostTerminal, false); assert.equal(signals.retainedNames.size, 0);
  const current = toolSelectionSignals('再来', { historySignals: ['调用本机终端执行conda list'] });
  assert.equal(current.hostTerminal, true);
  const distant = toolSelectionSignals('再来', { historySignals: ['调用本机终端', 'hello', 'hi', '你好'] });
  assert.equal(distant.hostTerminal, false, 'a distant old topic outside the bounded recent hints is not retained');
});

test('an explicit remote-browser task prioritizes browser schemas instead of substituting the local desktop', () => {
  for (const protocol of protocols) {
    const required = descriptors.filter(tool => ['tool.search', 'tool.load', 'tool.result.read', 'mcp.chrome-devtools.list_pages'].includes(tool.name));
    const tokenBudget = estimateTokens(JSON.stringify(toolDeclarations(protocol, wireCatalog(required))));
    for (const message of ['使用云端浏览器查看网页', 'Take a screenshot in the remote browser']) {
      const catalog = new ModelToolCatalog(descriptors, { protocol, tokenBudget, message,
        historySignals: ['打开本机浏览器'], previousToolNames: ['computer.launch'] });
      assert.ok(!catalog.selected.some(tool => tool.name.startsWith('computer.')));
      assert.ok(catalog.selected.some(tool => toolDiscoveryCategory(tool) === 'browser'));
      assert.ok(estimateTokens(JSON.stringify(toolDeclarations(protocol, catalog.wire()))) <= tokenBudget);
    }
    const local = new ModelToolCatalog(descriptors, { protocol, tokenBudget: 16000, message: '改为打开本机浏览器',
      historySignals: ['使用云端浏览器查看网页'], previousToolNames: ['mcp.chrome-devtools.list_pages'] });
    assert.ok(local.selected.some(tool => tool.name === 'computer.launch'));
  }
});

test('a retry follows the most recent explicit local or remote boundary, not all earlier browser subjects', () => {
  const previousToolNames = ['computer.launch', 'mcp.chrome-devtools.list_pages'];
  for (const [historySignals, expected] of [
    [['使用云端浏览器查看网页', '改为打开本机浏览器'], 'local'],
    [['打开本机浏览器', '改为用远程浏览器'], 'remote'],
    [['使用云端浏览器', '打开Chrome'], 'local'],
    [['本机浏览器', '我需要 remote browser'], 'remote']
  ]) {
    const signals = toolSelectionSignals('再次尝试打开', { historySignals, previousToolNames });
    assert.equal(signals.remoteBrowser, expected === 'remote', historySignals.join(' → '));
    assert.equal(signals.desktop, expected === 'local', historySignals.join(' → '));
  }
  for (const [message, remote] of [
    ['不要用远程浏览器，改为本机浏览器', false],
    ['本机浏览器先不用，改为远程浏览器', true],
    ['在云端浏览器打开Chrome', true],
    ['改为打开本地浏览器', false]
  ]) {
    const signals = toolSelectionSignals(message, { historySignals: ['使用云端浏览器'], previousToolNames });
    assert.equal(signals.remoteBrowser, remote, message);
    assert.equal(signals.desktop, !remote, message);
  }
  const unrelated = toolSelectionSignals('另外一件，写Node.js代码测试', {
    historySignals: ['打开本机浏览器'], previousToolNames });
  assert.equal(unrelated.desktop, false); assert.equal(unrelated.remoteBrowser, false);
});

test('tiny budgets keep discovery and allow loading individual capabilities without resurrecting disabled schemas', () => {
  const disabled = { ...builtinDescriptors.find(tool => tool.name === 'computer.launch'), enabled: false };
  const source = descriptors.filter(tool => tool.name !== disabled.name).concat(disabled);
  const discovery = source.filter(tool => ['tool.search', 'tool.load', 'tool.result.read', 'computer.apps'].includes(tool.name));
  for (const protocol of protocols) {
    const tokenBudget = estimateTokens(JSON.stringify(toolDeclarations(protocol, wireCatalog(discovery))));
    const catalog = new ModelToolCatalog(source, { protocol, tokenBudget, message: '再次尝试打开',
      previousToolNames: ['computer.launch', 'computer.apps', 'missing.tool'] });
    for (const name of ['tool.search', 'tool.load', 'tool.result.read'])
      assert.ok(catalog.selected.some(tool => tool.name === name));
    assert.ok(!catalog.selected.some(tool => tool.name === 'computer.launch'));
    assert.throws(() => catalog.load(['computer.launch']), { code: 'TOOL_NOT_FOUND' });
    assert.deepEqual(catalog.load(['computer.apps']).loaded, ['computer.apps']);
    assert.ok(estimateTokens(JSON.stringify(toolDeclarations(protocol, catalog.wire()))) <= tokenBudget);
  }
});

test('tool.search exposes ranked bilingual matches through the real service with ordinary pagination', async t => {
  const f = await toolFixture(t), ctx = await f.context('full');
  await f.service.catalog(ctx);
  const host = parsed(await f.run(ctx, 'tool.search', { query: '本机终端', limit: 1 }));
  assert.equal(host.tools[0].name, 'terminal.host.run'); assert.equal(host.offset, 0);
  const browser = parsed(await f.run(ctx, 'tool.search', { query: '浏览器', limit: 1 }));
  assert.ok(browser.total > 1); assert.equal(browser.hasMore, true);
  const second = parsed(await f.run(ctx, 'tool.search', { query: '浏览器', offset: browser.nextOffset, limit: 1 }));
  assert.notEqual(browser.tools[0].name, second.tools[0].name);
});

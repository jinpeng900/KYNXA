import assert from 'node:assert/strict';
import { test } from 'node:test';
import { builtinDescriptors } from '../official-tools/Tools/catalog.mjs';
import { ModelToolCatalog } from '../tools/tool-catalog.mjs';
import { searchTools, toolDiscoveryCategory, toolSelectionSignals } from '../tools/tool-discovery.mjs';
import { estimateTokens } from '../models/context.mjs';
import { MAX_MODEL_TOOLS, toolDeclarations, wireCatalog } from '../models/tool-protocols.mjs';
import { parsed, toolFixture } from './tool-fixture.mjs';
import { analyzeRequestClauses } from '../platform/request-clause-signals.mjs';

const remote = (name, description = 'Browser automation.') => ({ name, description, source: 'mcp:synthetic',
  inputSchema: { type: 'object', properties: {}, additionalProperties: false } });
const browserTools = [remote('mcp.chrome-devtools.list_pages'), remote('mcp.chrome-devtools.evaluate_script'),
  remote('mcp.playwright.browser_navigate'), remote('mcp.playwright.browser_take_screenshot')];
const descriptors = [...builtinDescriptors, ...browserTools];
const protocols = ['openai-completions', 'openai-responses', 'anthropic-messages'];
const names = tools => tools.map(tool => tool.name);
const nativeCapabilities = {
  desktopRunner: { capabilities: async () => ({ available: true, boundary: 'host-desktop',
    operations: builtinDescriptors.filter(tool => tool.name.startsWith('computer.')).map(tool => tool.name.slice('computer.'.length)) }),
    run: async () => assert.fail('tool discovery must not perform desktop actions') },
  hostTerminalRunner: { capabilities: async () => ({ available: true, boundary: 'host-terminal', shells: ['cmd', 'powershell'], backgroundJobs: true }),
    run: async () => assert.fail('tool discovery must not execute a host command') }
};

test('actual failed Chinese and multiword searches find enabled host and browser tools', () => {
  assert.ok(names(searchTools(descriptors, '浏览器')).includes('computer.launch'));
  for (const query of ['本机终端', '本地终端', 'host terminal', 'local terminal', 'conda'])
    assert.equal(searchTools(descriptors, query)[0].name, 'terminal.host.run', query);
  assert.equal(searchTools(descriptors, 'chrome devtools evaluate script')[0].name, 'mcp.chrome-devtools.evaluate_script');
  assert.equal(searchTools(descriptors, '网页截图')[0].name, 'mcp.playwright.browser_take_screenshot');
  assert.ok(names(searchTools(descriptors, '打开本机浏览器')).includes('computer.launch'));
  assert.deepEqual(searchTools(descriptors, '绝无此项功能'), []);
});

test('device status requests select the host shell without turning inspection into browser control', () => {
  for (const message of ['我的IP是什么', '查看我的公网IP', '当前网络', '检查当前网络配置', '查看代理设置',
    '检查 DNS', '列出网卡', '查看本机进程', '列出所有进程', '哪个程序占端口5218', '端口5218被哪个进程占用',
    'What is my IP?', 'Check my proxy settings', 'show current DNS servers', 'list local processes',
    'which process is listening on port 5218', 'Which app is using port 3000?', 'list network adapters', 'run ipconfig']) {
    const signals = toolSelectionSignals(message, { historySignals: ['打开本机浏览器'], previousToolNames: ['computer.launch'] });
    assert.equal(signals.deviceState, true, message);
    assert.equal(signals.hostTerminal, true, message);
    assert.equal(signals.desktop, false, message);
    assert.equal(signals.browser, false, message);
    assert.equal(signals.web, false, message);
    assert.equal(signals.retainedNames.size, 0, 'a new device task does not carry earlier attempted browser names');
  }
});

test('public IP ownership, networking explanations and script generation do not claim device access', () => {
  for (const message of ['查询8.8.8.8的归属', '看看 IPv6 2001:4860:4860::8888 的归属', '我的IP是8.8.8.8，查归属',
    'IP和DNS有什么区别', '解释DNS原理', '如何查看当前网络配置', 'ipconfig是什么', '给我一个列出本机进程的脚本',
    'Who owns IP 8.8.8.8?', 'What is DNS?', 'Explain ipconfig', 'How to configure a proxy?',
    'write a script to inspect my IP', 'current network industry trends']) {
    const signals = toolSelectionSignals(message, { historySignals: ['打开本机浏览器', '查看我的IP'],
      previousToolNames: ['computer.launch', 'terminal.host.run'] });
    assert.equal(signals.deviceState, false, message);
    assert.equal(signals.hostTerminal, false, message);
    assert.equal(signals.desktop, false, message);
    assert.equal(signals.retainedNames.size, 0, message);
  }
  assert.equal(toolSelectionSignals('Who owns IP 8.8.8.8?').web, true);
  assert.equal(toolSelectionSignals('打开我的iPad应用').deviceState, false, 'IP is an ASCII word, not part of iPad');
});

test('a short device retry follows the latest device subject while explicit browser boundaries still apply', () => {
  for (const message of ['再来', '再查一次', '刷新', '现在呢', 'refresh', 'what about now?']) {
    const signals = toolSelectionSignals(message, { historySignals: ['打开本机浏览器', '查看我的IP'],
      previousToolNames: ['computer.launch', 'mcp.chrome-devtools.list_pages', 'terminal.host.run'] });
    assert.equal(signals.deviceState, true, message);
    assert.equal(signals.hostTerminal, true, message);
    assert.equal(signals.browser, false, message);
    assert.equal(signals.desktop, false, message);
    assert.deepEqual([...signals.retainedNames], ['terminal.host.run']);
  }
  const combined = toolSelectionSignals('检查我的IP，并给本机桌面截图');
  assert.equal(combined.deviceState, true); assert.equal(combined.desktop, true);
  const remote = toolSelectionSignals('在云端浏览器查看我的IP');
  assert.equal(remote.deviceState, false); assert.equal(remote.hostTerminal, false);
  assert.equal(remote.remoteBrowser, true); assert.equal(remote.desktop, false);
  const browserRetry = toolSelectionSignals('再来', { historySignals: ['查看我的IP', '打开Chrome'],
    previousToolNames: ['computer.launch'] });
  assert.equal(browserRetry.desktop, true, 'a later browser topic keeps its existing local-browser followup behavior');
  assert.equal(browserRetry.deviceState, false, 'an older device inspection is not the current browser task');
});

test('device state search aliases rank the enabled host execution schema and never revive a disabled tool', () => {
  for (const query of ['我的IP是什么', '当前网络', 'DNS配置', '网卡', '本机进程', '哪个程序占端口',
    'my ip address', 'current network', 'proxy settings', 'network adapters', 'list processes', 'ipconfig'])
    assert.equal(searchTools(descriptors, query)[0]?.name, 'terminal.host.run', query);
  const disabled = descriptors.map(tool => tool.name === 'terminal.host.run' ? { ...tool, enabled: false } : tool);
  assert.ok(!names(searchTools(disabled, '我的IP')).includes('terminal.host.run'));
  for (const query of ['查询8.8.8.8的归属', 'Who owns IP 8.8.8.8?', 'IP和DNS区别', 'Explain ipconfig'])
    assert.notEqual(searchTools(descriptors, query)[0]?.name, 'terminal.host.run', query);
});

test('ordinary Chinese typing requests discover and select existing desktop input schemas', () => {
  for (const query of ['将文字键入光标所在的位置', '把你好填进记事本', '在输入框填写内容', '粘贴到当前窗口']) {
    assert.ok(names(searchTools(descriptors, query)).includes('computer.type'), query);
    const catalog = new ModelToolCatalog(descriptors, { protocol: protocols[0], tokenBudget: 16000, message: query });
    assert.ok(catalog.selected.some(tool => tool.name === 'computer.type'), query);
  }
  assert.ok(names(searchTools([...descriptors, remote('mcp.playwright.browser_fill_form')], '填写输入框'))
    .includes('mcp.playwright.browser_fill_form'));
  assert.equal(toolSelectionSignals('写一个处理输入文本的函数').desktop, false,
    'code input is not automatically desktop input');
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

test('wire aliases load only exact enabled tools and one unavailable item cannot suppress valid screenshot loading', () => {
  const disabled = { ...remote('mcp.synthetic.disabled_screenshot'), enabled: false };
  const catalog = new ModelToolCatalog([...descriptors, disabled], { protocol: protocols[0], tokenBudget: 16000 });
  const screenshot = builtinDescriptors.find(tool => tool.name === 'computer.screenshot');
  const alias = wireCatalog([screenshot])[0].wireName;
  assert.deepEqual(catalog.resolveNames([alias, 'missing.tool']), ['computer.screenshot', 'missing.tool']);
  const result = catalog.load(['mcp.unconnected.browser_screenshot', alias, 'computer.screenshot']);
  assert.deepEqual(result.loaded, ['computer.screenshot']);
  assert.deepEqual(result.unavailable, [{ name: 'mcp.unconnected.browser_screenshot', code: 'TOOL_NOT_FOUND' }]);
  assert.ok(catalog.wire().some(tool => tool.name === 'computer.screenshot'));
  assert.deepEqual(names(searchTools([...descriptors, disabled], alias)), ['computer.screenshot']);
  const disabledAlias = wireCatalog([disabled])[0].wireName;
  assert.deepEqual(searchTools([...descriptors, disabled], disabledAlias), []);
  const before = structuredClone(catalog.selected);
  assert.throws(() => catalog.load([disabledAlias, alias.replace(/.$/, alias.endsWith('a') ? 'b' : 'a')]), { code: 'TOOL_NOT_FOUND' });
  assert.deepEqual(catalog.selected, before, 'unknown or disabled aliases never mutate the usable selection');
});

test('tool classifications distinguish host control, host shell and browser automation without availability claims', () => {
  assert.equal(toolDiscoveryCategory({ name: 'computer.launch' }), 'computer');
  assert.equal(toolDiscoveryCategory({ name: 'terminal.host.run' }), 'host-terminal');
  for (const action of ['start', 'read', 'stop'])
    assert.equal(toolDiscoveryCategory({ name: `terminal.host.${action}` }), 'host-terminal');
  assert.equal(toolDiscoveryCategory({ name: 'terminal.run' }), 'sandbox-terminal');
  assert.equal(toolDiscoveryCategory({ name: 'mcp.playwright.browser_navigate' }), 'browser');
  assert.equal(toolDiscoveryCategory({ name: 'browser.local.read' }), 'browser');
  assert.equal(toolDiscoveryCategory({ name: 'mcp.exa.web_search_exa' }), 'web-search');
});

test('background host jobs are discoverable for terminal tasks and deferred during unrelated conversations', () => {
  for (const protocol of protocols) {
    const unrelated = new ModelToolCatalog(descriptors, { protocol, tokenBudget: 32000, message: '你好' });
    assert.ok(!unrelated.selected.some(tool => tool.name.startsWith('terminal.host.')));
    const background = new ModelToolCatalog(descriptors, { protocol, tokenBudget: 32000, message: '启动后台终端进程并监控输出' });
    for (const action of ['start', 'read', 'stop']) {
      const name = `terminal.host.${action}`;
      assert.ok(background.selected.some(tool => tool.name === name));
      assert.ok(names(searchTools(descriptors, '后台进程')).includes(name));
    }
  }
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
  const f = await toolFixture(t, nativeCapabilities), ctx = await f.context('full');
  await f.service.catalog(ctx);
  const host = parsed(await f.run(ctx, 'tool.search', { query: '本机终端', limit: 1 }));
  assert.equal(host.tools[0].name, 'terminal.host.run'); assert.equal(host.offset, 0);
  const device = parsed(await f.run(ctx, 'tool.search', { query: '我的IP是什么', limit: 1 }));
  assert.equal(device.tools[0].name, 'terminal.host.run');
  assert.equal(f.service.mcp.connections.size, 0, 'device tool discovery neither starts a browser provider nor executes a host command');
  const browser = parsed(await f.run(ctx, 'tool.search', { query: '浏览器', limit: 1 }));
  assert.ok(browser.total > 1); assert.equal(browser.hasMore, true);
  const second = parsed(await f.run(ctx, 'tool.search', { query: '浏览器', offset: browser.nextOffset, limit: 1 }));
  assert.notEqual(browser.tools[0].name, second.tools[0].name);
});

test('broker tool loading retains a valid screenshot schema beside an unavailable browser tool without executing either', async t => {
  const f = await toolFixture(t, nativeCapabilities), context = await f.context('full');
  await f.service.catalog(context);
  f.service.configureModelCatalog(context, { protocol: protocols[0], tokenBudget: 16000, message: '截图' });
  const screenshot = builtinDescriptors.find(tool => tool.name === 'computer.screenshot');
  const alias = wireCatalog([screenshot])[0].wireName;
  const result = parsed(await f.run(context, 'tool.load', { names: [alias, 'mcp.unconnected.browser_screenshot'] }));
  assert.deepEqual(result.loaded, ['computer.screenshot']);
  assert.deepEqual(result.unavailable, [{ name: 'mcp.unconnected.browser_screenshot', code: 'TOOL_NOT_FOUND' }]);
  assert.ok(f.service.modelCatalog(context).some(tool => tool.name === 'computer.screenshot'));
  assert.equal(f.service.mcp.connections.size, 0, 'loading cannot start an absent provider or perform desktop actions');
});

test('task switches, corrections and uncertain requests do not revive previous capability names', () => {
  const historySignals = ['打开本机浏览器', '查看我的IP'];
  const previousToolNames = ['computer.launch', 'terminal.host.run', 'mcp.chrome-devtools.list_pages'];
  for (const [message, taskRelation] of [
    ['换个话题，看看蛋糕为什么干', undefined],
    ['我说的是人际网络，看看怎么维护', undefined],
    ['看看大家如何 react to criticism', undefined],
    ['this cake seems dry', undefined],
    ['继续讨论另一件事情', { type: 'topic-switch', allowsInheritance: false }],
    ['再来', { type: 'correction', allowsInheritance: true }],
    ['再来', { type: 'new', allowsInheritance: false }],
    ['再来', { type: 'uncertain', allowsInheritance: false }]
  ]) {
    const signals = toolSelectionSignals(message, { historySignals, previousToolNames, taskRelation });
    assert.equal(signals.retainedNames.size, 0, message);
    assert.equal(signals.hostTerminal, false, message);
    assert.equal(signals.desktop, false, message);
    assert.equal(signals.browser, false, message);
    for (const protocol of protocols) {
      const catalog = new ModelToolCatalog(descriptors, { protocol, tokenBudget: 16000, message,
        historySignals, previousToolNames, taskRelation });
      assert.ok(!catalog.selected.some(tool => tool.name.startsWith('terminal.host.') || tool.name.startsWith('computer.')), message);
      for (const name of ['tool.search', 'tool.load']) assert.ok(catalog.selected.some(tool => tool.name === name));
      catalog.load(['terminal.host.run']);
      assert.ok(catalog.selected.some(tool => tool.name === 'terminal.host.run'), 'deferred capabilities remain discoverable');
    }
  }
});

test('social networks, react as a verb and brands are candidate clues rather than device/code decisions', () => {
  for (const message of ['检查我的人际网络', '查看我的关系网络', 'Check my professional network',
    'Inspect my network of mentors', 'How should I react to criticism?', '看看 OpenAI 品牌的历史']) {
    const signals = toolSelectionSignals(message, { historySignals: ['查看我的IP', '打开本机浏览器'],
      previousToolNames: ['terminal.host.run', 'computer.launch'] });
    assert.equal(signals.deviceState, false, message);
    assert.equal(signals.hostTerminal, false, message);
    assert.equal(signals.docs, false, message);
    assert.equal(signals.retainedNames.size, 0, message);
  }
  assert.equal(toolSelectionSignals('React framework documentation').docs, true);
  const socialSearch = remote('mcp.directory.search_people', 'Find professional relationships and a social network.');
  assert.ok(names(searchTools([...descriptors, socialSearch], 'professional relationships')).includes(socialSearch.name));
  assert.ok(names(searchTools([...descriptors, socialSearch], 'Inspect my network of mentors')).includes(socialSearch.name),
    'ambiguous lexical matches retain the social candidate for model evaluation rather than deciding intent');
});

test('continuations retain only the latest subject and explicit task relation cannot copy historical arguments', () => {
  const historySignals = ['调用本机终端', '打开本机浏览器'];
  const previousToolNames = ['terminal.host.run', 'computer.launch', 'mcp.chrome-devtools.list_pages'];
  const signals = toolSelectionSignals('继续', { historySignals, previousToolNames,
    taskRelation: { type: 'continue', allowsInheritance: true } });
  assert.equal(signals.hostTerminal, false);
  assert.equal(signals.desktop, true);
  assert.deepEqual([...signals.retainedNames], ['computer.launch', 'mcp.chrome-devtools.list_pages']);
  const correction = toolSelectionSignals('改为远程浏览器', { historySignals, previousToolNames,
    taskRelation: { type: 'continue', allowsInheritance: true } });
  assert.equal(correction.remoteBrowser, true);
  assert.equal(correction.desktop, false);
  assert.equal(correction.retainedNames.size, 0);
  const research = toolSelectionSignals('再来', { historySignals: ['打开Chrome', '查证今天的官方新闻'],
    previousToolNames: ['computer.launch'] });
  assert.equal(research.desktop, false, 'continuing a later research task does not resurrect an older browser boundary');
  assert.equal(research.browser, false);
  assert.equal(research.web, true, 'only the latest research capability hint is continued');
  assert.equal(research.retainedNames.size, 0);
  for (const latest of ['换个话题，蛋糕为什么干', '我说的是人际网络', '蛋糕为什么干']) {
    const next = toolSelectionSignals('再来', { historySignals: ['打开Chrome', latest],
      previousToolNames: ['computer.launch'] });
    assert.equal(next.desktop, false, latest);
    assert.equal(next.retainedNames.size, 0, latest);
  }
  assert.equal(toolSelectionSignals('再来', { historySignals: ['打开Chrome', '再来', '再试'] }).desktop, true,
    'a bounded chain of explicit retries can retain its original active capability');
});

test('negated and abandoned clauses cannot supply positive capability hints or revive old tool names', () => {
  const historySignals = ['在本机浏览器打开项目页面'], previousToolNames = ['computer.launch', 'mcp.chrome.navigate_page'];
  for (const message of ['先不聊浏览器了，谈谈人际网络怎么建立。',
    '不是打开浏览器，是研究 React 论文的方法。', '暂且搁置本机终端，介绍 OpenAI 品牌。',
    '别再谈浏览器，解释人际信任。', 'Not launching Chrome; explain a social network instead.',
    'Stop discussing the desktop. Discuss the recipe.', '不要检查我的IP，谈谈人际网络',
    '算了，聊点别的：这个方法如何帮助建立人际信任？',
    'Never mind the previous task. How can this method improve a social network?',
    '请注意：不是打开浏览器，是研究 React 论文的方法。', '我说的不是打开浏览器，是研究论文的方法。']) {
    const signals = toolSelectionSignals(message, { historySignals, previousToolNames });
    assert.equal(signals.browser, false, message); assert.equal(signals.desktop, false, message);
    assert.equal(signals.hostTerminal, false, message); assert.equal(signals.deviceState, false, message);
    assert.equal(signals.retainedNames.size, 0, message);
    for (const name of ['tool.search', 'tool.load']) {
      const catalog = new ModelToolCatalog(descriptors, { protocol: protocols[0], tokenBudget: 16000,
        message, historySignals, previousToolNames });
      assert.ok(catalog.selected.some(tool => tool.name === name), message);
    }
  }
  const lateExit = toolSelectionSignals('打开Chrome，算了，改为解释人际网络', { historySignals, previousToolNames });
  assert.equal(lateExit.desktop, false); assert.equal(lateExit.browser, false);
  assert.equal(lateExit.retainedNames.size, 0);
  for (const message of ['不要开新窗口，在当前本机浏览器读取页面', '不是打开Chrome，而是打开本机Edge',
    'Do not open the local browser, use the remote browser to read the page']) {
    const signals = toolSelectionSignals(message, { historySignals, previousToolNames });
    assert.equal(signals.browser, true, message);
    assert.equal(signals.retainedNames.size, 0, 'the positive clause supplies its own candidate, without old names');
  }
});

test('shared clause projection retains original text and records only explicit syntactic exclusions', () => {
  const original = '不是让你查看 src/worker.ts，我是想了解人际网络怎么建立';
  const projected = analyzeRequestClauses(original);
  assert.equal(original, '不是让你查看 src/worker.ts，我是想了解人际网络怎么建立');
  assert.equal(projected.boundary, 'correction');
  assert.doesNotMatch(projected.activeText, /src\/worker\.ts/);
  assert.match(projected.activeText, /人际网络/);
  for (const clause of projected.excludedClauses)
    assert.equal(original.slice(clause.startOffset, clause.endOffset), clause.text);
  assert.equal(projected.originalPreserved, true); assert.equal(projected.semanticVerified, false);
  const abandoned = '打开Chrome，算了，改为解释人际网络';
  const exited = analyzeRequestClauses(abandoned);
  assert.equal(exited.activeText, '改为解释人际网络');
  assert.ok(exited.excludedClauses.some(clause => clause.text === '打开Chrome' && clause.basis === 'superseded-by-task-boundary'));
  assert.ok(exited.provenance.some(item => item.rejected === '打开Chrome' && item.accepted === exited.activeText));
  for (const clause of exited.excludedClauses)
    assert.equal(abandoned.slice(clause.startOffset, clause.endOffset), clause.text);
  const english = analyzeRequestClauses('Correction: not open the browser; explain a paper instead.');
  assert.match(english.activeText, /explain a paper instead/);
  assert.doesNotMatch(english.activeText, /open the browser/);
  assert.match(analyzeRequestClauses('Rather than opening Chrome, explain a paper').activeText, /explain a paper/);
  assert.match(analyzeRequestClauses('更正：我要的是2026年英文论文，不是2025年').activeText, /2026/);
  assert.doesNotMatch(analyzeRequestClauses('更正：我要的是2026年英文论文，不是2025年').activeText, /2025/);
  for (const value of ['方法是实验方法，不涉及操作', 'open Chrome', 'src/worker.ts', 'https://example.invalid/path', 'C:\\Work\\note.ts'])
    assert.equal(analyzeRequestClauses(value).activeText, value, 'ordinary inputs remain unchanged');
});

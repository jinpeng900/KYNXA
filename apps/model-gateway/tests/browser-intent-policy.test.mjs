import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from 'node:test';
import { inferBrowserTaskIntent, canUseBrowserServer, assertBrowserLaunchAllowed } from '../tools/browser-intent-policy.mjs';
import { isBackgroundBrowserConnection } from '../tools/browser-connections.mjs';
import { WebFetchTool } from '../tools/web-fetch.mjs';
import { fetchPublicWebPage } from '../tools/web-http-transport.mjs';
import { toolFixture, parsed } from './tool-fixture.mjs';

const server = (id, args) => ({ id, name: id, command: 'npx', enabled: true, args });
const headed = server('visible-browser', ['chrome-devtools-mcp@1.10.1', '--isolated']);
const existing = server('signed-in-browser', ['chrome-devtools-mcp@1.10.1', '--autoConnect']);
const isolated = server('background-browser', ['chrome-devtools-mcp@1.10.1', '--headless', '--isolated']);
const remote = server('remote-browser', ['@playwright/mcp@0.0.83', '--cdp-endpoint', 'https://browser.example.invalid']);
const listing = value => ({ name: `mcp.${value.id}.take_snapshot`, serverId: value.id, toolName: 'take_snapshot', operation: 'tools/call',
  source: `mcp:${value.id}`, description: 'Synthetic browser DOM reading.',
  originalInputSchema: { type: 'object', properties: {} },
  inputSchema: { type: 'object', properties: { arguments: { type: 'object', properties: {} }, policy: { type: 'object' } }, required: ['arguments', 'policy'] } });

async function configuredFixture(t, options = {}) {
  const f = await toolFixture(t, options), config = await f.service.getConfig();
  await f.service.updateConfig({ ...config, expectedRevision: config.revision, mcpServers: [headed, existing, isolated, remote] });
  const discoveries = [], dispatched = [];
  // Mock the directory and execution port only; the real broker still controls filtering and result persistence.
  // 只模拟目录与执行端口；真实代理仍负责发现过滤、调用保护和结果持久化，不启动浏览器。
  f.service.mcp.catalog = async input => { discoveries.push(input.mcpServers.map(item => item.id)); return [headed, existing, isolated, remote].map(listing); };
  f.service.mcp.prepareBrowserExecution = async (_descriptor, input) => input;
  f.service.mcp.execute = async descriptor => { dispatched.push(descriptor.name); return { value: { content: 'Synthetic official page evidence.' }, isError: false }; };
  f.service.mcp.validateExecution = async () => ({ connection: {} });
  const context = message => f.service.createContext(f.conversationId, { requestId: randomUUID(), permissionMode: 'full', message });
  return { ...f, discoveries, dispatched, browserContext: context };
}

test('current user intent separates ordinary search, browser tasks, prohibitions and pure follow-ups', () => {
  for (const message of ['王者荣耀最新英雄是谁', '搜索官网最新公告', '读取 https://example.invalid 的网页', '不要打开浏览器，搜索网页',
    '请不要用本机浏览器搜索', '不用浏览器搜最新公告', 'knowledge search for latest releases',
    '> 打开 Google 浏览器', '```打开 Chrome```'])
    assert.equal(inferBrowserTaskIntent(message).allowLocalBrowser, false, message);
  for (const message of ['打开 Google 浏览器', '读取本机 Chrome 浏览器内容', '用浏览器登录网站', '给浏览器截图', 'open my local Chrome browser', 'open the browser'])
    assert.equal(inferBrowserTaskIntent(message).allowLocalBrowser, true, message);
  assert.equal(inferBrowserTaskIntent('操作云端浏览器').allowLocalBrowser, false);
  assert.equal(inferBrowserTaskIntent('再次尝试', ['打开本机浏览器']).allowLocalBrowser, true);
  assert.equal(inferBrowserTaskIntent('查最新新闻', ['打开本机浏览器']).allowLocalBrowser, false);
  assert.equal(inferBrowserTaskIntent('再次尝试', ['打开本机浏览器', '查最新新闻']).allowLocalBrowser, false);
});

test('browser brands and search engines as research subjects do not authorize local browser control', () => {
  for (const message of ['搜索 Google 最新新闻', '搜索 Chrome 最新版本', '查看 Chrome 最新版本', '查 Chrome 文档', '读取 Chrome 文档',
    '打开 Chrome 官网', '查 Edge 版本', '搜索谷歌浏览器最新消息', 'search Google for latest news', 'search Chrome latest version',
    'read Chrome documentation', 'inspect Firefox release notes', 'open Chrome documentation', 'search with Google for Chrome news',
    'look up browser documentation', 'read browser documentation', '搜索 Chrome 截图教程', '查看 Chrome 怎么截图',
    'search Chrome screenshot tutorial', 'read how to launch Chrome']) {
    const intent = inferBrowserTaskIntent(message);
    assert.equal(intent.allowLocalBrowser, false, message); assert.equal(intent.explicitBrowserTask, false, message);
  }
  for (const message of ['打开google浏览器', '用Chrome打开网址', '读取本机浏览器内容', '让浏览器截图', '把浏览器缩小一下',
    '使用 Chrome 搜索最新版本', '在 Chrome 中搜索最新新闻', 'use Chrome to open the URL', 'search in Chrome', 'read the local browser content'])
    assert.equal(inferBrowserTaskIntent(message).allowLocalBrowser, true, message);
});

test('a local-browser prohibition preserves a separate explicit remote-browser task', () => {
  const mixed = inferBrowserTaskIntent('不要打开本机浏览器，用云端浏览器读取页面');
  assert.equal(mixed.allowLocalBrowser, false); assert.equal(mixed.explicitBrowserTask, true);
  const english = inferBrowserTaskIntent('Do not open the local browser, use the remote browser to read the page');
  assert.equal(english.allowLocalBrowser, false); assert.equal(english.explicitBrowserTask, true);
  const ordinary = inferBrowserTaskIntent('不要开浏览器，上网查');
  assert.equal(ordinary.allowLocalBrowser, false); assert.equal(ordinary.explicitBrowserTask, false);
});

test('rejected operations and abandoned topics cannot authorize browser actions', () => {
  const history = ['在本机浏览器打开项目页面'];
  for (const message of ['不是打开浏览器，是研究 React 论文的方法。', '先不聊浏览器了，谈谈人际网络怎么建立。',
    '不是让你使用本机 Chrome，是解释品牌名称。', 'Not opening Chrome; explain a social network instead.',
    'Never mind the browser task. Discuss a recipe.', '不是在云端浏览器读取页面，是分析论文实验。',
    '请注意：不是打开浏览器，是研究 React 论文的方法。', '我说的不是打开浏览器，是研究 React 论文的方法。',
    'Correction: not open the browser; explain a paper instead.', 'Rather than opening Chrome, explain a paper']) {
    const intent = inferBrowserTaskIntent(message, history);
    assert.equal(intent.explicitBrowserTask, false, message);
    assert.equal(intent.allowLocalBrowser, false, message);
    assert.throws(() => assertBrowserLaunchAllowed({ message, browserTaskIntent: intent }, 'C:\\Fixture\\chrome.exe'),
      { code: 'BROWSER_TASK_NOT_AUTHORIZED' }, message);
    assert.equal(canUseBrowserServer({ message, browserTaskIntent: intent }, existing), false, message);
  }
  for (const message of ['不要打开新浏览器窗口，在当前本机浏览器打开学习通网站',
    '不是用 Chrome，而是用本机 Edge 打开网页', 'Do not open Chrome, use Edge to read the page',
    '不是搜索 Chrome 文档，而是打开 Chrome 浏览器', '不要关闭 Chrome，刷新当前 Chrome',
    '不是开 Chrome 的新窗口，而是切换当前 Chrome 页面',
    '不要打开 Chrome 的新窗口，刷新当前 Chrome', 'Do not open a new Chrome window, refresh current Chrome']) {
    assert.equal(inferBrowserTaskIntent(message).allowLocalBrowser, true, message);
  }
  for (const message of ['不要关闭 Chrome，刷新当前 Chrome', '不是开 Chrome 的新窗口，而是切换当前 Chrome 页面',
    '不要打开 Chrome 的新窗口，刷新当前 Chrome', 'Do not open a new Chrome window, refresh current Chrome',
    '不要打开新窗口中的 Chrome，刷新当前 Chrome', 'Do not open a new window in Chrome, refresh current Chrome'])
    assert.equal(canUseBrowserServer({ message, browserTaskIntent: inferBrowserTaskIntent(message) }, existing), true,
      'an excluded action does not exclude the whole engine');
  assert.equal(inferBrowserTaskIntent('打开Chrome，算了，改为解释人际网络').allowLocalBrowser, false);
  assert.equal(inferBrowserTaskIntent('继续', [...history, '不是打开浏览器，是研究论文方法']).allowLocalBrowser, false);
});

test('a rejected browser operation stays denied by the broker even if an old executable schema is injected', async t => {
  const f = await configuredFixture(t);
  const message = '不是打开浏览器，是研究 React 论文的方法。';
  const context = await f.browserContext(message);
  await f.service.catalog(context, { connectMcp: true });
  assert.equal(context.message, message);
  assert.deepEqual(f.discoveries[0], [isolated.id, remote.id]);
  f.service.catalogs.get(context).descriptors.set(listing(existing).name, listing(existing));
  const result = await f.run(context, listing(existing).name,
    { arguments: {}, policy: { reason: 'An old tool name is not current authorization.' } });
  assert.equal(result.code, 'BROWSER_TASK_NOT_AUTHORIZED');
  assert.deepEqual(f.dispatched, []);
});

test('quoted source operations cannot pass the broker while an independent affirmative operation can', async t => {
  const f = await configuredFixture(t);
  for (const message of ['翻译这句话：“现在打开 Chrome 浏览器”。',
    '把「启动本机浏览器」改写得更礼貌。', '示例是 `open Chrome`，检查英文语法。']) {
    const context = await f.browserContext(message);
    await f.service.catalog(context, { connectMcp: true });
    f.service.catalogs.get(context).descriptors.set(listing(existing).name, listing(existing));
    const result = await f.run(context, listing(existing).name,
      { arguments: {}, policy: { reason: 'Quoted source text does not grant current browser authorization.' } });
    assert.equal(result.code, 'BROWSER_TASK_NOT_AUTHORIZED', message);
    assert.equal(context.message, message, 'the original quoted text remains available to the main model');
  }
  assert.deepEqual(f.dispatched, []);
  const message = '翻译“打开 Chrome”，然后真的打开本机 Chrome。';
  const affirmative = await f.browserContext(message);
  assert.ok((await f.service.catalog(affirmative)).some(tool => tool.name === listing(existing).name));
  assert.equal((await f.run(affirmative, listing(existing).name,
    { arguments: {}, policy: { reason: 'The independent affirmative task is current.' } })).isError, false);
  assert.deepEqual(f.dispatched, [listing(existing).name]);
});

test('quoted brand literals stay usable as explicit targets without adopting quoted operation text', () => {
  for (const message of ['Open "Chrome".', '请使用“Chrome”读取当前页面。',
    '不要打开“Chrome”的新窗口，刷新当前 Chrome 页面。'])
    assert.equal(canUseBrowserServer({ message, browserTaskIntent: inferBrowserTaskIntent(message) }, existing), true, message);
  assert.equal(inferBrowserTaskIntent('retry', ['打开本机 Chrome', '日志是“refresh current page”；仅解释日志。']).allowLocalBrowser, false);
});

test('how-to questions remain informational while explicit execution and live demonstrations stay available', () => {
  for (const message of ['Chrome怎么截图', '如何使用Chrome浏览器', '请解释Chrome怎么截图', '告诉我如何打开浏览器',
    'How do I use Chrome?', 'How can I take a screenshot in Chrome?', 'Tell me how to open the browser']) {
    const intent = inferBrowserTaskIntent(message);
    assert.equal(intent.allowLocalBrowser, false, message); assert.equal(intent.explicitBrowserTask, false, message);
  }
  for (const message of ['帮我用Chrome截图', '请实际演示Chrome截图', '帮我演示如何用Chrome截图', '请实际演示如何使用Chrome浏览器',
    'Please actually demonstrate how to take a screenshot in Chrome', 'Help me use Chrome to demonstrate how screenshots work'])
    assert.equal(inferBrowserTaskIntent(message).allowLocalBrowser, true, message);
});

test('natural website follow-ups retain only the active browser task and do not authorize unrelated searches', () => {
  const history = ['打开本机浏览器'];
  for (const message of ['打开学习通网站', '登录学习通', '截图，我二维码登录', '刷新当前页面', '向下滚动页面',
    '查看有哪些作业未完成', '关闭当前页面']) {
    const intent = inferBrowserTaskIntent(message, history);
    assert.equal(intent.allowLocalBrowser, true, message);
    assert.equal(intent.inherited, true, message);
    history.push(message);
  }
  assert.equal(inferBrowserTaskIntent('搜索最新新闻', history).allowLocalBrowser, false);
  assert.equal(inferBrowserTaskIntent('继续', [...history, '搜索最新新闻']).allowLocalBrowser, false);
  assert.equal(inferBrowserTaskIntent('打开学习通网站', ['操作云端浏览器']).allowLocalBrowser, false);
  assert.equal(inferBrowserTaskIntent('刷新当前页面', ['打开本机浏览器', ...Array(220).fill('查看当前页面')]).allowLocalBrowser, true);
  assert.equal(inferBrowserTaskIntent('截图二维码', ['操作云端浏览器', '登录网站']).allowLocalBrowser, false);
  for (const message of ['刷新本机浏览器当前页面', '关闭本机浏览器', '向下滚动本机浏览器页面',
    '不要打开新浏览器窗口，在当前本机浏览器打开学习通网站', '不要用 Chrome，用本机 Edge 打开学习通网站'])
    assert.equal(inferBrowserTaskIntent(message).allowLocalBrowser, true, message);
});

test('an excluded browser brand does not prohibit another explicitly requested brand', () => {
  const context = { message: '不要用 Chrome，用本机 Edge 打开学习通网站' };
  assert.throws(() => assertBrowserLaunchAllowed(context, 'C:\\Fixture\\chrome.exe'), { code: 'BROWSER_TASK_NOT_AUTHORIZED' });
  assert.doesNotThrow(() => assertBrowserLaunchAllowed(context, 'C:\\Fixture\\msedge.exe'));
  assert.equal(canUseBrowserServer(context, existing), false);
  assert.equal(canUseBrowserServer(context, { ...existing, args: [...existing.args, '--executablePath=C:\\Fixture\\msedge.exe'] }), true);
  assert.equal(inferBrowserTaskIntent('打开学习通网站', ['操作云端浏览器', '打开学习通网站']).allowLocalBrowser, false);
});

test('app-wide restrictions survive explanatory window mentions while actual window objects stay scoped', () => {
  for (const message of ['不要使用 Chrome（窗口和标签都不用），改用本机 Edge 查看资料。',
    'Do not use Chrome (including its tabs); use local Edge to read the page.',
    '不要为了新建窗口启动 Chrome；打开本机 Edge。',
    '不要因为窗口标题写着 Chrome 就启动 Chrome；使用本机 Edge。',
    '不要为阅读页面而打开 Chrome，打开本机 Edge。']) {
    const context = { message, browserTaskIntent: inferBrowserTaskIntent(message) };
    assert.equal(context.browserTaskIntent.allowLocalBrowser, true, 'the independent Edge task remains available');
    assert.equal(canUseBrowserServer(context, existing), false, message);
    assert.throws(() => assertBrowserLaunchAllowed(context, 'C:\\Fixture\\chrome.exe'), { code: 'BROWSER_TASK_NOT_AUTHORIZED' });
    assert.doesNotThrow(() => assertBrowserLaunchAllowed(context, 'C:\\Fixture\\msedge.exe'));
  }
  for (const message of ['不要打开 Chrome 的新窗口；刷新当前 Chrome 页面。',
    "Do not open Chrome's current tab; refresh its existing Chrome tab.",
    'Do not open a new window in Chrome; refresh current Chrome.'])
    assert.equal(canUseBrowserServer({ message, browserTaskIntent: inferBrowserTaskIntent(message) }, existing), true,
      'a directly targeted window/tab is not an app-wide prohibition');
});

test('the broker rejects an app ban with a preceding window reason even when a Chrome schema is injected', async t => {
  const f = await configuredFixture(t);
  for (const message of ['不要为了新建窗口启动 Chrome；打开本机 Edge。',
    '不要因为窗口标题写着 Chrome 就启动 Chrome；使用本机 Edge。',
    '不要为阅读页面而打开 Chrome，打开本机 Edge。']) {
    const context = await f.browserContext(message);
    const catalog = await f.service.catalog(context, { connectMcp: true });
    assert.equal(context.message, message, 'the current original restriction remains authoritative');
    assert.ok(!catalog.some(tool => tool.name === listing(existing).name), 'discovery excludes the banned app');
    f.service.catalogs.get(context).descriptors.set(listing(existing).name, listing(existing));
    const result = await f.run(context, listing(existing).name,
      { arguments: {}, policy: { reason: 'A window reason cannot cancel the current Chrome prohibition.' } });
    assert.equal(result.code, 'BROWSER_TASK_NOT_AUTHORIZED', message);
  }
  assert.deepEqual(f.dispatched, [], 'the injected Chrome schema never causes dispatch');
});

test('explicit task boundaries clear old browser authorization before considering a new affirmative task', () => {
  const prior = '打开本机 Chrome 浏览器';
  for (const ending of ['不是刷新当前页面，而是讨论人际信任。',
    '先不聊刷新页面了，介绍论文实验。', '浏览器操作全部取消；现在只比较论文。']) {
    assert.equal(inferBrowserTaskIntent('retry', [prior, ending]).allowLocalBrowser, false, ending);
    assert.equal(inferBrowserTaskIntent(ending, [prior]).allowLocalBrowser, false, ending);
  }
  for (const replacement of ['不是读取论文，而是刷新本机 Chrome 当前页面。',
    '先不聊论文了，现在用本机 Chrome 打开网页。'])
    assert.equal(inferBrowserTaskIntent('retry', [prior, replacement]).allowLocalBrowser, true,
      'the new affirmative browser task establishes its own current boundary');
});

test('only trusted isolated headless or nonlocal remote configuration supports automatic background reading', () => {
  const context = { message: '查最新消息' };
  for (const value of [headed, existing, server('not-isolated', ['chrome-devtools-mcp@1.10.1', '--headless']),
    server('headless-conflict', ['chrome-devtools-mcp@1.10.1', '--headless', '--headless=false', '--isolated']),
    server('isolation-conflict', ['@playwright/mcp@0.0.83', '--headless', '--isolated', 'false']),
    server('custom', ['@playwright/mcp@0.0.83', '--headless', '--isolated', '--config', 'custom.json']),
    server('local-cdp', ['@playwright/mcp@0.0.83', '--cdp-endpoint=http://127.0.0.1:9222'])]) {
    assert.equal(isBackgroundBrowserConnection(value), false, value.id);
    assert.equal(canUseBrowserServer(context, value), false, value.id);
  }
  assert.equal(isBackgroundBrowserConnection(isolated), true); assert.equal(isBackgroundBrowserConnection(remote), true);
  assert.equal(canUseBrowserServer(context, isolated), true); assert.equal(canUseBrowserServer(context, remote), true);
});

test('ordinary search filters before connecting and in discovery/loading, while permitted background DOM remains callable', async t => {
  const f = await configuredFixture(t), context = await f.browserContext('查询最新官方消息');
  const catalog = await f.service.catalog(context, { connectMcp: true });
  assert.deepEqual(f.discoveries[0], [isolated.id, remote.id]);
  assert.ok(!catalog.some(tool => tool.name === listing(headed).name || tool.name === listing(existing).name));
  f.service.configureModelCatalog(context, { protocol: 'openai-completions', tokenBudget: 16000, message: context.message });
  const search = parsed(await f.run(context, 'tool.search', { query: 'browser' }));
  assert.ok(search.tools.every(tool => tool.name !== listing(headed).name && tool.name !== listing(existing).name));
  const load = await f.run(context, 'tool.load', { names: [listing(headed).name] });
  assert.equal(load.code, 'TOOL_NOT_FOUND');
  const result = await f.run(context, listing(isolated).name, { arguments: {}, policy: { reason: 'Read only background page.' } });
  assert.equal(result.isError, false); assert.deepEqual(f.dispatched, [listing(isolated).name]);
  // A stale/injected catalog must not bypass the execution guard through a model-written reason.
  // 即使旧目录被注入，模型编写的调用理由也不能绕过执行保护。
  f.service.catalogs.get(context).descriptors.set(listing(headed).name, listing(headed));
  const denied = await f.run(context, listing(headed).name, { arguments: {}, policy: { reason: 'The user wants to open Chrome.' } });
  assert.equal(denied.code, 'BROWSER_TASK_NOT_AUTHORIZED'); assert.equal(f.dispatched.length, 1);
});

test('explicit local browser operations and a bare retry stay available, a later new search revokes that intent', async t => {
  const f = await configuredFixture(t), explicit = await f.browserContext('打开并读取本机浏览器');
  assert.ok((await f.service.catalog(explicit)).some(tool => tool.name === listing(existing).name));
  assert.equal((await f.run(explicit, listing(existing).name, { arguments: {}, policy: { reason: 'User explicitly requested local browser.' } })).isError, false);
  await f.conversations.upsertMessage(f.conversationId, { Id: randomUUID(), Role: 'user', Content: '打开并读取本机浏览器', Status: 'completed', CreatedAt: new Date().toISOString() });
  const retry = await f.browserContext('再次尝试'); assert.equal(retry.browserTaskIntent.allowLocalBrowser, true);
  assert.ok((await f.service.catalog(retry)).some(tool => tool.name === listing(existing).name));
  await f.conversations.upsertMessage(f.conversationId, { Id: randomUUID(), Role: 'user', Content: '查最新新闻', Status: 'completed', CreatedAt: new Date().toISOString() });
  const subsequent = await f.browserContext('再次尝试'); assert.equal(subsequent.browserTaskIntent.allowLocalBrowser, false);
  assert.ok(!(await f.service.catalog(subsequent)).some(tool => tool.name === listing(existing).name));
});

test('ordinary research cannot open a known browser through computer.launch; unrelated explicit app launching is retained', async t => {
  const calls = [], desktopRunner = { capabilities: async () => ({ available: true, boundary: 'host-desktop', operations: ['launch'] }),
    run: async (action, input) => { calls.push({ action, input }); return { value: { launched: true }, isError: false }; } };
  const f = await configuredFixture(t, { desktopRunner }), research = await f.browserContext('搜索最新公告');
  const browserPath = join(f.workspace, 'chrome.exe'), editorPath = join(f.workspace, 'notepad.exe');
  await Promise.all([browserPath, editorPath].map(appPath => writeFile(appPath, 'Synthetic application metadata; mocked launch only.')));
  const denied = await f.run(research, 'computer.launch', { appPath: browserPath, reason: 'Need to verify the page.' });
  assert.equal(denied.code, 'BROWSER_TASK_NOT_AUTHORIZED'); assert.equal(calls.length, 0);
  const explicit = await f.browserContext('打开本机 Chrome 浏览器');
  assert.equal((await f.run(explicit, 'computer.launch', { appPath: browserPath, reason: 'Explicit local browser task.' })).isError, false);
  assert.equal((await f.run(research, 'computer.launch', { appPath: editorPath, reason: 'Unrelated existing app launch behavior.' })).isError, false);
  assert.equal(calls.length, 2);
});

test('fake DNS stays blocked before network access and only advertises an actually permitted background fallback', async t => {
  let dnsAddress = '198.18.0.2';
  const reader = new WebFetchTool({ fetchPage: (url, options) => fetchPublicWebPage(url, options, {
    lookup: async () => [{ address: dnsAddress, family: 4 }], httpsRequest: () => assert.fail('Nonpublic DNS must remain blocked')
  }) });
  const f = await configuredFixture(t, { webFetcher: reader }), context = await f.browserContext('搜索官方最新消息');
  await f.service.catalog(context);
  const args = { url: 'https://official.example.invalid/announcement', reason: 'Read a public official source.' };
  const failed = await f.run(context, 'web.fetch', args);
  assert.equal(failed.code, 'WEB_URL_BLOCKED'); assert.match(failed.content, /后台网页读取工具/); assert.match(failed.content, /不会打开本机浏览器/);
  dnsAddress = '127.0.0.1';
  const privateAddress = await f.run(context, 'web.fetch', args);
  assert.equal(privateAddress.code, 'WEB_URL_BLOCKED'); assert.doesNotMatch(privateAddress.content, /后台网页读取工具/);
  dnsAddress = '198.18.0.2';
  const snapshot = f.service.catalogs.get(context); snapshot.descriptors.delete(listing(isolated).name); snapshot.descriptors.delete(listing(remote).name);
  const unavailable = await f.run(context, 'web.fetch', args);
  assert.equal(unavailable.code, 'WEB_URL_BLOCKED'); assert.match(unavailable.content, /已有搜索证据/); assert.doesNotMatch(unavailable.content, /请使用已启用的浏览器/);
  assert.deepEqual(f.dispatched, []);
});

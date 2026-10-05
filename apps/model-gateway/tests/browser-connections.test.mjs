import assert from 'node:assert/strict';
import test from 'node:test';
import { randomUUID } from 'node:crypto';
import { browserConnection, browserConnectionPrompt } from '../tools/browser-connections.mjs';
import { toolFixture } from './tool-fixture.mjs';

const server = (id, args, extra = {}) => ({ id, name: id, command: 'npx', enabled: true, args, ...extra });

test('browser connection prompt distinguishes existing, isolated, custom and remote without leaking credentials', () => {
  const servers = [server('local', ['chrome-devtools-mcp@1.10.1', '--autoConnect']),
    server('independent', ['@playwright/mcp@0.0.83', '--headless', '--isolated']),
    server('remote', ['chrome-devtools-mcp@1.10.1', '--wsEndpoint=wss://fixture.invalid/browser?token=SYNTHETIC_NOT_FOR_MODEL']),
    server('env', ['@playwright/mcp@0.0.83'], { envRefs: { PLAYWRIGHT_MCP_CDP_ENDPOINT: 'SYNTHETIC_ENDPOINT_ENV' } })];
  assert.deepEqual(servers.map(value => browserConnection(value).mode),
    ['existing-browser', 'independent-browser', 'remote-browser', 'custom-browser']);
  const prompt = browserConnectionPrompt(servers).join('\n');
  assert.match(prompt, /existing signed-in pages/); assert.match(prompt, /headless \(not visible\)/);
  assert.match(prompt, /does not inherit/); assert.doesNotMatch(prompt, /SYNTHETIC|fixture.invalid|wss:\/\//);
  assert.deepEqual(browserConnectionPrompt(servers.map(value => ({ ...value, enabled: false }))), []);
  assert.equal(browserConnection(server('other', ['unrelated-package'])), null);
});

test('browser prompt belongs to the captured catalog and preserves current capability over historical reports', async t => {
  const f = await toolFixture(t), config = await f.service.getConfig();
  await f.service.updateConfig({ ...config, expectedRevision: config.revision, mcpServers: [
    server('fixture-browser', ['chrome-devtools-mcp@1.10.1', '--autoConnect'])] });
  const disconnected = await f.context();
  await f.service.catalog(disconnected); // A configured service without a discovered directory is not a ready capability.
  // 仅配置服务但未发现工具目录，不视为已就绪的能力。
  assert.doesNotMatch(await f.service.systemPrompt(disconnected), /Browser MCP fixture-browser:/);
  assert.equal(f.service.mcp.connections.size, 0, 'Passive discovery never starts a real browser or account connection');
  let remote = [{ name: 'mcp.fixture-browser.list_pages', description: 'List only synthetic fixture pages.',
    source: 'mcp:fixture-browser', serverId: 'fixture-browser', toolName: 'list_pages',
    inputSchema: { type: 'object', properties: {}, additionalProperties: false } }];
  // Model a discovered directory explicitly; no process or browser action is performed by this test.
  // 显式模拟已发现的工具目录；本测试不启动进程或操作浏览器。
  f.service.mcp.catalog = async () => remote;
  const context = await f.service.createContext(f.conversationId, { requestId: randomUUID(), permissionMode: 'ask', message: '读取本机浏览器内容' });
  await f.service.catalog(context);
  const prompt = await f.service.systemPrompt(context);
  assert.match(prompt, /fixture-browser: Connects to a local existing browser/);
  assert.match(prompt, /Current capabilities override historical unavailable reports/);
  assert.match(prompt, /signed-in browsing.*allowed/);
  remote = [];
  const next = await f.context();
  await f.service.catalog(next);
  assert.doesNotMatch(await f.service.systemPrompt(next), /Browser MCP fixture-browser:/,
    'A subsequent empty directory cannot advertise the configured browser as ready');
  assert.match(await f.service.systemPrompt(context), /fixture-browser: Connects to a local existing browser/,
    'The previous context retains its own captured capability snapshot');
  await f.service.releaseContext(disconnected);
  await f.service.releaseContext(next);
  await f.service.releaseContext(context);
});

test('a discovered browser with all tools disabled is not advertised as an available browser', async t => {
  const f = await toolFixture(t), config = await f.service.getConfig();
  await f.service.updateConfig({ ...config, expectedRevision: config.revision, mcpServers: [
    server('fixture-browser', ['chrome-devtools-mcp@1.10.1', '--autoConnect'], { disabledTools: ['list_pages'] })] });
  f.service.mcp.catalog = async () => [{ name: 'mcp.fixture-browser.list_pages', description: 'Synthetic pages.',
    source: 'mcp:fixture-browser', serverId: 'fixture-browser', toolName: 'list_pages',
    inputSchema: { type: 'object', properties: {}, additionalProperties: false } }];
  const context = await f.context();
  const catalog = await f.service.catalog(context);
  assert.ok(catalog.every(tool => tool.name !== 'mcp.fixture-browser.list_pages'));
  assert.doesNotMatch(await f.service.systemPrompt(context), /Browser MCP fixture-browser:/);
  await f.service.releaseContext(context);
});

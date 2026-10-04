import assert from 'node:assert/strict';
import test from 'node:test';
import { browserConnection, browserConnectionPrompt } from '../browser-connections.mjs';
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
  const context = await f.context();
  await f.service.catalog(context); // Discovery only: never start a real browser or account connection.
  const prompt = await f.service.systemPrompt(context);
  assert.match(prompt, /fixture-browser: Connects to a local existing browser/);
  assert.match(prompt, /Current capabilities override historical unavailable reports/);
  assert.match(prompt, /signed-in browsing.*allowed/);
  await f.service.releaseContext(context);
});

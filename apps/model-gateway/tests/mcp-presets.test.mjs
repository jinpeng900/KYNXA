import assert from 'node:assert/strict';
import test from 'node:test';
import { mcpPresetCatalog, addMcpPreset } from '../mcp-presets.mjs';
import { normalizeMcpConnection } from '../mcp-config.mjs';
import { toolFixture } from './tool-fixture.mjs';

test('public MCP catalog has pinned publisher packages, clear setup requirements and no duplicate builtin implementation', () => {
  const catalog = mcpPresetCatalog({ mcpServers: [] });
  assert.equal(catalog.presets.length, 11);
  assert.equal(new Set(catalog.presets.map(item => item.id)).size, 11);
  assert.equal(new Set(catalog.presets.flatMap(item => item.capabilities)).size, 11);
  assert.deepEqual(catalog.reusedCapabilities, ['filesystem', 'chat-memory', 'work-memory', 'tool-results', 'sandbox-terminal']);
  for (const preset of catalog.presets) {
    assert.equal(preset.server.enabled, false);
    assert.equal(preset.alreadyConfigured, false);
    assert.match(preset.sourceUrl, /^https:\/\/github\.com\//);
    assert.ok(preset.license && ['reference', 'vendor', 'community'].includes(preset.publisher));
    assert.ok(['local', 'remote', 'local-and-remote'].includes(preset.network));
    assert.doesNotThrow(() => normalizeMcpConnection(preset.server));
    if (preset.package) {
      assert.match(preset.package.version, /^\d+(?:\.\d+)+(?:a\d+)?$/);
      const separator = preset.package.registry === 'npm' ? '@' : '==';
      assert.ok(preset.server.args.includes(preset.package.name + separator + preset.package.version));
      assert.ok(preset.requirements.some(item => item.type === 'runtime' && item.required));
    } else assert.ok(preset.notes.some(note => /[Hh]osted.*version/.test(note)));
    assert.ok(!preset.capabilities.some(name => ['filesystem', 'memory', 'terminal'].includes(name)));
  }
  const changed = catalog.presets[0]; changed.server.args.push('modified'); changed.requirements[0].name = 'changed';
  const unchanged = mcpPresetCatalog({ mcpServers: [] }).presets[0];
  assert.equal(unchanged.server.args.includes('modified'), false); assert.notEqual(unchanged.requirements[0].name, 'changed');
});

test('credentialed presets save only environment references while anonymous services start without required secrets', () => {
  const presets = mcpPresetCatalog({ mcpServers: [] }).presets;
  const brave = presets.find(item => item.id === 'brave-search'), github = presets.find(item => item.id === 'github');
  assert.deepEqual(brave.server.envRefs, { BRAVE_API_KEY: 'KYNXA_BRAVE_API_KEY' });
  assert.deepEqual(github.server.headerEnv, { Authorization: 'KYNXA_GITHUB_AUTHORIZATION' });
  assert.equal(brave.server.args.includes('http'), false);
  for (const id of ['playwright', 'context7', 'chrome-devtools', 'exa']) {
    const preset = presets.find(item => item.id === id);
    assert.ok(!preset.requirements.some(item => item.type === 'environment' && item.required));
    assert.deepEqual(preset.server.envRefs ?? {}, {}); assert.deepEqual(preset.server.headerEnv ?? {}, {});
  }
  assert.deepEqual(presets.find(item => item.id === 'exa').server.disabledTools, ['web_fetch_exa']);
});

test('database and desktop presets restrict capabilities explicitly rather than advertising unavailable flags', () => {
  const presets = mcpPresetCatalog({ mcpServers: [] }).presets;
  const db = presets.find(item => item.id === 'dbhub'), desktop = presets.find(item => item.id === 'windows-screenshot');
  assert.equal(db.server.args.includes('--readonly'), false);
  assert.ok(db.server.args.includes('--config'));
  assert.match(db.configurationTemplate, /readonly = true/); assert.match(db.configurationTemplate, /max_rows = 100/);
  assert.match(db.configurationTemplate, /dsn = "\$\{DSN\}"/);
  assert.deepEqual(db.server.envRefs, { DSN: 'KYNXA_DBHUB_DSN' });
  assert.ok(db.requirements.some(item => item.type === 'configuration' && item.required));
  assert.deepEqual(desktop.server.args.slice(-2), ['--tools', 'Screenshot,Snapshot']);
  assert.equal(desktop.publisher, 'community'); assert.equal(desktop.server.env.ANONYMIZED_TELEMETRY, 'false');
  assert.ok(desktop.requirements.some(item => item.description.includes('3.14')));
});

test('preset adds persist all eleven disabled services exactly once and never connect public processes', async t => {
  const f = await toolFixture(t);
  let config = await f.service.getConfig();
  for (const preset of mcpPresetCatalog(config).presets) config = await addMcpPreset(f.service, preset.id, { expectedRevision: config.revision });
  assert.equal(config.mcpServers.length, 11); assert.ok(config.mcpServers.every(server => !server.enabled));
  const revision = config.revision;
  for (const preset of mcpPresetCatalog(config).presets) {
    assert.equal(preset.alreadyConfigured, true);
    config = await addMcpPreset(f.service, preset.id, { expectedRevision: revision });
  }
  assert.equal(config.revision, revision); assert.equal(f.service.mcp.connections.size, 0);
  assert.deepEqual(f.service.mcp.diagnostics(), []);
  await assert.rejects(addMcpPreset(f.service, 'fetch', { expectedRevision: 0 }), { code: 'AGENT_CONFIG_CONFLICT' });
  await assert.rejects(addMcpPreset(f.service, 'unknown', { expectedRevision: revision }), { code: 'MCP_PRESET_NOT_FOUND' });
});

test('publisher deduplication keeps custom versions, credentials, profiles and Python module installations intact', async t => {
  const f = await toolFixture(t), initial = await f.service.getConfig();
  const samples = [
    { id: 'custom-context', name: 'Custom docs', command: 'npx', args: ['-y', '@upstash/context7-mcp@3.0.0'], enabled: false },
    { id: 'custom-fetch', name: 'Custom fetch', command: 'python', args: ['-m', 'mcp_server_fetch'], enabled: false },
    { id: 'custom-git', name: 'Custom git', command: 'uvx', args: ['--from', 'mcp-server-git==2025.9.25', 'mcp-server-git'], enabled: false },
    { id: 'custom-markdown', name: 'Installed document converter', command: 'C:\\Fixture\\Scripts\\markitdown-mcp.exe', args: [], enabled: false },
    { id: 'custom-chrome', name: 'Installed debugging server', command: process.execPath,
      args: ['C:\\Fixture\\node_modules\\chrome-devtools-mcp\\build\\src\\index.js'], enabled: false },
    { id: 'custom-exa', name: 'Custom search', transport: 'streamable-http', command: '', args: [], url: 'https://mcp.exa.ai/mcp/',
      headerEnv: { 'x-api-key': 'KYNXA_SYNTHETIC_KEY_REFERENCE' }, enabled: false }
  ];
  let config = await f.service.updateConfig({ ...initial, expectedRevision: initial.revision, mcpServers: samples });
  const before = structuredClone(config);
  for (const [id, serverId] of [['context7', 'custom-context'], ['fetch', 'custom-fetch'], ['git', 'custom-git'],
    ['markitdown', 'custom-markdown'], ['chrome-devtools', 'custom-chrome'], ['exa', 'custom-exa']]) {
    assert.equal(mcpPresetCatalog(config).presets.find(item => item.id === id).configuredServerId, serverId);
    config = await addMcpPreset(f.service, id, { expectedRevision: config.revision });
  }
  assert.deepEqual(config, before);
});

test('preset ID collisions allocate a distinct ID without changing unrelated configured services', async t => {
  const f = await toolFixture(t), initial = await f.service.getConfig();
  const other = { id: 'context7', name: 'Unrelated configured service', command: process.execPath, args: ['unrelated.mjs'], enabled: false };
  const config = await f.service.updateConfig({ ...initial, expectedRevision: initial.revision, mcpServers: [other] });
  const added = await addMcpPreset(f.service, 'context7', { expectedRevision: config.revision });
  assert.equal(added.mcpServers[0].name, other.name); assert.equal(added.mcpServers[1].id, 'context7-2');
  assert.equal(mcpPresetCatalog(added).presets.find(item => item.id === 'context7').configuredServerId, 'context7-2');
});

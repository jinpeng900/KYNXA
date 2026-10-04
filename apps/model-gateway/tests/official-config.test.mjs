import assert from 'node:assert/strict';
import { readFile, writeFile, mkdir, mkdtemp, rm } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import test from 'node:test';
import { AgentConfigRepository } from '../agent-config.mjs';
import { curatedMcpPresets, OFFICIAL_TOOLS_ROOT, officialSkillIdentity, LEGACY_BUNDLED_SKILLS_DIRECTORY } from '../official-tools.mjs';
import { addMcpPreset } from '../mcp-presets.mjs';
import { toolFixture } from './tool-fixture.mjs';
import { mcpPresetCatalog } from '../mcp-presets.mjs';
import { canRunInParallel } from '../tool-scheduling.mjs';
import { canReuseObservation } from '../tool-observations.mjs';

const save = (config, changes = {}) => ({ ...config, expectedRevision: config.revision, ...changes });
const server = (config, presetId) => config.mcpServers.find(item => item.origin === 'official' && item.presetId === presetId);
const replace = (config, selected, changes) => config.mcpServers.map(item => item.id === selected.id ? { ...item, ...changes } : item);

test('fresh official defaults are visible but disabled and a no-op save persists no default copies', async t => {
  const f = await toolFixture(t, { officialTools: true });
  const initial = await f.service.getConfig();
  assert.equal(initial.mcpServers.length, 11);
  assert.ok(initial.mcpServers.every(item => item.origin === 'official' && !item.enabled && !item.overridden));
  assert.equal(initial.officialToolsRoot, OFFICIAL_TOOLS_ROOT);
  assert.equal(initial.userToolsRoot, f.conversations.root);
  assert.match(initial.officialPackageVersion, /^\d+\.\d+\.\d+$/);
  await assert.rejects(readFile(f.service.config.file), { code: 'ENOENT' });
  const generation = f.service.configGeneration;
  const saved = await f.service.updateConfig(save(initial));
  const disk = JSON.parse(await readFile(f.service.config.file, 'utf8'));
  assert.deepEqual(disk.mcpServers, []);
  assert.deepEqual(disk.officialMcpOverrides, []);
  assert.ok(!Object.hasOwn(disk, 'officialToolsRoot') && !Object.hasOwn(disk, 'userToolsRoot'));
  assert.equal(saved.mcpServers.length, 11);
  assert.equal(f.service.configGeneration, generation, 'an unchanged projection preserves request authority');
  assert.equal(f.service.mcp.connections.size, 0);
});

test('user choices persist as deltas; official reset removes the override without changing package bytes', async t => {
  const f = await toolFixture(t, { officialTools: true });
  const manifestBefore = await readFile(join(OFFICIAL_TOOLS_ROOT, 'manifest.json'));
  let config = await f.service.getConfig(), selected = server(config, 'context7');
  config = await f.service.updateConfig(save(config, { mcpServers: replace(config, selected,
    { enabled: true, name: 'My documentation', envRefs: { CONTEXT7_API_KEY: 'SYNTHETIC_DOCUMENTATION_TOKEN' } }) }));
  const disk = JSON.parse(await readFile(f.service.config.file, 'utf8'));
  assert.deepEqual(disk.mcpServers, []);
  assert.deepEqual(disk.officialMcpOverrides, [{ presetId: 'context7', id: selected.id,
    changes: { name: 'My documentation', envRefs: { CONTEXT7_API_KEY: 'SYNTHETIC_DOCUMENTATION_TOKEN' }, enabled: true } }]);
  assert.equal(server(config, 'context7').overridden, true);
  const restarted = new AgentConfigRepository(f.conversations.root, { officialPresets: curatedMcpPresets });
  const restored = await restarted.read();
  assert.equal(server(restored, 'context7').enabled, true);
  assert.equal(server(restored, 'context7').name, 'My documentation');
  const preset = curatedMcpPresets.find(item => item.id === 'context7');
  config = await f.service.updateConfig(save(config, { mcpServers: config.mcpServers.map(item => item.id === selected.id
    ? { ...preset.server, id: selected.id } : item) }));
  assert.equal(server(config, 'context7').overridden, false);
  assert.equal(server(config, 'context7').enabled, false);
  assert.deepEqual(JSON.parse(await readFile(f.service.config.file, 'utf8')).officialMcpOverrides, []);
  assert.deepEqual(await readFile(join(OFFICIAL_TOOLS_ROOT, 'manifest.json')), manifestBefore);
});

test('official package updates preserve explicit choices and update untouched defaults', async t => {
  const f = await toolFixture(t, { officialTools: true });
  let config = await f.service.getConfig();
  config = await f.service.updateConfig(save(config, { mcpServers: replace(config, server(config, 'playwright'), { enabled: true }) }));
  const upgraded = structuredClone(curatedMcpPresets);
  upgraded.find(item => item.id === 'playwright').server.args = ['-y', '@playwright/mcp@99.0.0', '--headless', '--isolated', '--synthetic-new-default'];
  const updatedRepository = new AgentConfigRepository(f.conversations.root, { officialPresets: upgraded });
  const updated = await updatedRepository.read();
  assert.equal(server(updated, 'playwright').enabled, true);
  assert.ok(server(updated, 'playwright').args.includes('@playwright/mcp@99.0.0'));
  assert.ok(server(updated, 'playwright').args.includes('--synthetic-new-default'));
  const originalArgs = ['-y', '@playwright/mcp@0.0.83', '--headless', '--custom-profile'];
  config = await f.service.updateConfig(save(config, { mcpServers: replace(config, server(config, 'playwright'), { args: originalArgs }) }));
  assert.deepEqual(server(await updatedRepository.read(), 'playwright').args, originalArgs, 'explicit custom arguments retain their chosen version');
});

test('legacy user MCP keeps its ID/version/credentials and shadows its matching official default', async t => {
  const f = await toolFixture(t, { officialTools: true });
  const rawRepository = new AgentConfigRepository(f.conversations.root);
  await rawRepository.update({ version: 1, expectedRevision: 0, skillDirectories: [], mcpServers: [
    { id: 'my-docs', name: 'Existing docs', command: 'npx', args: ['-y', '@upstash/context7-mcp@3.0.0'],
      enabled: false, envRefs: { CONTEXT7_API_KEY: 'SYNTHETIC_OLD_TOKEN' } }
  ] });
  let config = await f.service.getConfig();
  assert.equal(config.mcpServers.length, 11);
  assert.equal(config.mcpServers[0].id, 'my-docs');
  assert.equal(config.mcpServers[0].origin, 'user');
  assert.equal(config.mcpServers[0].presetId, 'context7');
  assert.equal(server(config, 'context7'), undefined);
  const before = JSON.parse(await readFile(rawRepository.file, 'utf8'));
  config = await f.service.updateConfig(save(config));
  assert.deepEqual(JSON.parse(await readFile(rawRepository.file, 'utf8')).mcpServers, before.mcpServers);
  config = await f.service.updateConfig(save(config, { mcpServers: config.mcpServers.filter(item => item.id !== 'my-docs') }));
  assert.equal(server(config, 'context7').enabled, false, 'removing the custom installation restores a disabled default');
});

test('hidden official entries remain hidden on unrelated saves/restart and explicit add restores a disabled default', async t => {
  const f = await toolFixture(t, { officialTools: true });
  let config = await f.service.getConfig();
  config = await f.service.updateConfig(save(config, { mcpServers: config.mcpServers.filter(item => item.presetId !== 'fetch') }));
  assert.ok(config.disabledOfficialMcpServers.includes('fetch'));
  assert.equal(server(config, 'fetch'), undefined);
  config = await f.service.updateConfig(save(config, { disabledSkills: ['a'.repeat(24)] }));
  assert.equal(server(await f.service.getConfig(), 'fetch'), undefined);
  const restored = await addMcpPreset(f.service, 'fetch', { expectedRevision: config.revision });
  assert.equal(server(restored, 'fetch').enabled, false);
  assert.equal(restored.disabledOfficialMcpServers.includes('fetch'), false);
  assert.equal(f.service.mcp.connections.size, 0);
});

test('existing custom IDs cannot be overwritten by official defaults or spoofed origin metadata', async t => {
  const f = await toolFixture(t, { officialTools: true });
  const raw = new AgentConfigRepository(f.conversations.root);
  await raw.update({ version: 1, expectedRevision: 0, skillDirectories: [], mcpServers: [
    { id: 'official-fetch', name: 'Unrelated custom process', command: process.execPath, args: ['unrelated.mjs'], enabled: false }
  ] });
  let config = await f.service.getConfig();
  assert.equal(config.mcpServers[0].origin, 'user');
  assert.equal(server(config, 'fetch').id, 'official-fetch-2');
  const selected = server(config, 'fetch');
  config = await f.service.updateConfig(save(config, { mcpServers: config.mcpServers.map(item =>
    item.id === selected.id ? { ...item, name: 'My fetch' } : { ...item, origin: 'official', presetId: 'fetch' }) }));
  assert.equal(config.mcpServers[0].name, 'Unrelated custom process');
  assert.equal(config.mcpServers[0].origin, 'user');
  assert.equal(server(config, 'fetch').name, 'My fetch');
  assert.equal(server(config, 'fetch').overridden, true);
});

test('official input validation/conflict rejects before changing user choices or executing processes', async t => {
  const f = await toolFixture(t, { officialTools: true });
  const initial = await f.service.getConfig();
  const saved = await f.service.updateConfig(save(initial));
  const before = await readFile(f.service.config.file);
  await assert.rejects(f.service.updateConfig(save(initial)), { code: 'AGENT_CONFIG_CONFLICT' });
  const selected = server(saved, 'github');
  await assert.rejects(f.service.updateConfig(save(saved, { mcpServers: replace(saved, selected,
    { url: 'https://secret:fixture@example.test/mcp' }) })), { code: 'INVALID_AGENT_CONFIG' });
  await assert.rejects(f.service.updateConfig(save(saved, { mcpServers: [...saved.mcpServers, { ...selected, id: 'duplicate' }] })),
    { code: 'DUPLICATE_MCP_ENDPOINT' });
  assert.deepEqual(await readFile(f.service.config.file), before);
  assert.equal(f.service.mcp.connections.size, 0);
});

test('official customized commands keep their catalog identity instead of creating a duplicate preset', async t => {
  const f = await toolFixture(t, { officialTools: true });
  let config = await f.service.getConfig();
  config = await f.service.updateConfig(save(config, { mcpServers: replace(config, server(config, 'context7'),
    { command: process.execPath, args: ['custom-docs.mjs'] }) }));
  const preset = mcpPresetCatalog(config).presets.find(item => item.id === 'context7');
  assert.equal(preset.alreadyConfigured, true);
  assert.equal(preset.configuredServerId, server(config, 'context7').id);
  assert.equal((await addMcpPreset(f.service, 'context7', { expectedRevision: config.revision })).revision, config.revision);
});

test('official stateless aliases retain parallel/cache scheduling including collision IDs, without including unknown/browser actions', () => {
  for (const name of ['mcp.official-exa.web_search_exa', 'mcp.official-brave-search.brave_web_search',
    'mcp.official-fetch.fetch', 'mcp.official-fetch-2.fetch', 'mcp.official-exa-12.web_search_exa']) {
    const call = { name, arguments: { arguments: { url: 'https://example.test/public' } } };
    assert.equal(canRunInParallel(call), true, name);
    assert.equal(canReuseObservation(call), true, name);
  }
  for (const name of ['mcp.official-fetch-1.fetch', 'mcp.official-fetch.fetch_extra', 'mcp.official-playwright.browser_navigate',
    'mcp.official-exa.fetch', 'mcp.custom.fetch']) {
    const call = { name, arguments: { arguments: { url: 'https://example.test/public' } } };
    assert.equal(canRunInParallel(call), false, name);
    assert.equal(canReuseObservation(call), false, name);
  }
  assert.equal(canRunInParallel({ name: 'mcp.official-fetch.fetch', arguments: { arguments: { url: 'file:///private' } } }), false);
});

test('official skill disablement migrates the old local ID and can be enabled again through the settings config', async t => {
  const f = await toolFixture(t, { officialTools: true });
  const { id, legacyIds } = officialSkillIdentity('safe-file-edit/SKILL.md');
  assert.ok(LEGACY_BUNDLED_SKILLS_DIRECTORY && legacyIds.length);
  const raw = new AgentConfigRepository(f.conversations.root);
  await raw.update({ version: 1, expectedRevision: 0, skillDirectories: [], mcpServers: [], disabledSkills: [legacyIds[0]] });
  let config = await f.service.getConfig();
  assert.deepEqual(config.disabledSkills, [id]);
  assert.equal((await f.service.listSkills(undefined, { includeDisabled: true })).find(skill => skill.id === id).enabled, false);
  config = await f.service.updateConfig(save(config, { disabledSkills: [] }));
  assert.equal((await f.service.listSkills()).find(skill => skill.id === id).enabled, true);
  assert.deepEqual(JSON.parse(await readFile(raw.file, 'utf8')).disabledSkills, []);
});

test('moving user tool storage preserves official IDs/choices and a future package override stays dormant and intact', async t => {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-official-relocation-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const first = new AgentConfigRepository(join(root, 'first'), { officialPresets: curatedMcpPresets });
  let config = await first.read();
  config = await first.update(save(config, { mcpServers: replace(config, server(config, 'exa'), { name: 'My search' }) }));
  const raw = JSON.parse(await readFile(first.file, 'utf8'));
  raw.officialMcpOverrides.push({ presetId: 'future-service', id: 'official-future-service', changes: { enabled: false } });
  const second = new AgentConfigRepository(join(root, 'second'), { officialPresets: curatedMcpPresets });
  await mkdir(second.folder, { recursive: true }); await writeFile(second.file, JSON.stringify(raw));
  const moved = await second.read();
  assert.equal(server(moved, 'exa').id, server(config, 'exa').id);
  assert.equal(server(moved, 'exa').name, 'My search');
  assert.ok(!moved.mcpServers.some(item => item.presetId === 'future-service'));
  await second.update(save(moved));
  assert.deepEqual(JSON.parse(await readFile(second.file, 'utf8')).officialMcpOverrides.find(item => item.presetId === 'future-service'),
    raw.officialMcpOverrides.at(-1));
});

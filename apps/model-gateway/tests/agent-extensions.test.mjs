import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdir, writeFile, readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { createModelServer } from '../server.mjs';
import { mcpPresetCatalog, addMcpPreset } from '../mcp-presets.mjs';
import { AppSkillService } from '../skill-service.mjs';
import { OFFICIAL_SKILLS_DIRECTORY } from '../official-tools.mjs';
import { toolFixture, parsed, pendingApproval, approve } from './tool-fixture.mjs';

const verified = { available: true, commands: ['node'], sandbox: 'appcontainer', failClosed: true, checksChildToken: true, network: false,
  skillExecution: { manifestVersion: 1, readOnlyPackage: true, hashChecked: true } };
async function saveSkill(f, name = 'fixture') {
  const root = join(f.workspace, '.kynxa', 'skills', name);
  await mkdir(join(root, 'scripts'), { recursive: true });
  await writeFile(join(root, 'SKILL.md'), `---\nname: ${name}\ndescription: Fixture skill\n---\nRead scripts/check.mjs as needed.\n`);
  await writeFile(join(root, 'scripts', 'check.mjs'), 'console.log("fixture");');
  return (await f.service.listSkills(await f.context())).find(skill => skill.name === name);
}

test('curated MCP presets are enabled, duplicate-safe, portable and revision checked without connecting', async t => {
  const f = await toolFixture(t);
  let config = await f.service.getConfig();
  const catalog = mcpPresetCatalog(config);
  assert.equal(catalog.presets.length, 11); assert.deepEqual(catalog.presets.slice(0, 2).map(item => item.id), ['playwright', 'github']);
  assert.ok(catalog.presets.every(item => item.server.enabled === true && !item.alreadyConfigured));
  assert.ok(catalog.presets[0].server.args.includes('@playwright/mcp@0.0.83'));
  config = await addMcpPreset(f.service, 'playwright', { expectedRevision: config.revision });
  assert.equal(config.mcpServers.length, 1);
  const again = await addMcpPreset(f.service, 'playwright', { expectedRevision: config.revision });
  assert.equal(again.revision, config.revision);
  await assert.rejects(addMcpPreset(f.service, 'github', { expectedRevision: 0 }), { code: 'AGENT_CONFIG_CONFLICT' });
  config = await f.service.updateConfig({ ...config, expectedRevision: config.revision,
    mcpServers: [{ ...config.mcpServers[0], id: 'custom-browser', args: ['@playwright/mcp@0.0.82', '--headless'] }] });
  assert.equal(mcpPresetCatalog(config).presets[0].configuredServerId, 'custom-browser');
  assert.equal((await addMcpPreset(f.service, 'playwright', { expectedRevision: config.revision })).mcpServers.length, 1);
  assert.equal(f.service.mcp.connections.size, 0, 'preset adds never start a process or network connection');
});

test('skill tools resolve package resources, check dependencies and require verified sandbox approval', async t => {
  let executed = 0, request;
  const sandboxRunner = { capabilities: async () => verified, run: async () => { throw Error('use runSkill'); },
    runSkill: async input => { executed++; request = input; return { sandbox: 'appcontainer', exitCode: 0, stdout: 'fixture', stderr: '',
      skillExecution: { ...verified.skillExecution, script: input.package.scriptRelativePath, fileCount: input.package.files.length,
        scriptSha256: input.package.files.find(file => file.relativePath === input.package.scriptRelativePath).sha256 } }; } };
  const f = await toolFixture(t, { sandboxRunner }), skill = await saveSkill(f), ctx = await f.context('ask');
  const resources = parsed(await f.run(ctx, 'skill.resource.read', { id: skill.id, path: 'scripts/check.mjs' }));
  assert.equal(resources.content, 'console.log("fixture");');
  assert.equal((await f.run(ctx, 'skill.resource.read', { id: skill.id, path: '../outside.txt' })).isError, true);
  assert.equal(parsed(await f.run(ctx, 'skill.check', { id: skill.id })).canRun, true);
  assert.equal((await f.run(ctx, 'skill.run', { id: skill.id, path: 'scripts/check.mjs', args: [] }, { interactive: false })).code, 'TOOL_APPROVAL_REQUIRED');
  const pending = await pendingApproval(f.service, ctx, f.call('skill.run', { id: skill.id, path: 'scripts/check.mjs', args: ['hello'] }));
  assert.equal(executed, 0); approve(f.service, ctx, pending.event.tool); parsed(await pending.result);
  assert.equal(executed, 1); assert.equal(request.package.scriptRelativePath, 'scripts/check.mjs');
  assert.ok(request.package.files.every(file => /^[0-9a-f]{64}$/.test(file.sha256)));
  await writeFile(join(f.workspace, '.kynxa', 'skills', 'fixture', 'package.json'), '{"dependencies":{"fake":"1.0.0"}}');
  assert.equal((await f.run(await f.context('full'), 'skill.run', { id: skill.id, path: 'scripts/check.mjs', args: [] })).code, 'APP_SKILL_ENVIRONMENT_UNAVAILABLE');
  assert.equal(executed, 1);
});

test('new builtin skills are standard packages and resource reuse needs no host commands', async t => {
  const f = await toolFixture(t);
  const bundledDirectory = OFFICIAL_SKILLS_DIRECTORY;
  const service = new AppSkillService(f.conversations.root, { bundledDirectory });
  const config = await f.service.getConfig(), list = await service.list(undefined, config);
  assert.ok(list.every(skill => skill.standardCompliant));
  const comms = list.find(skill => skill.name === 'internal-comms'); assert.ok(comms);
  const reference = await service.readResource(comms.id, 'examples/3p-updates.md', undefined, config);
  assert.ok(reference.content.length > 100);
  const environment = await service.checkEnvironment(comms.id, undefined, config);
  assert.equal(environment.scripts.length, 0);
});

test('old terminal helpers cannot silently accept and ignore skill execution', async t => {
  let executed = 0;
  const f = await toolFixture(t, { sandboxRunner: { capabilities: async () => ({ ...verified, skillExecution: undefined }),
    run: async () => { executed++; }, runSkill: async () => { executed++; } } });
  const skill = await saveSkill(f);
  const result = await f.run(await f.context('full'), 'skill.run', { id: skill.id, path: 'scripts/check.mjs', args: [] });
  assert.equal(result.code, 'SANDBOX_SKILL_UNSUPPORTED'); assert.equal(executed, 0);
});

test('late skill cancellation preserves a verified completion receipt without losing the tool record', async t => {
  const controller = new AbortController();
  const f = await toolFixture(t, { sandboxRunner: { capabilities: async () => verified, run: async () => {},
    runSkill: async ({ package: prepared }) => {
      controller.abort();
      throw Object.assign(new Error('late stop'), { name: 'AbortError', sandboxResult: {
        protocolVersion: 1, sandbox: 'appcontainer', tokenVerified: true, workspaceCopy: true, activeProcessesAfterExit: 0,
        cancelled: false, timedOut: false, exitCode: 0, stdout: 'already finished', stderr: '',
        skillExecution: { ...verified.skillExecution, script: prepared.scriptRelativePath, fileCount: prepared.files.length,
          scriptSha256: prepared.files.find(file => file.relativePath === prepared.scriptRelativePath).sha256 }
      } });
    } } });
  const skill = await saveSkill(f);
  const result = await f.run(await f.context('full'), 'skill.run', { id: skill.id, path: 'scripts/check.mjs', args: [] }, { signal: controller.signal });
  assert.equal(result.isError, false); assert.equal(result.status, 'completed'); assert.ok(result.resultRef);
  assert.match(result.content, /already finished/);
});

test('agent settings routes import packages, preserve disable switches and never expose private config via model files', async t => {
  const f = await toolFixture(t), skill = await saveSkill(f);
  const modelRuntime = { tools: f.service };
  const server = createModelServer({ modelStore: { dataHome: f.dataHome }, modelRuntime });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(() => new Promise(resolve => server.close(resolve)));
  const base = `http://127.0.0.1:${server.address().port}`;
  const catalog = await fetch(base + '/api/agent/mcp/catalog').then(response => response.json());
  assert.equal(catalog.presets.length, 11);
  const added = await fetch(base + '/api/agent/mcp/catalog/github/add', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: '{"expectedRevision":0}' }).then(response => response.json());
  assert.equal(added.mcpServers[0].enabled, true);
  const importBody = JSON.stringify({ directory: join(f.workspace, '.kynxa', 'skills', 'fixture') });
  const imported = await fetch(base + '/api/agent/skills/import', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: importBody }).then(response => response.json());
  assert.equal(imported.imported, true);
  const reused = await fetch(base + '/api/agent/skills/import', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: importBody }).then(response => response.json());
  assert.equal(reused.reused, true); assert.equal(reused.skill.id, imported.skill.id);
  const config = await f.service.updateConfig({ ...added, expectedRevision: added.revision, disabledSkills: [skill.id] });
  const all = await fetch(base + '/api/agent/skills?conversationId=' + f.conversationId).then(response => response.json());
  assert.equal(all.skills.find(item => item.id === skill.id).enabled, false);
  assert.equal((await f.service.listSkills(await f.context())).some(item => item.id === skill.id), false);
  const file = join(f.conversations.root, 'Agent', 'config.json');
  assert.equal((await f.run(await f.context('full'), 'filesystem.read', { path: file, reason: 'requested' })).code, 'PROTECTED_MODEL_CREDENTIALS');
  const search = parsed(await f.run(await f.context('full'), 'filesystem.search', { path: join(f.conversations.root, 'Agent'), query: 'mcpServers', reason: 'requested' }));
  assert.equal(JSON.stringify(search).includes('config.json'), false);
  assert.equal((await readFile(file, 'utf8')).includes('mcpServers'), true);
  assert.equal(config.disabledSkills.length, 1);
});

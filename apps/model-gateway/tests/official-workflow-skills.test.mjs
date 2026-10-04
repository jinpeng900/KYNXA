import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, relative, resolve, sep } from 'node:path';
import { test } from 'node:test';
import { AppSkillService } from '../skill-service.mjs';
import { OFFICIAL_SKILLS_DIRECTORY, officialSkillIdentity } from '../official-tools.mjs';
import { ModelStore } from '../store.mjs';
import { ModelRuntime } from '../runtime.mjs';
import { estimateTokens } from '../context.mjs';
import { estimateToolMessageTokens, toolDeclarations } from '../tool-protocols.mjs';
import { toolFixture, parsed } from './tool-fixture.mjs';

const workflowNames = ['workspace-inspect', 'safe-file-edit', 'browser-workflow',
  'desktop-workflow', 'terminal-workflow', 'web-research'];

for (const protocol of ['openai-completions', 'openai-responses', 'anthropic-messages']) {
  test(protocol + ': installed skill headers preserve desktop tool discovery on an unchanged 8K connection', async t => {
    const desktopRunner = { capabilities: async () => ({ protocolVersion: 1, boundary: 'host-desktop', available: true,
      interactiveWindows: true, operations: ['windows', 'apps', 'launch', 'read'] }) };
    const f = await toolFixture(t, { officialTools: true, desktopRunner });
    const models = new ModelStore({ dataHome: f.dataHome });
    await models.save({ providerId: 'workflow-budget-fixture', displayName: 'Workflow budget fixture', protocol,
      baseUrl: 'http://127.0.0.1:9/v1', apiKey: 'FAKE_LOCAL_WORKFLOW_ONLY', models: ['fixture-model'], contextWindowTokens: 8192 });
    const runtime = new ModelRuntime({ modelStore: models, dataHome: f.dataHome,
      conversationStore: f.conversations, toolService: f.service });
    t.after(() => runtime.close());
    const input = { conversationId: f.conversationId, requestId: randomUUID(), userMessageId: randomUUID(),
      provider: 'workflow-budget-fixture', model: 'fixture-model', permissionMode: 'full', message: '打开记事本并读取界面' };
    const prepared = await runtime.prepare(input, input.conversationId);
    try {
      assert.equal(prepared.contextMetrics.contextWindowTokens, 8192);
      assert.equal((await models.connectionFor(input.provider)).contextWindowTokens, 8192);
      assert.ok([...prepared.requestOptions.system.matchAll(/Application skill [a-f0-9]{24}:/g)].length <= 7,
        'Small-window headers are bounded and can all be discovered lazily');
      const listLoaded = await f.run(prepared.toolContext, 'tool.load', { names: ['skill.list'] }, { interactive: false });
      assert.equal(listLoaded.isError, false, listLoaded.code);
      const listed = parsed(await f.run(prepared.toolContext, 'skill.list', { offset: 0, limit: 128 }, { interactive: false }));
      assert.deepEqual(listed.skills.filter(skill => skill.origin === 'builtin').map(skill => skill.name).sort(),
        [...workflowNames, 'internal-comms'].sort());
      assert.equal(listed.hasMore, false);
      const readLoaded = await f.run(prepared.toolContext, 'tool.load', { names: ['skill.read'] }, { interactive: false });
      assert.equal(readLoaded.isError, false, readLoaded.code);
      const desktopSkill = parsed(await f.run(prepared.toolContext, 'skill.read',
        { id: officialSkillIdentity('desktop-workflow/SKILL.md').id }, { interactive: false }));
      assert.equal(desktopSkill.name, 'desktop-workflow');
      assert.equal(desktopSkill.content, await readFile(join(OFFICIAL_SKILLS_DIRECTORY, 'desktop-workflow', 'SKILL.md'), 'utf8'));
      const loaded = await f.run(prepared.toolContext, 'tool.load', { names: ['computer.read'] }, { interactive: false });
      assert.equal(loaded.isError, false, JSON.stringify({ code: loaded.code,
        schemaBudget: f.service.catalogs.get(prepared.toolContext).model.tokenBudget,
        inputBudgetTokens: prepared.inputBudgetTokens, systemTokens: estimateTokens(prepared.requestOptions.system) }));
      assert.deepEqual(parsed(loaded).loaded, ['computer.read']);
      const catalog = f.service.modelCatalog(prepared.toolContext);
      assert.ok(catalog.some(tool => tool.name === 'computer.read'));
      const schemaTokens = estimateTokens(JSON.stringify(toolDeclarations(protocol, catalog)));
      assert.ok(schemaTokens <= Math.floor(prepared.inputBudgetTokens * .40));
      assert.ok(schemaTokens + estimateToolMessageTokens(prepared.messages, prepared.requestOptions.system) <= prepared.inputBudgetTokens,
        'Real installed skill headers and desktop schema fit within the unchanged context limit');
    } finally { await f.service.releaseContext(prepared.toolContext); }
  });
}

test('installed workflow skills are standard, lazy-readable packages without executable dependencies', async t => {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-workflow-skills-'));
  t.after(async () => {
    const suffix = relative(resolve(tmpdir()), resolve(root));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith('..' + sep));
    await rm(root, { recursive: true, force: true });
  });
  const service = new AppSkillService(root), config = { skillDirectories: [], disabledSkills: [] };
  const available = await service.list(null, config);
  for (const name of workflowNames) {
    const skill = available.find(item => item.name === name);
    assert.ok(skill, name);
    assert.equal(skill.status, undefined, name);
    assert.equal(skill.standardCompliant, true, JSON.stringify(skill.diagnostics));
    assert.equal(skill.origin, 'builtin');
    assert.equal(skill.id, officialSkillIdentity(name + '/SKILL.md').id);
    assert.equal(Object.hasOwn(skill, 'content'), false, 'Discovery remains metadata-only');
    const loaded = await service.read(skill.id, null, config);
    assert.equal(loaded.content, await readFile(join(OFFICIAL_SKILLS_DIRECTORY, name, 'SKILL.md'), 'utf8'));
    const inventory = await service.inspect(skill.id, null, config);
    assert.equal(inventory.packageValid, true, name);
    assert.deepEqual(inventory.files.map(file => file.path), ['SKILL.md']);
    const resource = await service.readResource(skill.id, 'SKILL.md', null, config, { offset: 0, limit: 200 });
    assert.equal(resource.content, loaded.content.slice(0, 200));
    assert.equal(resource.hasMore, true);
    const environment = await service.checkEnvironment(skill.id, null, config, { sandboxCapabilities: { available: false } });
    assert.deepEqual(environment.scripts, []);
    assert.deepEqual(environment.dependencies, []);
    assert.equal(environment.canRun, false, 'Guidance packages are not executable scripts');
    assert.equal(environment.diagnostics.some(item => item.severity === 'error'), false,
      'Descriptive external capabilities remain reviewable rather than declaring unsupported script requirements');
    assert.equal(dirname(skill.source), join(OFFICIAL_SKILLS_DIRECTORY, name));
  }
});

test('official workflows use existing conflict-aware CRUD in an unlinked chat without a duplicate filesystem', async t => {
  const f = await toolFixture(t, { officialTools: true });
  const context = await f.context('full', f.standaloneId);
  assert.equal(context.isolatedWorkspace, true);
  assert.equal(context.linkedWorkspaceRoot, null);
  const readSkill = await f.run(context, 'skill.read', { id: officialSkillIdentity('safe-file-edit/SKILL.md').id });
  assert.equal(readSkill.status, 'completed');
  assert.ok(readSkill.resultRef);
  const calls = [];
  const invoke = async (name, args) => {
    const result = await f.run(context, name, args, { interactive: false });
    calls.push(result);
    if (!result.isError) assert.ok(result.resultRef, 'Completed outcomes retain their result references');
    return result;
  };
  assert.equal(parsed(await invoke('filesystem.mkdir', { path: 'generated' })).created, true);
  assert.equal(parsed(await invoke('filesystem.mkdir', { path: 'generated' })).created, false);
  const created = parsed(await invoke('filesystem.write', { path: 'generated/note.md',
    content: '# Generated\nOriginal user detail.\n', expectedHash: null }));
  const original = parsed(await invoke('filesystem.read', { path: 'generated/note.md' }));
  assert.equal(original.sha256, created.sha256);
  const changed = parsed(await invoke('filesystem.edit', { path: 'generated/note.md', oldText: 'Original',
    newText: 'Verified', expectedHash: original.sha256 }));
  const conflict = await invoke('filesystem.write', { path: 'generated/note.md', content: 'stale overwrite',
    expectedHash: original.sha256 });
  assert.equal(conflict.isError, true);
  assert.equal(conflict.code, 'TOOL_FILE_CONFLICT');
  const retained = parsed(await invoke('filesystem.read', { path: 'generated/note.md' }));
  assert.equal(retained.content, '# Generated\nVerified user detail.\n');
  assert.equal(retained.sha256, changed.sha256);
  const info = parsed(await invoke('filesystem.stat', { path: 'generated/note.md' }));
  assert.equal(info.sha256, retained.sha256);
  assert.equal(parsed(await invoke('filesystem.delete', { path: 'generated/note.md', expectedHash: info.sha256 })).deleted, true);
  assert.equal(parsed(await invoke('filesystem.delete', { path: 'generated', expectedHash: null })).deleted, true);
  assert.deepEqual(parsed(await invoke('filesystem.list', {})).entries.filter(entry => entry.name === 'generated'), []);
  assert.equal(calls.filter(result => result.isError).length, 1, 'Only the deliberate stale-hash overwrite fails');
});

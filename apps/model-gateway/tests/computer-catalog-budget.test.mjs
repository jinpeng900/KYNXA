import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from 'node:test';
import { ModelStore } from '../models/store.mjs';
import { ModelRuntime } from '../orchestration/runtime.mjs';
import { ModelToolCatalog } from '../tools/tool-catalog.mjs';
import { builtinDescriptors } from '../official-tools/Tools/catalog.mjs';
import { estimateTokens } from '../models/context.mjs';
import { estimateToolMessageTokens, toolDeclarations, wireCatalog } from '../models/tool-protocols.mjs';
import { parsed, toolFixture } from './tool-fixture.mjs';

const protocols = ['openai-completions', 'openai-responses', 'anthropic-messages'];
const discovery = ['tool.load', 'tool.result.read', 'tool.search'];
const desktopActions = ['windows', 'apps', 'launch', 'read'];
const target = { windowId: '12345', processId: 54321, reason: 'Only this synthetic desktop fixture.' };

for (const protocol of protocols) test(`${protocol}: an unchanged 8K connection can load and call desktop actions one at a time`, async t => {
  const actions = [];
  const desktopRunner = {
    capabilities: async () => ({ protocolVersion: 1, boundary: 'host-desktop', available: true,
      interactiveWindows: true, operations: desktopActions }),
    // No real application is opened or controlled. The actual runtime/broker/schema and archive paths are exercised.
    // 验证真实运行时、权限代理、schema 与归档链路；不打开或控制真实软件。
    run: async action => { actions.push(action); return { value: { completed: true, action, boundary: 'host-desktop' }, isError: false }; }
  };
  const f = await toolFixture(t, { desktopRunner });
  const fixtureApplication = join(f.workspace, 'fixture.exe');
  await writeFile(fixtureApplication, 'Synthetic executable metadata; never launched.');
  const models = new ModelStore({ dataHome: f.dataHome });
  await models.save({ providerId: 'small-window-fixture', displayName: 'Small window fixture', protocol,
    baseUrl: 'http://127.0.0.1:9/v1', apiKey: 'FAKE_LOCAL_TEST_ONLY', models: ['synthetic-desktop'], contextWindowTokens: 8192 });
  const runtime = new ModelRuntime({ modelStore: models, dataHome: f.dataHome, conversationStore: f.conversations, toolService: f.service });
  t.after(() => runtime.close());
  const input = { conversationId: f.conversationId, requestId: randomUUID(), userMessageId: randomUUID(),
    provider: 'small-window-fixture', model: 'synthetic-desktop', permissionMode: 'full', message: '打开记事本并读取界面' };
  const prepared = await runtime.prepare(input, input.conversationId);
  assert.equal(prepared.contextMetrics.contextWindowTokens, 8192);
  assert.equal((await models.connectionFor(input.provider)).contextWindowTokens, 8192);
  assert.equal((await models.connectionFor(input.provider)).maxOutputTokens, 262144);
  assert.ok(prepared.catalog.some(tool => tool.name.startsWith('computer.')), 'explicit desktop intent receives an actual desktop schema');
  const verifyCatalog = () => {
    const catalog = f.service.modelCatalog(prepared.toolContext), names = catalog.map(tool => tool.name);
    for (const name of discovery) assert.ok(names.includes(name), name);
    const declarations = toolDeclarations(protocol, catalog), schemaTokens = estimateTokens(JSON.stringify(declarations));
    assert.ok(schemaTokens <= Math.floor(prepared.inputBudgetTokens * .40));
    assert.ok(schemaTokens + estimateToolMessageTokens(prepared.messages, prepared.requestOptions.system) <= prepared.inputBudgetTokens,
      'the real prepared system/current message/output reservation still bounds every changed schema');
    return catalog;
  };
  verifyCatalog();
  try {
    for (const action of desktopActions) {
      const name = 'computer.' + action;
      const loaded = await f.run(prepared.toolContext, 'tool.load', { names: [name] }, { interactive: false });
      assert.equal(loaded.isError, false, `${name}: ${loaded.code}; schema budget=${f.service.catalogs.get(prepared.toolContext).model.tokenBudget}`);
      assert.deepEqual(parsed(loaded).loaded, [name]);
      assert.ok(verifyCatalog().some(tool => tool.name === name));
      const args = action === 'windows' || action === 'apps' ? { reason: target.reason }
        : action === 'launch' ? { appPath: fixtureApplication, reason: target.reason } : target;
      const result = await f.run(prepared.toolContext, name, args, { interactive: false });
      assert.equal(result.status, 'completed', result.content); assert.ok(result.resultRef);
    }
    assert.deepEqual(actions, desktopActions);
    const before = structuredClone(verifyCatalog());
    const tooMany = await f.run(prepared.toolContext, 'tool.load',
      { names: builtinDescriptors.filter(tool => tool.name.startsWith('computer.')).map(tool => tool.name) }, { interactive: false });
    assert.equal(tooMany.code, 'TOOL_CATALOG_BUDGET');
    assert.deepEqual(f.service.modelCatalog(prepared.toolContext), before, 'failed loads keep the previous usable selection');
  } finally { await f.service.releaseContext(prepared.toolContext); }
});

test('large catalogs preserve ordinary code/web selection and desktop discovery without raising the hard schema cap', () => {
  for (const protocol of protocols) {
    for (const message of ['写一个Node.js测试', '查证今天的官方新闻']) {
      const catalog = new ModelToolCatalog(builtinDescriptors, { protocol, tokenBudget: 16000, message });
      assert.equal(catalog.selected.some(tool => tool.name.startsWith('computer.')), false);
      assert.ok(catalog.selected.some(tool => tool.name === 'terminal.run'));
      catalog.load(['computer.read']);
      assert.ok(catalog.selected.some(tool => tool.name === 'computer.read'));
      for (const name of discovery) assert.ok(catalog.selected.some(tool => tool.name === name));
      assert.ok(estimateTokens(JSON.stringify(toolDeclarations(protocol, catalog.wire()))) <= 16000);
      const before = structuredClone(catalog.selected);
      assert.throws(() => catalog.load(['missing.tool']), { code: 'TOOL_NOT_FOUND' });
      assert.deepEqual(catalog.selected, before);
    }
    const desktop = new ModelToolCatalog(builtinDescriptors, { protocol, tokenBudget: 16000, message: '打开记事本并截图' });
    assert.ok(desktop.selected.length <= builtinDescriptors.filter(tool => !tool.name.startsWith('terminal.host.')).length);
    assert.ok(desktop.selected.some(tool => tool.name === 'computer.launch'));
    for (const name of discovery) assert.ok(desktop.selected.some(tool => tool.name === name));
    assert.ok(estimateTokens(JSON.stringify(toolDeclarations(protocol, desktop.wire()))) <= 16000);
  }
});

for (const protocol of protocols) test(`${protocol}: device inspection reserves the host shell within the unchanged schema budget`, () => {
  const required = builtinDescriptors.filter(tool => [...discovery, 'terminal.host.run'].includes(tool.name));
  const tokenBudget = estimateTokens(JSON.stringify(toolDeclarations(protocol, wireCatalog(required))));
  const catalog = new ModelToolCatalog(builtinDescriptors, { protocol, tokenBudget, message: '查看我的IP和DNS配置' });
  for (const name of [...discovery, 'terminal.host.run'])
    assert.ok(catalog.selected.some(tool => tool.name === name), name);
  assert.ok(!catalog.selected.some(tool => tool.name.startsWith('computer.')));
  assert.ok(estimateTokens(JSON.stringify(toolDeclarations(protocol, catalog.wire()))) <= tokenBudget);
  const disabled = builtinDescriptors.map(tool => tool.name === 'terminal.host.run' ? { ...tool, enabled: false } : tool);
  const unavailable = new ModelToolCatalog(disabled, { protocol, tokenBudget, message: '查看我的IP和DNS配置' });
  assert.ok(!unavailable.selected.some(tool => tool.name === 'terminal.host.run'));
  assert.throws(() => unavailable.load(['terminal.host.run']), { code: 'TOOL_NOT_FOUND' });
  for (const message of ['查询8.8.8.8的归属', '解释DNS工作原理']) {
    const publicTask = new ModelToolCatalog(builtinDescriptors, { protocol, tokenBudget: 16000, message });
    assert.ok(!publicTask.selected.some(tool => tool.name.startsWith('terminal.host.')), message);
  }
});

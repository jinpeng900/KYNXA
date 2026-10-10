import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { test } from 'node:test';
import { ModelStore } from '../models/store.mjs';
import { ModelRuntime } from '../orchestration/runtime.mjs';
import { estimateTokens } from '../models/context.mjs';
import { estimateToolMessageTokens, toolDeclarations } from '../models/tool-protocols.mjs';
import { parsed, toolFixture } from './tool-fixture.mjs';

const protocols = ['openai-completions', 'openai-responses', 'anthropic-messages'];
const actions = [
  { name: 'knowledge.plan', arguments: {} },
  { name: 'memory.read', arguments: { query: '', offset: 0 } },
  { name: 'memory.propose', arguments: { action: 'noop', reason: 'No memory change requested by this fixture.', isInference: false } }
];

for (const protocol of protocols) test(`${protocol}: all optional semantic actions load within the unchanged real 8K budget`, async t => {
  const f = await toolFixture(t), models = new ModelStore({ dataHome: f.dataHome });
  await models.save({ providerId: 'hybrid-budget', displayName: 'Synthetic small model', protocol,
    baseUrl: 'http://127.0.0.1:9/v1', apiKey: 'FAKE_LOCAL_TEST_ONLY', models: ['fixture'], contextWindowTokens: 8192 });
  const runtime = new ModelRuntime({ modelStore: models, dataHome: f.dataHome, conversationStore: f.conversations, toolService: f.service });
  t.after(() => runtime.close());
  const input = { conversationId: f.conversationId, requestId: randomUUID(), userMessageId: randomUUID(),
    provider: 'hybrid-budget', model: 'fixture', permissionMode: 'full', message: '解释当前工作的资料和约束。' };
  const prepared = await runtime.prepare(input, input.conversationId), context = prepared.toolContext;
  const schemaBudgetTokens = f.service.catalogs.get(context).model.tokenBudget;
  assert.equal(prepared.contextMetrics.contextWindowTokens, 8192);
  assert.ok(schemaBudgetTokens <= Math.floor(prepared.inputBudgetTokens * .40));
  const verifyBudget = () => {
    const catalog = f.service.modelCatalog(context), names = catalog.map(tool => tool.name);
    for (const name of ['tool.search', 'tool.load']) assert.ok(names.includes(name), name);
    const schemaTokens = estimateTokens(JSON.stringify(toolDeclarations(protocol, catalog)));
    assert.ok(schemaTokens <= schemaBudgetTokens);
    assert.ok(schemaTokens + estimateToolMessageTokens(prepared.messages, prepared.requestOptions.system) <= prepared.inputBudgetTokens);
    assert.equal(f.service.catalogs.get(context).model.tokenBudget, schemaBudgetTokens);
    return names;
  };
  try {
    for (const action of actions) {
      const found = parsed(await f.run(context, 'tool.search', { query: action.name }));
      assert.ok(found.tools.some(tool => tool.name === action.name));
      const loadedReceipt = await f.run(context, 'tool.load', { names: [action.name] });
      assert.equal(loadedReceipt.isError, false, `${action.name}: ${loadedReceipt.code}; schema budget=${schemaBudgetTokens}`);
      assert.deepEqual(parsed(loadedReceipt).loaded, [action.name]);
      assert.ok(verifyBudget().includes(action.name));
      assert.equal((await f.run(context, action.name, action.arguments)).isError, false, action.name);
      const resultPaging = parsed(await f.run(context, 'tool.search', { query: 'tool.result.read' }));
      assert.equal(resultPaging.tools[0].name, 'tool.result.read');
      if (!verifyBudget().includes('tool.result.read')) {
        assert.equal(resultPaging.tools[0].schemaState, 'deferred');
        assert.deepEqual(parsed(loadedReceipt).deferredDiscovery, ['tool.result.read']);
      }
      assert.deepEqual(parsed(await f.run(context, 'tool.load', { names: ['tool.result.read'] })).loaded, ['tool.result.read']);
      assert.ok(verifyBudget().includes('tool.result.read'));
      assert.equal((await f.run(context, 'tool.result.read', { id: loadedReceipt.resultRef.id, offset: 0, limit: 200 })).isError, false);
    }
    assert.equal(f.service.approvals.pending.size, 0);
    assert.deepEqual((await runtime.memory.contextFor(f.conversationId)).entries, []);
  } finally { await f.service.releaseContext(context); }
});

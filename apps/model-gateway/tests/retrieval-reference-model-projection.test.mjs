import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { randomUUID } from 'node:crypto';
import { writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from 'node:test';
import { ModelStore } from '../models/store.mjs';
import { ModelRuntime } from '../orchestration/runtime.mjs';
import { RetrievalCoordinator } from '../orchestration/retrieval/coordinator.mjs';
import { toolFixture, fixtureDeclaration } from './tool-fixture.mjs';
import { ToolProgressGuard } from '../tools/tool-observations.mjs';

test('a real model request reads a short reference at its matching section and reuses it after restart', { timeout: 15000 }, async t => {
  const fixture = await toolFixture(t), requests = [], models = new ModelStore({ dataHome: fixture.dataHome });
  let sourceRef;
  const upstream = createServer(async (request, response) => {
    try {
      let body = ''; for await (const part of request) body += part;
      const input = JSON.parse(body); requests.push(input);
      response.setHeader('Content-Type', 'application/json');
      if (requests.length === 1) {
        const evidence = input.messages.flatMap(message => typeof message.content === 'string' ? message.content.split('\n') : [])
          .flatMap(line => { try { return [JSON.parse(line)]; } catch { return []; } });
        sourceRef = evidence.find(item => item.excerpt?.includes('Mara Chen'))?.sourceRef;
        const declaration = fixtureDeclaration(input.tools, 'knowledge.read');
        response.end(JSON.stringify({ choices: [{ finish_reason: 'tool_calls', message: { role: 'assistant', content: '核对该章节中的条件。',
          tool_calls: [{ id: 'read-matching-section', type: 'function', function: { name: declaration.function.name,
            arguments: JSON.stringify({ sourceRef: sourceRef ?? 'missing-reference', mode: 'section', limit: 4000, gap: '确认 ORION 的实验窗口是否已批准' }) } }] } }] }));
      } else response.end(JSON.stringify({ choices: [{ finish_reason: 'stop', message: { role: 'assistant',
        content: 'ORION 的审查人为 Mara Chen；实验窗口尚未批准。来源：orion-guide.md。' } }] }));
    } catch (error) { response.writeHead(500); response.end(JSON.stringify({ error: error.message })); }
  });
  await new Promise(resolve => upstream.listen(0, '127.0.0.1', resolve));
  await models.save({ providerId: 'fixture', displayName: 'Fixture', baseUrl: `http://127.0.0.1:${upstream.address().port}/v1`,
    models: ['mock-model'], contextWindowTokens: 32768, maxOutputTokens: 8192 });
  const runtime = new ModelRuntime({ modelStore: models, dataHome: fixture.dataHome,
    conversationStore: fixture.conversations, toolService: fixture.service });
  // Synthetic service capabilities are explicit, independent of legacy UI settings.
  // 模拟服务明确提供能力，不依赖旧 UI 设置。
  runtime.localModels.observe = async () => ({ backend: 'ollama', runtimeContextTokens: 32768 });
  t.after(async () => {
    try { await runtime.close(); }
    finally { upstream.closeAllConnections(); await new Promise(resolve => upstream.close(resolve)); }
  });
  await runtime.retrieval.settings.patchGlobal({ expectedRevision: 0, patch: { local: { semantic: 'off' } } });
  const text = '# Unrelated introduction\n' + 'Irrelevant background information. '.repeat(100)
    + '\n\n# ORION review conditions\nThe ORION reviewer is Mara Chen. Its experimental window is not approved.\n'
    + '\n# Other project\nThe VEGA reviewer is Beatrice Hall.\n';
  // This case owns an imported source; avoid a second independently valid mounted copy of the same file.
  // 本例验证导入来源撤销；文件放在挂载目录外，避免另一独立有效工作文件来源。
  const path = join(fixture.root, 'orion-guide.md'); await writeFile(path, text, 'utf8');
  const imported = await runtime.retrieval.importSource({ path, scope: 'user' });
  await runtime.retrieval.activeJobs.get(imported.jobId)?.promise;
  const requestId = randomUUID();
  await runtime.reply({ conversationId: fixture.conversationId, requestId, message: '根据资料，ORION 的审查人和实验窗口批准条件是什么？',
    provider: 'fixture', model: 'mock-model', permissionMode: 'full' });
  assert.equal(requests.length, 2);
  assert.equal(sourceRef?.length, 29);
  assert.doesNotMatch(JSON.stringify(requests), /rag1:/u, 'Only the model view uses short handles; the original archive stays complete.');
  const envelope = JSON.parse(requests[1].messages.findLast(message => message.role === 'tool').content);
  // Current tool transcripts add status/environment metadata around the complete JSON output.
  // 当前工具历史在完整 JSON 输出外附带状态及执行环境信息，先解开契约外层再核对原文。
  const read = typeof envelope.output === 'string' ? JSON.parse(envelope.output) : envelope;
  assert.match(read.text, /Mara Chen/);
  assert.match(read.text, /not approved/);
  assert.ok(read.offset > 1000, 'The source read starts near the retrieved section, not the long introduction.');
  const assistant = (await fixture.conversations.readMessages(fixture.conversationId)).find(message => message.Id === requestId);
  assert.equal(assistant.Status, 'completed');
  const archive = await fixture.service.results.get({ conversationId: fixture.conversationId }, assistant.RetrievalResultRef.id);
  assert.ok(archive.structuredContent.items.every(item => item.sourceRef.startsWith('rag1:')));
  await runtime.retrieval.close();
  const restarted = new RetrievalCoordinator({ conversations: fixture.conversations, tools: fixture.service,
    memory: runtime.memory, embeddings: { close: async () => {}, status: () => ({ state: 'unavailable' }) }, reranker: null });
  fixture.service.retrieval = restarted;
  t.after(() => restarted.close());
  const context = await fixture.context('full');
  assert.match((await restarted.read(context, { sourceRef, mode: 'section', limit: 4000 })).text, /Mara Chen/);
  const otherChat = await fixture.context('full', fixture.standaloneId);
  await assert.rejects(restarted.read(otherChat, { sourceRef, mode: 'section' }), /引用|结果|证据|回执|reference|scope|receipt/i);
  await restarted.removeSource(imported.id, { expectedRevision: (await restarted.library.list()).revision });
  await assert.rejects(restarted.read(context, { sourceRef, mode: 'section' }), /不存在|撤销|更改|过期/);
});

test('new short archive handles do not disguise repeated unchanged evidence as progress', async t => {
  const fixture = await toolFixture(t);
  const retrieval = new RetrievalCoordinator({ conversations: fixture.conversations, tools: fixture.service,
    memory: { contextFor: async () => ({ entries: [] }) },
    embeddings: { close: async () => {}, status: () => ({ state: 'unavailable' }) }, reranker: null });
  fixture.service.retrieval = retrieval;
  await retrieval.settings.patchGlobal({ expectedRevision: 0, patch: { local: { semantic: 'off' } } });
  await retrieval.library.add([{ path: join(fixture.workspace, 'repeat.md'), title: 'ORION reviewer',
    text: 'ORION reviewer is Mara Chen.' }], { scope: 'user' });
  const context = await fixture.context('full'), guard = new ToolProgressGuard(), references = new Set(), fingerprints = new Set();
  let state;
  for (let round = 0; round < 4; round++) {
    const call = fixture.call('knowledge.search', { query: 'ORION reviewer', gap: 'Who reviewed ORION?' });
    const result = await fixture.service.execute(context, call);
    assert.equal(result.isError, false);
    fingerprints.add(result.observationHash);
    references.add(JSON.parse(result.content).items[0].sourceRef);
    state = guard.observeRound([{ call, result }]);
  }
  assert.equal(references.size, 4, 'Independent receipts retain independent short handles.');
  assert.equal(fingerprints.size, 1, 'Progress reflects retrieved observations, not archive IDs.');
  assert.equal(state.finalize, true);
});

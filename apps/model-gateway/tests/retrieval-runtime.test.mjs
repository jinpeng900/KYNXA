import assert from 'node:assert/strict';
import { test } from 'node:test';
import { createServer } from 'node:http';
import { randomUUID } from 'node:crypto';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { ModelRuntime } from '../orchestration/runtime.mjs';
import { ModelStore } from '../models/store.mjs';
import { MemoryService } from '../data/memory-service.mjs';
import { buildContext, estimateTokens } from '../models/context.mjs';
import { estimateToolMessageTokens } from '../models/tool-protocols.mjs';
import { RetrievalCoordinator } from '../orchestration/retrieval/coordinator.mjs';
import { readSourceTree } from '../tools/retrieval/source-reader.mjs';
import { normalizeWebSources, WebSearchTool } from '../tools/retrieval/web-search.mjs';
import { toolFixture } from './tool-fixture.mjs';

const unavailableEmbeddings = () => ({ status: () => ({ state: 'unavailable', profileId: 'builtin-multilingual', dimensions: 384 }),
  embedQuery: async () => { throw Object.assign(new Error('Fixture missing assets'), { code: 'EMBEDDING_ASSET_MISSING' }); }, close: async () => {} });

function coordinator(f) {
  const retrieval = new RetrievalCoordinator({ conversations: f.conversations, memory: new MemoryService({ conversationStore: f.conversations }),
    tools: f.service, embeddings: unavailableEmbeddings(), excludedRoots: [f.dataHome] });
  f.service.retrieval = retrieval; return retrieval;
}

async function waitJob(retrieval, id) {
  const active = retrieval.activeJobs.get(id);
  if (active) await active.promise;
  return retrieval.jobs.get(id);
}

test('greetings skip tool-host, MCP, retrieval and model loading without deleting formal messages', async t => {
  const f = await toolFixture(t), models = new ModelStore({ dataHome: f.dataHome });
  await models.save({ providerId: 'fixture', displayName: 'Fixture', baseUrl: 'http://127.0.0.1:9/v1', models: ['mock-model'] });
  const runtime = new ModelRuntime({ modelStore: models, dataHome: f.dataHome, conversationStore: f.conversations, toolService: f.service });
  t.after(() => runtime.close());
  f.service.createContext = async () => { throw new Error('Greeting must not prepare tools'); };
  runtime.retrieval.evidence = async () => { throw new Error('Greeting must not retrieve'); };
  runtime.localModels.observe = async () => { throw new Error('Greeting must not wait for optional process observation'); };
  const turn = await runtime.prepare({ conversationId: f.conversationId, message: '你好！', permissionMode: 'full', provider: 'fixture', model: 'mock-model' }, f.conversationId);
  assert.equal(turn.toolContext, null); assert.deepEqual(turn.catalog, []);
  assert.match(turn.requestOptions.system, /short, natural greeting/);
  assert.equal(runtime.retrieval.index.worker, null); assert.equal(runtime.retrieval.embeddings.status().loaded, false);
  assert.equal(runtime.retrieval.reranker.status().loaded, false);
  assert.equal((await f.conversations.readMessages(f.conversationId)).filter(item => item.Role === 'user').at(-1).Content, '你好！');
});

async function runtimeWithMockModel(t, f, answer, { contextWindowTokens = 1048576, maxOutputTokens = 262144 } = {}) {
  const models = new ModelStore({ dataHome: f.dataHome }), requests = [];
  const upstream = createServer(async (request, response) => {
    let body = ''; for await (const part of request) body += part;
    requests.push(JSON.parse(body));
    response.setHeader('Content-Type', 'application/json');
    response.end(JSON.stringify({ choices: [{ finish_reason: 'stop', message: { role: 'assistant', content: answer } }] }));
  });
  await new Promise(resolve => upstream.listen(0, '127.0.0.1', resolve));
  t.after(() => new Promise(resolve => upstream.close(resolve)));
  await models.save({ providerId: 'fixture', displayName: 'Fixture', baseUrl: `http://127.0.0.1:${upstream.address().port}/v1`,
    models: ['mock-model'], contextWindowTokens, maxOutputTokens });
  const runtime = new ModelRuntime({ modelStore: models, dataHome: f.dataHome, conversationStore: f.conversations, toolService: f.service });
  t.after(() => runtime.close());
  await runtime.retrieval.settings.patchGlobal({ expectedRevision: 0, patch: { local: { semantic: 'off' } } });
  return { runtime, requests };
}

test('a model turn resolves a local follow-up without replacing current query entities', async t => {
  const f = await toolFixture(t), { runtime, requests } = await runtimeWithMockModel(t, f, 'SQLite 恢复步骤需要检查备份。');
  const userId = randomUUID();
  await f.conversations.upsertMessage(f.conversationId, { Id: userId, Role: 'user', Status: 'completed',
    Content: '根据项目资料比较 SQLite 和 PostgreSQL 的存储方案' });
  await f.conversations.upsertMessage(f.conversationId, { Id: randomUUID(), Role: 'assistant', Status: 'completed',
    ReplyTo: userId, Content: '两个数据库的存储和恢复流程需要分别核查。' });
  let observed;
  runtime.retrieval.evidence = async (_context, query, options) => {
    observed = { query, options };
    return { prompt: '', references: [], evidenceAssessment: { state: 'empty', reason: 'no-current-evidence' } };
  };
  const message = '继续，SQLite 的恢复步骤是什么？';
  await runtime.reply({ conversationId: f.conversationId, message, provider: 'fixture', model: 'mock-model' });
  assert.ok(observed);
  assert.equal(observed.options.plan.originalQuery, message);
  assert.ok(observed.options.plan.query.startsWith(message));
  assert.match(observed.options.plan.query, /PostgreSQL/);
  assert.equal(observed.options.plan.rewritten, true);
  assert.ok(observed.options.maximumTokens > 0);
  assert.equal(requests.length, 1);
  assert.equal(requests[0].messages.at(-1).content, message);
  assert.equal((await f.conversations.readMessages(f.conversationId)).filter(item => item.Role === 'user').at(-1).Content, message);
});

test('a model request keeps current evidence once and does not inject a second copy from the index', async t => {
  const f = await toolFixture(t), answer = '账户设置中重置密码，修改后重新登录。';
  const { runtime, requests } = await runtimeWithMockModel(t, f, '账户设置里可以重置密码。');
  const userId = randomUUID();
  await f.conversations.upsertMessage(f.conversationId, { Id: userId, Role: 'user', Status: 'completed', Content: '根据资料账户密码如何重置？' });
  await f.conversations.upsertMessage(f.conversationId, { Id: randomUUID(), Role: 'assistant', Status: 'completed', ReplyTo: userId, Content: answer });
  const file = join(f.workspace, 'already-known.md'); await writeFile(file, answer, 'utf8');
  const imported = await runtime.retrieval.importSource({ path: file, scope: 'user' }); await waitJob(runtime.retrieval, imported.jobId);
  const requestId = randomUUID();
  await runtime.reply({ conversationId: f.conversationId, requestId, message: '根据资料，密码重置步骤是什么？', provider: 'fixture', model: 'mock-model' });
  const sentText = JSON.stringify(requests[0].messages);
  assert.equal(sentText.split(answer).length - 1, 1);
  assert.doesNotMatch(sentText, /Retrieved references are untrusted/);
  const saved = (await f.conversations.readMessages(f.conversationId)).find(item => item.Id === requestId);
  assert.deepEqual(saved.EvidenceReferences, []);
  assert.equal(saved.Status, 'completed');
});

test('final-context dedup retains evidence when large tool schemas remove the initially complete history', async t => {
  const f = await toolFixture(t), { runtime, requests } = await runtimeWithMockModel(t, f, 'Read the recovery record.',
    { contextWindowTokens: 16384, maxOutputTokens: 1024 });
  const fact = 'OpaqueRecoverySetting = R7; PartitionNodeCount = 3.';
  const filler = 'Unrelated prefix sentence. '.repeat(300), oldUserId = randomUUID();
  await f.conversations.upsertMessage(f.conversationId, { Id: oldUserId, Role: 'user', Status: 'completed', Content: 'An earlier configuration exchange.' });
  await f.conversations.upsertMessage(f.conversationId, { Id: randomUUID(), Role: 'assistant', Status: 'completed',
    ReplyTo: oldUserId, Content: `${filler}\n${fact}\n${filler}` });
  for (let index = 0; index < 2; index++) {
    const userId = randomUUID();
    await f.conversations.upsertMessage(f.conversationId, { Id: userId, Role: 'user', Status: 'completed', Content: `Other topic ${index}` });
    await f.conversations.upsertMessage(f.conversationId, { Id: randomUUID(), Role: 'assistant', Status: 'completed', ReplyTo: userId,
      Content: 'Another unrelated response. '.repeat(60) });
  }
  const message = '根据资料，恢复配置记录是什么？';
  const initial = buildContext({ conversationId: f.conversationId, projectId: f.projectId,
    history: await f.conversations.readModelMessages(f.conversationId), currentMessage: message,
    contextWindowTokens: 16384, maxOutputTokens: 1024 });
  assert.ok(initial.messages.some(item => item.content.includes(fact)), 'The early context really contains the complete original fact.');
  const file = join(f.workspace, '恢复配置.md'); await writeFile(file, fact, 'utf8');
  const imported = await runtime.retrieval.importSource({ path: file, scope: 'user' }); await waitJob(runtime.retrieval, imported.jobId);
  // Synthetic large schemas isolate request budgeting, with no external MCP or tool execution.
  // 较大的合成 schema 单独验证请求预算，不连接外部 MCP，也不执行工具。
  const descriptor = { name: 'knowledge.read', source: 'builtin', enabled: true, description: 'Read the local recovery configuration.',
    inputSchema: { type: 'object', properties: { sourceRef: { type: 'string', description: 'payload '.repeat(900) } },
      required: ['sourceRef'], additionalProperties: false } };
  f.service.catalog = async context => {
    f.service.catalogs.set(context, { generation: f.service.configGeneration, descriptors: new Map([[descriptor.name, descriptor]]), servers: new Map() });
    return [descriptor];
  };
  f.service.systemPrompt = async () => 'Source tools context. '.repeat(200);
  const originalSearch = runtime.retrieval.search.bind(runtime.retrieval); let searches = 0;
  runtime.retrieval.search = (...args) => { searches++; return originalSearch(...args); };
  const originalFinalize = runtime.retrieval.finalizeEvidence.bind(runtime.retrieval); let finalRetainedContext;
  runtime.retrieval.finalizeEvidence = (context, prepared, options) => {
    finalRetainedContext = options.existingContext;
    return originalFinalize(context, prepared, options);
  };
  const originalPrepare = runtime.prepare.bind(runtime); let preparedTurn;
  runtime.prepare = async (...args) => { preparedTurn = await originalPrepare(...args); return preparedTurn; };
  const requestId = randomUUID();
  await runtime.reply({ conversationId: f.conversationId, requestId, message, permissionMode: 'full', provider: 'fixture', model: 'mock-model' });
  assert.equal(searches, 1); assert.equal(requests.length, 1);
  assert.ok(finalRetainedContext, 'Production finalization was used.');
  assert.doesNotMatch(JSON.stringify(finalRetainedContext), /OpaqueRecoverySetting/,
    'The final retained history and summary cannot provide the complete fact.');
  assert.equal(JSON.stringify(requests[0].messages).split(fact).length - 1, 1);
  assert.equal(requests[0].tools.length, 1, 'The bounded schema remains available instead of falling back to a tool-free request.');
  assert.equal(requests[0].max_tokens, 1024);
  assert.ok(estimateToolMessageTokens(requests[0].messages) + estimateTokens(JSON.stringify(requests[0].tools)) <= preparedTurn.inputBudgetTokens);
  const saved = (await f.conversations.readMessages(f.conversationId)).find(item => item.Id === requestId);
  assert.ok(saved.EvidenceReferences.length); assert.ok(saved.RetrievalResultRef);
  const archived = await f.service.results.read({ conversationId: f.conversationId }, saved.RetrievalResultRef.id);
  assert.match(archived.text, /OpaqueRecoverySetting/);
  assert.ok((await f.conversations.readMessages(f.conversationId)).some(item => item.Content?.includes(fact)), 'Formal history is retained.');
});

test('weak local evidence still reaches a model turn with an explicit source-read guidance', async t => {
  const f = await toolFixture(t), { runtime, requests } = await runtimeWithMockModel(t, f, '现有资料缺少恢复条件，需要回读或补充说明。');
  const file = join(f.workspace, 'partial.md'); await writeFile(file, '数据库目录只有位置说明。', 'utf8');
  const imported = await runtime.retrieval.importSource({ path: file, scope: 'user' }); await waitJob(runtime.retrieval, imported.jobId);
  const requestId = randomUUID();
  const answer = await runtime.reply({ conversationId: f.conversationId, requestId,
    message: '根据资料，数据库恢复权限冲突和备份校验失败如何处理？', provider: 'fixture', model: 'mock-model' });
  assert.match(answer, /需要回读/);
  assert.equal(requests.length, 1);
  assert.match(JSON.stringify(requests[0].messages), /Evidence support is unverified/);
  assert.equal((await f.conversations.readMessages(f.conversationId)).find(item => item.Id === requestId).Status, 'completed');
});

test('a small-talk model turn avoids tool discovery and local inference', async t => {
  const f = await toolFixture(t), { runtime, requests } = await runtimeWithMockModel(t, f, '不客气。');
  f.service.createContext = async () => { throw new Error('Small talk must not prepare tools'); };
  runtime.retrieval.evidence = async () => { throw new Error('Small talk must not retrieve'); };
  const answer = await runtime.reply({ conversationId: f.conversationId, message: '谢谢你',
    permissionMode: 'full', provider: 'fixture', model: 'mock-model' });
  assert.equal(answer, '不客气。');
  assert.equal(requests.length, 1);
  assert.equal(runtime.retrieval.index.worker, null);
  assert.equal(runtime.retrieval.embeddings.status().loaded, false);
  assert.equal(runtime.retrieval.reranker.status().loaded, false);
});

test('direct execution preserves a dependent file tool loop and on-demand knowledge tools without automatic retrieval', async t => {
  for (const language of ['zh', 'en']) await t.test(language, async child => {
    const f = await toolFixture(child), requests = [], models = new ModelStore({ dataHome: f.dataHome });
    const inputFile = `input-${randomUUID()}.txt`, outputFile = `output-${randomUUID()}.txt`, content = `PUBLIC-${randomUUID()}`;
    await writeFile(join(f.workspace, inputFile), content, 'utf8');
    const upstream = createServer(async (request, response) => {
      try {
        let text = ''; for await (const chunk of request) text += chunk;
        const body = JSON.parse(text); requests.push(body);
        const step = requests.length, observed = body.messages.findLast(item => item.role === 'tool');
        let result;
        if (step <= 3) {
          const operation = step === 2 ? 'filesystem.write' : 'filesystem.read';
          const descriptor = body.tools.find(item => item.function.description.startsWith(`${operation}:`));
          assert.ok(descriptor, `The actual request declares ${operation}`);
          const args = step === 1 ? { path: inputFile } : step === 2
            ? { path: outputFile, content: JSON.parse(observed.content).content, expectedHash: null } : { path: outputFile };
          result = { choices: [{ finish_reason: 'tool_calls', message: { role: 'assistant', content: '',
            tool_calls: [{ type: 'function', id: `dependent-${step}`, function: { name: descriptor.function.name, arguments: JSON.stringify(args) } }] } }] };
        } else {
          assert.equal(JSON.parse(observed.content).content, content, 'The broker read back the written value.');
          result = { choices: [{ finish_reason: 'stop', message: { role: 'assistant', content: language === 'zh' ? '完成。' : 'Done.' } }] };
        }
        response.setHeader('Content-Type', 'application/json'); response.end(JSON.stringify(result));
      } catch (error) { response.writeHead(500); response.end(JSON.stringify({ error: error.message })); }
    });
    await new Promise(resolve => upstream.listen(0, '127.0.0.1', resolve));
    child.after(() => new Promise(resolve => upstream.close(resolve)));
    await models.save({ providerId: 'fixture', displayName: 'Fixture', baseUrl: `http://127.0.0.1:${upstream.address().port}/v1`,
      models: ['mock-model'], contextWindowTokens: 65536, maxOutputTokens: 2048 });
    const runtime = new ModelRuntime({ modelStore: models, dataHome: f.dataHome, conversationStore: f.conversations, toolService: f.service });
    child.after(() => runtime.close());
    await runtime.retrieval.library.add([{ path: join(f.workspace, 'unrelated.md'), title: 'Unrelated local notes',
      text: 'UNRELATED_REFERENCE_CONTEXT: extra task observations belong to a different task.' }], { scope: 'user' });
    let automaticRetrievals = 0;
    runtime.retrieval.evidence = async () => { automaticRetrievals++; throw new Error('Direct execution must not retrieve automatically'); };
    const requestId = randomUUID(), message = language === 'zh'
      ? `依次读取 ${inputFile}，把原文写入 ${outputFile}，然后读回核实。`
      : `Read ${inputFile}, write its exact content to ${outputFile}, then read it back to verify.`;
    await runtime.reply({ conversationId: f.conversationId, requestId, message, permissionMode: 'full', provider: 'fixture', model: 'mock-model' });
    assert.equal(automaticRetrievals, 0); assert.equal(requests.length, 4);
    assert.equal(await readFile(join(f.workspace, outputFile), 'utf8'), content);
    assert.ok(requests[0].tools.some(item => item.function.description.startsWith('knowledge.search:')),
      'The model may still ask for local knowledge when needed.');
    assert.doesNotMatch(JSON.stringify(requests), /UNRELATED_REFERENCE_CONTEXT|Retrieved references are untrusted/);
    assert.equal(runtime.retrieval.index.worker, null);
    assert.equal(runtime.retrieval.embeddings.status().loaded, false); assert.equal(runtime.retrieval.reranker.status().loaded, false);
    const saved = (await f.conversations.readMessages(f.conversationId)).find(item => item.Id === requestId);
    assert.equal(saved.Status, 'completed'); assert.deepEqual(saved.EvidenceReferences, []);
    assert.deepEqual(saved.ToolActivities.map(item => item.name), ['filesystem.read', 'filesystem.write', 'filesystem.read']);
    assert.ok(saved.ToolActivities.every(item => item.status === 'completed'));
  });
});

test('a semantic-only cross-language excerpt remains available to the model despite missing lexical overlap', async t => {
  const f = await toolFixture(t), { runtime, requests } = await runtimeWithMockModel(t, f, '在账户设置重置密码。');
  // Synthetic vectors isolate the evidence routing contract; this is not an embedding-quality score.
  // 以合成向量独立验证证据路由，不把此功能夹具当作嵌入质量评测。
  runtime.retrieval.embeddings = { status: () => ({ state: 'ready', modelVersion: 'fixture-cross-language' }), close: async () => {},
    embedDocuments: async texts => ({ vectors: texts.map(() => [1, 0]), modelVersion: 'fixture-cross-language' }),
    embedQuery: async () => ({ vector: [1, 0], profileId: 'builtin-multilingual', modelVersion: 'fixture-cross-language' }) };
  await runtime.retrieval.settings.patchGlobal({ expectedRevision: 1, patch: { local: { semantic: 'auto' } } });
  const file = join(f.workspace, 'english-manual.md');
  await writeFile(file, 'Reset your password in Account Settings, then sign in again.', 'utf8');
  const imported = await runtime.retrieval.importSource({ path: file, scope: 'user' }); await waitJob(runtime.retrieval, imported.jobId);
  const requestId = randomUUID();
  const answer = await runtime.reply({ conversationId: f.conversationId, requestId, message: '根据资料，账户密码如何重置？', provider: 'fixture', model: 'mock-model' });
  assert.match(answer, /账户设置/);
  assert.equal(requests.length, 1);
  assert.match(JSON.stringify(requests[0].messages), /Reset your password in Account Settings/);
  assert.match(JSON.stringify(requests[0].messages), /cross-language matches may still be relevant/);
  const saved = (await f.conversations.readMessages(f.conversationId)).find(item => item.Id === requestId);
  assert.ok(saved.EvidenceReferences.length);
  assert.equal(saved.Status, 'completed');
});

test('local evidence reaches the real model-request projection and its exact excerpts remain archived', async t => {
  const f = await toolFixture(t), models = new ModelStore({ dataHome: f.dataHome });
  let sent;
  const upstream = createServer(async (request, response) => {
    let body = ''; for await (const part of request) body += part;
    sent = JSON.parse(body); response.setHeader('Content-Type', 'application/json');
    response.end(JSON.stringify({ choices: [{ finish_reason: 'stop', message: { role: 'assistant', content: '账户设置中重置密码。（资料：help.md）' } }] }));
  });
  await new Promise(resolve => upstream.listen(0, '127.0.0.1', resolve)); t.after(() => new Promise(resolve => upstream.close(resolve)));
  await models.save({ providerId: 'fixture', displayName: 'Fixture', baseUrl: `http://127.0.0.1:${upstream.address().port}/v1`,
    models: ['mock-model'], contextWindowTokens: 1048576, maxOutputTokens: 262144 });
  const runtime = new ModelRuntime({ modelStore: models, dataHome: f.dataHome, conversationStore: f.conversations, toolService: f.service });
  t.after(() => runtime.close());
  await runtime.retrieval.settings.patchGlobal({ expectedRevision: 0, patch: { local: { semantic: 'off' } } });
  const file = join(f.workspace, 'help.md'); await writeFile(file, '账户设置中重置密码，修改后重新登录。', 'utf8');
  const imported = await runtime.retrieval.importSource({ path: file, scope: 'user' });
  assert.equal((await waitJob(runtime.retrieval, imported.jobId)).status, 'completed');
  const requestId = randomUUID();
  assert.match(await runtime.reply({ conversationId: f.conversationId, requestId, message: '根据资料，账户密码如何重置？', provider: 'fixture', model: 'mock-model' }), /账户设置/);
  assert.match(JSON.stringify(sent.messages), /账户设置中重置密码/);
  assert.equal(sent.max_tokens, 262144);
  const answer = (await f.conversations.readMessages(f.conversationId)).find(item => item.Id === requestId);
  assert.ok(answer.EvidenceReferences.length); assert.ok(answer.RetrievalResultRef);
  const archived = await f.service.results.read({ conversationId: f.conversationId }, answer.RetrievalResultRef.id);
  assert.match(archived.text, /账户设置中重置密码/);
});

test('chat history never includes sibling transcripts or the current question, and revoked imports cannot be read', async t => {
  const f = await toolFixture(t), retrieval = coordinator(f); t.after(() => retrieval.close());
  const siblingId = randomUUID(), currentUserId = randomUUID(), requestId = randomUUID();
  const catalog = await f.conversations.catalog();
  catalog.Projects[0].Chats.push({ Id: siblingId, Title: 'Private sibling', Messages: [
    { Id: randomUUID(), Role: 'user', Content: '绝密旁聊暗号：蓝色鲸鱼', Status: 'completed' }] });
  await f.conversations.saveCatalog(catalog);
  await f.conversations.upsertMessage(f.conversationId, { Id: currentUserId, Role: 'user', Content: '当前独特问题星球编号', Status: 'completed' });
  await f.conversations.upsertMessage(f.conversationId, { Id: requestId, Role: 'assistant', ReplyTo: currentUserId, Content: '', Status: 'streaming' });
  const context = { conversationId: f.conversationId, projectId: f.projectId, requestId };
  for (const query of ['绝密旁聊蓝色鲸鱼', '当前独特问题星球编号']) assert.equal((await retrieval.search(context, { query })).items.length, 0);
  const file = join(f.workspace, 'manual.md'); await writeFile(file, '密码重置指南：请打开账户设置。', 'utf8');
  const imported = await retrieval.importSource({ path: file, scope: 'project', projectId: f.projectId }); await waitJob(retrieval, imported.jobId);
  const found = await retrieval.search(context, { query: '密码重置' }); assert.ok(found.items.length);
  const reference = found.items.find(item => item.sourceId === imported.id).sourceRef;
  await retrieval.removeSource(imported.id, { expectedRevision: imported.revision });
  await assert.rejects(retrieval.read(context, { sourceRef: reference }), /不存在|撤销/);
});

test('mounted sources update by content version, honor gitignore and disappear immediately when indexing is disabled', async t => {
  const f = await toolFixture(t), retrieval = coordinator(f); t.after(() => retrieval.close());
  // This case isolates mounted lexical freshness; deliberately absent embedding assets are covered separately.
  // 本例独立验证挂载资料词法时效，刻意缺失嵌入资产的降级另行覆盖。
  await retrieval.settings.patchGlobal({ expectedRevision: 0, patch: { local: { semantic: 'off' } } });
  await mkdir(join(f.workspace, 'ignored')); await writeFile(join(f.workspace, '.gitignore'), 'ignored/\n', 'utf8');
  await writeFile(join(f.workspace, 'ignored', 'secret.md'), '不应索引的特殊关键字', 'utf8');
  const file = join(f.workspace, 'guide.md'); await writeFile(file, '版本甲功能使用说明', 'utf8');
  await retrieval.settings.patchProject(f.projectId, { expectedRevision: 0, patch: { indexingSources: { mountedFolder: { enabled: true } } } });
  const job = await retrieval.rebuild({ projectId: f.projectId }); assert.equal((await waitJob(retrieval, job.jobId)).status, 'completed');
  const context = { conversationId: f.conversationId, projectId: f.projectId };
  const old = (await retrieval.search(context, { query: '版本甲功能' })).items[0]; assert.ok(old);
  assert.equal((await retrieval.search(context, { query: '不应索引特殊关键字' })).items.length, 0);
  await writeFile(file, '版本乙功能新的实现说明', 'utf8');
  const update = await retrieval.rebuild({ projectId: f.projectId }); assert.equal((await waitJob(retrieval, update.jobId)).status, 'completed');
  assert.ok((await retrieval.search(context, { query: '版本乙功能' })).items.some(item => /版本乙/u.test(item.excerpt)));
  await assert.rejects(retrieval.read(context, { sourceRef: old.sourceRef }), /过期|更改|撤销/);
  await retrieval.settings.patchProject(f.projectId, { expectedRevision: 1, patch: { indexingSources: { mountedFolder: { enabled: false } } } });
  assert.equal((await retrieval.search(context, { query: '版本乙功能' })).items.length, 0);
  await assert.rejects(retrieval.read(context, { sourceRef: old.sourceRef }), /不存在|撤销|更改/);
});

test('a background embedding job does not hold the foreground query queue or resurrect a removed source', async t => {
  const f = await toolFixture(t), retrieval = coordinator(f); t.after(() => retrieval.close());
  let release, entered;
  const ready = new Promise(resolve => { entered = resolve; });
  retrieval.embeddings = { status: () => ({ state: 'ready' }), close: async () => release?.(),
    embedDocuments: async texts => { entered(); await new Promise(resolve => { release = resolve; }); return { vectors: texts.map(() => [1, 0]), modelVersion: 'synthetic' }; },
    embedQuery: async () => ({ vector: [1, 0], profileId: 'builtin-multilingual', modelVersion: 'synthetic' }) };
  const file = join(f.workspace, 'manual.md'); await writeFile(file, '后台资料的关键词', 'utf8');
  const imported = await retrieval.importSource({ path: file, scope: 'user' }); await ready;
  const context = { conversationId: f.conversationId, projectId: f.projectId };
  const found = await Promise.race([retrieval.search(context, { query: '后台资料' }), new Promise((_, reject) => { const timer = setTimeout(() => reject(new Error('Foreground query blocked by embedding')), 2000); timer.unref(); })]);
  assert.ok(found.items.length); await retrieval.removeSource(imported.id, { expectedRevision: 1 });
  release(); await waitJob(retrieval, imported.jobId);
  assert.equal((await retrieval.search(context, { query: '后台资料' })).items.length, 0);
});

test('web normalization and search budgets retain concise public sources and bound network work only', async () => {
  const sources = normalizeWebSources(JSON.stringify({ results: [{ url: 'https://example.com/docs', title: 'Docs', text: 'Useful evidence' },
    { url: 'http://127.0.0.1/private', text: 'Must never publish' }, { url: 'https://example.com/docs', text: 'Duplicate' }] }));
  assert.equal(sources.length, 1); assert.equal(sources[0].title, 'Docs');
  const tools = { retrieval: { effective: async () => ({ web: { mode: 'auto', depth: 'standard', providerId: 'auto' } }) } };
  const web = new WebSearchTool(tools), context = {};
  await web.take(context, 'query'); await web.take(context, 'query');
  await assert.rejects(web.take(context, 'query'), { code: 'WEB_STAGE_BUDGET_EXHAUSTED' });
  assert.equal(context.cancelled, undefined);
  tools.retrieval.effective = async () => ({ web: { mode: 'off' } });
  await assert.rejects(web.take({}, 'query'), { code: 'WEB_SEARCH_DISABLED' });
});

test('bounded source scanning rejects partial imports and skips invalid encoding/sensitive files', async t => {
  const f = await toolFixture(t);
  await writeFile(join(f.workspace, '.env'), 'FAKE_SECRET_ONLY=value', 'utf8');
  await writeFile(join(f.workspace, 'binary.txt'), Buffer.from([255, 128]));
  await writeFile(join(f.workspace, 'a.md'), 'safe text', 'utf8'); await writeFile(join(f.workspace, 'b.md'), 'other text', 'utf8');
  assert.equal((await readSourceTree(f.workspace)).length, 2);
  await assert.rejects(readSourceTree(f.workspace, { maximumFiles: 1 }), { code: 'RETRIEVAL_SCAN_LIMIT' });
});

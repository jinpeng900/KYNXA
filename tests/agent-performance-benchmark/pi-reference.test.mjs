import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { access, readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test as nodeTest } from 'node:test';
import { toolFixture } from '../../apps/model-gateway/tests/tool-fixture.mjs';
import { RetrievalCoordinator } from '../../apps/model-gateway/orchestration/retrieval/coordinator.mjs';
import { createPiReference, PI_REFERENCE_VERSION, PI_REFERENCE_SDK_ROOT } from './pi-reference.mjs';

const LIMITS = { maxRounds: 12, maxToolCalls: 24, maxGeneratedTokens: 98304, maxDurationMs: 10000 };
async function sdkIsAvailable(root) {
  try { await Promise.all(['package-lock.json', 'node_modules/@earendil-works/pi-agent-core/dist/index.js',
    'node_modules/@earendil-works/pi-ai/dist/api/openai-completions.js', 'node_modules/@earendil-works/pi-telemetry/package.json']
    .map(path => access(join(root, path)))); return true; }
  catch (error) { if (error.code !== 'ENOENT') throw error; return false; }
}
const sdkAvailable = await sdkIsAvailable(PI_REFERENCE_SDK_ROOT);
// The optional, ignored SDK is restored only for this integration benchmark; ordinary clones stay testable.
// 此集成评测按需恢复被忽略的 SDK 依赖，普通克隆仓库可以执行其余测试，并明确报告未运行的验收。
const test = (name, fn) => nodeTest(name, { skip: sdkAvailable ? false
  : 'Optional pinned pi-agent-core 1.0.3 benchmark SDK is not installed in artifacts; restore it to run this integration test.' }, fn);

nodeTest('a missing optional SDK is detectable without installing packages or opening fixture data', async () => {
  assert.equal(await sdkIsAvailable(join(PI_REFERENCE_SDK_ROOT, 'not-installed-fixture')), false);
});

async function provider(t, respond) {
  const requests = [], errors = [];
  const server = createServer(async (request, response) => {
    const bytes = [];
    for await (const chunk of request) bytes.push(chunk);
    const payload = JSON.parse(Buffer.concat(bytes).toString('utf8')); requests.push(payload);
    try {
      const message = await respond(payload, requests.length);
      if (message.httpError) { response.writeHead(message.httpError, { 'content-type': 'application/json' });
        response.end('{"error":{"message":"Synthetic provider error"}}'); return; }
      const calls = message.calls ?? (message.tool ? [{ id: `fixture-call-${requests.length}`, name: message.tool, args: message.args }] : []);
      const delta = calls.length ? { tool_calls: calls.map((call, index) => ({ index, id: call.id, type: 'function',
        function: { name: call.name, arguments: JSON.stringify(call.args) } })) } : { content: message.text };
      const base = { id: `fixture-response-${requests.length}`, object: 'chat.completion.chunk', created: 1, model: payload.model };
      response.writeHead(200, { 'content-type': 'text/event-stream' });
      response.write('data: ' + JSON.stringify({ ...base, choices: [{ index: 0, delta, finish_reason: null }] }) + '\n\n');
      response.write('data: ' + JSON.stringify({ ...base, choices: [{ index: 0, delta: {}, finish_reason: calls.length ? 'tool_calls' : 'stop' }] }) + '\n\n');
      response.write('data: ' + JSON.stringify({ ...base, choices: [], usage: { prompt_tokens: 40, completion_tokens: 7,
        total_tokens: 47, prompt_cache_hit_tokens: 4, prompt_cache_miss_tokens: 36 } }) + '\n\n');
      response.end('data: [DONE]\n\n');
    } catch (error) { errors.push(String(error)); response.writeHead(500); response.end(); }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(() => new Promise(resolve => { server.closeAllConnections(); server.close(resolve); }));
  return { requests, errors, connection: { protocol: 'openai-completions', baseUrl: `http://127.0.0.1:${server.address().port}/v1`, apiKey: 'synthetic-fixture-key' } };
}

function formalReceipts(fixture) {
  const records = new Map();
  return async (activity, context) => {
    if (!records.has(context.requestId)) records.set(context.requestId, new Map());
    records.get(context.requestId).set(activity.toolCallId, activity);
    await fixture.conversations.upsertMessage(context.conversationId, { Id: context.requestId, Role: 'assistant', Content: '',
      Status: 'streaming', ToolActivities: [...records.get(context.requestId).values()] });
  };
}

const toolName = (payload, name) => payload.tools.find(tool => tool.function.description.startsWith(name + ':')).function.name;
const lastResult = payload => JSON.parse(payload.messages.findLast(message => message.role === 'tool').content);

test('actual Pi SDK performs SSE → real dependent read/write/readback → SSE and preserves follow-up history and updated system', async t => {
  const f = await toolFixture(t), context = await f.context('full');
  await writeFile(join(f.workspace, 'seed.txt'), 'ticket=SYNTHETIC-389\n', 'utf8');
  const p = await provider(t, (payload, round) => {
    assert.equal(payload.stream, true); assert.equal(payload.max_tokens, 8192); assert.equal(payload.model, 'synthetic-fixed-model');
    if (round === 1) { assert.ok(payload.messages.some(message => message.content === 'Earlier synthetic context.'));
      return { tool: toolName(payload, 'filesystem.read'), args: { path: 'seed.txt', maxChars: 200 } }; }
    if (round === 2) return { tool: toolName(payload, 'filesystem.write'), args: { path: 'receipt.txt', content: lastResult(payload).content, expectedHash: null } };
    if (round === 3) { assert.match(lastResult(payload).sha256, /^[a-f0-9]{64}$/u);
      return { tool: toolName(payload, 'filesystem.read'), args: { path: 'receipt.txt', maxChars: 200 } }; }
    if (round === 4) return { text: 'Verified ' + lastResult(payload).content.trim() };
    assert.equal(payload.messages[0].content, 'Updated synthetic system.');
    assert.ok(payload.messages.some(message => message.content === 'Verified ticket=SYNTHETIC-389'));
    return { text: 'Follow-up ticket=SYNTHETIC-389' };
  });
  const catalog = (await f.service.catalog(context)).filter(tool => ['filesystem.read', 'filesystem.write'].includes(tool.name));
  const reference = await createPiReference({ connection: p.connection, model: 'synthetic-fixed-model', service: f.service, catalog,
    limits: LIMITS, systemPrompt: 'Original synthetic system.', initialMessages: [{ role: 'user', content: 'Earlier synthetic context.' }],
    onToolActivity: formalReceipts(f) });
  t.after(() => reference.close());
  const first = await reference.prompt('Read, write and verify the actual ticket.', { context });
  assert.equal(first.errorCode, undefined, JSON.stringify({ errors: p.errors, tools: first.assistant.ToolActivities.map(item => [item.name, item.status]) }));
  assert.equal(first.assistant.Content, 'Verified ticket=SYNTHETIC-389');
  assert.deepEqual(first.assistant.ToolActivities.map(activity => activity.name), ['filesystem.read', 'filesystem.write', 'filesystem.read']);
  assert.ok(first.assistant.ToolActivities.every(activity => activity.status === 'completed' && activity.resultRef));
  assert.equal(await readFile(join(f.workspace, 'receipt.txt'), 'utf8'), 'ticket=SYNTHETIC-389\n');
  const second = await reference.prompt('Continue with the verified ticket.', { context: await f.context('full'), systemPrompt: 'Updated synthetic system.' });
  assert.equal(second.assistant.Content, 'Follow-up ticket=SYNTHETIC-389');
  const snapshot = reference.snapshot();
  assert.equal(snapshot.reference.version, PI_REFERENCE_VERSION); assert.match(snapshot.reference.dependencyLockSha256, /^[a-f0-9]{64}$/u);
  assert.equal(snapshot.modelCalls.length, 5); assert.equal(snapshot.toolCalls, 3);
  assert.ok(snapshot.modelCalls.every(call => call.usage.status === 'reported' && call.usage.inputTokens === 40 && call.usage.outputTokens === 7 && call.usage.cacheReadTokens === 4));
  assert.equal(JSON.stringify(snapshot).includes(p.connection.apiKey), false);
  const formal = (await f.conversations.readModelMessages(f.conversationId)).find(message => message.Id === context.requestId);
  assert.equal(formal.ToolActivities.length, 3);
});

test('actual Pi search receipt is persisted before its short reference is consumed by a real knowledge.read', async t => {
  const f = await toolFixture(t), retrieval = new RetrievalCoordinator({ conversations: f.conversations,
    memory: { contextFor: async () => ({ entries: [] }) }, tools: f.service,
    embeddings: { status: () => ({ state: 'unavailable' }), close: async () => {} }, reranker: null });
  f.service.retrieval = retrieval;
  await retrieval.settings.patchGlobal({ expectedRevision: 0, patch: { local: { semantic: 'off' } } });
  await retrieval.library.add([{ path: join(f.workspace, 'public.md'), title: 'Public fixture', text: '# Public source\n\nAtlas review owner is Synthetic Reviewer.\n' }], { scope: 'user' });
  const context = await f.context('full'), p = await provider(t, (payload, round) => {
    if (round === 1) return { tool: toolName(payload, 'knowledge.search'), args: { query: 'Atlas review owner', limit: 3 } };
    if (round === 2) { const sourceRef = lastResult(payload).items[0].sourceRef;
      assert.match(sourceRef, /^ev1:/u); assert.equal(sourceRef.length, 29);
      return { tool: toolName(payload, 'knowledge.read'), args: { sourceRef, mode: 'section', gap: 'Verify the reviewer name in the source section.' } }; }
    assert.match(lastResult(payload).text, /Synthetic Reviewer/u); return { text: 'Confirmed Synthetic Reviewer.' };
  });
  const catalog = (await f.service.catalog(context)).filter(tool => tool.name.startsWith('knowledge.'));
  const reference = await createPiReference({ connection: p.connection, model: 'synthetic-fixed-model', service: f.service, catalog,
    limits: LIMITS, onToolActivity: formalReceipts(f) });
  t.after(() => reference.close());
  const result = await reference.prompt('Find and verify the actual source reviewer.', { context });
  assert.equal(result.errorCode, undefined, JSON.stringify({ errors: p.errors, tools: result.assistant.ToolActivities.map(item => [item.name, item.status]) }));
  assert.equal(result.assistant.Content, 'Confirmed Synthetic Reviewer.');
  assert.deepEqual(result.assistant.ToolActivities.map(activity => activity.status), ['completed', 'completed']);
  assert.equal(reference.snapshot().modelCalls.length, 3);
});

test('provider failure is one real SDK request with unknown usage, no automatic retry and a measured failure duration', async t => {
  const f = await toolFixture(t), context = await f.context('full'), p = await provider(t, () => ({ httpError: 503 }));
  const reference = await createPiReference({ connection: p.connection, model: 'synthetic-fixed-model', service: f.service, catalog: [],
    limits: LIMITS, onToolActivity: formalReceipts(f) });
  t.after(() => reference.close());
  const result = await reference.prompt('Respond once.', { context }), snapshot = reference.snapshot();
  assert.equal(result.errorCode, 'PI_REFERENCE_MODEL_ERROR'); assert.equal(p.requests.length, 1);
  assert.equal(snapshot.modelCalls[0].status, 'error'); assert.equal(snapshot.modelCalls[0].usage.status, 'unknown');
  assert.ok(snapshot.modelCalls[0].durationMs > 0); assert.equal(snapshot.assistants[0].Status, 'interrupted');
});

test('input budget rejection stops before provider fetch or real tool effects', async t => {
  const f = await toolFixture(t), context = await f.context('full'), p = await provider(t, () => ({ text: 'Should not run.' }));
  const reference = await createPiReference({ connection: p.connection, model: 'synthetic-fixed-model', service: f.service, catalog: [],
    limits: LIMITS, inputBudgetTokens: 32, onToolActivity: formalReceipts(f) });
  t.after(() => reference.close());
  const result = await reference.prompt('Excessive current request text. '.repeat(100), { context });
  assert.equal(result.errorCode, 'PI_REFERENCE_CONTEXT_BUDGET_EXCEEDED'); assert.equal(p.requests.length, 0);
  assert.equal(reference.snapshot().toolCalls, 0);
});

test('round limit stops before another provider request without reclassifying the completed call or counting its usage again', async t => {
  const f = await toolFixture(t), context = await f.context('full');
  await writeFile(join(f.workspace, 'seed.txt'), 'Synthetic observed value.', 'utf8');
  const p = await provider(t, payload => ({ tool: toolName(payload, 'filesystem.read'), args: { path: 'seed.txt', maxChars: 200 } }));
  const reference = await createPiReference({ connection: p.connection, model: 'synthetic-fixed-model', service: f.service,
    catalog: (await f.service.catalog(context)).filter(tool => tool.name === 'filesystem.read'),
    limits: { ...LIMITS, maxRounds: 1 }, onToolActivity: formalReceipts(f) });
  t.after(() => reference.close());
  const result = await reference.prompt('Read then summarize.', { context }), snapshot = reference.snapshot();
  assert.equal(result.errorCode, 'BENCHMARK_MODEL_CALL_LIMIT'); assert.equal(p.requests.length, 1);
  assert.equal(snapshot.modelCalls.length, 1); assert.equal(snapshot.modelCalls[0].status, 'completed');
  assert.equal(snapshot.generatedTokens, 7); assert.equal(snapshot.toolTrace[0].status, 'completed');
});

test('external cancellation settles the actual SDK request and leaves a bounded interrupted receipt', async t => {
  const f = await toolFixture(t), context = await f.context('full'), controller = new AbortController();
  let release, entered;
  const ready = new Promise(resolve => { entered = resolve; }), gate = new Promise(resolve => { release = resolve; });
  const p = await provider(t, async () => { entered(); await gate; return { text: 'No result may be claimed after stop.' }; });
  const reference = await createPiReference({ connection: p.connection, model: 'synthetic-fixed-model', service: f.service,
    catalog: [], limits: LIMITS, signal: controller.signal, onToolActivity: formalReceipts(f) });
  t.after(() => reference.close());
  const pending = reference.prompt('Wait for actual evidence.', { context });
  await ready; controller.abort(); const result = await pending; release();
  assert.equal(result.errorCode, 'PI_REFERENCE_ABORTED'); assert.equal(p.requests.length, 1);
  assert.equal(result.assistant.Status, 'interrupted'); assert.equal(reference.snapshot().toolCalls, 0);
});

test('a parallel batch exceeding maxToolCalls executes only its admitted first call and saves both outcomes', async t => {
  const f = await toolFixture(t), context = await f.context('full');
  await writeFile(join(f.workspace, 'seed.txt'), 'Synthetic observed value.', 'utf8');
  const p = await provider(t, payload => ({ calls: ['admitted-first', 'rejected-second'].map(id => ({ id,
    name: toolName(payload, 'filesystem.read'), args: { path: 'seed.txt', maxChars: 200 } })) }));
  let executed = 0;
  const execute = f.service.execute.bind(f.service); f.service.execute = async (...args) => { executed++; return execute(...args); };
  const reference = await createPiReference({ connection: p.connection, model: 'synthetic-fixed-model', service: f.service,
    catalog: (await f.service.catalog(context)).filter(tool => tool.name === 'filesystem.read'),
    limits: { ...LIMITS, maxToolCalls: 1 }, onToolActivity: formalReceipts(f) });
  t.after(() => reference.close());
  const result = await reference.prompt('Read using bounded actual calls.', { context }), snapshot = reference.snapshot();
  assert.equal(result.errorCode, 'BENCHMARK_TOOL_CALL_LIMIT'); assert.equal(p.requests.length, 1);
  assert.equal(executed, 1); assert.equal(snapshot.executedToolCalls, 1); assert.equal(snapshot.toolCalls, 2);
  assert.deepEqual(result.assistant.ToolActivities.map(activity => [activity.toolCallId, activity.status]).toSorted(),
    [['admitted-first', 'completed'], ['rejected-second', 'error']]);
});

test('duplicate provider call IDs stop the whole batch before any real tool execution', async t => {
  const f = await toolFixture(t), context = await f.context('full');
  const p = await provider(t, payload => ({ calls: [1, 2].map(() => ({ id: 'duplicate-id',
    name: toolName(payload, 'filesystem.read'), args: { path: 'unused.txt', maxChars: 200 } })) }));
  let executed = 0; f.service.execute = async () => { executed++; throw new Error('Duplicate batch must not execute'); };
  const reference = await createPiReference({ connection: p.connection, model: 'synthetic-fixed-model', service: f.service,
    catalog: (await f.service.catalog(context)).filter(tool => tool.name === 'filesystem.read'), limits: LIMITS, onToolActivity: formalReceipts(f) });
  t.after(() => reference.close());
  const result = await reference.prompt('Do not execute repeated identities.', { context });
  assert.equal(result.errorCode, 'PI_REFERENCE_DUPLICATE_CALL_ID'); assert.equal(executed, 0);
});

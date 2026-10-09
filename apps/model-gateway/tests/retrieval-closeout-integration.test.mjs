import assert from 'node:assert/strict';
import { test } from 'node:test';
import { createServer } from 'node:http';
import { randomUUID } from 'node:crypto';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { ModelRuntime } from '../orchestration/runtime.mjs';
import { ModelStore } from '../models/store.mjs';
import { runToolLoop } from '../orchestration/tool-loop.mjs';
import { buildContext, estimateTokens } from '../models/context.mjs';
import { ModelHistoryProjection } from '../models/model-history.mjs';
import { estimateToolMessageTokens, toolDeclarations, wireCatalog } from '../models/tool-protocols.mjs';
import { toolFixture } from './tool-fixture.mjs';

const protocols = ['openai-completions', 'openai-responses', 'anthropic-messages'];
const descriptors = wireCatalog([
  { name: 'filesystem.write', description: 'Write the one declared temporary note.', inputSchema: { type: 'object', properties: { path: { type: 'string' } } } },
  { name: 'knowledge.read', description: 'Read only the requested source reference.', inputSchema: { type: 'object', properties: { sourceRef: { type: 'string' } } } },
  { name: 'filesystem.read', description: 'Read a synthetic file.', inputSchema: { type: 'object', properties: { path: { type: 'string' } } } },
]);

function resourceFixture(denied = false) {
  const requests = [], released = [];
  return { requests, released,
    async acquire(request) {
      requests.push(request);
      return denied ? { status: 'denied', reason: 'RESOURCE_FIXTURE_PRESSURE', mode: 'fixture' }
        : { status: 'granted', leaseId: `fixture-${requests.length}`, cpuThreads: 1, memoryBytes: request.memoryBytes, mode: 'fixture' };
    },
    async renew() { return { status: 'renewed' }; },
    async release(id) { released.push(id); return { status: 'released' }; },
    async report() { return { status: 'reported' }; },
    async snapshot() { return { mode: 'fixture', gpu: { state: 'unknown' } }; },
  };
}

async function plainRuntime(t, { denied = false, responseBody = '{invalid JSON' } = {}) {
  const fixture = await toolFixture(t), requests = [], resources = resourceFixture(denied);
  const upstream = createServer(async (request, response) => {
    let body = ''; for await (const chunk of request) body += chunk;
    requests.push(JSON.parse(body));
    response.writeHead(200, { 'Content-Type': 'application/json' }); response.end(responseBody);
  });
  await new Promise(done => upstream.listen(0, '127.0.0.1', done));
  t.after(async () => { upstream.closeAllConnections(); await new Promise(done => upstream.close(done)); });
  const models = new ModelStore({ dataHome: fixture.dataHome });
  await models.save({ providerId: 'fixture-api', displayName: 'Synthetic test API',
    baseUrl: `http://127.0.0.1:${upstream.address().port}/v1`, models: ['mock-model'], contextWindowTokens: 4096, maxOutputTokens: 1024 });
  const runtime = new ModelRuntime({ modelStore: models, dataHome: fixture.dataHome,
    conversationStore: fixture.conversations, toolService: fixture.service, resourceService: resources });
  t.after(() => runtime.close());
  const input = { conversationId: fixture.conversationId, requestId: randomUUID(), provider: 'fixture-api', model: 'mock-model', message: '你好' };
  return { fixture, runtime, input, requests, resources };
}

test('plain generation admission refusal preserves the resource cause without dispatching any upstream HTTP request', async t => {
  const f = await plainRuntime(t, { denied: true });
  try {
    await assert.rejects(f.runtime.reply(f.input), error => {
      assert.equal(error.code, 'RESOURCE_FIXTURE_PRESSURE');
      assert.equal(error.statusCode, 503);
      assert.doesNotMatch(error.message, /JSON/iu);
      return true;
    });
    assert.equal(f.requests.length, 0, 'resource admission precedes actual model dispatch');
    assert.equal(f.resources.requests.length, 1);
    const assistant = (await f.fixture.conversations.readMessages(f.input.conversationId)).find(message => message.Id === f.input.requestId);
    assert.equal(assistant.Status, 'error');
    assert.doesNotMatch(assistant.Error, /JSON/iu);
  } finally { await f.runtime.close(); }
});

test('an admitted plain generation with genuinely malformed JSON reports MODEL_INVALID_JSON and releases its reservation', async t => {
  const f = await plainRuntime(t);
  try {
    await assert.rejects(f.runtime.reply(f.input), { code: 'MODEL_INVALID_JSON' });
    assert.equal(f.requests.length, 1);
    assert.equal(f.resources.requests.length, 1);
    assert.deepEqual(f.resources.released, ['fixture-1']);
    const assistant = (await f.fixture.conversations.readMessages(f.input.conversationId)).find(message => message.Id === f.input.requestId);
    assert.equal(assistant.Status, 'error');
    assert.doesNotMatch(assistant.Error, /invalid JSON/u, 'private upstream response bytes are not copied into a saved error');
  } finally { await f.runtime.close(); }
});

function continuation(protocol, content, calls) {
  if (protocol === 'anthropic-messages') return [{ role: 'assistant', content: [
    ...(content ? [{ type: 'text', text: content }] : []), ...calls.map(call => ({ type: 'tool_use', id: call.id,
      name: descriptors.find(descriptor => descriptor.name === call.name).wireName, input: call.arguments }))] }];
  if (protocol === 'openai-responses') return calls.map(call => ({ type: 'function_call', call_id: call.id,
    name: descriptors.find(descriptor => descriptor.name === call.name).wireName, arguments: JSON.stringify(call.arguments) }));
  return [{ role: 'assistant', content, tool_calls: calls.map(call => ({ type: 'function', id: call.id,
    function: { name: descriptors.find(descriptor => descriptor.name === call.name).wireName, arguments: JSON.stringify(call.arguments) } })) }];
}

function pairedIds(messages, protocol) {
  if (protocol === 'anthropic-messages') {
    const blocks = messages.flatMap(message => Array.isArray(message.content) ? message.content : []);
    return [blocks.filter(block => block.type === 'tool_use').map(block => block.id),
      blocks.filter(block => block.type === 'tool_result').map(block => block.tool_use_id)];
  }
  if (protocol === 'openai-responses') return [messages.filter(message => message.type === 'function_call').map(message => message.call_id),
    messages.filter(message => message.type === 'function_call_output').map(message => message.call_id)];
  return [messages.flatMap(message => (message.tool_calls ?? []).map(call => call.id)),
    messages.filter(message => message.role === 'tool').map(message => message.tool_call_id)];
}

async function ownedDirectory(t) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-closeout-short-'));
  t.after(async () => {
    const suffix = relative(resolve(tmpdir()), resolve(root));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(root, { recursive: true, force: true });
  });
  return root;
}

// Simulated model decisions exercise the real loop; actual temporary side effects prove there is no replay.
// 模拟模型决策驱动真实循环，以临时目录中的实际写入和回读核验成功副作用不会被重放。
for (const protocol of protocols) test(`${protocol}: final freshness repair rereads the changed source and never seals its stale draft or replays a completed write`, async t => {
  const root = await ownedDirectory(t), sourcePath = join(root, 'policy.txt'), notePath = join(root, 'note.txt');
  await writeFile(sourcePath, 'Initial policy.', 'utf8');
  const executed = [], events = [], receipts = [], snapshots = [];
  let rounds = 0, finalChecks = 0;
  const result = await runToolLoop({ protocol, messages: [{ role: 'user', content: 'Summarize the ORION policy.' }], system: '',
    declarations: toolDeclarations(protocol, descriptors), inputBudgetTokens: 8192,
    context: { requestId: randomUUID(), conversationId: randomUUID(), message: 'Summarize the ORION policy.', workspaceRoot: root },
    emit: event => events.push(structuredClone(event)), saveActivity: async receipt => receipts.push(structuredClone(receipt)),
    validateFinal: async () => {
      finalChecks++;
      if (finalChecks === 1) {
        await writeFile(sourcePath, 'Updated policy: review is still pending.', 'utf8');
        return { current: false, invalidSources: [{ sourceRef: 'changed-source-A', reason: 'SOURCE_VERSION_CHANGED' }] };
      }
      return { current: true, invalidSources: [] };
    },
    service: { execute: async (_context, call) => {
      executed.push(call.name);
      if (call.name === 'filesystem.write') { await writeFile(notePath, 'Completed note.', 'utf8'); return { content: '{"saved":true}', status: 'completed' }; }
      assert.equal(call.name, 'knowledge.read'); assert.equal(call.arguments.sourceRef, 'changed-source-A');
      return { content: JSON.stringify({ text: await readFile(sourcePath, 'utf8') }), status: 'completed' };
    } },
    requestTurn: async messages => {
      snapshots.push(structuredClone(messages));
      const round = rounds++;
      let content, calls;
      if (round === 0) { content = 'Recording one note.'; calls = [{ id: 'write-once', name: 'filesystem.write', arguments: { path: 'note.txt' } }]; }
      else if (round === 1) { content = 'Stale draft based on the initial policy.'; calls = []; }
      else if (round === 2) {
        assert.match(JSON.stringify(messages), /KYNXA_SOURCE_VERSION_CHANGED/u);
        assert.match(JSON.stringify(messages), /changed-source-A/u);
        assert.doesNotMatch(JSON.stringify(messages), /unrelated-source-B/u);
        content = 'Reading the affected source again.'; calls = [{ id: 'reread-source', name: 'knowledge.read', arguments: { sourceRef: 'changed-source-A' } }];
      } else { assert.equal(round, 3); content = 'Review remains pending under the updated policy.'; calls = []; }
      return { content, reasoning: '', calls, continuation: continuation(protocol, content, calls) };
    } });
  assert.equal(result.content, 'Review remains pending under the updated policy.');
  assert.equal(finalChecks, 2); assert.equal(rounds, 4);
  assert.deepEqual(executed, ['filesystem.write', 'knowledge.read']);
  assert.equal(await readFile(notePath, 'utf8'), 'Completed note.');
  const draft = result.assistantSegments.find(segment => segment.content.startsWith('Stale draft'));
  assert.equal(draft.phase, 'commentary');
  assert.equal(result.assistantSegments.filter(segment => segment.phase === 'final_answer').length, 1);
  assert.equal(events.filter(event => event.type === 'assistant_segment' && event.segment.phase === 'final_answer').length, 1);
  assert.equal(receipts.filter(receipt => receipt.status === 'completed').length, 2);
  for (const messages of snapshots) assert.deepEqual(...pairedIds(messages, protocol));
});

test('4K contexts reserve output, declarations, system and paired history before admitting further evidence in all protocols', async () => {
  for (const protocol of protocols) {
    const history = [];
    for (let index = 0; index < 8; index++) {
      const userId = randomUUID();
      history.push({ Id: userId, Role: 'user', Status: 'completed', Content: `Read source ${index}.` },
        { Id: randomUUID(), Role: 'assistant', Status: 'completed', ReplyTo: userId, Content: `Observed source ${index}.`,
          ToolActivities: [{ round: 1, toolCallId: `old-call-${index}`, name: 'filesystem.read', arguments: { path: `source-${index}.md` },
            status: 'completed', result: JSON.stringify({ text: `PAIRED_OBSERVATION_${index} ` + 'x'.repeat(1600) }) }] });
    }
    const unchanged = structuredClone(history), declarations = toolDeclarations(protocol, descriptors);
    const schemaTokens = estimateTokens(JSON.stringify(declarations));
    const historyView = new ModelHistoryProjection({ protocol, history, inputBudgetTokens: 4096,
      availableTools: [{ name: 'filesystem.read' }] });
    const built = buildContext({ conversationId: randomUUID(), history, currentMessage: 'Locate current policy.',
      contextWindowTokens: 4096, maxOutputTokens: 1024, reservedInputTokens: schemaTokens,
      additionalSystem: 'Use current permitted evidence. '.repeat(8), historyTurns: historyView.historyTurns,
      projectTurn: turn => historyView.projectTurn(turn), estimateContextMessages: estimateToolMessageTokens });
    assert.ok(built.metrics.omittedTurnCount > 0 && built.metrics.includedTurnCount > 0, 'the fixture exercises whole-turn budget selection');
    assert.equal(built.metrics.outputReserveTokens, 1024);
    assert.ok(built.metrics.estimatedInputTokens + schemaTokens + built.metrics.outputReserveTokens + built.metrics.safetyMarginTokens <= 4096);
    let budget, seen;
    await runToolLoop({ protocol, messages: built.messages, system: built.system, declarations,
      inputBudgetTokens: built.metrics.inputBudgetTokens,
      context: { requestId: randomUUID(), conversationId: randomUUID(), message: 'Locate current policy.' },
      service: { setEvidenceBudget: (_context, maximumTokens) => { budget = maximumTokens; } },
      emit: () => {}, saveActivity: async () => {},
      requestTurn: async messages => { seen = messages; return { content: 'Budget checked.', reasoning: '', calls: [], continuation: [] }; } });
    const remaining = Math.max(0, built.metrics.inputBudgetTokens - estimateToolMessageTokens(seen, built.system) - schemaTokens - 512);
    assert.ok(budget >= 0 && budget <= remaining);
    const [calls, results] = pairedIds(seen, protocol);
    assert.ok(calls.length > 0); assert.deepEqual(calls, results, 'no historical tool result is separated from its assistant call');
    assert.deepEqual(history, unchanged);
  }
});

test('the real tool broker applies remaining evidence capacity per frozen context despite an explicit 32K tool request', async t => {
  const f = await toolFixture(t), observed = [];
  f.service.retrieval = { search: async (context, args) => {
    observed.push({ requestId: context.requestId, maximumTokens: args.maximumTokens });
    return { items: [], strategy: 'synthetic-empty-evidence' };
  } };
  const small = await f.context('full'), large = await f.context('full');
  assert.equal(Object.isFrozen(small), true);
  f.service.setEvidenceBudget(small, 137);
  f.service.setEvidenceBudget(large, 8192);
  const input = { query: 'Synthetic ORION policy', maximumTokens: 32768 };
  for (const context of [small, large, small]) {
    const result = await f.run(context, 'knowledge.search', input);
    assert.equal(result.isError, false, result.content);
  }
  assert.deepEqual(observed.map(item => item.maximumTokens), [137, 8192, 137], 'one context cannot enlarge another context\'s capacity');
  assert.equal(input.maximumTokens, 32768, 'model-supplied input stays separate from the internal budgeted request');
});

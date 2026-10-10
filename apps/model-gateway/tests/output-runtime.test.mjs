import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { randomUUID } from 'node:crypto';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { ModelStore } from '../models/store.mjs';
import { ModelRuntime, completedContext } from '../orchestration/runtime.mjs';
import { responseText } from '../models/protocols.mjs';
import { decodeToolTurn, wireCatalog } from '../models/tool-protocols.mjs';
import { isolateFixtureMcpCatalog } from './tool-fixture.mjs';

function nativeReply(protocol, content, truncated) {
  if (protocol === 'anthropic-messages') return { content: [{ type: 'thinking', thinking: 'visible-thought' }, { type: 'text', text: content }], stop_reason: truncated ? 'max_tokens' : 'end_turn' };
  if (protocol === 'openai-responses') return { status: truncated ? 'incomplete' : 'completed',
    ...(truncated ? { incomplete_details: { reason: 'max_output_tokens' } } : {}),
    output: [{ type: 'reasoning', summary: [{ type: 'summary_text', text: 'visible-thought' }] },
      { type: 'message', content: [{ type: 'output_text', text: content }] }] };
  return { choices: [{ message: { role: 'assistant', content, reasoning_content: 'visible-thought' }, finish_reason: truncated ? 'length' : 'stop' }] };
}

async function fixture(t, protocol, { grammarRejection = false } = {}) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-output-runtime-'));
  const seen = [];
  let truncatedRounds = 0;
  const upstream = createServer(async (request, response) => {
    let source = '';
    for await (const chunk of request) source += chunk;
    seen.push(JSON.parse(source));
    response.setHeader('Content-Type', 'application/json');
    if (grammarRejection && seen.length === 1) {
      response.statusCode = 400;
      response.end(JSON.stringify({ error: { message: 'Failed to initialize samplers: failed to parse grammar' } }));
      return;
    }
    const truncated = truncatedRounds > 0;
    if (truncated) truncatedRounds--;
    response.end(JSON.stringify(nativeReply(protocol, truncated ? 'partial-code' : 'complete-answer', truncated)));
  });
  await new Promise(resolve => upstream.listen(0, '127.0.0.1', resolve));
  const store = new ModelStore({ dataHome: root });
  await store.save({ providerId: 'output-model', displayName: 'Output model', protocol,
    baseUrl: `http://127.0.0.1:${upstream.address().port}/v1`, models: ['model'], contextWindowTokens: 128_000, maxOutputTokens: 32_768 });
  const runtime = new ModelRuntime({ modelStore: store, dataHome: root });
  // This protocol fixture declares its service capability independently of the legacy UI field.
  // 此协议夹具独立声明服务能力，不再把旧 UI 字段当作运行时窗口。
  runtime.localModels.observe = async () => grammarRejection
    ? { backend: 'llama.cpp', source: 'llama-cpp-props', observationOnly: true,
      endpointOrigin: `http://127.0.0.1:${upstream.address().port}`, runtimeContextTokens: 8192 }
    : { backend: 'ollama', runtimeContextTokens: 128_000 };
  isolateFixtureMcpCatalog(runtime.tools);
  t.after(async () => { await runtime.close(); upstream.closeAllConnections(); await new Promise(resolve => upstream.close(resolve)); await rm(root, { recursive: true, force: true }); });
  const input = { conversationId: randomUUID(), requestId: randomUUID(), userMessageId: randomUUID(), provider: 'output-model', model: 'model', message: '现在写代码' };
  return { runtime, seen, input, setTruncated: rounds => { truncatedRounds = rounds; } };
}

for (const protocol of ['openai-completions', 'openai-responses', 'anthropic-messages']) {
  test(`${protocol}: larger independent output is sent upstream and visible reasoning persists`, async t => {
    const f = await fixture(t, protocol);
    assert.equal(await f.runtime.reply(f.input), 'complete-answer');
    assert.equal(f.seen.length, 1, 'a normal stop does not trigger automatic continuation');
    assert.equal(f.seen[0].max_output_tokens ?? f.seen[0].max_tokens, 32_768);
    const history = await f.runtime.conversations.readMessages(f.input.conversationId);
    assert.equal(history.at(-1).Reasoning, 'visible-thought');
    assert.equal(history.at(-1).Status, 'completed');
  });

  for (const tools of [false, true]) test(`${protocol}: ${tools ? 'tool-capable' : 'plain'} JSON length receipt continues the same task and saves the complete body`, async t => {
    const f = await fixture(t, protocol);
    f.setTruncated(1);
    const input = { ...f.input, ...(tools ? { permissionMode: 'ask' } : {}) };
    assert.equal(await f.runtime.reply(input), 'partial-codecomplete-answer');
    assert.equal(f.seen.length, 2);
    assert.match(JSON.stringify(f.seen[1]), /KYNXA_OUTPUT_CONTINUATION/);
    const history = await f.runtime.conversations.readMessages(input.conversationId);
    const assistant = history.at(-1);
    assert.equal(assistant.Content, 'partial-codecomplete-answer');
    assert.equal(assistant.Status, 'completed');
    const modelHistory = await f.runtime.conversations.readModelMessages(input.conversationId);
    assert.equal(modelHistory.at(-1).ModelTranscript.rounds.length, 2);
    assert.ok(assistant.ToolActivities.some(activity => activity.name === 'context.compact' && activity.status === 'completed'));
    assert.equal(assistant.ToolRun.diagnostics.executedToolCalls, 0);
  });

  test(`${protocol}: JSON stream fallback continues and seals one complete final answer`, async t => {
    const f = await fixture(t, protocol);
    f.setTruncated(1);
    const events = [];
    const result = await f.runtime.replyStream({ ...f.input, permissionMode: 'ask' }, event => events.push(event));
    assert.equal(result.content, 'partial-codecomplete-answer');
    assert.equal(f.seen.length, 2);
    assert.equal((await f.runtime.conversations.readMessages(f.input.conversationId)).at(-1).Status, 'completed');
    assert.ok(events.some(event => event.delta === 'partial-code' || event.content === 'partial-code'));
    assert.ok(events.some(event => event.type === 'tool_result' && event.tool.name === 'context.compact'));
    assert.equal(result.assistantSegments.at(-1).phase, 'final_answer');
    assert.equal(result.assistantSegments.at(-1).content, result.content);
  });

  test(`${protocol}: truncated valid-looking tool arguments are refused before dispatch`, () => {
    const catalog = wireCatalog([{ name: 'filesystem.write', description: 'Write', inputSchema: { type: 'object' } }]);
    const name = catalog[0].wireName;
    const args = { path: 'never.txt', content: 'never', reason: 'fixture' };
    const raw = nativeReply(protocol, 'partial-code', true);
    if (protocol === 'anthropic-messages') raw.content.push({ type: 'tool_use', id: 'call1', name, input: args });
    else if (protocol === 'openai-responses') raw.output.push({ type: 'function_call', call_id: 'call1', name, arguments: JSON.stringify(args) });
    else raw.choices[0].message.tool_calls = [{ id: 'call1', type: 'function', function: { name, arguments: JSON.stringify(args) } }];
    assert.throws(() => decodeToolTurn(protocol, raw, catalog), error => error.type === 'interrupted');
    assert.throws(() => responseText(protocol, raw), error => error.type === 'interrupted' && error.content === 'partial-code');
  });
}

test('local llama grammar rejection retries only the rejected step with simpler declarations and honors the actual 8K window', async t => {
  const f = await fixture(t, 'openai-completions', { grammarRejection: true });
  const result = await f.runtime.replyStream({ ...f.input, permissionMode: 'ask' }, () => {});
  assert.equal(result.content, 'complete-answer');
  assert.equal(f.seen.length, 2);
  assert.ok(f.seen[0].tools.length > 0);
  assert.deepEqual(f.seen[1].tools.map(tool => tool.function.name), f.seen[0].tools.map(tool => tool.function.name));
  assert.doesNotMatch(JSON.stringify(f.seen[0].tools), /"maxLength"|"maxItems"/);
  const assistant = (await f.runtime.conversations.readMessages(f.input.conversationId)).at(-1);
  assert.equal(assistant.Status, 'completed');
  assert.equal(assistant.ContextAssembly.localToolRecovery.attempts, 1);
  assert.equal(assistant.ContextAssembly.localToolRecovery.toolOperationsReplayed, 0);
  assert.equal(result.contextUsage.contextWindowTokens, 8192);
});

test('the legacy completed-context helper no longer imposes a 100-message history cutoff', () => {
  const messages = Array.from({ length: 120 }, (_, index) => [
    { Id: `u${index}`, Role: 'user', Content: `q${index}`, Status: 'completed' },
    { Id: `a${index}`, Role: 'assistant', Content: `a${index}`, Status: 'completed' },
  ]).flat();
  assert.equal(completedContext(messages).length, 240);
});

test('local context pressure borrows output reserve without changing the current task or native continuation', async t => {
  const f = await fixture(t, 'openai-completions', { grammarRejection: true });
  const turn = await f.runtime.prepare({ ...f.input, permissionMode: 'ask' }, f.input.conversationId);
  const originalMessages = turn.messages;
  const originalDeclarations = turn.declarations;
  const originalSystem = turn.requestOptions.system;
  const originalInputBudgetTokens = turn.inputBudgetTokens;
  const originalOutputTokens = turn.requestOptions.maxOutputTokens;
  const adapted = await turn.rebalanceContextBudget(5000);
  assert.ok(adapted.inputBudgetTokens >= 5000);
  assert.ok(adapted.inputBudgetTokens > originalInputBudgetTokens);
  assert.ok(turn.requestOptions.maxOutputTokens < originalOutputTokens);
  assert.equal(turn.messages, originalMessages);
  assert.equal(turn.declarations, originalDeclarations);
  assert.equal(turn.requestOptions.system, originalSystem);
  assert.equal(turn.inputBudgetTokens + turn.requestOptions.maxOutputTokens + turn.contextMetrics.safetyMarginTokens, 8192);
  assert.equal(turn.assistant.ContextAssembly.contextPressure.reason, 'current-round-context-pressure');
  assert.equal(await turn.rebalanceContextBudget(5000), null);
  assert.equal(await turn.rebalanceContextBudget(10000), null);
  assert.equal(f.seen.length, 0, 'allocation does not dispatch model calls or replay tools');
  if (turn.toolContext) await f.runtime.tools.releaseContext(turn.toolContext);
});

test('local context pressure respects a provider input ceiling and leaves the previous allocation intact on rejection', async t => {
  const f = await fixture(t, 'openai-completions', { grammarRejection: true });
  const turn = await f.runtime.prepare(f.input, f.input.conversationId);
  turn.contextMetrics.providerMaxInputTokens = 4500;
  const inputBudgetTokens = turn.inputBudgetTokens;
  const outputTokens = turn.requestOptions.maxOutputTokens;
  assert.equal(await turn.rebalanceContextBudget(5000), null);
  assert.equal(turn.inputBudgetTokens, inputBudgetTokens);
  assert.equal(turn.requestOptions.maxOutputTokens, outputTokens);
  assert.equal(turn.contextMetrics.pressureAdjustments, undefined);
});

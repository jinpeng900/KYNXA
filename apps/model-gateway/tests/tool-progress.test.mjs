import assert from 'node:assert/strict';
import { createHash, randomUUID } from 'node:crypto';
import { writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from 'node:test';
import { runToolLoop } from '../tool-loop.mjs';
import { approve, toolFixture } from './tool-fixture.mjs';

const warning = '[KYNXA_NO_PROGRESS_WARNING]';
const finalHint = '[KYNXA_NO_PROGRESS_FINAL]';
const observed = content => ({ content, isError: false, status: 'completed' });
const readCall = (round, index = 0, args = { path: 'source.txt' }, name = 'filesystem.read') =>
  ({ id: `call-${round}-${index}`, name, arguments: args });
const plainText = (content = 'The source was found, but the remaining fact could not be verified.') =>
  ({ content, reasoning: '', calls: [] });

function toolTurn(protocol, calls, content = 'Checking the requested source.') {
  let continuation;
  if (protocol === 'anthropic-messages') continuation = [{ role: 'assistant', content: [
    { type: 'thinking', thinking: 'Public fixture read summary.', signature: 'fixture-signature-' + calls[0].id },
    { type: 'text', text: content }, ...calls.map(call => ({ type: 'tool_use', id: call.id, name: call.name, input: call.arguments }))
  ] }];
  else if (protocol === 'openai-responses') continuation = [
    { type: 'reasoning', id: 'reasoning-' + calls[0].id, encrypted_content: 'fixture-encrypted-' + calls[0].id, summary: [] },
    { type: 'message', role: 'assistant', content: [{ type: 'output_text', text: content }] },
    ...calls.map(call => ({ type: 'function_call', call_id: call.id, name: call.name, arguments: JSON.stringify(call.arguments) }))
  ];
  else continuation = [{ role: 'assistant', content, tool_calls: calls.map(call => ({ id: call.id,
    type: 'function', function: { name: call.name, arguments: JSON.stringify(call.arguments) } })) }];
  return { content, reasoning: '', calls, continuation };
}

function declarations(protocol) {
  const parameters = { type: 'object', properties: { path: { type: 'string' } }, required: ['path'] };
  if (protocol === 'anthropic-messages') return [{ name: 'read_source', description: 'Read the requested source.', input_schema: parameters }];
  if (protocol === 'openai-responses') return [{ type: 'function', name: 'read_source', description: 'Read the requested source.', parameters }];
  return [{ type: 'function', function: { name: 'read_source', description: 'Read the requested source.', parameters } }];
}

function loopFixture({ protocol = 'openai-completions', plan, execute = async () => observed('SOURCE_ALPHA'),
  context = { conversationId: randomUUID(), requestId: randomUUID(), permissionMode: 'full' } } = {}) {
  const requests = [], activities = [], events = [], states = [], executions = [], modelRounds = [];
  const availableTools = declarations(protocol);
  const run = () => runToolLoop({ protocol, context, system: 'Answer only from verified observations.',
    messages: [{ role: 'user', content: 'Read the requested source and verify the fact. Preserve any unresolved blocker.' }],
    declarations: availableTools, declarationsForRound: () => availableTools, inputBudgetTokens: 64000,
    limits: { maxRounds: 20 }, emit: event => events.push(structuredClone(event)),
    saveActivity: async activity => activities.push(structuredClone(activity)),
    saveRunState: async state => states.push(structuredClone(state)),
    saveModelRound: async round => modelRounds.push(structuredClone(round)),
    service: { execute: async (turnContext, call, options) => {
      executions.push(structuredClone(call));
      return execute(call, { context: turnContext, options, number: executions.length });
    } },
    requestTurn: async (messages, tools, signal, emit) => {
      const request = { round: requests.length + 1, messages: structuredClone(messages), tools: structuredClone(tools) };
      requests.push(request);
      const turn = await plan({ ...request, protocol, signal });
      if (turn.content) emit({ type: 'text_delta', delta: turn.content });
      return turn;
    } });
  return { run, requests, activities, events, states, executions, modelRounds };
}

function hintCount(request, marker) {
  return request.messages.filter(message => message.role === 'user' && typeof message.content === 'string'
    && message.content.startsWith(marker)).length;
}

function assertNormalCalls(requests) {
  assert.ok(requests.every(request => request.tools.length > 0), 'legitimate progress keeps its declared tools');
  assert.ok(requests.every(request => hintCount(request, warning) === 0 && hintCount(request, finalHint) === 0),
    'legitimate progress does not receive a stall warning or forced final prompt');
}

for (const protocol of ['openai-completions', 'openai-responses', 'anthropic-messages']) {
  test(`${protocol}: repeated unchanged reads get one warning and one honest final opportunity`, async () => {
    const answer = 'SOURCE_ALPHA was read. The requested latest fact is still unverified.';
    const f = loopFixture({ protocol, plan: async ({ round, tools }) => round === 5
      ? (assert.deepEqual(tools, []), plainText(answer)) : toolTurn(protocol, [readCall(round)]) });
    const result = await f.run();
    assert.equal(result.content, answer, 'the controller preserves the model response rather than manufacturing a conclusion');
    assert.equal(f.requests.length, 5);
    assert.equal(f.executions.length, 4, 'the first observation is progress and the next three rounds are unchanged');
    assert.deepEqual(f.requests.map(request => hintCount(request, warning)), [0, 0, 0, 1, 1]);
    assert.deepEqual(f.requests.map(request => hintCount(request, finalHint)), [0, 0, 0, 0, 1]);
    assert.ok(f.requests.slice(0, 4).every(request => request.tools.length > 0));
    assert.equal(f.activities.filter(activity => activity.status === 'completed').length, 4);
    assert.deepEqual(result.assistantSegments.map(segment => segment.phase), ['commentary', 'commentary', 'commentary', 'commentary', 'final_answer']);
    assert.ok(!JSON.stringify(result.assistantSegments).includes(warning) && !result.content.includes(finalHint),
      'controller instructions do not become fabricated assistant prose');
    const finalHistory = f.requests.at(-1).messages;
    for (const call of f.executions) {
      if (protocol === 'anthropic-messages') {
        const assistant = finalHistory.find(message => message.role === 'assistant'
          && message.content?.some?.(block => block.type === 'tool_use' && block.id === call.id));
        assert.deepEqual(assistant.content.find(block => block.type === 'tool_use'),
          { type: 'tool_use', id: call.id, name: call.name, input: call.arguments });
        assert.equal(assistant.content.find(block => block.type === 'thinking').signature, 'fixture-signature-' + call.id);
        const observation = finalHistory.flatMap(message => Array.isArray(message.content) ? message.content : [])
          .find(block => block.type === 'tool_result' && block.tool_use_id === call.id);
        assert.deepEqual(observation, { type: 'tool_result', tool_use_id: call.id, content: 'SOURCE_ALPHA', is_error: false });
      } else if (protocol === 'openai-responses') {
        assert.deepEqual(finalHistory.find(item => item.type === 'function_call' && item.call_id === call.id),
          { type: 'function_call', call_id: call.id, name: call.name, arguments: JSON.stringify(call.arguments) });
        assert.equal(finalHistory.find(item => item.type === 'reasoning' && item.id === 'reasoning-' + call.id).encrypted_content,
          'fixture-encrypted-' + call.id);
        assert.deepEqual(finalHistory.find(item => item.type === 'function_call_output' && item.call_id === call.id),
          { type: 'function_call_output', call_id: call.id, output: 'SOURCE_ALPHA' });
      } else {
        const assistant = finalHistory.find(message => message.role === 'assistant' && message.tool_calls?.some(item => item.id === call.id));
        assert.deepEqual(assistant.tool_calls.find(item => item.id === call.id), { id: call.id, type: 'function',
          function: { name: call.name, arguments: JSON.stringify(call.arguments) } });
        assert.deepEqual(finalHistory.find(message => message.role === 'tool' && message.tool_call_id === call.id),
          { role: 'tool', tool_call_id: call.id, content: 'SOURCE_ALPHA' });
      }
    }
    for (const [request, marker] of [[f.requests[3], warning], [f.requests[4], finalHint]]) {
      assert.equal(request.messages.at(-1).role, 'user');
      assert.ok(request.messages.at(-1).content.startsWith(marker), 'guidance is appended after the complete call/result exchange');
      const preceding = request.messages.at(-2), id = 'call-' + (request.round - 1) + '-0';
      if (protocol === 'anthropic-messages') {
        assert.equal(preceding.role, 'user');
        assert.ok(preceding.content.some(block => block.type === 'tool_result' && block.tool_use_id === id));
      } else if (protocol === 'openai-responses') assert.equal(preceding.call_id, id);
      else assert.equal(preceding.tool_call_id, id);
    }
    assert.ok(!JSON.stringify(result.assistantSegments).includes('fixture-signature-')
      && !JSON.stringify(result.assistantSegments).includes('fixture-encrypted-'), 'private continuation fields remain outside public assistant stages');
    assert.equal(f.states.at(-1).phase, 'finalizing');
    assert.equal(f.states.at(-1).diagnostics.modelCalls, 5);
    assert.equal(f.states.at(-1).diagnostics.executedToolCalls, 4);
    assert.equal(f.states.at(-1).diagnostics.reusedToolCalls, 0);
    assert.equal(f.states.at(-1).diagnostics.noProgressRounds, 3);
  });
}

test('a tool call during the final opportunity is interrupted before a fifth effect', async () => {
  const f = loopFixture({ plan: async ({ protocol, round }) => toolTurn(protocol, [readCall(round)]) });
  await assert.rejects(f.run(), { code: 'TOOL_RUN_NO_PROGRESS', type: 'interrupted' });
  assert.equal(f.requests.length, 5);
  assert.deepEqual(f.requests.at(-1).tools, []);
  assert.equal(f.executions.length, 4);
  assert.equal(f.activities.filter(activity => activity.status === 'completed').length, 4);
  assert.ok(!f.activities.some(activity => activity.toolCallId === 'call-5-0'), 'the rejected continuation has no running or completed execution record');
  assert.equal(f.states.at(-1).phase, 'interrupted');
  assert.equal(f.states.at(-1).code, 'TOOL_RUN_NO_PROGRESS');
});

test('multiple identical reads in one model turn count as one unchanged round', async () => {
  const f = loopFixture({ plan: async ({ protocol, round }) => round === 5 ? plainText()
    : toolTurn(protocol, Array.from({ length: 4 }, (_, index) => readCall(round, index))) });
  await f.run();
  assert.equal(f.requests.length, 5);
  assert.equal(f.executions.length, 16);
  assert.deepEqual(f.requests.map(request => hintCount(request, warning)), [0, 0, 0, 1, 1]);
  assert.deepEqual(f.requests.at(-1).tools, []);
  assert.equal(f.activities.filter(activity => activity.status === 'completed').length, 16);
});

test('new information after a warning resets the unchanged-round streak', async () => {
  const f = loopFixture({ execute: async (_call, { number }) => observed(number < 4 ? 'SOURCE_ALPHA' : 'SOURCE_BETA'),
    plan: async ({ protocol, round }) => round === 6 ? plainText('New information from SOURCE_BETA is now available.')
      : toolTurn(protocol, [readCall(round)]) });
  const result = await f.run();
  assert.equal(result.content, 'New information from SOURCE_BETA is now available.');
  assert.equal(f.executions.length, 5);
  assert.ok(f.requests.every(request => request.tools.length > 0), 'a useful new observation avoids the forced-final round');
  assert.deepEqual(f.requests.map(request => hintCount(request, warning)), [0, 0, 0, 1, 1, 1]);
  assert.ok(f.requests.every(request => hintCount(request, finalHint) === 0));
});

const progressingCases = [
  { label: 'paginated reads with different offsets', name: 'tool.result.read',
    args: round => ({ id: 'owned-archived-result', offset: (round - 1) * 100, limit: 100 }), result: () => observed('Repeated headings on this page.') },
  { label: 'the same path returning changing content', name: 'filesystem.read', args: () => ({ path: 'source.txt' }),
    result: round => observed('SOURCE_VERSION_' + round) },
  { label: 'error results that may need a recovery attempt', name: 'filesystem.read', args: () => ({ path: 'source.txt' }),
    result: () => ({ content: 'Source unavailable.', isError: true, status: 'error', code: 'FIXTURE_SOURCE_UNAVAILABLE' }) },
  { label: 'unknown execution outcomes', name: 'filesystem.read', args: () => ({ path: 'source.txt' }),
    result: () => ({ content: 'Result not confirmed.', isError: false, status: 'unknown' }) },
  { label: 'stateful file writes', name: 'filesystem.write', args: () => ({ path: 'output.txt', content: 'requested', expectedHash: null }),
    result: () => observed('Requested write finished.') },
  { label: 'unknown MCP operations despite read-only annotations', name: 'mcp.fixture.custom_read', args: () => ({ value: 'source' }),
    result: () => observed('A custom operation returned.') }
];
for (const example of progressingCases) {
  test(`${example.label} do not trigger the read-only no-progress guard`, async () => {
    const f = loopFixture({ execute: async (_call, { number }) => example.result(number),
      plan: async ({ protocol, round }) => round === 6 ? plainText('Completed the requested sequence.')
        : toolTurn(protocol, [{ ...readCall(round, 0, example.args(round), example.name), annotations: { readOnlyHint: true } }]) });
    assert.equal((await f.run()).content, 'Completed the requested sequence.');
    assert.equal(f.executions.length, 5);
    assertNormalCalls(f.requests);
  });
}

test('business argument key order and changing approval reasons do not manufacture progress', async () => {
  const f = loopFixture({ plan: async ({ protocol, round }) => round === 5 ? plainText()
    : toolTurn(protocol, [readCall(round, 0, {
      arguments: round % 2 ? { url: 'https://fixture.invalid/source', max_length: 1000 }
        : { max_length: 1000, url: 'https://fixture.invalid/source' },
      policy: { reason: 'Read the requested public source in round ' + round }
    }, 'mcp.fetch.fetch')]) });
  await f.run();
  assert.equal(f.executions.length, 4);
  assert.deepEqual(f.requests.map(request => hintCount(request, warning)), [0, 0, 0, 1, 1]);
  assert.deepEqual(f.requests.at(-1).tools, []);
});

test('fresh archive references do not turn an unchanged public observation into new evidence', async () => {
  const observationHash = createHash('sha256').update('Same canonical source.').digest('hex');
  const f = loopFixture({ execute: async () => {
    const resultRef = { id: randomUUID(), bytes: 256, sha256: observationHash };
    return { ...observed(JSON.stringify({ preview: 'Same canonical source.', resultRef })), observationHash, resultRef };
  }, plan: async ({ protocol, round }) => round === 5 ? plainText() : toolTurn(protocol, [readCall(round)]) });
  await f.run();
  const completed = f.activities.filter(activity => activity.status === 'completed');
  assert.equal(new Set(completed.map(activity => activity.resultRef.id)).size, 4, 'each execution keeps its own receipt identity');
  assert.deepEqual(f.requests.map(request => hintCount(request, warning)), [0, 0, 0, 1, 1]);
  assert.deepEqual(f.requests.at(-1).tools, []);
});

test('progress tracking starts fresh for a new request in the same conversation', async () => {
  const conversationId = randomUUID();
  const requests = [];
  for (let index = 0; index < 2; index++) {
    const f = loopFixture({ context: { conversationId, requestId: randomUUID(), permissionMode: 'full' },
      plan: async ({ protocol, round }) => round === 5 ? plainText() : toolTurn(protocol, [readCall(round)]) });
    await f.run();
    requests.push(f.requests);
  }
  for (const run of requests) {
    assert.equal(run.length, 5);
    assert.deepEqual(run.map(request => hintCount(request, warning)), [0, 0, 0, 1, 1]);
    assert.deepEqual(run.at(-1).tools, []);
  }
});

test('a genuine file changed outside the agent is reread and becomes new evidence', async t => {
  const f = await toolFixture(t), context = await f.context('full');
  const path = join(f.workspace, 'source.txt');
  await writeFile(path, 'SOURCE_ALPHA');
  const results = [];
  const loop = loopFixture({ context, execute: async (call, { options, number }) => {
    if (number === 2) await writeFile(path, 'SOURCE_BETA');
    const result = await f.service.execute(context, call, options);
    results.push(result);
    return result;
  }, plan: async ({ protocol, round }) => round === 4 ? plainText('SOURCE_BETA is the current file content.')
    : toolTurn(protocol, [readCall(round)]) });
  assert.equal((await loop.run()).content, 'SOURCE_BETA is the current file content.');
  assert.equal(results.length, 3);
  assert.equal(results[0].isError, false); assert.ok(results[0].content.includes('SOURCE_ALPHA'));
  assert.equal(results[1].isError, false); assert.ok(results[1].content.includes('SOURCE_BETA'));
  assert.ok(results[2].content.includes('SOURCE_BETA'));
  assertNormalCalls(loop.requests);
});

test('configuration invalidation stops before another model turn and preserves the returned result', async () => {
  const f = loopFixture({ execute: async () => ({ content: 'The configured tools changed.', isError: true,
    status: 'error', code: 'AGENT_CONFIG_CHANGED' }), plan: async ({ protocol, round }) => toolTurn(protocol, [readCall(round)]) });
  await assert.rejects(f.run(), { code: 'AGENT_CONFIG_CHANGED', type: 'interrupted' });
  assert.equal(f.requests.length, 1); assert.equal(f.executions.length, 1);
  assert.equal(f.activities.at(-1).code, 'AGENT_CONFIG_CHANGED');
  assert.equal(f.activities.at(-1).status, 'error');
  assert.equal(f.events.filter(event => event.type === 'tool_result').length, 1);
  assert.equal(f.states.at(-1).code, 'AGENT_CONFIG_CHANGED');
});

test('real approval wait and model/tool work are diagnosed separately without copying arguments', { timeout: 5000 }, async t => {
  const f = await toolFixture(t), context = await f.context('ask');
  const privateMarker = 'PRIVATE_FIXTURE_TOOL_INPUT';
  const loop = loopFixture({ context, execute: async (call, { options }) => {
    let ready;
    const event = new Promise(resolve => { ready = resolve; });
    const pending = f.service.execute(context, call, { ...options, interactive: true, emit: value => {
      options.emit(value); ready(value);
    } });
    if (call.name === 'filesystem.write') {
      const approval = await event;
      assert.equal(approval.type, 'approval_required');
      await new Promise(resolve => setTimeout(resolve, 12));
      approve(f.service, context, approval.tool);
    }
    return await pending;
  }, plan: async ({ protocol, round }) => {
    await new Promise(resolve => setTimeout(resolve, 3));
    if (round === 3) return plainText('The approved file was written and verified.');
    const call = round === 1 ? readCall(round, 0, { path: 'timed.txt', content: privateMarker, expectedHash: null }, 'filesystem.write')
      : readCall(round, 0, { path: 'timed.txt' });
    return toolTurn(protocol, [call]);
  } });
  assert.equal((await loop.run()).content, 'The approved file was written and verified.');
  const diagnostics = loop.states.at(-1).diagnostics;
  for (const field of ['totalModelMs', 'totalToolMs', 'totalApprovalWaitMs', 'modelCalls',
    'executedToolCalls', 'reusedToolCalls', 'noProgressRounds']) assert.ok(Number.isSafeInteger(diagnostics[field]) && diagnostics[field] >= 0);
  assert.ok(diagnostics.totalModelMs > 0 && diagnostics.totalToolMs > 0 && diagnostics.totalApprovalWaitMs > 0);
  assert.equal(diagnostics.modelCalls, 3); assert.equal(diagnostics.executedToolCalls, 2);
  assert.equal(diagnostics.reusedToolCalls, 0); assert.equal(diagnostics.noProgressRounds, 0);
  assert.equal(diagnostics.modelRounds.length, 3); assert.equal(diagnostics.toolCallsTiming.length, 2);
  assert.equal(diagnostics.totalModelMs, diagnostics.modelRounds.reduce((sum, item) => sum + item.durationMs, 0));
  assert.equal(diagnostics.totalApprovalWaitMs, diagnostics.toolCallsTiming.reduce((sum, item) => sum + item.approvalMs, 0));
  assert.equal(diagnostics.totalToolMs, diagnostics.toolCallsTiming.reduce((sum, item) => sum + item.durationMs - item.approvalMs, 0));
  assert.ok(!JSON.stringify(diagnostics).includes(privateMarker), 'timing records contain public measurements, not tool inputs or observations');
});

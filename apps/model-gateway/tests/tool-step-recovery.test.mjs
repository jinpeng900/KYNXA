import assert from 'node:assert/strict';
import { test } from 'node:test';
import { mkdtemp, writeFile, appendFile, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { runToolLoop } from '../orchestration/tool-loop.mjs';
import { readToolStream } from '../models/tool-streaming.mjs';
import { ToolCallDecodeFailure, MAX_TURN_TOOL_CALLS } from '../models/tool-call-validation.mjs';
import { decodeToolTurn, toolDeclarations, wireCatalog } from '../models/tool-protocols.mjs';
import { ToolRecoveryLedger } from '../orchestration/tool-recovery.mjs';

const catalog = wireCatalog([{ name: 'filesystem.write', description: 'Write fixture', inputSchema: { type: 'object' } }]);
function rawTurn(protocol, id, argumentsValue) {
  const name = catalog[0].wireName;
  if (protocol === 'anthropic-messages') return { stop_reason: 'tool_use', content: [{ type: 'tool_use', id, name, input: argumentsValue }] };
  if (protocol === 'openai-responses') return { status: 'completed', output: [{ type: 'function_call', call_id: id, name, arguments: JSON.stringify(argumentsValue) }] };
  return { choices: [{ finish_reason: 'tool_calls', message: { content: '', tool_calls: [{ id, type: 'function', function: { name, arguments: JSON.stringify(argumentsValue) } }] } }] };
}
const finalTurn = content => ({ content, reasoning: '', calls: [], continuation: [] });
function options(requestTurn, service, extra = {}) {
  return { protocol: 'openai-completions', context: { message: '创建文件并核验', conversationId: 'fixture' },
    messages: [{ role: 'user', content: 'Create the fixture once.' }], system: '', inputBudgetTokens: 32000,
    declarations: [], emit: () => {}, saveActivity: async () => {}, requestTurn, service, ...extra };
}

test('each round budgets, dispatches and saves the refreshed system without replaying a completed action', async () => {
  const savedSystems = [], dispatchedSystems = [];
  let rounds = 0, effects = 0;
  const result = await runToolLoop(options(async (_messages, _tools, _signal, _receive, _catalog, system) => {
    dispatchedSystems.push(system);
    rounds++;
    if (rounds === 2) return finalTurn('The current source has been checked.');
    return decodeToolTurn('openai-completions', rawTurn('openai-completions', 'once', { path: 'fixture', content: 'once' }), catalog);
  }, { execute: async () => { effects++; return { status: 'completed', isError: false, content: 'saved once' }; } }, {
    system: 'old confirmed memory', declarations: toolDeclarations('openai-completions', catalog), catalogForRound: () => catalog,
    systemForRound: async () => rounds ? 'current confirmed memory' : 'old confirmed memory',
    saveModelRound: async step => savedSystems.push(step.system)
  }));
  assert.equal(effects, 1);
  assert.equal(result.content, 'The current source has been checked.');
  assert.deepEqual(dispatchedSystems, ['old confirmed memory', 'current confirmed memory']);
  assert.deepEqual(savedSystems, dispatchedSystems);
});

for (const protocol of ['openai-completions', 'openai-responses', 'anthropic-messages']) {
  test(`${protocol}: failed model step repairs without replaying a completed write or losing tool pairs`, async t => {
    const root = await mkdtemp(join(tmpdir(), 'kynxa-step-recovery-'));
    t.after(() => rm(root, { recursive: true, force: true }));
    const file = join(root, 'once.txt'), receipts = [], snapshots = [];
    let rounds = 0, effects = 0;
    const argumentsValue = { path: 'once.txt', content: 'preserved', expectedHash: null };
    const result = await runToolLoop(options(async messages => {
      rounds++;
      if (rounds === 2) throw new ToolCallDecodeFailure('工具调用序号无效。', 'MODEL_TOOL_INDEX_INVALID');
      if (rounds === 3) {
        assert.match(JSON.stringify(messages), /KYNXA_TOOL_STEP_REPAIR/);
        assert.match(JSON.stringify(messages), /first_write/);
      }
      return rounds === 4 ? finalTurn('已创建一次并保留结果。') : decodeToolTurn(protocol,
        rawTurn(protocol, rounds === 1 ? 'first_write' : 'replacement_id', argumentsValue), catalog);
    }, { execute: async () => { effects++; await writeFile(file, 'preserved', { flag: 'wx' });
      return { content: '{"written":true}', status: 'completed', isError: false }; } }, {
      protocol, declarations: toolDeclarations(protocol, catalog), catalogForRound: () => catalog,
      saveActivity: async receipt => receipts.push(receipt), saveModelRound: async step => snapshots.push(step)
    }));
    assert.equal(effects, 1);
    assert.equal(await readFile(file, 'utf8'), 'preserved');
    assert.equal(result.content, '已创建一次并保留结果。');
    assert.equal(receipts.at(-1).reused, true);
    assert.equal(snapshots.length, 3);
    assert.deepEqual(result.recovery.events.map(event => event.code), ['MODEL_TOOL_INDEX_INVALID']);
    assert.equal(result.assistantSegments[1].status, 'interrupted');
    assert.equal(result.assistantSegments.at(-1).phase, 'final_answer');
  });
}

test('repeated decoder errors make one repair and one no-tool summary; no partial arguments execute', async () => {
  let rounds = 0, effects = 0;
  const declarations = [{ type: 'function', function: { name: 'fixture' } }];
  const result = await runToolLoop(options(async (_messages, tools) => {
    rounds++;
    if (rounds === 3) { assert.equal(tools.length, 0); return finalTurn('参数仍不完整，尚未执行操作。'); }
    throw new ToolCallDecodeFailure('参数不完整', 'MODEL_TOOL_ARGUMENT_INVALID');
  }, { execute: async () => { effects++; } }, { declarations }));
  assert.equal(rounds, 3); assert.equal(effects, 0);
  assert.equal(result.completionStatus, 'interrupted');
  assert.match(result.content, /尚未执行/);
});

test('the last failed summary and transport output are counted before the interrupted state is saved', async () => {
  let rounds = 0, state;
  const result = await runToolLoop(options(async () => {
    rounds++;
    throw Object.assign(new ToolCallDecodeFailure('Unusable response', 'MODEL_TOOL_ARGUMENT_INVALID'),
      { estimatedGeneratedTokens: 123 });
  }, {}, { saveRunState: async value => { state = structuredClone(value); } }));
  assert.equal(rounds, 3);
  assert.equal(result.completionStatus, 'interrupted');
  assert.equal(state.diagnostics.modelCalls, rounds);
  assert.equal(state.estimatedGeneratedTokens, 369);
  assert.equal(state.diagnostics.modelRounds.at(-1).round, rounds);
  rounds = 0;
  await assert.rejects(runToolLoop(options(async () => {
    rounds++;
    throw Object.assign(new Error('Transport lost after partial generation'), { estimatedGeneratedTokens: 251 });
  }, {}, { saveRunState: async value => { state = structuredClone(value); } })), /Transport lost/);
  assert.equal(state.diagnostics.modelCalls, 1);
  assert.equal(state.estimatedGeneratedTokens, 251);
});

test('resource admission failure retains its category without counting a model request that was never dispatched', async () => {
  let state;
  await assert.rejects(runToolLoop(options(async () => assert.fail('resource denial cannot invoke the model'),
    { resources: { acquire: async () => ({ status: 'denied', reason: 'RESOURCE_PRESSURE' }) } },
    { saveRunState: async value => { state = structuredClone(value); } })), { code: 'RESOURCE_PRESSURE' });
  assert.equal(state.diagnostics.modelCalls, 0);
  assert.equal(state.estimatedGeneratedTokens, 0);
});

test('decoded generation is counted even when journaling fails before dispatch', async () => {
  let state;
  await assert.rejects(runToolLoop(options(async () => finalTurn('Produced before persistence failed. '.repeat(200)),
    { execute: async () => assert.fail('unsaved steps cannot execute') }, {
      saveModelRound: async () => { throw new Error('synthetic journal failure'); },
      saveRunState: async value => { state = structuredClone(value); }
    })), /synthetic journal failure/);
  assert.equal(state.diagnostics.modelCalls, 1);
  assert.ok(state.estimatedGeneratedTokens > 1024);
});

test('non-streaming incomplete Responses also charges complete-looking tool arguments without executing', async () => {
  let state;
  await assert.rejects(runToolLoop(options(async () => decodeToolTurn('openai-responses', { status: 'incomplete', incomplete_details: { reason: 'stop' },
    output: [{ type: 'function_call', call_id: 'incomplete', name: catalog[0].wireName,
      arguments: JSON.stringify({ content: 'nonstream argument '.repeat(1000) }) }] }, catalog),
    { execute: async () => assert.fail('incomplete model steps cannot execute') }, {
      saveRunState: async value => { state = structuredClone(value); }
    })), /未完整结束/);
  assert.equal(state.diagnostics.modelCalls, 1);
  assert.ok(state.estimatedGeneratedTokens > 1024);
});

test('recovery summaries still verify revoked sources and preserve a normal answer if verification is unavailable', async () => {
  for (const unavailable of [false, true]) {
    let checked = 0;
    const result = await runToolLoop(options(async () => {
      throw new ToolCallDecodeFailure('unusable output', 'MODEL_TOOL_ARGUMENT_INVALID');
    }, {}, { validateFinal: async () => {
      checked++;
      if (unavailable) throw Object.assign(new Error('scope changed'), { code: 'RETRIEVAL_SCOPE_CHANGED' });
      return { current: false, invalidSources: [{ sourceId: 'revoked-fixture' }] };
    } }));
    assert.equal(checked, 1);
    assert.equal(result.completionStatus, 'interrupted');
    assert.match(result.content, /撤销或无法核验/);
    assert.match(result.content, /记录已保留/);
  }
});

test('uncertain effects have a bounded observation stage and a normal partial answer without replay', async () => {
  let rounds = 0, effects = 0;
  const result = await runToolLoop(options(async (_messages, declarations) => {
    rounds++;
    if (!declarations.length) return finalTurn('The original dispatch remains unconfirmed; saved results are preserved.');
    const call = rounds === 1 ? { id: 'unknown-start', name: 'terminal.host.start', arguments: {} }
      : { id: `read-${rounds}`, name: 'filesystem.read', arguments: { path: `state-${rounds}.txt` } };
    return { content: '', reasoning: '', calls: [call], continuation: [] };
  }, { execute: async (_context, call) => {
    if (call.name === 'terminal.host.start') { effects++; return { content: 'unconfirmed', status: 'unknown', isError: true }; }
    return { content: `New observation ${rounds}`, status: 'completed', isError: false };
  } }, { declarations: [{ fixture: true }] }));
  assert.equal(effects, 1);
  assert.equal(rounds, 10);
  assert.equal(result.completionStatus, 'interrupted');
  assert.equal(result.taskCompletion.state, 'execution-unconfirmed');
  assert.match(result.content, /preserved/);
  assert.equal(result.recovery.events.at(-1).code, 'TOOL_EFFECT_UNCONFIRMED');
});

test('broker-confirmed dispatch resumes independent work while protecting the verified original effect', async () => {
  const original = { id: 'start', name: 'terminal.host.start', arguments: { command: 'once' } };
  let rounds = 0;
  const executions = [], receipts = [];
  const result = await runToolLoop(options(async () => {
    rounds++;
    const call = rounds === 1 ? original : rounds === 2
      ? { id: 'read', name: 'terminal.host.read', arguments: { jobId: 'owned-job' } } : rounds === 3
        ? { ...original, id: 'repeated-start' } : rounds === 4
          ? { id: 'independent', name: 'filesystem.write', arguments: { path: 'next.txt' } } : null;
    return call ? { content: '', reasoning: '', calls: [call], continuation: [] } : finalTurn('Started once; independent work is finished.');
  }, { execute: async (_context, call) => {
    executions.push(call.name);
    return call.id === 'start' ? { content: 'lost acknowledgement', status: 'unknown', isError: true }
      : { content: 'observed or saved', status: 'completed', isError: false };
  }, verifyUnknownEffects: async (_context, records, observation) => {
    assert.equal(records[0].call.id, original.id);
    assert.equal(observation.call.id, 'read');
    return [{ toolCallId: original.id, observationToolCallId: 'read', outcome: 'dispatch-confirmed',
      result: { content: 'broker verified start only', status: 'completed', isError: false } }];
  } }, { saveActivity: async receipt => receipts.push(receipt) }));
  assert.deepEqual(executions, ['terminal.host.start', 'terminal.host.read', 'filesystem.write']);
  assert.equal(receipts.find(item => item.toolCallId === 'repeated-start' && item.status === 'completed').reused, true);
  assert.equal(result.recovery.unknownEffects, 0);
  assert.equal(result.recovery.verifiedEffects.length, 1);
  assert.equal(result.completionStatus, undefined);
});

test('an uncertain final-round dispatch still returns saved partial results without obtaining a fresh budget', async () => {
  let modelCalls = 0;
  const result = await runToolLoop(options(async () => {
    modelCalls++;
    return { content: '', reasoning: '', calls: [{ id: 'last-dispatch', name: 'terminal.host.start', arguments: {} }], continuation: [] };
  }, { execute: async () => ({ content: 'unknown', isError: true, status: 'unknown' }) }, { limits: { maxRounds: 1 } }));
  assert.equal(modelCalls, 1);
  assert.equal(result.completionStatus, 'interrupted');
  assert.match(result.content, /记录已保留/);
});

test('unknown side effects allow state observation but never replay or certify completion', async () => {
  let rounds = 0, executions = 0;
  const calls = [{ id: 'start', name: 'terminal.host.start', arguments: {} },
    { id: 'read', name: 'terminal.host.read', arguments: { jobId: 'fixture' } }];
  const result = await runToolLoop(options(async () => {
    rounds++;
    if (rounds > 2) return finalTurn('启动结果仍待确认，已查询状态。');
    const call = calls[rounds - 1];
    return { content: '', reasoning: '', calls: [call], continuation: [{ role: 'assistant', content: '',
      tool_calls: [{ id: call.id, type: 'function', function: { name: call.name, arguments: JSON.stringify(call.arguments) } }] }] };
  }, { execute: async (_context, call) => {
    executions++;
    return call.id === 'start' ? { content: 'unconfirmed', status: 'unknown', isError: true }
      : { content: 'still running', status: 'completed', isError: false };
  } }));
  assert.equal(executions, 2); assert.equal(rounds, 3);
  assert.equal(result.completionStatus, 'interrupted');
  assert.equal(result.taskCompletion.state, 'execution-unconfirmed');
});

test('pre-failure effects remain protected after a recovery observation while later effects keep normal semantics', async t => {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-delayed-replay-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const file = join(root, 'effects.txt');
  let rounds = 0, effects = 0;
  const receipts = [], callTurn = (id, name, args) => ({ content: '', reasoning: '',
    calls: [{ id, name, arguments: args }], continuation: [] });
  const args = { command: 'Add-Content effects.txt effect', reason: 'Append once' };
  await runToolLoop(options(async () => {
    rounds++;
    if (rounds === 2) throw new ToolCallDecodeFailure('Invalid ordinal', 'MODEL_TOOL_INDEX_INVALID');
    if (rounds === 3) return callTurn('verify', 'filesystem.read', { path: 'effects.txt' });
    if (rounds === 5) return finalTurn('Preserved the original effect.');
    return callTurn(rounds === 1 ? 'original' : 'after-observation', 'terminal.host.run',
      { ...args, reason: rounds === 1 ? args.reason : 'Recover the same requested operation' });
  }, { execute: async (_context, call) => {
    if (call.name === 'terminal.host.run') { effects++; await appendFile(file, 'effect\n'); }
    return { content: '{"ok":true}', status: 'completed', isError: false };
  } }, { saveActivity: async receipt => receipts.push(receipt) }));
  assert.equal(effects, 1);
  assert.equal(await readFile(file, 'utf8'), 'effect\n');
  assert.equal(receipts.at(-1).reused, true);
  assert.equal(receipts.at(-1).recoveryOfToolCallId, 'original');
  const ledger = new ToolRecoveryLedger();
  const first = { id: 'first', name: 'terminal.host.run', arguments: { command: 'first' } };
  const later = { id: 'later', name: 'terminal.host.run', arguments: { command: 'later' } };
  ledger.observe(first, { status: 'completed', isError: false });
  assert.equal(ledger.previous(first), undefined, 'ordinary execution before a decoder failure is not deduplicated');
  ledger.protectCompletedEffects(); ledger.observe(later, { status: 'completed', isError: false });
  ledger.protectCompletedEffects();
  assert.equal(ledger.previous(first).call.id, first.id);
  assert.equal(ledger.previous(later), undefined, 'post-failure effects do not expand the frozen protection set');
});

test('failed SSE decoding still charges emitted output against the original total generation budget', async () => {
  const frame = item => `data: ${JSON.stringify(item)}\n\n`;
  const content = frame({ choices: [{ index: 0, delta: { content: 'generated content '.repeat(1000) } }] }) +
    frame({ choices: [{ index: 0, delta: { tool_calls: [{ index: -1, id: 'invalid',
      function: { name: catalog[0].wireName, arguments: '{}' } }] } }] });
  let modelCalls = 0, effects = 0, state;
  await assert.rejects(runToolLoop(options(async (_messages, _tools, _signal, emit) => {
    modelCalls++;
    if (modelCalls > 1) return finalTurn('A small recovery answer.');
    return readToolStream(new Response(content, { headers: { 'content-type': 'text/event-stream' } }),
      'openai-completions', catalog, emit);
  }, { execute: async () => { effects++; } }, { limits: { maxGeneratedTokens: 1024 },
    saveRunState: async value => { state = value; } })), { code: 'TOOL_RUN_OUTPUT_LIMIT' });
  assert.equal(effects, 0);
  assert.equal(modelCalls, 1, 'a decoder retry cannot obtain a fresh generation budget');
  assert.ok(state.estimatedGeneratedTokens > 1024);
});

test('unknown desktop effects permit broker-checked computer.read without allowing new effects', async () => {
  const catalog = wireCatalog([
    { name: 'computer.launch', description: 'Launch an application', inputSchema: { type: 'object' } },
    { name: 'computer.read', description: 'Read current window contents', inputSchema: { type: 'object' } },
  ]);
  let rounds = 0;
  const executions = [], calls = [
    { id: 'launch', name: 'computer.launch', arguments: { path: 'fixture.exe' } },
    { id: 'observe', name: 'computer.read', arguments: { windowId: 'fixture-window' } },
  ];
  const result = await runToolLoop(options(async (_messages, _declarations, _signal, _emit, currentCatalog) => {
    rounds++;
    if (rounds > 1) {
      assert.ok(currentCatalog.some(tool => tool.name === 'computer.read'));
      assert.ok(!currentCatalog.some(tool => tool.name === 'computer.launch'));
    }
    if (rounds > 2) return finalTurn('Observed the window; launch outcome remains unconfirmed.');
    return { content: '', reasoning: '', calls: [calls[rounds - 1]], continuation: [] };
  }, { execute: async (_context, call) => {
    executions.push(call.name);
    return call.name === 'computer.launch' ? { content: 'Unknown launch outcome', status: 'unknown', isError: true }
      : { content: 'Current visible window text', status: 'completed', isError: false };
  } }, { catalogForRound: () => catalog }));
  assert.deepEqual(executions, ['computer.launch', 'computer.read']);
  assert.equal(result.completionStatus, 'interrupted');
  assert.equal(result.taskCompletion.state, 'execution-unconfirmed');
});

test('unknown effects stay unconfirmed when repeated undeclared observation names exhaust finalization', async () => {
  const catalog = wireCatalog([
    { name: 'terminal.host.start', description: 'Start fixture job', inputSchema: { type: 'object' } },
    { name: 'filesystem.read', description: 'Read fixture state', inputSchema: { type: 'object' } },
  ]);
  let rounds = 0, effects = 0;
  const result = await runToolLoop(options(async (_messages, _declarations, _signal, _emit, currentCatalog) => {
    rounds++;
    const name = rounds === 1 ? catalog[0].wireName : 'filesystem.read';
    // Canonical names are not wire declarations; even a no-tools request may receive this provider error.
    // 正式名称不等于供应商声明别名；供应商可能在无工具请求中仍返回这种错误调用。
    return decodeToolTurn('openai-completions', { choices: [{ finish_reason: 'tool_calls', message: {
      content: '', tool_calls: [{ id: `unknown-${rounds}`, function: { name, arguments: '{}' } }] } }] }, currentCatalog);
  }, { execute: async () => { effects++; return { content: 'Unknown dispatched result', status: 'unknown', isError: true }; } },
  { catalogForRound: () => catalog }));
  assert.equal(effects, 1);
  assert.equal(rounds, 4);
  assert.equal(result.completionStatus, 'interrupted');
  assert.equal(result.taskCompletion.state, 'execution-unconfirmed');
});

test('cancellation at a successful final-model boundary keeps prior receipts and cannot complete the request', async () => {
  const controller = new AbortController(), receipts = [];
  let rounds = 0, effects = 0;
  await assert.rejects(runToolLoop(options(async () => {
    rounds++;
    if (rounds === 1) return { content: '', reasoning: '', calls: [{ id: 'original', name: 'terminal.host.run',
      arguments: { command: 'fixture-once' } }], continuation: [] };
    controller.abort(); return finalTurn('Late answer after cancellation.');
  }, { execute: async () => { effects++; return { content: '{"ok":true}', status: 'completed', isError: false }; } },
  { signal: controller.signal, saveActivity: async receipt => receipts.push(receipt) })), { name: 'AbortError' });
  assert.equal(effects, 1);
  assert.equal(receipts.at(-1).status, 'completed');
});

test('user cancellation and configuration revocation never enter decoder repair', async () => {
  const controller = new AbortController(); controller.abort();
  await assert.rejects(runToolLoop(options(async () => { throw new ToolCallDecodeFailure('bad', 'MODEL_TOOL_INDEX_INVALID'); }, {},
    { signal: controller.signal })), { name: 'AbortError' });
  await assert.rejects(runToolLoop(options(async () => { throw Object.assign(new Error('revoked'), { code: 'AGENT_CONFIG_CHANGED' }); }, {})),
    { code: 'AGENT_CONFIG_CHANGED' });
});

test('safe sparse stream ordinals are independent of the distinct-call budget', async () => {
  const frame = item => `data: ${JSON.stringify(item)}\n\n`;
  const content = frame({ choices: [{ index: 0, delta: { tool_calls: [{ index: 250, id: 'sparse',
    function: { name: catalog[0].wireName, arguments: '{}' } }] }, finish_reason: 'tool_calls' }] });
  const turn = await readToolStream(new Response(content, { headers: { 'content-type': 'text/event-stream' } }),
    'openai-completions', catalog);
  assert.equal(turn.calls.length, 1); assert.equal(turn.calls[0].id, 'sparse');
  const tooMany = rawTurn('openai-completions', 'first', {});
  tooMany.choices[0].message.tool_calls = Array.from({ length: MAX_TURN_TOOL_CALLS + 1 }, (_, index) => ({
    id: `call_${index}`, function: { name: catalog[0].wireName, arguments: '{}' } }));
  assert.throws(() => decodeToolTurn('openai-completions', tooMany, catalog), { code: 'MODEL_TOOL_CALL_LIMIT' });
});

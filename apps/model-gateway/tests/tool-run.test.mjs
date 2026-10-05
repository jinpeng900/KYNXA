import assert from 'node:assert/strict';
import { test } from 'node:test';
import { runToolLoop } from '../orchestration/tool-loop.mjs';
import { toolRunLimits } from '../orchestration/tool-run.mjs';

function fixture(overrides = {}) {
  const states = [], activities = [];
  let executions = 0, rounds = 0;
  const run = () => runToolLoop({ protocol: 'openai-completions', messages: [{ role: 'user', content: 'Inspect and verify until finished' }],
    system: '', declarations: [], inputBudgetTokens: 32000, context: {}, emit: () => {},
    saveActivity: async item => activities.push(item), saveRunState: async item => states.push(item),
    service: { execute: async () => { executions++; return { content: 'inspection complete', isError: false }; } },
    requestTurn: async () => {
      rounds++;
      if (rounds > 13) return { content: 'Verified all requested checks.', reasoning: '', calls: [] };
      const id = `call_${rounds}`, call = { id, name: 'inspection', arguments: { step: rounds } };
      return { content: '', reasoning: '', calls: [call], continuation: [{ role: 'assistant', content: '',
        tool_calls: [{ id, type: 'function', function: { name: call.name, arguments: JSON.stringify(call.arguments) } }] }] };
    }, ...overrides });
  return { run, states, activities, get executions() { return executions; }, get rounds() { return rounds; } };
}

test('one continuous request finishes beyond the old 12-round limit without user prompting', async () => {
  const f = fixture();
  assert.equal((await f.run()).content, 'Verified all requested checks.');
  assert.equal(f.rounds, 14); assert.equal(f.executions, 13);
  assert.equal(f.states.at(-1).phase, 'finalizing');
  assert.equal(f.states.at(-1).toolCalls, 13);
  assert.equal(f.states.at(-1).limits.maxRounds, 64);
});

test('call and round limits stop before the next effect and retain completed receipts', async () => {
  for (const [limits, code] of [[{ maxToolCalls: 1 }, 'TOOL_RUN_CALL_LIMIT'], [{ maxRounds: 1 }, 'TOOL_RUN_ROUND_LIMIT']]) {
    const f = fixture({ limits });
    await assert.rejects(f.run(), { code });
    assert.equal(f.executions, 1);
    assert.equal(f.activities.at(-1).status, 'completed');
    assert.equal(f.states.at(-1).phase, 'interrupted');
    assert.equal(f.states.at(-1).code, code);
  }
});

test('the generated-output run budget rejects tool arguments before dispatch', async () => {
  const f = fixture({ limits: { maxGeneratedTokens: 1024 }, requestTurn: async () => ({ content: '', reasoning: '',
    calls: [{ id: 'large-call', name: 'write', arguments: { content: 'A'.repeat(1100) } }] }) });
  await assert.rejects(f.run(), { code: 'TOOL_RUN_OUTPUT_LIMIT' });
  assert.equal(f.executions, 0); assert.equal(f.activities.length, 0);
});

test('a failed progress checkpoint prevents execution and records no fabricated result', async () => {
  const f = fixture({ saveRunState: async state => { if (state.phase === 'tool') throw Error('fixture storage unavailable'); } });
  await assert.rejects(f.run(), /storage unavailable/);
  assert.equal(f.executions, 0);
  assert.deepEqual(f.activities.map(item => item.status), ['running']);
});

test('invalid continuous budgets fail without coercion or unknown fields', () => {
  for (const input of [null, [], { maxRounds: '64' }, { maxToolCalls: 0 }, { maxDurationMs: 3600001 }, { unlimited: true }])
    assert.throws(() => toolRunLimits(input), { code: 'INVALID_TOOL_RUN_LIMITS' });
  for (const key of ['constructor', 'toString', '__proto__'])
    assert.throws(() => toolRunLimits(JSON.parse(`{"${key}":true}`)), { code: 'INVALID_TOOL_RUN_LIMITS' });
});

test('deadline cancellation returns a durable interrupted run with a stable error code', async () => {
  const f = fixture({ limits: { maxDurationMs: 1000 }, requestTurn: async (_messages, _declarations, signal) => {
    await new Promise((resolve, reject) => {
      const keepAlive = setTimeout(resolve, 2000);
      signal.addEventListener('abort', () => { clearTimeout(keepAlive); reject(signal.reason); }, { once: true });
    });
  } });
  await assert.rejects(f.run(), { code: 'TOOL_RUN_TIME_LIMIT', type: 'interrupted' });
  assert.equal(f.executions, 0);
  assert.equal(f.states.at(-1).code, 'TOOL_RUN_TIME_LIMIT');
});

test('the visible completed receipt survives a later progress checkpoint failure', async () => {
  const events = [];
  const f = fixture({ emit: event => events.push(event), saveRunState: async state => {
    if (state.phase === 'continuing') throw Error('fixture storage unavailable');
  } });
  await assert.rejects(f.run(), /storage unavailable/);
  assert.equal(f.executions, 1);
  assert.equal(f.activities.at(-1).status, 'completed');
  assert.equal(events.at(-1).type, 'tool_result');
  assert.equal(events.at(-1).tool.status, 'completed');
});

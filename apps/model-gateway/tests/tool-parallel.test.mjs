import assert from 'node:assert/strict';
import { test } from 'node:test';
import { runToolLoop } from '../orchestration/tool-loop.mjs';

const deferred = () => { let resolve; const promise = new Promise(done => { resolve = done; }); return { promise, resolve }; };
const continuation = calls => [{ role: 'assistant', content: 'Reading the sources.', tool_calls: calls.map(call =>
  ({ id: call.id, type: 'function', function: { name: call.name, arguments: JSON.stringify(call.arguments) } })) }];

test('independent reads run at most four together, preserve call order and keep writes between batches', { timeout: 5000 }, async () => {
  const calls = Array.from({ length: 5 }, (_, index) => ({ id: `read-${index}`, name: 'filesystem.read', arguments: { path: `file-${index}` } }));
  calls.push({ id: 'write', name: 'filesystem.write', arguments: {} }, { id: 'last-read', name: 'filesystem.read', arguments: {} });
  const gate = deferred(), events = [], activities = [], finished = [];
  let active = 0, maximum = 0, executions = 0, rounds = 0;
  const result = await runToolLoop({ protocol: 'openai-completions', messages: [], system: '', declarations: [], inputBudgetTokens: 32000, context: {},
    emit: event => events.push(event), saveActivity: async activity => {
      if (activity.toolCallId === 'read-0' && activity.status === 'running') await new Promise(done => setImmediate(done));
      activities.push(activity);
    }, service: { execute: async (_context, call) => {
      if (call.id === 'write') assert.deepEqual(finished, calls.slice(0, 5).map(item => item.id));
      if (call.id === 'last-read') assert.ok(finished.includes('write'));
      active++; maximum = Math.max(maximum, active); executions++;
      if (executions <= 4) { if (executions === 4) gate.resolve(); await gate.promise; }
      active--; finished.push(call.id);
      return { content: call.id };
    } }, requestTurn: async messages => {
      if (++rounds === 1) return { content: 'Reading the sources.', reasoning: '', calls, continuation: continuation(calls) };
      assert.deepEqual(messages.filter(item => item.role === 'tool').map(item => [item.tool_call_id, item.content]),
        calls.map(call => [call.id, call.id]));
      return { content: 'Done.', reasoning: '', calls: [] };
    } });
  assert.equal(result.content, 'Done.'); assert.equal(maximum, 4); assert.equal(executions, 7);
  assert.deepEqual(events.filter(event => event.type === 'tool_call').map(event => event.tool.order), [1, 2, 3, 4, 5, 6, 7]);
  assert.equal(activities.filter(item => item.status === 'completed').length, 7);
  assert.equal(result.assistantSegments[1].order, 8);
});

test('browser and unknown MCP operations remain serial even if passed read-only annotations', async () => {
  let active = 0, maximum = 0, rounds = 0;
  const calls = ['mcp.chrome-devtools.navigate_page', 'mcp.playwright.browser_navigate', 'mcp.unknown.read']
    .map((name, index) => ({ id: String(index), name, arguments: {}, annotations: { readOnlyHint: true } }));
  await runToolLoop({ protocol: 'openai-completions', messages: [], system: '', declarations: [], inputBudgetTokens: 32000, context: {},
    emit: () => {}, saveActivity: async () => {}, service: { execute: async () => {
      active++; maximum = Math.max(maximum, active); await new Promise(done => setImmediate(done)); active--;
      return { content: 'Read complete.' };
    } }, requestTurn: async () => ++rounds === 1
      ? { content: '', reasoning: '', calls, continuation: continuation(calls) }
      : { content: 'Done.', reasoning: '', calls: [] } });
  assert.equal(maximum, 1);
});

test('cancellation settles all started reads and preserves their returned receipts before stopping', { timeout: 5000 }, async () => {
  const controller = new AbortController(), gate = deferred(), receipts = [], events = [];
  let started = 0, rounds = 0;
  const calls = Array.from({ length: 3 }, (_, index) => ({ id: `read-${index}`, name: 'filesystem.read', arguments: {} }));
  await assert.rejects(runToolLoop({ protocol: 'openai-completions', messages: [], system: '', declarations: [], inputBudgetTokens: 32000,
    context: {}, signal: controller.signal, emit: event => events.push(event), saveActivity: async item => receipts.push(item),
    service: { execute: async (_context, call) => {
      if (++started === 3) { controller.abort(); gate.resolve(); }
      await gate.promise;
      return { content: `Known complete: ${call.id}` };
    } }, requestTurn: async () => { rounds++; return { content: 'Reading.', reasoning: '', calls, continuation: continuation(calls) }; }
  }), { name: 'AbortError' });
  assert.equal(rounds, 1); assert.equal(started, 3);
  assert.equal(receipts.filter(item => item.status === 'completed').length, 3);
  assert.equal(events.filter(item => item.type === 'tool_result').length, 3);
});

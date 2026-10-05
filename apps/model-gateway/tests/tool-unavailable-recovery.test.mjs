import assert from 'node:assert/strict';
import { test } from 'node:test';
import { runToolLoop } from '../orchestration/tool-loop.mjs';
import { decodeToolTurn, wireCatalog } from '../models/tool-protocols.mjs';

test('an undeclared call during no-progress finalization completes normally without another execution', async () => {
  const catalog = wireCatalog([{ name: 'filesystem.read', inputSchema: { type: 'object' }, description: 'Read fixture.' }]);
  let rounds = 0, executions = 0;
  const receipts = [], states = [];
  const result = await runToolLoop({ protocol: 'openai-completions', messages: [], system: '', inputBudgetTokens: 16000,
    context: { conversationId: 'synthetic', message: 'Read the fixture.' },
    catalogForRound: () => catalog, service: { execute: async () => {
      executions++;
      return { content: 'unchanged synthetic observation', status: 'completed', isError: false };
    } },
    emit: () => {}, saveActivity: async activity => receipts.push(activity), saveRunState: async state => states.push(state),
    requestTurn: async (_messages, declarations, _signal, _emit, roundCatalog) => {
      rounds++;
      if (rounds === 5) assert.deepEqual(declarations, []);
      return decodeToolTurn('openai-completions', { choices: [{ finish_reason: 'tool_calls', message: {
        role: 'assistant', content: '', tool_calls: [{ id: `call_${rounds}`, type: 'function',
          function: { name: catalog[0].wireName, arguments: JSON.stringify({ path: 'fixture.txt' }) } }]
      } }] }, roundCatalog);
    }
  });
  assert.equal(rounds, 5); assert.equal(executions, 4);
  assert.match(result.content, /unavailable/);
  assert.equal(receipts.at(-1).code, 'MODEL_TOOL_UNAVAILABLE');
  assert.equal(JSON.parse(receipts.at(-1).result).executed, false);
  assert.equal(states.at(-1).phase, 'finalizing');
  assert.equal(result.assistantSegments.at(-1).phase, 'final_answer');
});

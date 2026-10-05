import assert from 'node:assert/strict';
import { test } from 'node:test';
import { buildContext, estimateMessageTokens, estimateTokens } from '../models/context.mjs';
import { appendToolResults, estimateToolMessageTokens } from '../models/tool-protocols.mjs';
import { runToolLoop } from '../orchestration/tool-loop.mjs';

test('ordinary code history uses the same budget before and during the tool loop without counting transport escapes twice', async () => {
  const code = 'print("hello")\n'.repeat(250);
  const context = buildContext({ conversationId: 'synthetic-budget-chat', contextWindowTokens: 8192, maxOutputTokens: 2048,
    history: [{ Id: 'user', Role: 'user', Content: code, Status: 'completed' },
      { Id: 'assistant', Role: 'assistant', ReplyTo: 'user', Content: code, Status: 'completed' }], currentMessage: 'continue' });
  assert.equal(context.metrics.includedTurnCount, 1);
  assert.equal(estimateToolMessageTokens(context.messages, context.system), context.metrics.estimatedInputTokens);
  assert.ok(context.metrics.estimatedInputTokens + estimateTokens('[]') <= context.metrics.inputBudgetTokens);
  assert.ok(estimateTokens(JSON.stringify(context.messages)) > context.metrics.inputBudgetTokens, 'old transport-based estimate rejected this valid projection');
  let requests = 0;
  const result = await runToolLoop({ protocol: 'openai-completions', messages: context.messages, system: context.system,
    declarations: [], inputBudgetTokens: context.metrics.inputBudgetTokens, context: {}, service: {},
    emit: () => {}, saveActivity: async () => {}, requestTurn: async () => {
      requests++; return { content: 'Synthetic model sees the complete code history.', reasoning: '', calls: [] };
    } });
  assert.equal(requests, 1); assert.equal(result.content, 'Synthetic model sees the complete code history.');
  const ordinary = [{ role: 'user', content: '\\"\r\n\t'.repeat(100) }, { role: 'assistant', content: '中文代码与原文' }];
  assert.equal(estimateToolMessageTokens(ordinary, 'Synthetic system'), estimateMessageTokens(ordinary, 'Synthetic system'));
});

test('native calls, typed text, errors, reasoning and tool results all contribute to each provider continuation budget', () => {
  const params = { path: 'synthetic\\file.txt', note: 'Argument text must remain in the estimate.' };
  const args = JSON.stringify(params), output = 'Synthetic result\nwith "quotes" and \\slashes.'.repeat(20);
  const base = [{ role: 'user', content: 'Read a synthetic file' }];
  const call = { id: 'synthetic-call', name: 'filesystem.read', arguments: params };
  const result = { content: output, isError: true };
  const continuations = {
    'openai-completions': [{ role: 'assistant', content: 'Before the call.', reasoning_content: 'Visible continuation reasoning.',
      tool_calls: [{ id: call.id, type: 'function', function: { name: 'synthetic_wire_name', arguments: args } }] }],
    'anthropic-messages': [{ role: 'assistant', content: [
      { type: 'text', text: 'Before the call.' }, { type: 'thinking', thinking: 'Visible continuation reasoning.', signature: 'synthetic-signature' },
      { type: 'tool_use', id: call.id, name: 'synthetic_wire_name', input: params }
    ] }],
    'openai-responses': [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: 'Before the call.', annotations: [] }] },
      { type: 'reasoning', summary: [{ type: 'summary_text', text: 'Visible continuation reasoning.' }], encrypted_content: 'synthetic-encrypted-reasoning' },
      { type: 'function_call', id: 'synthetic-output-id', call_id: call.id, name: 'synthetic_wire_name', arguments: args }]
  };
  for (const [protocol, continuation] of Object.entries(continuations)) {
    const messages = appendToolResults(protocol, base, { continuation }, [{ call, result }]);
    const cost = estimateToolMessageTokens(messages);
    assert.ok(cost >= estimateToolMessageTokens(base) + estimateTokens(args) + estimateTokens(output), protocol);
    const encodedWithoutResult = estimateToolMessageTokens([...base, ...continuation]);
    assert.ok(cost > encodedWithoutResult, `${protocol} result is counted`);
  }
  const text = [{ role: 'user', content: [{ type: 'text', text: output }] }];
  const withMedia = [{ role: 'user', content: [{ type: 'text', text: output },
    { type: 'image', source: { type: 'base64', media_type: 'image/png', data: 'syntheticimage'.repeat(50) } }] }];
  assert.ok(estimateToolMessageTokens(withMedia) > estimateToolMessageTokens(text));
  const errorMessage = [{ role: 'user', content: [{ type: 'tool_result', tool_use_id: call.id, content: output }] }];
  assert.ok(estimateToolMessageTokens([{ role: 'user', content: [{ ...errorMessage[0].content[0], is_error: true }] }])
    > estimateToolMessageTokens(errorMessage));
});

test('each round still enforces native result and changed schema budgets before requesting the model again', async () => {
  for (const scenario of ['large result', 'changed declarations']) {
    let requests = 0, executions = 0, saves = [];
    const small = [{ type: 'function', function: { name: 'synthetic', parameters: { type: 'object' } } }];
    let declarations = small;
    const call = { id: 'synthetic-call', name: 'synthetic', arguments: {} };
    await assert.rejects(runToolLoop({ protocol: 'openai-completions', messages: [{ role: 'user', content: 'Run once' }], system: '',
      declarations: small, declarationsForRound: () => declarations, inputBudgetTokens: 1000, context: {},
      signal: new AbortController().signal, emit: () => {}, saveActivity: async activity => { saves.push(activity); },
      service: { execute: async () => {
        executions++;
        if (scenario === 'changed declarations') declarations = [{ ...small[0], description: 'largeschema'.repeat(150) }];
        return { content: scenario === 'large result' ? 'largeoutput'.repeat(150) : 'Synthetic small result', isError: false };
      } },
      requestTurn: async () => {
        requests++; return { content: '', reasoning: '', calls: [call], continuation: [{ role: 'assistant', content: '',
          tool_calls: [{ id: call.id, type: 'function', function: { name: 'synthetic', arguments: '{}' } }] }] };
      } }), /工具结果超过本次上下文预算/);
    assert.equal(requests, 1, scenario); assert.equal(executions, 1, scenario);
    assert.deepEqual(saves.map(activity => activity.status), ['running', 'completed'], 'completed effect stays recorded on budget exhaustion');
  }
});

import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { writeFile, readdir } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from 'node:test';
import { runToolLoop } from '../tool-loop.mjs';
import { appendToolResults, estimateToolMessageTokens } from '../tool-protocols.mjs';
import { ToolContextProjection } from '../tool-context.mjs';
import { toolFixture } from './tool-fixture.mjs';

const protocols = ['openai-completions', 'openai-responses', 'anthropic-messages'];
function nativeTurn(protocol, calls, round) {
  const args = call => JSON.stringify(call.arguments);
  const continuation = protocol === 'openai-completions' ? [{ role: 'assistant', content: `Round ${round}`,
    reasoning_content: `private-cc-${round}`, tool_calls: calls.map(call => ({ id: call.id, type: 'function',
      function: { name: 'synthetic_wire', arguments: args(call) } })) }]
    : protocol === 'openai-responses' ? [{ type: 'reasoning', encrypted_content: `private-responses-${round}`, summary: [] },
      ...calls.map(call => ({ type: 'function_call', id: 'provider_' + call.id, call_id: call.id, name: 'synthetic_wire', arguments: args(call) }))]
      : [{ role: 'assistant', content: [{ type: 'thinking', thinking: `Private native thought ${round}`, signature: `private-claude-${round}` },
        ...calls.map(call => ({ type: 'tool_use', id: call.id, name: 'synthetic_wire', input: call.arguments }))] }];
  return { content: '', reasoning: '', calls, continuation };
}
function outputs(protocol, messages) {
  if (protocol === 'anthropic-messages') return messages.flatMap(message => Array.isArray(message.content)
    ? message.content.filter(item => item.type === 'tool_result').map(item => ({ id: item.tool_use_id, text: item.content })) : []);
  return messages.filter(item => protocol === 'openai-responses' ? item.type === 'function_call_output' : item.role === 'tool')
    .map(item => ({ id: item.call_id ?? item.tool_call_id, text: item.output ?? item.content }));
}
function assertPairs(protocol, messages) {
  const calls = protocol === 'anthropic-messages' ? messages.flatMap(message => Array.isArray(message.content)
    ? message.content.filter(item => item.type === 'tool_use').map(item => item.id) : [])
    : protocol === 'openai-responses' ? messages.filter(item => item.type === 'function_call').map(item => item.call_id)
      : messages.flatMap(message => (message.tool_calls ?? []).map(item => item.id));
  assert.deepEqual(calls.toSorted(), outputs(protocol, messages).map(item => item.id).toSorted());
  assert.equal(new Set(calls).size, calls.length);
}

for (const protocol of protocols) {
  test(`${protocol} compacts old archived results, keeps native pairing/reasoning and continues without replay`, async t => {
    const f = await toolFixture(t), context = await f.context('ask');
    await writeFile(join(f.workspace, 'long.txt'), 'bounded result words with quotes " and emoji 😀\n'.repeat(260));
    const original = new Map(), snapshots = [], metrics = [], saved = [];
    let executions = 0, requests = 0;
    const initial = [{ role: 'user', content: 'Read this mounted file three times with distinct tool calls.' }];
    const outcome = await runToolLoop({ protocol, messages: initial, system: 'Keep the fixed permission policy.', declarations: [],
      inputBudgetTokens: 9500, context, signal: new AbortController().signal, emit: () => {}, saveActivity: async activity => saved.push(activity),
      onContextCompacted: item => metrics.push(item), service: { execute: async (...args) => {
        executions++; const result = await f.service.execute(...args); original.set(args[1].id, result); return result;
      } }, requestTurn: async messages => {
        requests++; assert.ok(estimateToolMessageTokens(messages, 'Keep the fixed permission policy.') <= 9500);
        assertPairs(protocol, messages); snapshots.push(structuredClone(messages));
        if (requests === 4) return { content: 'Continued after compaction.', reasoning: '', calls: [] };
        return nativeTurn(protocol, [{ id: 'call_' + requests, name: 'filesystem.read', arguments: { path: 'long.txt' } }], requests);
      } });
    assert.equal(outcome.content, 'Continued after compaction.'); assert.equal(executions, 3); assert.equal(requests, 4);
    assert.deepEqual(saved.map(item => item.status), ['running', 'completed', 'running', 'completed', 'running', 'completed']);
    assert.ok(metrics.some(item => item.compactedToolResultCount > 0));
    const last = snapshots.at(-1), compacted = outputs(protocol, last).filter(item => item.text !== original.get(item.id).content);
    assert.ok(compacted.length > 0);
    const newest = outputs(protocol, last).at(-1);
    assert.equal(newest.text, original.get(newest.id).content, 'most recent result stays verbatim when it fits');
    for (const item of compacted) {
      const envelope = JSON.parse(item.text);
      assert.equal(envelope.contextCompacted, true);
      assert.equal(envelope.resultRef.id, original.get(item.id).resultRef.id);
      assert.equal(envelope.navigation.tool, 'tool.result.read');
      let offset = 0, recovered = '';
      do { const page = await f.service.results.read(context, envelope.resultRef.id, { offset, limit: 4096 });
        recovered += page.text; offset = page.nextOffset; if (!page.truncated) break; } while (true);
      assert.equal(JSON.parse(recovered).structuredContent.content, JSON.parse(original.get(item.id).content).content);
    }
    if (protocol === 'anthropic-messages') assert.ok(last.some(item => item.content?.some?.(block => block.signature === 'private-claude-1')));
    if (protocol === 'openai-responses') assert.ok(last.some(item => item.encrypted_content === 'private-responses-1'));
    if (protocol === 'openai-completions') assert.ok(last.some(item => item.reasoning_content === 'private-cc-1'));
    assert.equal((await readdir(join(await f.conversations.resolveSessionDirectory(context.conversationId), 'tool-results'))).length, 3,
      'compaction never saves a second copy of an existing result');
    assert.deepEqual(initial, [{ role: 'user', content: 'Read this mounted file three times with distinct tool calls.' }]);
  });

  test(`${protocol} first large archived result becomes a legal preview with pageable complete text`, async t => {
    const f = await toolFixture(t), context = await f.context('ask');
    const text = 'long log line with public original text 😀\n'.repeat(1800);
    await writeFile(join(f.workspace, 'large.txt'), text);
    let requests = 0, archived;
    const metrics = [];
    await runToolLoop({ protocol, messages: [{ role: 'user', content: 'Read the requested long log.' }], declarations: [], inputBudgetTokens: 5000,
      context, signal: new AbortController().signal, emit: () => {}, saveActivity: async () => {}, onContextCompacted: item => metrics.push(item),
      service: { execute: async (...args) => archived = await f.service.execute(...args) }, requestTurn: async messages => {
        requests++; assertPairs(protocol, messages);
        if (requests === 1) return nativeTurn(protocol, [{ id: 'first-large', name: 'filesystem.read', arguments: { path: 'large.txt', maxChars: 64000 } }], 1);
        const envelope = JSON.parse(outputs(protocol, messages)[0].text);
        assert.equal(envelope.resultRef.id, archived.resultRef.id);
        assert.ok(estimateToolMessageTokens(messages) <= 5000);
        let offset = 0, recovered = '', pages = 0;
        do { const page = await f.service.results.read(context, envelope.resultRef.id, { offset, limit: 4096 });
          recovered += page.text; offset = page.nextOffset; pages++; if (!page.truncated) break; } while (true);
        assert.ok(pages > 10); assert.equal(JSON.parse(recovered).structuredContent.content, text.slice(0, 64000));
        return { content: 'Read the complete archived output.', reasoning: '', calls: [] };
      } });
    assert.equal(requests, 2); assert.ok(metrics[0].compactedLatestResultCount > 0);
  });
}

test('identified old complete history is compressed in stages with exact message IDs and current policy intact', () => {
  const messages = [{ role: 'user', content: 'Original older user text '.repeat(240) },
    { role: 'assistant', content: 'Original older assistant text '.repeat(240) }, { role: 'user', content: 'Current immutable request.' }];
  const copy = structuredClone(messages), system = 'Fixed system permissions must remain unchanged.';
  const projection = new ToolContextProjection({ protocol: 'openai-completions', messages, conversationId: 'legacy-chat-id',
    historySources: [{ messageId: 'legacy-user-id', role: 'user', turnIndex: 0 },
      { messageId: 'legacy-assistant-id', role: 'assistant', turnIndex: 0 }, { messageId: 'current-user', role: 'user', turnIndex: null }] });
  const result = projection.compact(messages, { system, declarations: [], inputBudgetTokens: 1600 });
  assert.ok(result.metrics.compactedHistoryTurnCount > 0); assert.ok(estimateToolMessageTokens(result.messages, system) <= 1600);
  assert.equal(JSON.parse(result.messages[0].content).source.messageId, 'legacy-user-id');
  assert.equal(JSON.parse(result.messages[1].content).navigation.arguments.messageId, 'legacy-assistant-id');
  assert.deepEqual(result.messages.at(-1), messages.at(-1)); assert.deepEqual(messages, copy);
});

test('parallel native results keep all exact call IDs after first-round pressure compaction', () => {
  for (const protocol of protocols) {
    const initial = [{ role: 'user', content: 'Current request.' }], projection = new ToolContextProjection({ protocol, messages: initial, conversationId: 'chat' });
    const calls = ['parallel-a', 'parallel-b'].map(id => ({ id, name: 'synthetic', arguments: {} }));
    const messages = appendToolResults(protocol, initial, nativeTurn(protocol, calls, 0), calls.map(call => ({ call,
      result: { content: 'parallel output text '.repeat(1000), resultRef: { id: randomUUID(), bytes: 21000, sha256: 'a'.repeat(64) } } })),
    { onResult: (message, pair) => projection.observeResult(message, pair, 0) });
    const compacted = projection.compact(messages, { declarations: [], inputBudgetTokens: 4000 });
    assertPairs(protocol, compacted.messages); assert.equal(outputs(protocol, compacted.messages).length, 2);
    assert.ok(compacted.metrics.compactedLatestResultCount > 0);
  }
});

test('hard current-message or schema budget failures happen before any model request or tool effects', async () => {
  for (const failure of ['current', 'schema']) {
    let requests = 0, executions = 0, saves = 0;
    await assert.rejects(runToolLoop({ protocol: 'openai-completions', messages: [{ role: 'user', content: failure === 'current' ? 'large current request '.repeat(500) : 'Read.' }],
      declarations: failure === 'schema' ? [{ description: 'large necessary schema '.repeat(500) }] : [], inputBudgetTokens: 1000,
      context: { conversationId: 'chat' }, service: { execute: async () => { executions++; } }, emit: () => {}, saveActivity: async () => { saves++; },
      requestTurn: async () => { requests++; } }), { code: 'TOOL_CONTEXT_BUDGET_EXCEEDED' });
    assert.equal(requests, 0); assert.equal(executions, 0); assert.equal(saves, 0);
  }
});

import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { writeFile, readdir } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from 'node:test';
import { runToolLoop } from '../orchestration/tool-loop.mjs';
import { appendToolResults, estimateToolMessageTokens } from '../models/tool-protocols.mjs';
import { ToolContextProjection } from '../models/tool-context.mjs';
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
    // Leave room for one complete broker projection plus older paired receipts; three full results still exceed the budget.
    // 为一个完整代理投影及旧调用配对回执留出空间；三个完整结果仍超过既定预算。
    await writeFile(join(f.workspace, 'long.txt'), 'bounded result words with quotes " and emoji 😀\n'.repeat(160));
    const original = new Map(), originalWire = new Map(), snapshots = [], metrics = [], saved = [];
    let executions = 0, requests = 0;
    const initial = [{ role: 'user', content: 'Read this mounted file three times with distinct tool calls.' }];
    let uncompactedMessages = initial;
    const outcome = await runToolLoop({ protocol, messages: initial, system: 'Keep the fixed permission policy.', declarations: [],
      inputBudgetTokens: 9500, context, signal: new AbortController().signal, emit: () => {}, saveActivity: async activity => saved.push(activity),
      onContextCompacted: item => metrics.push(item), service: { execute: async (...args) => {
        executions++; const result = await f.service.execute(...args); original.set(args[1].id, result);
        uncompactedMessages = appendToolResults(protocol, uncompactedMessages, nativeTurn(protocol, [args[1]], executions),
          [{ call: args[1], result }]);
        originalWire.set(args[1].id, outputs(protocol, uncompactedMessages).at(-1).text);
        return result;
      } }, requestTurn: async messages => {
        requests++; assert.ok(estimateToolMessageTokens(messages, 'Keep the fixed permission policy.') <= 9500);
        assertPairs(protocol, messages); snapshots.push(structuredClone(messages));
        if (requests === 4) return { content: 'Continued after compaction.', reasoning: '', calls: [] };
        return nativeTurn(protocol, [{ id: 'call_' + requests, name: 'filesystem.read', arguments: { path: 'long.txt' } }], requests);
      } });
    assert.equal(outcome.content, 'Continued after compaction.'); assert.equal(executions, 3); assert.equal(requests, 4);
    assert.deepEqual(saved.filter(item => item.name !== 'context.compact').map(item => item.status),
      ['running', 'completed', 'running', 'completed', 'running', 'completed']);
    assert.ok(saved.filter(item => item.name === 'context.compact').every(item => ['running', 'completed'].includes(item.status)));
    assert.ok(metrics.some(item => item.compactedToolResultCount > 0));
    assert.ok(estimateToolMessageTokens(uncompactedMessages, 'Keep the fixed permission policy.') > 9500,
      'the fixture produces real pressure after accounting for native broker projection');
    const last = snapshots.at(-1), compacted = outputs(protocol, last).filter(item => item.text !== originalWire.get(item.id));
    assert.ok(compacted.length > 0);
    const newest = outputs(protocol, last).at(-1);
    assert.equal(newest.text, originalWire.get(newest.id), 'most recent native result stays verbatim when it fits');
    const newestEnvelope = JSON.parse(newest.text);
    assert.equal(newestEnvelope.status, 'completed');
    assert.deepEqual(newestEnvelope.executionEnvironment, original.get(newest.id).executionEnvironment);
    assert.equal(newestEnvelope.output, original.get(newest.id).content, 'the complete newest business output is preserved');
    assert.ok(compacted.every(item => item.id !== newest.id), 'older results are compacted before the newest complete observation');
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

test('current-message pressure returns an honest reply and oversized schemas allow a no-tool reply without effects', async () => {
  for (const failure of ['current', 'schema']) {
    let requests = 0, executions = 0, saves = 0;
    const result = await runToolLoop({ protocol: 'openai-completions', messages: [{ role: 'user', content: failure === 'current' ? 'large current request '.repeat(500) : 'Read.' }],
      declarations: failure === 'schema' ? [{ description: 'large necessary schema '.repeat(500) }] : [], inputBudgetTokens: 1000,
      context: { conversationId: 'chat' }, service: { execute: async () => { executions++; } }, emit: () => {}, saveActivity: async () => { saves++; },
      requestTurn: async (_messages, declarations) => { requests++; assert.deepEqual(declarations, []);
        return { content: 'A normal partial response.', reasoning: '', calls: [] }; } });
    assert.equal(requests, failure === 'current' ? 0 : 1); assert.equal(executions, 0);
    assert.ok(result.content.length > 0); assert.equal(result.completionStatus, 'interrupted');
    assert.equal(saves, 2);
  }
});

for (const protocol of protocols) test(`${protocol} settled checkpoint archives public receipts without opaque reasoning or replay`, async () => {
  const initial = [{ role: 'user', content: 'Continue this task; never delete anything.' }];
  let archive;
  const projection = new ToolContextProjection({ protocol, messages: initial, resultContext: {},
    resultStore: { save: async (_context, _call, value) => { archive = value.content;
      return { id: randomUUID(), bytes: value.content.length, sha256: 'a'.repeat(64) }; } } });
  const call = { id: 'settled', name: 'synthetic', arguments: { path: 'file' } };
  const result = { status: 'completed', content: 'Verified public result.',
    resultRef: { id: randomUUID(), bytes: 23, sha256: 'b'.repeat(64) } };
  const messages = appendToolResults(protocol, initial, nativeTurn(protocol, [call], 1), [{ call, result }],
    { onResult: (message, pair) => projection.observeResult(message, pair, 1) });
  const copy = structuredClone(messages);
  const checkpoint = await projection.checkpoint(messages);
  assert.deepEqual(checkpoint.messages[0], initial[0]); assert.deepEqual(messages, copy);
  assert.ok(archive.includes('Verified public result.'));
  for (const secret of ['private-cc', 'private-responses', 'Private native thought', 'private-claude'])
    assert.ok(!archive.includes(secret));
  assert.equal(JSON.parse(checkpoint.messages.at(-1).content).recentReceipts[0].id, call.id);
  const incomplete = nativeTurn(protocol, [call], 1).continuation;
  assert.equal(await projection.checkpoint([...initial, ...incomplete]), null);
});

test('budget pressure rebalances a complete native step and continues in the same loop', async () => {
  const events = []; let adjustments = 0, requests = 0;
  const messages = [{ role: 'user', content: 'Keep this original request.' },
    { role: 'assistant', content: 'prior output '.repeat(400) }];
  const result = await runToolLoop({ protocol: 'openai-completions', messages, declarations: [], inputBudgetTokens: 800,
    context: { conversationId: 'chat' }, service: {}, emit: event => events.push(event), saveActivity: async () => {},
    contextForPressure: async needed => { adjustments++; assert.ok(needed > 800); return { inputBudgetTokens: 6000 }; },
    requestTurn: async received => { requests++; assert.deepEqual(received, messages);
      return { content: 'Continued successfully.', reasoning: '', calls: [] }; } });
  assert.equal(adjustments, 1); assert.equal(requests, 1); assert.equal(result.content, 'Continued successfully.');
  assert.ok(events.some(event => event.type === 'tool_result' && event.tool.name === 'context.compact' && event.tool.status === 'completed'));
});

for (const protocol of protocols) test(`${protocol} current-step checkpoint continues and prevents replay of a completed write`, async () => {
  let requests = 0, executions = 0, archives = 0; const events = [];
  const ref = () => ({ id: randomUUID(), bytes: 25, sha256: 'c'.repeat(64) });
  const result = await runToolLoop({ protocol, messages: [{ role: 'user', content: 'Complete the original task.' }],
    declarations: [], inputBudgetTokens: 1800, context: { conversationId: 'chat', message: 'Complete the original task.' },
    emit: event => events.push(event), saveActivity: async () => {},
    service: { results: { save: async () => { archives++; return ref(); },
      get: async () => ({ content: 'File saved.' }),
      modelResult: async () => JSON.stringify({ status: 'completed', output: 'File saved.' }) },
      execute: async () => { executions++; return { status: 'completed', content: 'File saved.', resultRef: ref() }; } },
    requestTurn: async messages => {
      requests++;
      if (requests === 2) assert.ok(messages.some(message => typeof message.content === 'string' &&
        message.content.includes('"contextCheckpoint":true')));
      if (requests === 3) return { content: 'Continued with the saved operation.', reasoning: '', calls: [] };
      return nativeTurn(protocol, [{ id: `write-${requests}`, name: 'filesystem.write',
        arguments: { path: 'note.txt', content: 'a long original file body '.repeat(1500), expectedHash: null } }], requests);
    } });
  assert.equal(executions, 1); assert.equal(requests, 3); assert.ok(archives >= 1);
  assert.equal(result.content, 'Continued with the saved operation.');
  assert.ok(events.some(event => event.type === 'tool_result' && event.tool.name === 'context.compact' && event.tool.status === 'completed'));
});

test('an upstream context rejection checkpoints and continues instead of terminating the reply', async () => {
  let requests = 0;
  const result = await runToolLoop({ protocol: 'openai-completions',
    messages: [{ role: 'user', content: 'Keep working on this task.' }], declarations: [], inputBudgetTokens: 1800,
    context: { conversationId: 'chat' }, service: { results: { save: async () =>
      ({ id: randomUUID(), bytes: 100, sha256: 'd'.repeat(64) }) } },
    emit: () => {}, saveActivity: async () => {},
    requestTurn: async messages => {
      if (++requests === 1) throw Object.assign(new Error('Verified context rejection'), { code: 'MODEL_CONTEXT_LIMIT_REJECTED' });
      assert.ok(messages.at(-1).content.includes('"contextCheckpoint":true'));
      return { content: 'The original task continued.', reasoning: '', calls: [] };
    } });
  assert.equal(requests, 2); assert.equal(result.content, 'The original task continued.');
});

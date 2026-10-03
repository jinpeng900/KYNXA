import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { randomUUID } from 'node:crypto';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { ModelStore } from '../store.mjs';
import { ModelRuntime, completedContext } from '../runtime.mjs';
import { responseText } from '../protocols.mjs';
import { decodeToolTurn, wireCatalog } from '../tool-protocols.mjs';

function nativeReply(protocol, content, truncated) {
  if (protocol === 'anthropic-messages') return { content: [{ type: 'thinking', thinking: 'visible-thought' }, { type: 'text', text: content }], stop_reason: truncated ? 'max_tokens' : 'end_turn' };
  if (protocol === 'openai-responses') return { status: truncated ? 'incomplete' : 'completed',
    ...(truncated ? { incomplete_details: { reason: 'max_output_tokens' } } : {}),
    output: [{ type: 'reasoning', summary: [{ type: 'summary_text', text: 'visible-thought' }] },
      { type: 'message', content: [{ type: 'output_text', text: content }] }] };
  return { choices: [{ message: { role: 'assistant', content, reasoning_content: 'visible-thought' }, finish_reason: truncated ? 'length' : 'stop' }] };
}

async function fixture(t, protocol) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-output-runtime-'));
  const seen = [];
  let truncated = false;
  const upstream = createServer(async (request, response) => {
    let source = '';
    for await (const chunk of request) source += chunk;
    seen.push(JSON.parse(source));
    response.setHeader('Content-Type', 'application/json');
    response.end(JSON.stringify(nativeReply(protocol, truncated ? 'partial-code' : 'complete-answer', truncated)));
  });
  await new Promise(resolve => upstream.listen(0, '127.0.0.1', resolve));
  const store = new ModelStore({ dataHome: root });
  await store.save({ providerId: 'output-model', displayName: 'Output model', protocol,
    baseUrl: `http://127.0.0.1:${upstream.address().port}/v1`, models: ['model'], contextWindowTokens: 128_000, maxOutputTokens: 32_768 });
  const runtime = new ModelRuntime({ modelStore: store, dataHome: root });
  t.after(async () => { await runtime.close(); upstream.closeAllConnections(); await new Promise(resolve => upstream.close(resolve)); await rm(root, { recursive: true, force: true }); });
  const input = { conversationId: randomUUID(), requestId: randomUUID(), userMessageId: randomUUID(), provider: 'output-model', model: 'model', message: '现在写代码' };
  return { runtime, seen, input, setTruncated: value => { truncated = value; } };
}

for (const protocol of ['openai-completions', 'openai-responses', 'anthropic-messages']) {
  test(`${protocol}: larger independent output is sent upstream and visible reasoning persists`, async t => {
    const f = await fixture(t, protocol);
    assert.equal(await f.runtime.reply(f.input), 'complete-answer');
    assert.equal(f.seen[0].max_output_tokens ?? f.seen[0].max_tokens, 32_768);
    const history = await f.runtime.conversations.readMessages(f.input.conversationId);
    assert.equal(history.at(-1).Reasoning, 'visible-thought');
    assert.equal(history.at(-1).Status, 'completed');
  });

  for (const tools of [false, true]) test(`${protocol}: ${tools ? 'tool-capable' : 'plain'} JSON truncation keeps draft, marks interrupted, and does not retry`, async t => {
    const f = await fixture(t, protocol);
    f.setTruncated(true);
    const input = { ...f.input, ...(tools ? { permissionMode: 'ask' } : {}) };
    await assert.rejects(f.runtime.reply(input), error => error.type === 'interrupted' && /上限/.test(error.message));
    assert.equal(f.seen.length, 1);
    const history = await f.runtime.conversations.readMessages(input.conversationId);
    assert.equal(history.at(-1).Content, 'partial-code');
    assert.equal(history.at(-1).Reasoning, 'visible-thought');
    assert.equal(history.at(-1).Status, 'interrupted');
    assert.equal(history.at(-1).ToolActivities?.length ?? 0, 0);
    f.setTruncated(false);
    await f.runtime.reply({ ...input, requestId: randomUUID(), userMessageId: randomUUID(), message: '新的问题' });
    assert.equal(JSON.stringify(f.seen[1]).includes('partial-code'), false);
  });

  test(`${protocol}: JSON stream fallback retains a truncated response and reports failure`, async t => {
    const f = await fixture(t, protocol);
    f.setTruncated(true);
    const events = [];
    await assert.rejects(f.runtime.replyStream({ ...f.input, permissionMode: 'ask' }, event => events.push(event)),
      error => error.type === 'interrupted' && error.content === 'partial-code');
    assert.equal((await f.runtime.conversations.readMessages(f.input.conversationId)).at(-1).Status, 'interrupted');
    assert.ok(events.some(event => event.delta === 'partial-code' || event.content === 'partial-code'));
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

test('the legacy completed-context helper no longer imposes a 100-message history cutoff', () => {
  const messages = Array.from({ length: 120 }, (_, index) => [
    { Id: `u${index}`, Role: 'user', Content: `q${index}`, Status: 'completed' },
    { Id: `a${index}`, Role: 'assistant', Content: `a${index}`, Status: 'completed' },
  ]).flat();
  assert.equal(completedContext(messages).length, 240);
});

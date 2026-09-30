import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { randomUUID } from 'node:crypto';
import { mkdtemp, readFile, readdir } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { test } from 'node:test';
import { ModelStore } from '../store.mjs';
import { ModelRuntime } from '../runtime.mjs';
import { createModelServer } from '../server.mjs';
import { readSse } from '../streaming.mjs';
import { chatRequest } from '../protocols.mjs';

const frame = value => `data: ${typeof value === 'string' ? value : JSON.stringify(value)}\r\n\r\n`;
const chatDelta = (content, extra = {}) => ({ choices: [{ index: 0, delta: { content, ...extra } }] });
const finish = { choices: [{ index: 0, delta: {}, finish_reason: 'stop' }] };
const deferred = () => { let resolve; const promise = new Promise(r => { resolve = r; }); return { promise, resolve }; };

async function listen(server, t) {
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(() => { server.closeAllConnections(); server.close(); });
  return `http://127.0.0.1:${server.address().port}`;
}

async function fixture(t, protocol, handler, runtimeOptions = {}) {
  const seen = [];
  const upstream = createServer(async (request, response) => {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    seen.push({ path: request.url, headers: request.headers, body: JSON.parse(Buffer.concat(chunks)) });
    await handler(response, seen.length);
  });
  const endpoint = await listen(upstream, t);
  const dataHome = await mkdtemp(join(tmpdir(), 'kynxa-stream-'));
  const modelStore = new ModelStore({ dataHome });
  await modelStore.save({ providerId: 'test-api', displayName: 'Test', protocol,
    baseUrl: `${endpoint}/v1`, models: ['test-model'], apiKey: 'secret-not-for-events' });
  const modelRuntime = new ModelRuntime({ modelStore, dataHome, ...runtimeOptions });
  t.after(() => modelRuntime.close());
  const gateway = createModelServer({ modelStore, modelRuntime });
  const base = await listen(gateway, t);
  const input = { conversationId: randomUUID(), requestId: randomUUID(),
    message: '你好', provider: 'test-api', model: 'test-model', permissionMode: 'ask' };
  return { input, seen, dataHome, modelRuntime, gateway,
    post: (body = input, signal) => fetch(`${base}/api/chat/stream`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body), signal
    }) };
}

async function events(response) {
  assert.equal(response.status, 200);
  assert.match(response.headers.get('content-type'), /text\/event-stream/);
  const result = [];
  for await (const event of readSse(response.body)) result.push(JSON.parse(event.data));
  return result;
}

async function fragmented(response, text) {
  const bytes = Buffer.from(text);
  // Deliberately split multibyte Chinese, CRLF and JSON tokens across writes.
  for (let index = 0; index < bytes.length; index += 2) {
    response.write(bytes.subarray(index, index + 2));
    await delay(1);
  }
}

async function sessions(home) {
  const names = await readdir(join(home, 'sessions')).catch(() => []);
  return Promise.all(names.filter(x => x.endsWith('.json')).map(async name => JSON.parse(await readFile(join(home, 'sessions', name), 'utf8'))));
}

test('Chat Completions streams before completion, separates reasoning, and replays committed IDs exactly once', async t => {
  const release = deferred();
  const f = await fixture(t, 'openai-completions', async (response, count) => {
    response.writeHead(200, { 'Content-Type': 'text/event-stream' });
    if (count === 1) {
      await fragmented(response, ': heartbeat\r\n\r\n' + frame(chatDelta('', { reasoning_content: '先想一想' })) + frame(chatDelta('你')));
      await release.promise;
    }
    response.end(frame(chatDelta(count === 1 ? '好' : '下一条')) + frame(finish) + frame('[DONE]'));
  });
  const received = [];
  for await (const event of readSse((await f.post()).body)) {
    const value = JSON.parse(event.data); received.push(value);
    if (value.type === 'text_delta') { assert.equal(received.some(x => x.type === 'completed'), false); release.resolve(); }
  }
  assert.equal(received[0].type, 'started');
  assert.equal(received.at(-1).type, 'completed');
  assert.equal(received.at(-1).content, '你好');
  assert.equal(received.at(-1).reasoning, '先想一想');
  assert.ok(received.every(x => x.requestId === f.input.requestId && x.conversationId === f.input.conversationId));
  assert.equal(f.seen[0].body.stream, true);
  assert.equal(f.seen[0].headers.authorization, 'Bearer secret-not-for-events');
  assert.equal(f.seen[0].path, '/v1/chat/completions');
  assert.equal(JSON.stringify(received).includes('secret-not-for-events'), false);
  // Simulates a lost terminal event followed by a retry with the same ID.
  const replay = await events(await f.post());
  assert.deepEqual(replay.map(x => x.type), ['started', 'completed']);
  assert.equal(replay.at(-1).reasoning, '先想一想');
  assert.equal(f.seen.length, 1);
  const mismatched = await events(await f.post({ ...f.input, message: '不同的内容' }));
  assert.equal(mismatched.at(-1).type, 'error');
  assert.equal(f.seen.length, 1);
  await events(await f.post({ ...f.input, requestId: randomUUID(), message: '继续' }));
  assert.deepEqual(f.seen[1].body.messages, [
    { role: 'user', content: '你好' }, { role: 'assistant', content: '你好' }, { role: 'user', content: '继续' }
  ]);
  const saved = (await sessions(f.dataHome))[0];
  assert.equal(saved.length, 4);
  assert.equal(saved[1].requestId, f.input.requestId);
  assert.equal(saved[1].reasoning, '先想一想');
});

test('Responses streams reasoning summaries and reconciles the final response snapshot', async t => {
  const f = await fixture(t, 'openai-responses', async response => {
    response.writeHead(200, { 'Content-Type': 'text/event-stream' });
    await fragmented(response, frame({ type: 'response.reasoning_summary_text.delta', delta: '摘要' }) +
      frame({ type: 'response.output_text.delta', delta: '草稿' }));
    response.end(frame({ type: 'response.completed', response: { status: 'completed', output: [
      { type: 'reasoning', summary: [{ type: 'summary_text', text: '摘要完成' }] },
      { type: 'message', content: [{ type: 'output_text', text: '最终答案' }] }
    ] } }));
  });
  const received = await events(await f.post());
  assert.equal(received.find(x => x.type === 'text_delta').delta, '草稿');
  assert.equal(received.at(-1).content, '最终答案');
  assert.equal(received.at(-1).reasoning, '摘要完成');
  assert.equal(f.seen[0].path, '/v1/responses');
  assert.equal(f.seen[0].body.reasoning, undefined);
  assert.equal(f.seen[0].body.store, false);
  assert.equal((await sessions(f.dataHome))[0][1].content, '最终答案');
});

test('Anthropic streams visible thinking separately and ignores opaque signatures', async t => {
  const f = await fixture(t, 'anthropic-messages', async response => {
    response.writeHead(200, { 'Content-Type': 'text/event-stream' });
    await fragmented(response,
      frame({ type: 'content_block_start', index: 0, content_block: { type: 'thinking', thinking: '' } }) +
      frame({ type: 'content_block_delta', index: 0, delta: { type: 'thinking_delta', thinking: '思考过程' } }) +
      frame({ type: 'content_block_delta', index: 0, delta: { type: 'signature_delta', signature: 'opaque-private-signature' } }) +
      frame({ type: 'content_block_delta', index: 1, delta: { type: 'text_delta', text: '答复' } }));
    response.end(frame({ type: 'message_delta', delta: { stop_reason: 'end_turn' } }) + frame({ type: 'message_stop' }));
  });
  const received = await events(await f.post());
  assert.equal(received.at(-1).content, '答复');
  assert.equal(received.at(-1).reasoning, '思考过程');
  assert.equal(JSON.stringify(received).includes('opaque-private-signature'), false);
  assert.equal(f.seen[0].path, '/v1/messages');
  assert.equal(f.seen[0].headers['x-api-key'], 'secret-not-for-events');
  assert.equal(f.seen[0].body.max_tokens, 8192);
});

for (const protocol of ['openai-completions', 'openai-responses', 'anthropic-messages']) {
  test(`${protocol}: accepts compatible JSON fallback without sending a second request`, async t => {
    const f = await fixture(t, protocol, async response => {
      response.setHeader('Content-Type', 'application/json');
      response.end(JSON.stringify(protocol === 'openai-completions'
        ? { choices: [{ message: { content: '回答', reasoning_content: '思考' }, finish_reason: 'stop' }] }
        : protocol === 'anthropic-messages'
          ? { content: [{ type: 'thinking', thinking: '思考' }, { type: 'text', text: '回答' }], stop_reason: 'end_turn' }
          : { status: 'completed', output: [{ type: 'reasoning', summary: [{ type: 'summary_text', text: '思考' }] },
            { type: 'message', content: [{ type: 'output_text', text: '回答' }] }] }));
    });
    const received = await events(await f.post());
    assert.equal(received.at(-1).content, '回答');
    assert.equal(received.at(-1).reasoning, '思考');
    assert.equal(f.seen.length, 1);
  });
}

for (const [name, ending, type] of [
  ['early EOF', '', 'interrupted'],
  ['output limit', frame({ choices: [{ index: 0, delta: {}, finish_reason: 'length' }] }), 'interrupted'],
  ['provider error', frame({ error: { message: 'secret-not-for-events', code: 'test' } }), 'error'],
  ['invalid event JSON', 'data: invalid secret-not-for-events\n\n', 'error']
]) {
  test(`${name}: preserves partials but never commits them or leaks provider errors`, async t => {
    const f = await fixture(t, 'openai-completions', async response => {
      response.setHeader('Content-Type', 'text/event-stream');
      response.end(frame(chatDelta('', { reasoning_content: '想法' })) + frame(chatDelta('部分答案')) + ending);
    });
    const received = await events(await f.post());
    assert.equal(received.at(-1).type, type);
    assert.equal(received.at(-1).content, '部分答案');
    assert.equal(received.at(-1).reasoning, '想法');
    assert.equal(JSON.stringify(received).includes('secret-not-for-events'), false);
    assert.equal((await sessions(f.dataHome)).length, 0);
    assert.equal(f.seen.length, 1);
  });
}

test('HTTP failure is a sanitized SSE terminal event after started', async t => {
  const f = await fixture(t, 'openai-completions', async response => {
    response.writeHead(401, { 'Content-Type': 'application/json' });
    response.end(JSON.stringify({ error: 'secret-not-for-events' }));
  });
  const received = await events(await f.post());
  assert.deepEqual(received.map(x => x.type), ['started', 'error']);
  assert.match(received.at(-1).error, /HTTP 401/);
  assert.equal(JSON.stringify(received).includes('secret-not-for-events'), false);
});

test('disconnect cancels the upstream and retry excludes unfinished context', async t => {
  const closed = deferred();
  const f = await fixture(t, 'openai-completions', async (response, count) => {
    response.setHeader('Content-Type', 'text/event-stream');
    if (count === 1) {
      response.once('close', () => closed.resolve());
      response.write(frame(chatDelta('未完成')));
    } else response.end(frame(chatDelta('重新回答')) + frame(finish) + frame('[DONE]'));
  });
  const controller = new AbortController();
  const reader = readSse((await f.post(f.input, controller.signal)).body);
  for await (const event of reader) {
    if (JSON.parse(event.data).type === 'text_delta') { controller.abort(); break; }
  }
  await Promise.race([closed.promise, delay(2000).then(() => { throw new Error('upstream not aborted'); })]);
  assert.equal((await sessions(f.dataHome)).length, 0);
  assert.equal((await events(await f.post())).at(-1).content, '重新回答');
  assert.deepEqual(f.seen[1].body.messages, [{ role: 'user', content: '你好' }]);
});

test('idle timeout preserves text and aborts the stalled upstream body', async t => {
  const closed = deferred();
  const f = await fixture(t, 'openai-completions', async response => {
    response.setHeader('Content-Type', 'text/event-stream');
    response.once('close', () => closed.resolve());
    response.write(frame(chatDelta('已经生成')));
  }, { idleTimeoutMs: 80 });
  const received = await events(await f.post());
  assert.equal(received.at(-1).type, 'interrupted');
  assert.match(received.at(-1).error, /长时间/);
  assert.equal(received.at(-1).content, '已经生成');
  await Promise.race([closed.promise, delay(2000).then(() => { throw new Error('upstream not aborted'); })]);
  assert.equal((await sessions(f.dataHome)).length, 0);
});

test('overall timeout bounds a stream even while heartbeats keep resetting idle timeout', async t => {
  const f = await fixture(t, 'openai-completions', async response => {
    response.setHeader('Content-Type', 'text/event-stream');
    response.write(frame(chatDelta('部分')));
    const heartbeat = setInterval(() => response.write(': ping\n\n'), 15);
    response.once('close', () => clearInterval(heartbeat));
  }, { idleTimeoutMs: 80, streamTimeoutMs: 200 });
  const received = await events(await f.post());
  assert.equal(received.at(-1).type, 'interrupted');
  assert.match(received.at(-1).error, /时间超过上限/);
  assert.equal((await sessions(f.dataHome)).length, 0);
});

test('shutdown aborts generation and sends a terminal partial without committing history', async t => {
  const f = await fixture(t, 'openai-completions', async response => {
    response.setHeader('Content-Type', 'text/event-stream');
    response.write(frame(chatDelta('部分')));
  });
  const received = [];
  for await (const event of readSse((await f.post()).body)) {
    const value = JSON.parse(event.data); received.push(value);
    if (value.type === 'text_delta') await f.gateway.shutdownModelRuntime();
  }
  assert.equal(received.at(-1).type, 'interrupted');
  assert.equal(received.at(-1).content, '部分');
  assert.equal((await sessions(f.dataHome)).length, 0);
});

test('only known OpenAI Responses reasoning models request summaries', () => {
  const request = (baseUrl, model) => chatRequest({ protocol: 'openai-responses', baseUrl }, model, [], { stream: true }).body;
  assert.deepEqual(request('https://api.openai.com/v1', 'gpt-5-mini').reasoning, { summary: 'auto' });
  assert.deepEqual(request('https://api.openai.com/v1', 'gpt-6-astra').reasoning, { summary: 'auto' });
  assert.equal(request('https://api.openai.com/v1', 'gpt-5-chat-latest').reasoning, undefined);
  assert.equal(request('https://api.openai.com/v1', 'o3-mini').reasoning, undefined);
  assert.equal(request('https://api.openai.com/v1', 'gpt-4.1').reasoning, undefined);
  assert.equal(request('https://custom.example/v1', 'gpt-5-mini').reasoning, undefined);
  assert.equal(request('http://127.0.0.1:8080/v1', 'qwen-8b').reasoning, undefined);
});

test('streaming and full replies share ordering; cancelling a queued stream does not send it', async t => {
  const firstStarted = deferred(), release = deferred();
  const f = await fixture(t, 'openai-completions', async (response, count) => {
    response.setHeader('Content-Type', 'application/json');
    if (count === 1) { firstStarted.resolve(); await release.promise; }
    response.end(JSON.stringify({ choices: [{ message: { content: `回答${count}` } }] }));
  });
  const first = f.modelRuntime.replyStream(f.input, () => {});
  await firstStarted.promise;
  const cancel = new AbortController();
  const skipped = f.modelRuntime.replyStream({ ...f.input, requestId: randomUUID(), message: '不发送' }, () => {}, cancel.signal);
  cancel.abort();
  await assert.rejects(Promise.race([skipped, delay(500).then(() => { throw new Error('cancel waited for queue'); })]), /已停止/);
  const last = f.modelRuntime.reply({ ...f.input, message: '后续消息' });
  assert.equal(f.seen.length, 1);
  release.resolve();
  await first; await last;
  assert.equal(f.seen.length, 2);
  assert.deepEqual(f.seen[1].body.messages.map(x => x.content), ['你好', '回答1', '后续消息']);
  assert.equal((await sessions(f.dataHome))[0].length, 4);
});

test('SSE decoder accepts multiline data, lone CR and comments', async () => {
  const source = ': first\r\revent: test\rdata: {"content":\rdata: "你好"}\r\r';
  const bytes = Buffer.from(source);
  const stream = new ReadableStream({ start(controller) {
    for (const byte of bytes) controller.enqueue(Uint8Array.of(byte));
    controller.close();
  } });
  const result = [];
  for await (const value of readSse(stream)) result.push(value);
  assert.equal(result.length, 1);
  assert.equal(result[0].event, 'test');
  assert.equal(JSON.parse(result[0].data).content, '你好');
});

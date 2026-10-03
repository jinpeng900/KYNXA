import assert from 'node:assert/strict';
import { createServer, request as httpRequest } from 'node:http';
import { mkdtemp, readFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createHash } from 'node:crypto';
import { test } from 'node:test';
import { ModelRuntime } from '../runtime.mjs';
import { atomicJson } from '../store.mjs';

// server.mjs initializes a default runtime at import time. Keep even that unused default isolated.
const testRoot = await mkdtemp(join(process.env.KYNXA_CANCEL_TEST_ROOT ?? tmpdir(), 'kynxa-cancellation-'));
process.env.KYNXA_MODEL_HOME = join(testRoot, 'unused-default');
const { createModelServer } = await import('../server.mjs');
const conversationId = '8cb45842-7b8a-44d1-b621-3188d1f5d4d0';
const deferred = () => {
  let resolve;
  const promise = new Promise(value => { resolve = value; });
  return { promise, resolve };
};
async function within(promise) {
  let timer;
  try {
    return await Promise.race([promise, new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error('Timed out waiting for a local test event.')), 3000);
    })]);
  } finally { clearTimeout(timer); }
}
async function listen(server) {
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  return `http://127.0.0.1:${server.address().port}`;
}
function clientRequest(url, input) {
  const request = httpRequest(`${url}/api/chat`, { method: 'POST', headers: { 'Content-Type': 'application/json' } });
  const done = new Promise(resolve => {
    request.once('error', error => resolve({ error }));
    request.once('response', async response => {
      let body = '';
      for await (const chunk of response) body += chunk;
      resolve({ status: response.statusCode, body });
    });
  });
  request.end(JSON.stringify(input));
  return { request, done };
}
async function fixture(t, handler, timeoutMs = 180000) {
  const dataHome = await mkdtemp(join(testRoot, 'session-'));
  const seen = [], releases = [], upstreamCloses = [], invocations = [];
  const starts = Array.from({ length: 10 }, deferred);
  const disconnects = Array.from({ length: 10 }, deferred);
  const upstream = createServer(async (request, response) => {
    let body = '';
    for await (const chunk of request) body += chunk;
    const index = seen.length;
    seen.push(JSON.parse(body));
    const closed = deferred();
    upstreamCloses.push(closed);
    response.once('close', () => closed.resolve({ finished: response.writableFinished }));
    await handler({ response, body: seen[index], index, releases });
    if (!response.destroyed && !response.writableEnded) {
      response.end(JSON.stringify({ choices: [{ message: { content: 'mock reply' } }] }));
    }
  });
  const upstreamUrl = await listen(upstream);
  const connection = { providerId: 'mock', displayName: 'Mock', models: ['mock-model'],
    baseUrl: upstreamUrl, protocol: 'openai-completions' };
  const modelStore = { list: async () => [connection], connectionFor: async () => connection };
  const runtime = new ModelRuntime({ modelStore, dataHome, timeoutMs });
  const gateway = createModelServer({ modelStore, modelRuntime: { reply: async input => {
    const invocation = { input, settled: deferred() };
    const index = invocations.length;
    invocations.push(invocation);
    starts[index].resolve();
    try { return await runtime.reply(input); }
    catch (error) { invocation.error = error; throw error; }
    finally { invocation.settled.resolve(); }
  } } });
  let requestCount = 0;
  gateway.on('request', (_, response) => {
    const index = requestCount++;
    response.once('close', () => disconnects[index]?.resolve());
  });
  const url = await listen(gateway);
  t.after(async () => {
    releases.forEach(gate => gate.resolve());
    await runtime.close();
    gateway.closeAllConnections(); upstream.closeAllConnections();
    await Promise.all([new Promise(resolve => gateway.close(resolve)), new Promise(resolve => upstream.close(resolve))]);
  });
  const input = message => ({ conversationId, provider: 'mock', model: 'mock-model', message });
  const history = async () => {
    const key = createHash('sha256').update(JSON.stringify([conversationId, 'mock', 'mock-model'])).digest('hex');
    try { return JSON.parse(await readFile(join(dataHome, 'sessions', `${key}.json`), 'utf8')); }
    catch (error) { if (error.code === 'ENOENT') return []; throw error; }
  };
  const post = message => fetch(`${url}/api/chat`, { method: 'POST', body: JSON.stringify(input(message)) });
  return { url, input, history, post, runtime, dataHome, upstreamUrl, seen, invocations, starts, disconnects, upstreamCloses };
}

test('client disconnect cancels generation, preserves successful history, and retry adds the user only once', async t => {
  const accepted = deferred(), release = deferred();
  const context = await fixture(t, async ({ response, body, releases }) => {
    response.setHeader('Content-Type', 'application/json');
    if (body.messages.at(-1).content === 'cancel me' && !accepted.done) {
      accepted.done = true;
      releases.push(release);
      accepted.resolve();
      await release.promise;
    }
  });
  const seed = await context.post('successful seed');
  assert.equal(seed.status, 200);
  await seed.json();
  const originalHistory = await context.history();
  assert.equal(originalHistory.length, 2);
  const canceled = clientRequest(context.url, context.input('cancel me'));
  await within(accepted.promise);
  canceled.request.destroy();
  assert.ok((await within(canceled.done)).error);
  await within(context.disconnects[1].promise);
  await within(context.invocations[1].settled.promise);
  assert.equal(context.invocations[1].error.name, 'AbortError');
  assert.equal((await within(context.upstreamCloses[1].promise)).finished, false);
  assert.deepEqual(await context.history(), originalHistory);
  const retry = await context.post('cancel me');
  assert.equal(retry.status, 200);
  assert.equal((await retry.json()).content, 'mock reply');
  assert.deepEqual(context.seen[2].messages.map(message => message.content), ['successful seed', 'mock reply', 'cancel me']);
  assert.equal((await context.history()).filter(message => message.role === 'user' && message.content === 'cancel me').length, 1);
});

test('a disconnected queued turn never calls the provider or changes the ordered history', async t => {
  const accepted = deferred(), release = deferred();
  const context = await fixture(t, async ({ response, index, releases }) => {
    response.setHeader('Content-Type', 'application/json');
    if (index === 0) { releases.push(release); accepted.resolve(); await release.promise; }
  });
  const first = clientRequest(context.url, context.input('first'));
  await within(accepted.promise);
  const second = clientRequest(context.url, context.input('queued canceled'));
  await within(context.starts[1].promise);
  second.request.destroy();
  await within(second.done);
  await within(context.disconnects[1].promise);
  release.resolve();
  assert.equal((await within(first.done)).status, 200);
  await within(context.invocations[1].settled.promise);
  assert.equal(context.invocations[1].error.name, 'AbortError');
  assert.equal(context.seen.length, 1);
  assert.deepEqual((await context.history()).map(message => message.content), ['first', 'mock reply']);
  const next = await context.post('next');
  assert.equal(next.status, 200);
  await next.json();
  assert.deepEqual(context.seen[1].messages.map(message => message.content), ['first', 'mock reply', 'next']);
});

function observeBodyRead(t, upstreamUrl) {
  const reading = deferred();
  const fetchOriginal = globalThis.fetch;
  t.mock.method(globalThis, 'fetch', async (...args) => {
    const response = await fetchOriginal(...args);
    if (String(args[0]) === `${upstreamUrl}/chat/completions`) {
      const jsonOriginal = response.json.bind(response);
      response.json = (...jsonArgs) => { reading.resolve(); return jsonOriginal(...jsonArgs); };
    }
    return response;
  });
  return reading.promise;
}
const holdBody = async ({ response, releases }) => {
  const release = deferred();
  releases.push(release);
  response.writeHead(200, { 'Content-Type': 'application/json' });
  response.write('{"choices":[');
  await release.promise;
};

test('disconnect during response.json is cancellation rather than invalid JSON and does not persist a turn', async t => {
  const context = await fixture(t, holdBody);
  const reading = observeBodyRead(t, context.upstreamUrl);
  const client = clientRequest(context.url, context.input('cancel body'));
  await within(reading);
  client.request.destroy();
  await within(client.done);
  await within(context.disconnects[0].promise);
  await within(context.invocations[0].settled.promise);
  assert.equal(context.invocations[0].error.name, 'AbortError');
  assert.equal((await within(context.upstreamCloses[0].promise)).finished, false);
  assert.deepEqual(await context.history(), []);
});

test('timeout during body reading retains the timeout diagnosis and leaves no session', async t => {
  const context = await fixture(t, holdBody, 500);
  const reading = observeBodyRead(t, context.upstreamUrl);
  const attempt = context.runtime.reply(context.input('timeout body'));
  const rejected = assert.rejects(attempt, /模型响应超时/);
  await within(reading);
  await rejected;
  assert.deepEqual(await context.history(), []);
});

test('shutdown during body reading retains the shutdown diagnosis and leaves no session', async t => {
  const context = await fixture(t, holdBody);
  const reading = observeBodyRead(t, context.upstreamUrl);
  const attempt = context.runtime.reply(context.input('shutdown body'));
  const rejected = assert.rejects(attempt, /模型服务已停止/);
  await within(reading);
  await context.runtime.close();
  await rejected;
  assert.deepEqual(await context.history(), []);
});

test('an already canceled session write cannot create the destination', async () => {
  const root = await mkdtemp(join(testRoot, 'write-'));
  const controller = new AbortController();
  controller.abort();
  const filename = join(root, 'sessions', 'never-committed.json');
  await assert.rejects(atomicJson(filename, [{ role: 'user', content: 'canceled' }], { signal: controller.signal }), { name: 'AbortError' });
  await assert.rejects(readFile(filename), { code: 'ENOENT' });
});

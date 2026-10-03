import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { Readable } from 'node:stream';
import { test } from 'node:test';
import { openEventStream, readJsonBody } from '../http-transport.mjs';

test('request decoding keeps UTF-8 intact across fragmented incoming chunks', async () => {
  const input = { message: '你好\n世界' };
  const bytes = Buffer.from(JSON.stringify(input));
  const fragmented = () => Readable.from([...bytes].map(byte => Buffer.from([byte])));
  assert.deepEqual(await readJsonBody(fragmented(), bytes.length), input);
  await assert.rejects(readJsonBody(fragmented(), bytes.length - 1), { message: '请求体过大。' });
});

// A controllable response makes blocked output and heartbeat time deterministic.
class RecordedResponse extends EventEmitter {
  chunks = [];
  destroyed = false;
  writableEnded = false;
  writableLength = 0;
  writeHead(status, headers) { this.status = status; this.headers = headers; }
  flushHeaders() {}
  write(chunk) { this.chunks.push(chunk); }
  end() { this.writableEnded = true; this.emit('close'); }
  destroy() { this.destroyed = true; this.emit('close'); }
}

test('SSE keeps frame identity, escaped data and 15 second comment heartbeats', t => {
  t.mock.timers.enable({ apis: ['setInterval'] });
  const response = new RecordedResponse();
  const controller = new AbortController();
  const identity = { conversationId: 'conversation', requestId: 'request', createdAt: '2026-01-01T00:00:00.000Z' };
  const stream = openEventStream(response, identity, controller);
  t.after(() => stream.end());
  assert.equal(response.status, 200);
  assert.deepEqual(response.headers, {
    'Content-Type': 'text/event-stream; charset=utf-8',
    'Cache-Control': 'no-cache, no-transform', 'X-Accel-Buffering': 'no'
  });
  const event = { type: 'text_delta', delta: '你好\n下一行' };
  stream.emit(event);
  assert.equal(response.chunks[0], `event: text_delta\ndata: ${JSON.stringify({ ...identity, ...event })}\n\n`);
  t.mock.timers.tick(14999);
  assert.equal(response.chunks.length, 1);
  t.mock.timers.tick(1);
  assert.equal(response.chunks[1], ': keep-alive\n\n');
  stream.end();
  t.mock.timers.tick(30000);
  stream.emit({ type: 'completed' });
  assert.equal(response.chunks.length, 2);
  assert.equal(controller.signal.aborted, false);
  assert.equal(response.listenerCount('close'), 0);
  assert.equal(response.listenerCount('error'), 0);
});

test('response disconnect and errors cancel generation while normal completion does not', () => {
  for (const event of ['close', 'error']) {
    const response = new RecordedResponse();
    const controller = new AbortController();
    const stream = openEventStream(response, {}, controller);
    response.emit(event);
    assert.equal(controller.signal.aborted, true, event);
    stream.end();
  }
  const response = new RecordedResponse();
  const controller = new AbortController();
  const stream = openEventStream(response, {}, controller);
  stream.end();
  response.emit('close');
  assert.equal(controller.signal.aborted, false);
});

test('SSE aborts blocked generation only after pending output exceeds one MiB', () => {
  const response = new RecordedResponse();
  const controller = new AbortController();
  const stream = openEventStream(response, {}, controller);
  try {
    response.writableLength = 1024 * 1024;
    stream.emit({ type: 'text_delta', delta: 'boundary' });
    assert.equal(controller.signal.aborted, false);
    assert.equal(response.chunks.length, 1);
    response.writableLength++;
    stream.emit({ type: 'text_delta', delta: 'blocked' });
    assert.equal(controller.signal.aborted, true);
    assert.equal(response.destroyed, true);
    stream.emit({ type: 'completed' });
    assert.equal(response.chunks.length, 1);
  } finally { stream.end(); }
});

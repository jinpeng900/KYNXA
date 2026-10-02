import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { mkdtemp } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { ModelStore, validateConnection } from '../store.mjs';
import { ModelRuntime } from '../runtime.mjs';
import { createModelServer } from '../server.mjs';

for (const protocol of ['anthropic-messages', 'openai-responses']) {
  test(`${protocol}: discovers models and completes two HTTP chat turns using native protocol`, async t => {
    const seen = [];
    const native = createServer(async (request, response) => {
      let body = '';
      for await (const chunk of request) body += chunk;
      seen.push({ path: request.url, headers: request.headers, body: body ? JSON.parse(body) : null });
      response.setHeader('Content-Type', 'application/json');
      if (new URL(request.url, 'http://localhost').pathname === '/v1/models')
        response.end(JSON.stringify({ data: [{ id: 'native-model' }] }));
      else response.end(JSON.stringify(protocol === 'anthropic-messages'
        ? { content: [{ type: 'thinking', thinking: 'private reasoning' }, { type: 'text', text: '真实接口格式' }] }
        : { status: 'completed', output: [{ type: 'reasoning' },
          { type: 'message', content: [{ type: 'output_text', text: '真实接口格式' }] }] }));
    });
    await new Promise(resolve => native.listen(0, '127.0.0.1', resolve));
    t.after(() => native.close());
    const dataHome = await mkdtemp(join(tmpdir(), 'kynxa-native-protocol-'));
    const modelStore = new ModelStore({ dataHome });
    const modelRuntime = new ModelRuntime({ modelStore, dataHome });
    t.after(() => modelRuntime.close());
    const gateway = createModelServer({ modelStore, modelRuntime });
    await new Promise(resolve => gateway.listen(0, '127.0.0.1', resolve));
    t.after(() => gateway.close());
    const base = `http://127.0.0.1:${gateway.address().port}`;
    const post = (path, body) => fetch(`${base}${path}`, { method: 'POST', body: JSON.stringify(body) });
    const config = { providerId: 'native-api', displayName: 'Native', protocol,
      baseUrl: `http://127.0.0.1:${native.address().port}/v1`, models: ['native-model'], apiKey: 'test-key' };
    assert.equal((await post('/api/models', config)).status, 200);
    assert.equal((await (await fetch(`${base}/api/models`)).json()).providers[0].protocol, protocol);
    assert.equal((await post('/api/models/test', { ...config, apiKey: undefined })).status, 200);
    for (const message of ['one', 'two']) {
      const reply = await post('/api/chat', { conversationId: '8cb45842-7b8a-44d1-b621-3188d1f5d4d0',
        message, provider: config.providerId, model: 'native-model' });
      assert.equal(reply.status, 200);
      assert.equal((await reply.json()).content, '真实接口格式');
    }
    const sent = seen.at(-1);
    assert.equal(sent.body.model, 'native-model');
    if (protocol === 'anthropic-messages') {
      assert.equal(sent.path, '/v1/messages');
      assert.equal(sent.headers['anthropic-version'], '2023-06-01');
      assert.equal(sent.headers['x-api-key'], 'test-key');
      assert.equal(seen[0].headers['x-api-key'], 'test-key');
      assert.equal(sent.headers.authorization, undefined);
      assert.equal(sent.body.max_tokens, 8192);
      assert.deepEqual(sent.body.messages.map(m => m.content), ['one', '真实接口格式', 'two']);
    } else {
      assert.equal(sent.path, '/v1/responses');
      assert.equal(sent.headers.authorization, 'Bearer test-key');
      assert.equal(sent.body.store, false);
      assert.deepEqual(sent.body.input.map(m => m.content), ['one', '真实接口格式', 'two']);
    }
  });
}

test('rejects unsupported protocol names', () => {
  assert.throws(() => validateConnection({ providerId: 'invalid', displayName: 'Invalid',
    baseUrl: 'http://127.0.0.1:8080/v1', models: ['test'], protocol: 'other' }), /协议无效/);
});

import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { mkdtemp } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { resolve, join } from 'node:path';
import { test } from 'node:test';
import { HarnessModelStore } from '../store.mjs';
import { HarnessRuntime } from '../harness.mjs';

const harnessRoot = resolve(import.meta.dirname, '../../../../../deepseek-harness/deepseek-harness');
const patchPath = resolve(import.meta.dirname, '../kynxa-sdk.cordis.patch.yml');

test('real Harness SDK uses a saved KYNXA model route without executable tools', { timeout: 60000 }, async t => {
  const requests = [];
  const upstream = createServer((request, response) => {
    let body = '';
    request.on('data', chunk => { body += chunk; });
    request.on('end', () => {
      requests.push(JSON.parse(body));
      response.writeHead(200, { 'content-type': 'text/event-stream' });
      response.write('data: {"choices":[{"delta":{"role":"assistant","content":null}}]}\n\n');
      response.write('data: {"choices":[{"delta":{"content":"来自 Harness"}}]}\n\n');
      response.write('data: {"choices":[{"delta":{"content":""},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":2}}\n\n');
      response.end('data: [DONE]\n\n');
    });
  });
  await new Promise(ready => upstream.listen(0, '127.0.0.1', ready));
  t.after(() => upstream.close());
  const home = await mkdtemp(join(tmpdir(), 'kynxa-real-harness-'));
  const baseUrl = `http://127.0.0.1:${upstream.address().port}/v1`;
  await new HarnessModelStore({ harnessRoot, dataHome: home }).save({
    providerId: 'local-test', displayName: 'Local test', baseUrl, models: ['test-model']
  });
  const runtime = new HarnessRuntime({ harnessRoot, dataHome: home, workspaceRoot: home, patchPath });
  t.after(() => runtime.close());
  const result = await runtime.reply({ conversationId: '8cb45842-7b8a-44d1-b621-3188d1f5d4d0',
    message: '你好', provider: 'local-test', model: 'test-model' });
  assert.equal(result, '来自 Harness');
  assert.equal(requests.length, 1);
  assert.equal(requests[0].model, 'test-model');
  assert.deepEqual(requests[0].tools ?? [], []);
});

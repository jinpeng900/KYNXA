import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { test } from 'node:test';
import { access } from 'node:fs/promises';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { installProviderMeter, runComparative } from './run-comparative.mjs';
import { PI_REFERENCE_SDK_ROOT } from './pi-reference.mjs';

const sdkAvailable = await access(join(PI_REFERENCE_SDK_ROOT, 'node_modules', '@earendil-works', 'pi-agent-core', 'package.json')).then(() => true, () => false);
async function upstream(t, respond) {
  const requests = [], errors = [];
  const server = createServer(async (request, response) => {
    const chunks = []; for await (const chunk of request) chunks.push(chunk);
    const body = JSON.parse(Buffer.concat(chunks).toString()); requests.push(body);
    try {
      const result = await respond(body), base = { id: 'synthetic-comparative', object: 'chat.completion.chunk', created: 1, model: body.model };
      const delta = result.tool ? { tool_calls: [{ index: 0, id: `synthetic-call-${requests.length}`, type: 'function',
        function: { name: result.tool, arguments: JSON.stringify(result.arguments) } }] } : { content: result.content };
      response.writeHead(200, { 'content-type': 'text/event-stream' });
      for (const frame of [{ ...base, choices: [{ index: 0, delta, finish_reason: null }] },
        { ...base, choices: [{ index: 0, delta: {}, finish_reason: result.tool ? 'tool_calls' : 'stop' }] },
        { ...base, choices: [], usage: { prompt_tokens: 80, completion_tokens: 12, total_tokens: 92, prompt_cache_hit_tokens: 5 } }])
        response.write('data: ' + JSON.stringify(frame) + '\n\n');
      response.end('data: [DONE]\n\n');
    } catch (error) { errors.push(String(error)); response.writeHead(500); response.end(); }
  });
  await new Promise(done => server.listen(0, '127.0.0.1', done));
  t.after(() => new Promise(done => { server.closeAllConnections(); server.close(done); }));
  return { requests, errors, connection: { providerId: 'synthetic', displayName: 'Synthetic', protocol: 'openai-completions',
    baseUrl: `http://127.0.0.1:${server.address().port}/v1`, apiKey: 'synthetic-only-not-real', models: ['synthetic-model'] } };
}

test('comparative CLI without a connection prints a bounded plan without provider inference', () => {
  const result = spawnSync(process.execPath, ['tests/agent-performance-benchmark/run-comparative.mjs'], { encoding: 'utf8' });
  assert.equal(result.status, 0); const plan = JSON.parse(result.stdout);
  assert.equal(plan.realCalls, 0); assert.equal(plan.tasks.length, 12);
});

test('SSE meter counts true attempts, usage and first text, while rejecting other network destinations', async t => {
  const p = await upstream(t, () => ({ content: 'Synthetic response' }));
  const meter = installProviderMeter(p.connection, { deadline: performance.now() + 10000, maxCalls: 1 });
  t.after(() => meter.close());
  const response = await fetch(p.connection.baseUrl + '/chat/completions', { method: 'POST', body: JSON.stringify({
    model: 'synthetic-model', stream: true, messages: [{ role: 'user', content: 'Hello' }] }) });
  await response.text();
  await assert.rejects(fetch('https://example.org'), { code: 'BENCHMARK_NETWORK_DENIED' });
  await assert.rejects(fetch(p.connection.baseUrl + '/chat/completions'), { code: 'BENCHMARK_MODEL_CALL_LIMIT' });
  await meter.close();
  assert.equal(p.requests[0].stream_options.include_usage, true); assert.equal(meter.calls.length, 1);
  assert.equal(meter.calls[0].usage.totalTokens, 92); assert.equal(meter.calls[0].usage.cacheReadTokens, 5);
  assert.ok(Number.isFinite(meter.calls[0].firstTextMs));
  assert.equal(JSON.stringify(meter.calls).includes(p.connection.apiKey), false);
});

test('paired native loops execute real write/readback, freeze the plan first and preserve complete receipts',
  { skip: sdkAvailable ? false : 'Optional pinned Pi SDK fixture is not installed; run its documented isolated install.' }, async t => {
    const p = await upstream(t, body => {
      assert.equal(body.stream_options.include_usage, true);
      const last = body.messages.findLast(item => item.role === 'tool');
      const value = last ? JSON.parse(last.content) : null;
      const rounds = body.messages.filter(item => item.role === 'tool').length;
      const tool = operation => body.tools.find(item => item.function.description.startsWith(operation + ':')).function.name;
      if (rounds === 0) return { tool: tool('filesystem.read'), arguments: { path: 'seed.txt' } };
      if (rounds === 1) return { tool: tool('filesystem.write'), arguments: { path: 'receipt.txt', content: value.content, expectedHash: null } };
      if (rounds === 2) return { tool: tool('filesystem.read'), arguments: { path: 'receipt.txt' } };
      return { content: JSON.stringify({ verified: true, ticket: value.content.trim().split('=')[1], sources: ['seed.txt', 'receipt.txt'] }) };
    });
    let frozen = false;
    const result = await runComparative({ connection: p.connection, model: 'synthetic-model', semantic: 'off', repeats: 1,
      taskIds: ['dependent-file-cycle'], measurementKind: 'functional-regression',
      onPlan: async plan => { assert.match(plan.fixtureHash, /^[a-f0-9]{64}$/u); assert.equal(p.requests.length, 0); frozen = true; },
      onProgress: () => assert.equal(frozen, true) });
    assert.deepEqual(p.errors, []);
    assert.equal(result.runs.length, 2);
    for (const run of result.runs) {
      assert.equal(run.success, true, JSON.stringify({ engine: run.engine, checks: run.checks, errorCode: run.errorCode }));
      assert.equal(run.modelCalls.length, 4); assert.equal(run.toolCalls, 3); assert.equal(run.finishedToolActivities, 3);
      assert.equal(run.disk['receipt.txt'], 'ticket=KYNXA-V2-862451\n');
      assert.equal(run.modelCalls.every(call => call.usage.status === 'reported' && call.request.orphanToolResults === 0 && call.request.missingToolResults === 0), true);
    }
    assert.equal(result.engines.kynxa.allCost.inputTokens, 320);
    assert.equal(result.engines['pi-sdk'].allCost.inputTokens, 320);
  });

import assert from 'node:assert/strict';
import { test } from 'node:test';
import { createServer } from 'node:http';
import { once } from 'node:events';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { fileURLToPath } from 'node:url';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { readUsage, summarizeRuns } from './evaluation.mjs';
import { CACHE_ORDER_TASKS, cacheOrderPlan, projectReservations, tokenCost, summarizeCacheOrder, fixtureId } from './cache-order.mjs';
import { parseOptions, runCacheOrder, runNativeTask, sendNativeTurn, safeErrorCode, removeOwnedDirectory,
  startupDiagnostic, nativeJson } from './run-cache-order.mjs';

test('benchmark counters follow native inclusive input and cache write semantics without inventing zeros', () => {
  const openai = readUsage('openai-responses', { usage: { input_tokens: 100, output_tokens: 3,
    input_tokens_details: { cached_tokens: 70, cache_write_tokens: 20 } } });
  assert.equal(openai.inputTokens, 100); assert.equal(openai.cacheReadTokens, 70);
  assert.equal(openai.cacheWriteTokens, 20); assert.equal(openai.uncachedInputTokens, 10);
  const completions = readUsage('openai-completions', { usage: { prompt_tokens: 100, completion_tokens: 3,
    prompt_tokens_details: { cached_tokens: 80, cache_write_tokens: 0 } } });
  assert.equal(completions.cacheWriteTokens, 0); assert.equal(completions.uncachedInputTokens, 20);
  const deepseek = readUsage('openai-completions', { usage: { prompt_tokens: 100, completion_tokens: 3,
    prompt_cache_hit_tokens: 80, prompt_cache_miss_tokens: 20 } });
  assert.equal(deepseek.inputTokens, 100); assert.equal(deepseek.cacheWriteTokens, null);
  assert.equal(deepseek.uncachedInputTokens, 20); assert.equal(deepseek.cacheMissTokens, 20);
  const deepseekWrite = readUsage('openai-completions', { usage: { prompt_tokens: 100, completion_tokens: 3,
    prompt_cache_hit_tokens: 80, prompt_cache_miss_tokens: 20, prompt_tokens_details: { cache_write_tokens: 10 } } });
  assert.equal(deepseekWrite.cacheMissTokens, 20); assert.equal(deepseekWrite.uncachedInputTokens, 10);
  assert.equal(deepseekWrite.cacheReadTokens + deepseekWrite.cacheWriteTokens + deepseekWrite.uncachedInputTokens, 100);
  const anthropic = readUsage('anthropic-messages', { usage: { input_tokens: 10, output_tokens: 3,
    cache_read_input_tokens: 70, cache_creation_input_tokens: 20 } });
  assert.equal(anthropic.inputTokens, 100); assert.equal(anthropic.totalTokens, 103);
  assert.equal(anthropic.uncachedInputTokens, 10);
  const unknown = readUsage('anthropic-messages', { usage: { input_tokens: 10, output_tokens: 3 } });
  assert.equal(unknown.inputTokens, 10); assert.equal(unknown.cacheReadTokens, null); assert.equal(unknown.cacheWriteTokens, null);
  const ollama = readUsage('ollama', { usage: { prompt_eval_count: 10, eval_count: 3 } });
  assert.equal(ollama.totalTokens, 13);
});

test('malformed or impossible cache parts and unsafe sums remain unknown', () => {
  const tooLarge = readUsage('openai-completions', { usage: { prompt_tokens: 100, completion_tokens: 1,
    prompt_tokens_details: { cached_tokens: 80, cache_write_tokens: 30 } } });
  assert.equal(tooLarge.cacheReadTokens, 80); assert.equal(tooLarge.cacheWriteTokens, null);
  assert.equal(tooLarge.uncachedInputTokens, null);
  assert.equal(readUsage('openai-completions', { usage: { prompt_tokens: 100, completion_tokens: 1,
    prompt_cache_hit_tokens: 80, prompt_cache_miss_tokens: 30 } }).cacheMissTokens, null);
  assert.equal(readUsage('anthropic-messages', { usage: { input_tokens: 10, output_tokens: 1,
    cache_read_input_tokens: 'invalid' } }).inputTokens, null);
  assert.equal(readUsage('anthropic-messages', { usage: { input_tokens: Number.MAX_SAFE_INTEGER,
    output_tokens: 1, cache_creation_input_tokens: 1 } }).inputTokens, null);
  const summary = summarizeRuns([{ modelCalls: [{ durationMs: 1, usage: readUsage('openai-completions',
    { usage: { prompt_tokens: 10, completion_tokens: 1 } }) }], toolCalls: 0, durationMs: 1, setupMs: 0 }]);
  assert.equal(summary.allCost.cacheReadTokens, null); assert.equal(summary.allCost.knownCacheReadRequests, 0);
});

const reservation = (id, providerUsage, extras = {}) => ({ taskId: 'task', reservationId: id, state: 'settled',
  dispatched: true, providerUsage, usage: { unknownTokenRequests: providerUsage ? 0 : 1 },
  createdAtMs: 100, updatedAtMs: 120, ...extras });
const usage = { inputTokens: 100, outputTokens: 10, cacheReadTokens: 60, cacheCreationTokens: 20,
  uncachedInputTokens: 20, clientMetrics: { firstVisibleTextMs: 15 } };

test('projection deduplicates original reservations while counting failed, compaction and unknown dispatches', () => {
  const original = reservation('main', usage);
  const calls = projectReservations([original, original, reservation('compaction', usage), reservation('failed', null),
    reservation('unknown', null, { state: 'unknown', dispatched: null }), reservation('unsubmitted', null, { state: 'released', dispatched: false })]);
  assert.equal(calls.length, 4); assert.equal(calls[3].dispatchKnown, false); assert.equal(calls[2].inputTokens, null);
  const summary = summarizeCacheOrder([{ modelCalls: calls, success: false, durationMs: 50, setupMs: 1,
    toolSelection: { observed: 3, correct: 2 } }]);
  assert.equal(summary.modelRequests, 4); assert.equal(summary.unknownTokenRequests, 2);
  assert.equal(summary.inputTokens, null); assert.equal(summary.knownInputTokens, 200);
  assert.equal(summary.cacheReadTokens, null); assert.equal(summary.firstVisibleTextMs, 15);
  assert.equal(summary.toolSelectionAccuracy, 2 / 3); assert.equal(summary.estimatedTokenCost, null);
});

test('explicit pricing counts reads and writes once and refuses a complete cost when usage is missing', () => {
  const price = { currency: 'USD', inputPerMillion: 2, outputPerMillion: 6,
    cacheReadPerMillion: 0.2, cacheWritePerMillion: 2.5 };
  const call = projectReservations([reservation('main', usage)])[0];
  assert.equal(tokenCost([call], price).estimatedTokenCost, (20 * 2 + 60 * 0.2 + 20 * 2.5 + 10 * 6) / 1_000_000);
  const missing = tokenCost([call, ...projectReservations([reservation('failed', null)])], price);
  assert.equal(missing.estimatedTokenCost, null); assert.equal(missing.callsMissingPriceOrUsage, 1);
  assert.ok(missing.knownEstimatedTokenCost > 0);
  const noWriteCounter = { ...call, cacheWriteTokens: null };
  assert.equal(tokenCost([noWriteCounter], price).estimatedTokenCost, null);
  assert.ok(tokenCost([noWriteCounter], { ...price, cacheWritePerMillion: price.inputPerMillion }).estimatedTokenCost > 0);
  assert.equal(tokenCost([{ ...call, cacheReadTokens: null }], price).estimatedTokenCost, null);
  const unmeasured = summarizeCacheOrder([{ modelCalls: [call], success: false, durationMs: 1, setupMs: 0,
    unmeasuredCalls: true }], price);
  assert.equal(unmeasured.estimatedTokenCost, null); assert.equal(unmeasured.modelRequests, null);
  assert.equal(unmeasured.knownModelRequests, 1); assert.equal(unmeasured.inputTokens, null);
  assert.ok(unmeasured.knownEstimatedTokenCost > 0);
  assert.throws(() => tokenCost([call], { ...price, cacheReadPerMillion: -1 }), /INVALID_BENCHMARK_PRICE/u);
});

test('cleanup rejects outside targets before touching the filesystem', async () => {
  await assert.rejects(removeOwnedDirectory(join(tmpdir(), 'owned-does-not-exist'),
    join(tmpdir(), 'unrelated-target'), 'unused'), /BENCHMARK_CLEANUP_OUTSIDE_OWNED_ROOT/u);
  await assert.rejects(removeOwnedDirectory('relative-root', 'relative-root/work', 'unused'),
    /BENCHMARK_CLEANUP_OUTSIDE_OWNED_ROOT/u);
});

test('fixture identities already match canonical native UUID URL identities', () => {
  assert.match(fixtureId('greeting-zh:conversation'), /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/u);
  assert.equal(fixtureId('greeting-zh:conversation'), fixtureId('greeting-zh:conversation'));
});

test('native HTTP failure diagnostics preserve route and machine code without private IDs, queries or body', async () => {
  const server = createServer((_request, response) => {
    response.writeHead(404, { 'content-type': 'application/json' });
    response.end(JSON.stringify({ code: 'CONVERSATION_NOT_FOUND', error: 'private-error-body' }));
  });
  server.listen(0, '127.0.0.1'); await once(server, 'listening');
  try {
    await assert.rejects(nativeJson(`http://127.0.0.1:${server.address().port}`,
      '/api/conversations/private-chat/messages?private-query=private-token'), error => {
      assert.deepEqual(error.httpDiagnostic, { method: 'GET', pathTemplate: '/api/conversations/:conversationId/messages',
        status: 404, gatewayCode: 'CONVERSATION_NOT_FOUND' });
      assert.equal(JSON.stringify(error.httpDiagnostic).includes('private'), false);
      return true;
    });
  } finally { server.closeAllConnections(); await new Promise(resolveClose => server.close(resolveClose)); }
});

test('native startup failures retain only machine codes and exit status without stderr secrets', () => {
  const diagnostic = startupDiagnostic({ exitCode: 1,
    stderr: 'RESOURCE_RUNTIME_UNAVAILABLE: secret path and key\n{"code":"INDEX_START_FAILED","message":"private URL"}\n' });
  assert.deepEqual(diagnostic.gatewayCodes, ['RESOURCE_RUNTIME_UNAVAILABLE', 'INDEX_START_FAILED']);
  assert.equal(diagnostic.exitCodeHex, '0x00000001');
  assert.equal(JSON.stringify(diagnostic).includes('secret'), false);
  assert.equal(JSON.stringify(diagnostic).includes('private'), false);
  assert.equal(startupDiagnostic({ exitCode: 3221225781 }).exitCodeHex, '0xc0000135');
});

test('default planning performs no IO or paid call and exposes the same tasks in alternating arms', async () => {
  const options = parseOptions(['--connection-file', 'must-not-be-read.json', '--output', 'must-not-be-written.json']);
  const plan = await runCacheOrder(options);
  assert.equal(plan.measurementKind, 'plan-only'); assert.equal(plan.realCalls, 0);
  assert.deepEqual(plan.sequence, [['A', 'B'], ['B', 'A']]);
  assert.equal(plan.fixtureHash, cacheOrderPlan().fixtureHash);
  assert.equal(plan.taskIds.length, CACHE_ORDER_TASKS.length);
  assert.throws(() => parseOptions(['--execute']), /CACHE_BENCHMARK_EXPLICIT_CONFIGURATION_REQUIRED/u);
  assert.equal(safeErrorCode(new Error('secret key and private URL')), 'NATIVE_BENCHMARK_FAILED');
  const runner = fileURLToPath(new URL('./run-cache-order.mjs', import.meta.url));
  const result = await promisify(execFile)(process.execPath, [runner], { windowsHide: true });
  assert.equal(JSON.parse(result.stdout).realCalls, 0);
});

test('mock native SSE framing retains split UTF-8 and records visible text, not reasoning, then reads formal reservations', async () => {
  const observed = [], task = CACHE_ORDER_TASKS.find(item => item.id === 'greeting-zh');
  const server = createServer(async (request, response) => {
    observed.push(request.url);
    if (request.url === '/api/chat/stream') {
      for await (const ignored of request) void ignored;
      response.writeHead(200, { 'content-type': 'text/event-stream' });
      response.write(`data: ${JSON.stringify({ type: 'reasoning_delta', delta: 'private reasoning' })}\r\n\r\n`);
      const visible = Buffer.from(`data: ${JSON.stringify({ type: 'text_delta', delta: '你好！' })}\n\n`);
      const split = visible.indexOf(Buffer.from('你')) + 1;
      response.write(visible.subarray(0, split)); response.write(visible.subarray(split));
      response.end(`data: ${JSON.stringify({ type: 'completed' })}\n\n`);
      return;
    }
    response.setHeader('content-type', 'application/json');
    let body;
    if (request.url.includes('/messages?')) body = { messages: [{ Role: 'assistant', Status: 'completed', Content: '你好！', ToolActivities: [] }] };
    else if (request.url.startsWith('/api/tasks?')) body = { tasks: [{ taskId: 'root' }], hasMore: false };
    else if (request.url.startsWith('/api/tasks/root?')) body = { task: { state: 'completed' }, children: [],
      modelReservations: [reservation('main', usage), reservation('compaction', usage), reservation('failed-attempt', null)] };
    else if (request.url.startsWith('/api/usage/summary?')) body = { modelRequests: 3, unknownTokenRequests: 1 };
    else { response.statusCode = 404; body = {}; }
    response.end(JSON.stringify(body));
  });
  server.listen(0, '127.0.0.1'); await once(server, 'listening');
  const baseUrl = `http://127.0.0.1:${server.address().port}`;
  try {
    const run = await runNativeTask({ baseUrl, task, connection: { providerId: 'fixture' }, model: 'fixture', workRoot: 'unused' });
    assert.equal(run.success, true); assert.equal(run.modelCalls.length, 3);
    assert.ok(run.taskFirstVisibleTextMs >= 0); assert.equal(run.usageSummary.unknownTokenRequests, 1);
    assert.equal(observed.filter(path => path === '/api/chat/stream').length, 1);
    assert.ok(observed.includes(`/api/tasks?conversationId=${fixtureId(`${task.id}:conversation`)}&limit=100`));
  } finally { server.closeAllConnections(); await new Promise(resolveClose => server.close(resolveClose)); }
});

test('a truncated native stream is a failed observation rather than a successful task', async () => {
  const server = createServer((_request, response) => {
    response.writeHead(200, { 'content-type': 'text/event-stream' });
    response.end(`data: ${JSON.stringify({ type: 'text_delta', delta: 'partial' })}\n\n`);
  });
  server.listen(0, '127.0.0.1'); await once(server, 'listening');
  try { await assert.rejects(sendNativeTurn(`http://127.0.0.1:${server.address().port}`, {}, 1000), /NATIVE_BENCHMARK_INCOMPLETE_SSE/u); }
  finally { server.closeAllConnections(); await new Promise(resolveClose => server.close(resolveClose)); }
});

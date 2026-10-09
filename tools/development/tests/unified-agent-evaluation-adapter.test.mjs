import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { test } from 'node:test';
import { createEvaluationAdapter, evaluationResourceView } from '../unified-agent-evaluation-adapter.mjs';
import { evaluatePlan, RETRIEVAL_ABLATIONS } from '../evaluate-unified-retrieval.mjs';

const manifestPath = resolve('tools/development/fixtures/unified-agent/smoke-manifest.json');
const isolation = { separateDataRoot: true, waitForPreviousExecutors: true, noHeavyPolling: true };

async function fixtureServer(t, handler) {
  const requests = [], errors = [];
  const server = createServer(async (request, response) => {
    let text = ''; for await (const chunk of request) text += chunk;
    const body = JSON.parse(text); requests.push(body);
    try {
      const reply = handler(body, requests);
      response.writeHead(200, { 'Content-Type': 'application/json' }); response.end(JSON.stringify(reply));
    } catch (error) {
      errors.push(error.message);
      response.writeHead(502, { 'Content-Type': 'application/json' }); response.end(JSON.stringify({ error: 'stub assertion failed' }));
    }
  });
  await new Promise(resolveReady => server.listen(0, '127.0.0.1', resolveReady));
  t.after(() => new Promise(resolveClosed => { server.close(resolveClosed); server.closeIdleConnections?.(); }));
  return { environment: { KYNXA_EVAL_BASE_URL: `http://127.0.0.1:${server.address().port}/v1`, KYNXA_EVAL_API_KEY: 'synthetic-evaluation-key' }, requests, errors };
}

function toolReply(body, name, args, id) {
  const descriptor = body.tools.find(item => item.function.description.startsWith(`${name}:`));
  assert.ok(descriptor, `${name} is in the actual immutable catalog`);
  return { choices: [{ finish_reason: 'tool_calls', message: { role: 'assistant', content: '',
    tool_calls: [{ id, type: 'function', function: { name: descriptor.function.name, arguments: JSON.stringify(args) } }] } }],
  usage: { prompt_tokens: 100, completion_tokens: 10 } };
}

test('actual ModelRuntime performs isolated five-way repair, archives real evidence and independently validates', async t => {
  const manifest = JSON.parse(await readFile(manifestPath, 'utf8'));
  const original = await readFile(resolve(manifest.fixtureRoot, 'addition.mjs'), 'utf8');
  const fixture = await fixtureServer(t, body => {
    const results = body.messages.filter(item => item.role === 'tool');
    switch (results.length) {
      case 0: return toolReply(body, 'knowledge.search', { query: 'add addition sum operands' }, 'search');
      case 1: return toolReply(body, 'filesystem.read', { path: 'addition.mjs' }, 'read');
      case 2: {
        assert.doesNotThrow(() => JSON.parse(results.at(-1).content), results.at(-1).content);
        const read = JSON.parse(results.at(-1).content);
        return toolReply(body, 'filesystem.edit', { path: 'addition.mjs', oldText: 'left * right', newText: 'left + right', expectedHash: read.sha256 }, 'edit');
      }
      case 3: return toolReply(body, 'terminal.run', { command: 'node', args: ['--test', 'addition-test'] }, 'test');
      default: return { choices: [{ finish_reason: 'stop', message: { role: 'assistant', content: '加法实现已修复，测试通过。' } }], usage: { prompt_tokens: 100, completion_tokens: 10 } };
    }
  });
  const diagnostics = [];
  const adapter = await createEvaluationAdapter(manifest, { environment: fixture.environment, onDiagnostic: value => diagnostics.push(value) });
  const observed = [];
  const run = adapter.run.bind(adapter);
  adapter.run = async options => { const result = await run(options); observed.push({ variant: options.variant, result }); return result; };
  const report = await evaluatePlan(manifest, adapter, { smoke: true });
  assert.deepEqual(fixture.errors, []);
  assert.equal(report.qualityBenchmarkCompleted, false);
  assert.equal(observed.length, 5);
  for (const { variant, result } of observed) {
    assert.equal(result.errorCode, undefined, JSON.stringify(diagnostics));
    assert.ok(result.sourceIds.includes('addition-contract'), variant.id);
    assert.equal(result.verifiedSources, true);
    assert.equal(result.acceptanceReceipts[0].origin, 'independent-validator');
    assert.equal(result.acceptanceReceipts[0].exitCode, 0, result.acceptanceReceipts[0].output);
    assert.equal(result.acceptanceReceipts[0].passed, true);
    assert.equal(result.acceptanceReceipts[0].completedTests, 1);
    assert.equal(result.attempts.length, 1, 'ordinary agent rounds are not task retries');
    assert.equal(result.attempts[0].modelCalls, 5);
    assert.equal(result.modelRequests.length, 5);
    assert.equal(result.attempts[0].toolCalls, 4);
    assert.equal(result.observedScopeViolations, 0);
    assert.equal(result.executionPolicy.cpuThreadsLimit, variant.adaptiveResources ? null : 2);
    assert.equal(result.executionPolicy.fixedBudget?.source ?? null, variant.adaptiveResources ? null : 'evaluation-fixed');
    assert.deepEqual(result.executionPolicy.flags, { gaps: variant.gaps, adaptiveResources: variant.adaptiveResources,
      relations: variant.relations, experience: variant.experience });
    assert.ok(result.peakRssBytes > 0);
    assert.equal(result.peakGpuBytes, null, 'global GPU usage cannot pretend to measure one case');
    assert.doesNotMatch(JSON.stringify(result), /synthetic-evaluation-key/);
    assert.equal(report.variants[variant.id].summary.verifiedTaskSuccessRate, 1);
    assert.equal(report.variants[variant.id].summary.measuredRetries, 0);
  }
  assert.equal(fixture.requests.length, 25);
  assert.ok(fixture.requests.every(body => body.model === manifest.model && body.temperature === 0 && body.seed === undefined));
  for (const [index, { variant }] of observed.entries()) {
    const tools = fixture.requests[index * 5].tools.map(item => item.function.description.split(':')[0]);
    assert.equal(tools.includes('knowledge.relations'), variant.relations);
    assert.equal(tools.includes('knowledge.experience'), variant.experience);
    assert.equal(tools.includes('knowledge.assess'), variant.gaps);
    assert.ok(tools.every(name => !name.startsWith('mcp.') && !name.startsWith('computer.') && !name.startsWith('web.')));
  }
  assert.equal(await readFile(resolve(manifest.fixtureRoot, 'addition.mjs'), 'utf8'), original, 'original fixture remains unchanged');
});

test('scope/command denial is recorded, unsupported variants fail and unknown provider costs stay unknown', async t => {
  const manifest = JSON.parse(await readFile(manifestPath, 'utf8'));
  const fixture = await fixtureServer(t, body => {
    const results = body.messages.filter(item => item.role === 'tool');
    if (!results.length) return toolReply(body, 'filesystem.read', { path: resolve(manifest.fixtureRoot, 'addition.mjs') }, 'escape');
    if (results.length === 1) return toolReply(body, 'terminal.run', { command: 'node', args: ['--test', 'undeclared'] }, 'command');
    return { choices: [{ finish_reason: 'stop', message: { content: '没有修改样本。' } }] };
  });
  const adapter = await createEvaluationAdapter(manifest, { environment: fixture.environment });
  t.after(() => adapter.dispose());
  const result = await adapter.run({ task: manifest.tasks[0], variant: RETRIEVAL_ABLATIONS[0], seed: manifest.seed, isolation });
  assert.equal(result.observedScopeViolations, 2);
  assert.equal(result.attempts.at(-1).inputTokens, null);
  assert.equal(result.attempts.at(-1).outputTokens, null);
  assert.equal(result.acceptanceReceipts[0].passed, false, 'a model final claim never substitutes for execution');
  await assert.rejects(adapter.run({ task: manifest.tasks[0], variant: { ...RETRIEVAL_ABLATIONS[0], gaps: true }, seed: 0, isolation }),
    { code: 'EVALUATION_VARIANT_UNSUPPORTED' });
  await assert.rejects(adapter.run({ task: { ...manifest.tasks[0], query: 'x'.repeat(4001) }, variant: RETRIEVAL_ABLATIONS[0], seed: 0, isolation }),
    { code: 'EVALUATION_QUERY_LIMIT' });
});

test('an initial no-tool draft continues through validation and only the completed last segment is final', async t => {
  const manifest = JSON.parse(await readFile(manifestPath, 'utf8'));
  const fixture = await fixtureServer(t, body => {
    const results = body.messages.filter(item => item.role === 'tool');
    if (!results.length) return toolReply(body, 'filesystem.read', { path: 'addition.mjs' }, 'read');
    if (results.length === 1) return toolReply(body, 'filesystem.edit', { path: 'addition.mjs', oldText: 'left * right',
      newText: 'left + right', expectedHash: JSON.parse(results[0].content).sha256 }, 'edit');
    if (results.length === 2 && body.messages.some(item => item.role === 'user' && item.content.includes('KYNXA_VALIDATION_REQUIRED')))
      return toolReply(body, 'terminal.run', { command: 'node', args: ['--test', 'addition-test'] }, 'test');
    return { choices: [{ finish_reason: 'stop', message: { content: results.length === 2 ? '修改已完成，还要验证。' : '独立测试已执行，完成修复。' } }],
      usage: { prompt_tokens: 100, completion_tokens: 10 } };
  });
  const adapter = await createEvaluationAdapter(manifest, { environment: fixture.environment });
  t.after(() => adapter.dispose());
  const result = await adapter.run({ task: manifest.tasks[0], variant: RETRIEVAL_ABLATIONS[0], seed: manifest.seed, isolation });
  assert.deepEqual(fixture.errors, []);
  assert.equal(result.errorCode, undefined);
  assert.equal(result.assistantOutcome.status, 'completed');
  assert.deepEqual(result.assistantOutcome.segmentPhases, ['commentary', 'commentary', 'commentary', 'commentary', 'final_answer']);
  assert.equal(result.attempts.length, 1);
  assert.equal(result.attempts[0].modelCalls, 5);
  assert.equal(result.acceptanceReceipts[0].passed, true);
});

test('fixed facade requests real hard-limited CPU leases and never reports tuning or substitutes GPU capacity', async () => {
  let requested, reports = 0;
  const actualAuthority = { acquire: async input => { requested = input; return { leaseId: 'actual', cpuThreads: input.cpuThreads, suggestions: {} }; },
    report: async () => reports++, renew: async () => {}, release: async () => {}, registerExecutor: async () => {}, snapshot: async () => ({}) };
  const fixed = evaluationResourceView(actualAuthority, false);
  const grant = await fixed.acquire({ taskId: 'test', cpuThreads: 32, memoryBytes: 2048 });
  assert.equal(requested.cpuThreads, 2); assert.equal(requested.memoryBytes, 2048);
  assert.equal(grant.leaseId, 'actual'); assert.equal(grant.suggestions.batchMultiplier, 1);
  assert.equal(grant.suggestions.candidateLimit, 48);
  await fixed.report(grant.leaseId, { allocationFailure: true }); assert.equal(reports, 0);
  assert.equal((await fixed.acquire({ gpuMemoryBytes: 1024 })).code, 'EVALUATION_CPU_ONLY');
  assert.equal(evaluationResourceView(actualAuthority, true), actualAuthority);
});

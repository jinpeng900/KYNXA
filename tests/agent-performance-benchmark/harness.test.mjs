import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { mkdir, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import { test } from 'node:test';
import { loadBenchmarkConnection, runBenchmark } from './run-agent.mjs';
import { assessReviewerRelations, benchmarkPlan, readUsage, summarizeRuns, verifyTask } from './evaluation.mjs';
import { rescoreResults } from './rescore.mjs';
import { ModelRuntime } from '../../apps/model-gateway/orchestration/runtime.mjs';

const nativeText = content => ({ choices: [{ finish_reason: 'stop', message: { role: 'assistant', content } }] });
const nativeTool = (body, operation, args, callNumber) => {
  const descriptor = body.tools.find(item => item.function.description.startsWith(operation + ':'));
  assert.ok(descriptor, `production tool declaration: ${operation}`);
  return { choices: [{ finish_reason: 'tool_calls', message: { role: 'assistant', content: '',
    tool_calls: [{ id: `fixture_${callNumber}`, type: 'function', function: { name: descriptor.function.name, arguments: JSON.stringify(args) } }] } }] };
};

async function cleanup(root) {
  const suffix = relative(resolve(tmpdir()), resolve(root));
  assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
  await rm(root, { recursive: true, force: true });
}

test('no connection prints the plan and performs no implicit inference', () => {
  const result = spawnSync(process.execPath, ['tests/agent-performance-benchmark/run-agent.mjs'], { encoding: 'utf8' });
  assert.equal(result.status, 0);
  assert.deepEqual(JSON.parse(result.stdout), benchmarkPlan());
  assert.equal(JSON.parse(result.stdout).realCalls, 0);
});

test('a connection document is selected and validated in memory without exposing its values in errors', async t => {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-agent-connection-')); t.after(() => cleanup(root));
  const file = join(root, 'connection.json'), value = { providerId: 'synthetic', displayName: 'Fixture',
    baseUrl: 'http://127.0.0.1:9/v1', apiKey: 'synthetic-not-a-real-key', models: ['fixture-one', 'fixture-two'] };
  await writeFile(file, JSON.stringify({ version: 1, providers: [value] }));
  const result = await loadBenchmarkConnection(file, { provider: 'synthetic', model: 'fixture-one' });
  assert.equal(result.model, 'fixture-one'); assert.equal(result.connection.maxOutputTokens, 8192);
  assert.ok(Object.isFrozen(result.connection)); assert.ok(Object.isFrozen(result.connection.models));
  await assert.rejects(loadBenchmarkConnection(file), { code: 'BENCHMARK_MODEL_REQUIRED' });
  await assert.rejects(loadBenchmarkConnection(file, { provider: 'absent' }), { code: 'BENCHMARK_PROVIDER_REQUIRED' });
  assert.equal((await readFile(file, 'utf8')), JSON.stringify({ version: 1, providers: [value] }));
});

test('CLI refuses an existing output before any upstream call and preserves its result bytes', async t => {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-agent-cli-')); t.after(() => cleanup(root));
  const artifacts = fileURLToPath(new URL('../../artifacts/verification/agent-performance-benchmark/', import.meta.url));
  await mkdir(artifacts, { recursive: true });
  const output = await mkdtemp(join(artifacts, 'output-guard-'));
  t.after(async () => {
    const suffix = relative(resolve(artifacts), resolve(output));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(output, { recursive: true, force: true });
  });
  const preserved = '{"existingResult":"must remain unchanged"}\n';
  await writeFile(join(output, 'results.json'), preserved);
  let calls = 0;
  const upstream = createServer((_request, response) => { calls++; response.end(JSON.stringify(nativeText('你好！'))); });
  await new Promise(done => upstream.listen(0, '127.0.0.1', done));
  t.after(async () => { upstream.closeAllConnections(); await new Promise(done => upstream.close(done)); });
  const connectionFile = join(root, 'connection.json');
  await writeFile(connectionFile, JSON.stringify({ providerId: 'synthetic', displayName: 'Synthetic upstream',
    baseUrl: `http://127.0.0.1:${upstream.address().port}/v1`, models: ['fixture-model'] }));
  const result = spawnSync(process.execPath, ['tests/agent-performance-benchmark/run-agent.mjs', '--connection-file', connectionFile,
    '--output', output, '--rag', 'off', '--semantic', 'off', '--functional-regression'], { encoding: 'utf8', timeout: 5000 });
  assert.equal(result.status, 1); assert.match(result.stderr, /BENCHMARK_OUTPUT_EXISTS/u);
  await new Promise(done => setImmediate(done));
  assert.equal(calls, 0); assert.equal(await readFile(join(output, 'results.json'), 'utf8'), preserved);
});

test('provider usage counts cache tokens once and keeps unknown usage unknown', () => {
  assert.equal(readUsage('openai-completions', { usage: { prompt_tokens: 100, completion_tokens: 20,
    prompt_cache_hit_tokens: 30, prompt_cache_miss_tokens: 70 } }).totalTokens, 120);
  const responsesUsage = readUsage('openai-responses', { usage: { input_tokens: 100, output_tokens: 20, input_tokens_details: { cached_tokens: 30 } } });
  assert.equal(responsesUsage.totalTokens, 120); assert.equal(responsesUsage.cacheReadTokens, 30);
  assert.equal(readUsage('anthropic-messages', { usage: { input_tokens: 10, output_tokens: 20,
    cache_read_input_tokens: 30, cache_creation_input_tokens: 40 } }).totalTokens, 100);
  assert.equal(readUsage('openai-completions', {}).status, 'unknown');
  assert.equal(readUsage('openai-completions', {}).totalTokens, null);
});

test('verifiers reject an unread citation, invented schedule, and a same-round predicted file cycle', () => {
  const assistants = [{ Status: 'completed', Content: 'ORION 的实验窗口为 2031 年 11 月 14 日 17:40 UTC，'
    + '安全审查人 Mara Chen，尚未确认发射。引用 fixture-notes.md。' }];
  assert.equal(verifyTask('english-source-chinese-answer', { assistants, sourceId: 'actual-source' }).success, false);
  assistants[0].EvidenceReferences = [{ sourceId: 'actual-source' }];
  assert.equal(verifyTask('english-source-chinese-answer', { assistants, sourceId: 'actual-source' }).success, true);
  assert.equal(verifyTask('followup-entity', { sourceId: 'actual-source', assistants: [
    { Status: 'completed', Content: 'ORION 17:40 UTC；VEGA 09:15 UTC。来源：fixture-notes.md。', EvidenceReferences: [{ sourceId: 'actual-source' }] },
    { Status: 'completed', Content: 'ORION 的审查人是 Mara Chen。来源：fixture-notes.md。' }
  ] }).success, true);
  assert.equal(verifyTask('greeting', { assistants: [{ Status: 'completed', Content: '你好，有什么可以帮你的吗？' }] }).success, true);
  assert.equal(verifyTask('greeting', { assistants: [{ Status: 'completed', Content: '你好，我可以搜索资料、写代码和操作文件。' }] }).success, false);
  assert.equal(verifyTask('insufficient-evidence', { assistants: [{ Status: 'completed', Content: '无法确认，但可能是 2031 年。' }] }).success, false);
  const content = 'ticket=KYNXA-731926\n';
  assert.equal(verifyTask('dependent-file-cycle', { finalFile: content, assistants: [{ Status: 'completed', Content: content,
    ToolActivities: [
      { name: 'filesystem.read', arguments: { path: 'seed.txt' }, status: 'completed', round: 1, result: JSON.stringify({ content }) },
      { name: 'filesystem.write', arguments: { path: 'receipt.txt' }, status: 'completed', round: 1 },
      { name: 'filesystem.read', arguments: { path: 'receipt.txt' }, status: 'completed', round: 1, result: JSON.stringify({ content }) }
    ] }] }).success, false);
});

test('entity relations permit correct VEGA contrast and reject swapped, negated or contradictory ORION attribution', () => {
  const accepted = [
    'ORION 的安全审查人是 **Mara Chen**。同一资料中 VEGA 的安全审查人为 **Beatrice Hall**。',
    'ORION 的安全评审人是 Mara Chen。作为对照：VEGA 的安全评审人是 Beatrice Hall。',
    'Mara Chen is the reviewer for ORION; Beatrice Hall is the reviewer for VEGA.',
    'ORION 和 VEGA 的审查人分别是 Mara Chen 和 Beatrice Hall。',
    '| ORION | Mara Chen |\n| VEGA | Beatrice Hall |',
    'ORION 的审查人不是 Beatrice Hall 而是 Mara Chen。'
  ];
  const rejected = [
    'ORION 的安全审查人是 Beatrice Hall；VEGA 的安全审查人是 Mara Chen。',
    'ORION 的审查人是 Mara Chen。补充：ORION 的审查人是 Beatrice Hall。',
    'ORION 的审查人不是 Mara Chen，而是 Beatrice Hall。',
    'Beatrice Hall is the reviewer for ORION. Mara Chen reviews VEGA.',
    'ORION 和 VEGA 的审查人分别是 Beatrice Hall 和 Mara Chen。',
    '这里讨论 ORION。VEGA 的审查人是 Mara Chen。',
    'ORION is not reviewed by Mara Chen.'
  ];
  for (const answer of accepted) assert.equal(assessReviewerRelations(answer).entityRetained, true, answer);
  for (const answer of rejected) assert.equal(assessReviewerRelations(answer).entityRetained, false, answer);
});

test('targeted rescoring preserves observations and the original score while language is a diagnostic only', () => {
  const call = { durationMs: 5, usage: readUsage('openai-completions', { usage: { prompt_tokens: 10, completion_tokens: 2 } }) };
  const runs = [{ taskId: 'followup-entity', configuration: 'rag-on', executionStatus: 'completed', success: false,
    checks: { completed: true, twoTurns: true, initialFacts: true, entityRetained: false, groundedCitation: true },
    modelCalls: [call], toolCalls: 0, durationMs: 5, setupMs: 1, answers: [{ content: 'ORION=Mara Chen；VEGA=Beatrice Hall。' }] },
  { taskId: 'dependent-file-cycle', configuration: 'rag-on', executionStatus: 'completed', success: true,
    checks: { completed: true, dependentToolRounds: true, diskFinalState: true, readBackState: true, finalReport: true },
    modelCalls: [call], toolCalls: 3, durationMs: 5, setupMs: 1, answers: [{ content: 'Steps completed and verified.' }] }];
  const original = { schemaVersion: 1, fixtureVersion: 'agent-synthetic-v1', fixtureHash: 'original-fixture-hash', runs,
    summary: summarizeRuns(runs), configurations: { 'rag-on': summarizeRuns(runs) } };
  const preserved = structuredClone(original), rescored = rescoreResults(original);
  assert.deepEqual(original, preserved); assert.equal(original.summary.successRate, 0.5);
  assert.equal(rescored.summary.successRate, 1); assert.equal(rescored.rescoring.newModelCalls, 0);
  assert.equal(rescored.rescoring.originalSummary.successRate, 0.5);
  assert.deepEqual(rescored.summary.allCost, original.summary.allCost);
  assert.deepEqual(rescored.runs[0].modelCalls, original.runs[0].modelCalls);
  assert.equal(rescored.runs[1].success, true); assert.equal(rescored.runs[1].languageDiagnostic.affectsSuccess, false);
  assert.equal(rescored.runs[1].languageDiagnostic.observation, 'English-only response to Chinese prompt');
  assert.equal(rescored.fixtureHash, original.fixtureHash); assert.equal(rescored.sourceFingerprint, undefined);
});

test('production runtime/tool/storage/retrieval loops pass synthetic upstream functional regression, not reasoning evaluation', async t => {
  let requestCount = 0, rejectRequests = false;
  const seen = [];
  // This scripted upstream returns fixed fixture answers; its score has no model capability meaning.
  // 此脚本上游只返回固定测试答案，通过率不代表模型或 Agent 推理能力。
  const upstream = createServer(async (request, response) => {
    try {
      let text = ''; for await (const part of request) text += part;
      const body = JSON.parse(text); seen.push(body); requestCount++;
      if (rejectRequests) { response.writeHead(503, { 'Content-Type': 'application/json' }); response.end('{}'); return; }
      const current = body.messages.findLast(item => item.role === 'user')?.content ?? '';
      const tools = body.messages.filter(item => item.role === 'tool');
      let reply;
      if (current === '你好！') {
        assert.equal(body.tools, undefined); reply = nativeText('你好！');
      } else if (current.includes('seed.txt')) {
        if (!tools.length) reply = nativeTool(body, 'filesystem.read', { path: 'seed.txt' }, requestCount);
        else if (tools.length === 1) {
          const observed = JSON.parse(tools[0].content).content;
          assert.match(observed, /^ticket=/);
          reply = nativeTool(body, 'filesystem.write', { path: 'receipt.txt', content: observed, expectedHash: null }, requestCount);
        } else if (tools.length === 2) reply = nativeTool(body, 'filesystem.read', { path: 'receipt.txt' }, requestCount);
        else reply = nativeText(`已写入并读回核实：${JSON.parse(tools.at(-1).content).content.trim()}`);
      } else if (current.includes('NOVA')) reply = nativeText('资料未确认 NOVA 的正式发射日期与时间，无法确定。');
      else if (!tools.some(item => item.content.includes('Mara Chen')))
        reply = nativeTool(body, 'filesystem.read', { path: 'fixture-notes.md' }, requestCount);
      else if (current.startsWith('继续')) reply = nativeText('ORION 的安全审查人是 Mara Chen。来源：fixture-notes.md。');
      else if (current.includes('比较')) reply = nativeText('ORION 实验窗口为 17:40 UTC，VEGA 为 09:15 UTC。来源：fixture-notes.md。');
      else reply = nativeText('ORION 的实验窗口为 2031 年 11 月 14 日 17:40 UTC，安全审查人是 Mara Chen。'
        + '这是实验窗口，尚未确认发射。来源：fixture-notes.md。');
      reply.usage = { prompt_tokens: 100, completion_tokens: 20, total_tokens: 120,
        prompt_cache_hit_tokens: 30, prompt_cache_miss_tokens: 70 };
      response.writeHead(200, { 'Content-Type': 'application/json' }); response.end(JSON.stringify(reply));
    } catch { response.writeHead(500); response.end('{}'); }
  });
  await new Promise(done => upstream.listen(0, '127.0.0.1', done));
  t.after(async () => { upstream.closeAllConnections(); await new Promise(done => upstream.close(done)); });
  const connection = { providerId: 'synthetic', displayName: 'Synthetic upstream',
    baseUrl: `http://127.0.0.1:${upstream.address().port}/v1`, apiKey: 'synthetic-secret-not-real', models: ['fixture-model'] };
  const result = await runBenchmark({ connection, model: 'fixture-model', semantic: 'off', measurementKind: 'functional-regression' });
  assert.equal(result.measurementKind, 'functional-regression'); assert.equal(result.runs.length, 10);
  assert.equal(result.summary.successful, 10, JSON.stringify(result.runs.filter(run => !run.success)
    .map(({ taskId, configuration, checks, errorCode, answers, toolTrace }) => ({ taskId, configuration, checks, errorCode, answers, toolTrace })), null, 2));
  assert.equal(result.summary.allCost.modelCalls, requestCount);
  assert.equal(result.summary.allCost.totalTokens, requestCount * 120);
  assert.equal(result.summary.failedCost.modelCalls, 0);
  assert.ok(result.runs.filter(run => run.taskId === 'dependent-file-cycle').every(run => run.toolCalls === 3 && run.toolRounds === 3));
  assert.ok(seen.every(body => body.max_tokens === 8192));
  assert.ok(seen.every(body => !(body.tools ?? []).some(tool => /terminal|computer|web_fetch|web_search/.test(tool.function.name))));
  assert.ok(!JSON.stringify(result).includes(connection.apiKey)); assert.ok(!JSON.stringify(result).includes(connection.baseUrl));
  assert.ok(!JSON.stringify(seen).includes(connection.apiKey)); assert.ok(!JSON.stringify(seen).includes(connection.baseUrl));
  const beforeRepeated = requestCount;
  const repeated = await runBenchmark({ connection, model: 'fixture-model', semantic: 'off', repeats: 2, measurementKind: 'functional-regression' });
  assert.equal(repeated.runs.length, 20); assert.equal(repeated.summary.successful, 20);
  assert.equal(repeated.summary.allCost.modelCalls, requestCount - beforeRepeated);
  assert.equal(repeated.repetitions[1].summary.attempted, 10); assert.equal(repeated.repetitions[2].summary.attempted, 10);
  assert.deepEqual(repeated.runs.slice(0, 2).map(run => [run.repetition, run.configuration]), [[1, 'rag-off'], [1, 'rag-on']]);
  assert.deepEqual(repeated.runs.slice(10, 12).map(run => [run.repetition, run.configuration]), [[2, 'rag-on'], [2, 'rag-off']]);
  assert.equal(repeated.fixtureHash, result.fixtureHash); assert.equal(repeated.fixtureVersion, result.fixtureVersion);
  assert.match(repeated.verifierVersion, /relations-v3$/u); assert.match(repeated.sourceFingerprint.combinedHash, /^[a-f0-9]{64}$/u);
  assert.ok(repeated.sourceFingerprint.files.every(file => !file.path.includes('/Data/') && /^[a-f0-9]{64}$/u.test(file.sha256)));
  assert.ok(repeated.sourceFingerprint.files.some(file => file.path === 'apps/model-gateway/orchestration/runtime.mjs'));
  assert.ok(repeated.runs.filter(run => run.taskId === 'dependent-file-cycle').every(run => run.languageDiagnostic.affectsSuccess === false));
  const failed = { ...result.runs[0], success: false, modelCalls: [{ usage: readUsage('openai-completions', null) }] };
  assert.equal(summarizeRuns([failed]).failedCost.callsMissingUsage, 1);
  assert.equal(summarizeRuns([failed]).allCost.totalTokens, null);
  rejectRequests = true;
  const beforeFailure = requestCount;
  const failures = await runBenchmark({ connection, model: 'fixture-model', rag: 'off', semantic: 'off', measurementKind: 'functional-regression' });
  assert.equal(failures.summary.successful, 0);
  assert.equal(failures.summary.failedCost.modelCalls, requestCount - beforeFailure);
  assert.equal(failures.summary.failedCost.modelCalls, 5);
  assert.equal(failures.summary.failedCost.callsMissingUsage, 5);
  assert.equal(failures.summary.failedCost.totalTokens, null);
  assert.ok(failures.runs.every(run => run.modelCalls[0].httpStatus === 503 && run.executionStatus === 'error'));
});

test('a close failure preserves the fixture, blocks further runs and keeps measured results without credentials', async t => {
  const upstream = createServer((_request, response) => {
    response.writeHead(200, { 'Content-Type': 'application/json' });
    response.end(JSON.stringify({ ...nativeText('你好！'), usage: { prompt_tokens: 10, completion_tokens: 2 } }));
  });
  await new Promise(done => upstream.listen(0, '127.0.0.1', done));
  t.after(async () => { upstream.closeAllConnections(); await new Promise(done => upstream.close(done)); });
  const close = ModelRuntime.prototype.close;
  ModelRuntime.prototype.close = async function () {
    await close.call(this);
    throw Object.assign(new Error('Synthetic close failure'), { code: 'SYNTHETIC_CLOSE_FAILED' });
  };
  let result;
  const connection = { providerId: 'synthetic', displayName: 'Synthetic upstream',
    baseUrl: `http://127.0.0.1:${upstream.address().port}/v1`, apiKey: 'synthetic-close-key', models: ['fixture-model'] };
  try { result = await runBenchmark({ connection, model: 'fixture-model', semantic: 'off', measurementKind: 'functional-regression' }); }
  finally { ModelRuntime.prototype.close = close; }
  assert.equal(result.runs.length, 1); assert.equal(result.runs[0].executionStatus, 'close-error');
  assert.equal(result.globalBlocked.code, 'BENCHMARK_CLOSE_FAILED'); assert.equal(result.unexecutedRuns.length, 9);
  assert.equal(result.summary.failedCost.totalTokens, 12);
  const retained = result.globalBlocked.retainedFixturePath; t.after(() => cleanup(retained));
  assert.match(await readFile(join(retained, 'Work', 'fixture-notes.md'), 'utf8'), /Mara Chen/);
  async function inspectTextFiles(directory) {
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) await inspectTextFiles(path);
      else if (/\.(?:json|jsonl|md|txt)$/u.test(entry.name)) {
        const content = await readFile(path, 'utf8');
        assert.ok(!content.includes(connection.apiKey)); assert.ok(!content.includes(connection.baseUrl));
      }
    }
  }
  await inspectTextFiles(retained);
});

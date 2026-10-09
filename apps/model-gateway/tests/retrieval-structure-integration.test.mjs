import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { EventEmitter } from 'node:events';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';
import { RetrievalStructureService } from '../data/retrieval/structure-service.mjs';
import { RetrievalIndex } from '../data/retrieval/index.mjs';
import { MemoryService } from '../data/memory-service.mjs';
import { RetrievalCoordinator } from '../orchestration/retrieval/coordinator.mjs';
import { buildRetrievalIntent, retrievalPlan } from '../orchestration/retrieval/query-plan.mjs';
import { toolFixture, parsed } from './tool-fixture.mjs';
import { createRetrievalEvaluationDataset, retrievalEvaluationReport } from '../../../tests/retrieval-benchmark/metrics.mjs';

const source = (sourceId, filename, text, extra = {}) => ({ sourceId, scopeKey: 'user', sourceType: 'work-file',
  title: filename, locator: { relativePath: filename }, text, ...extra });

async function indexFixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-structure-integration-'));
  const structures = new RetrievalStructureService();
  const legacy = new RetrievalIndex({ root: join(root, 'legacy'), vectorEnabled: false });
  const upgraded = new RetrievalIndex({ root: join(root, 'upgraded'), vectorEnabled: false });
  t.after(async () => {
    await Promise.all([structures.close(), legacy.close(), upgraded.close()]);
    const suffix = relative(resolve(tmpdir()), resolve(root));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(root, { recursive: true, force: true });
  });
  return { root, structures, legacy, upgraded };
}

test('local code queries retrieve candidates with a soft domain and exact files remain distinct from declarations', () => {
  for (const message of ['仓库中的 CancelJob 在哪里', '解释 `JobIndex.CancelJob`']) {
    assert.equal(retrievalPlan(message).shouldRetrieve, true, message);
    assert.equal(retrievalPlan(message).domain, 'mixed', message);
    assert.equal(retrievalPlan(message).preferredDomain, 'code', message);
  }
  assert.equal(retrievalPlan('查找 CancelJob 方法的定义').domain, 'mixed');
  assert.equal(buildRetrievalIntent('查找 CancelJob 方法的定义').symbol, undefined,
    'a bare method-shaped name still needs evidence that it is a code declaration');
  assert.deepEqual(buildRetrievalIntent('查看 `settings.json` 中的设置'), { domain: 'mixed', path: 'settings.json' });
  assert.deepEqual(buildRetrievalIntent('在 JobIndex.cs 查找 CancelJob 方法'),
    { domain: 'mixed', symbol: 'CancelJob', path: 'jobindex.cs', preferredDomain: 'code' });
  assert.equal(retrievalPlan('你好').shouldRetrieve, false);
  assert.equal(retrievalPlan('运行 node 验证脚本').shouldRetrieve, false);
  assert.equal(retrievalPlan('然后它怎么处理取消', { history: [{ Role: 'user', Content: '仓库中 CancelJob 方法如何处理取消' }] }).shouldRetrieve, true);
});

test('real parser worker preserves declaration ranges, raw text and a bounded reusable derivation', async t => {
  const { structures } = await indexFixture(t);
  const text = 'namespace Kynxa;\r\nclass JobIndex(string name) {\r\n  private readonly List<string> _jobs = [];\r\n' +
    '  public bool CancelJob() { return true; }\r\n}\r\n';
  const input = source('job-index', 'JobIndex.cs', text);
  const first = await structures.parse(input);
  assert.equal(first.structure.language, 'csharp');
  assert.equal(first.structure.parseStatus, 'parsed');
  assert.match(first.structure.parserVersion, /csharp-0\.23\.5/);
  const method = first.chunks.find(chunk => chunk.structure.symbolName === 'CancelJob');
  assert.ok(method);
  assert.equal(method.text, text.slice(method.startOffset, method.endOffset));
  assert.equal(method.structure.qualifiedName, 'Kynxa.JobIndex.CancelJob');
  const sequence = structures.sequence;
  const cached = await structures.parse({ ...input, sourceRevision: 2 });
  assert.deepEqual(cached.chunks, first.chunks);
  assert.equal(structures.sequence, sequence);
  assert.equal(structures.status().cacheHits, 1);
  cached.chunks[0].text = 'A caller cannot poison the cached result.';
  assert.equal((await structures.parse(input)).chunks[0].text, first.chunks[0].text);
  assert.ok(structures.status().cacheBytes <= 8 * 1024 * 1024);
  const changed = await structures.parse({ ...input, text: text.replace('return true', 'return false') });
  assert.ok(changed.chunks.some(chunk => chunk.text.includes('return false')));
});

test('parser cancellation, late replies and shutdown never cache or revive an abandoned request', async () => {
  class ControlledWorker extends EventEmitter {
    calls = [];
    postMessage(value) { this.calls.push(value); }
    ref() {}
    unref() {}
    async terminate() { this.emit('exit', 0); return 0; }
  }
  const worker = new ControlledWorker(), structures = new RetrievalStructureService({ workerFactory: () => worker });
  try {
    const controller = new AbortController();
    const abandoned = structures.parse(source('abandoned', 'doc.md', '# Old\nText.'), { signal: controller.signal });
    const stopped = assert.rejects(abandoned, { name: 'AbortError' });
    controller.abort(); await stopped;
    worker.emit('message', { id: worker.calls[0].id, result: { structure: { parseStatus: 'parsed' }, chunks: [] } });
    assert.equal(structures.status().cacheEntries, 0);
    const waiting = structures.parse(source('pending', 'doc.md', '# Current\nText.'));
    const closed = assert.rejects(waiting, { name: 'AbortError' });
    await structures.close(); await closed;
    assert.equal(structures.status().pendingRequests, 0);
    assert.equal(structures.status().state, 'closed');
    await assert.rejects(structures.parse(source('new', 'doc.md', 'Text.')), { code: 'STRUCTURE_SERVICE_CLOSED' });
  } finally { await structures.close(); }
});

test('imported code uses real broker search, unit read and stale-source rejection without an API model call', async t => {
  const f = await toolFixture(t);
  const retrieval = new RetrievalCoordinator({ conversations: f.conversations,
    memory: new MemoryService({ conversationStore: f.conversations }), tools: f.service });
  f.service.retrieval = retrieval;
  await retrieval.settings.patchGlobal({ expectedRevision: 0, patch: { local: { semantic: 'off' } } });
  const filename = join(f.workspace, 'JobIndex.cs');
  await writeFile(filename, 'namespace Kynxa; class JobIndex { public string CancelJob() { return "cancelled"; } }', 'utf8');
  const imported = await retrieval.importSource({ path: filename, scope: 'project', projectId: f.projectId });
  await retrieval.activeJobs.get(imported.jobId)?.promise;
  assert.equal((await retrieval.jobs.get(imported.jobId)).status, 'completed');
  const context = await f.context('full');
  const call = f.call('knowledge.search', { query: '查找 JobIndex.CancelJob 定义',
    domain: 'code', symbol: 'JobIndex.CancelJob', path: 'JobIndex.cs' });
  const receipt = await f.service.execute(context, call), search = parsed(receipt);
  const canonical = (await f.service.results.get(context, receipt.resultRef.id)).structuredContent.items[0];
  await f.conversations.upsertMessage(f.conversationId, { Id: context.requestId, Role: 'assistant', Content: '',
    ToolActivities: [{ toolCallId: call.id, name: call.name, status: 'completed', resultRef: receipt.resultRef }] });
  assert.equal(search.items[0].structure.symbolName, 'CancelJob');
  assert.equal(search.coverage.complete, false);
  assert.ok(search.structuredChannels.symbolCandidates > 0);
  const item = search.items[0];
  const whole = parsed(await f.run(context, 'knowledge.read', { sourceRef: item.sourceRef, mode: 'unit', limit: 8000 }));
  assert.equal(whole.window.mode, 'unit');
  assert.match(whole.text, /return "cancelled"/u);
  assert.equal(whole.window.unit.qualifiedName, 'Kynxa.JobIndex.CancelJob');
  const listed = (await retrieval.library.list(f.projectId)).sources.find(entry => entry.id === canonical.sourceId);
  assert.ok(listed);
  await retrieval.removeSource(canonical.sourceId, { expectedRevision: listed.revision });
  await assert.rejects(retrieval.read(context, { sourceRef: item.sourceRef, mode: 'unit' }), error =>
    ['RETRIEVAL_SOURCE_NOT_FOUND', 'STALE_RETRIEVAL_SOURCE'].includes(error.code));
});

test('cancelled parsing stays bounded until acknowledged and timeout retires the worker before recovery', async () => {
  class PausedWorker extends EventEmitter {
    calls = [];
    postMessage(value) { this.calls.push(value); }
    ref() {}
    unref() {}
    terminated = 0;
    async terminate() { this.terminated++; this.emit('exit', 0); return 0; }
  }
  const workers = [], structures = new RetrievalStructureService({ workerFactory: () => {
    const worker = new PausedWorker(); workers.push(worker); return worker;
  }, timeoutMs: 10 });
  try {
    for (let index = 0; index < 32; index++) {
      const controller = new AbortController();
      const pending = structures.parse(source(`cancel-${index}`, 'doc.md', 'Temporary text'), { signal: controller.signal });
      const rejected = assert.rejects(pending, { name: 'AbortError' });
      controller.abort(); await rejected;
    }
    const worker = workers[0];
    assert.equal(worker.calls.length, 32);
    assert.equal(structures.status().pendingRequests, 0);
    assert.equal(structures.status().inFlightRequests, 32);
    await assert.rejects(structures.parse(source('blocked', 'doc.md', 'New text')), { code: 'STRUCTURE_QUEUE_FULL' });
    assert.equal(worker.calls.length, 32);
    for (const call of worker.calls) worker.emit('message', { id: call.id, result: { structure: { parseStatus: 'parsed' }, chunks: [] } });
    assert.equal(structures.status().cacheEntries, 0);
    await assert.rejects(structures.parse(source('timeout', 'doc.md', 'New text')), { code: 'STRUCTURE_PARSE_TIMED_OUT' });
    await structures.retiring;
    assert.equal(worker.terminated, 1);
    assert.equal(structures.status().inFlightRequests, 0);
    assert.equal(structures.worker, null);
    worker.emit('message', { id: worker.calls.at(-1).id, error: { code: 'ABORT_ERR', name: 'AbortError', message: 'Cancelled' } });
    assert.equal(structures.status().inFlightRequests, 0);
    const pending = structures.parse(source('failed', 'doc.md', 'Text'));
    const rejected = assert.rejects(pending, { code: 'STRUCTURE_WORKER_FAILED' });
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(workers.length, 2);
    workers[1].emit('error', new Error('Synthetic worker failure')); await rejected;
    assert.equal(structures.status().state, 'unavailable');
    assert.equal(structures.status().cacheEntries, 0);
    // This intentionally paused replacement still times out, but permanent failure no longer blocks a fresh request.
    // 此夹具的替代 worker 仍故意暂停并超时，但后续新请求不再被一次旧故障永久阻断。
    await assert.rejects(structures.parse(source('later', 'doc.md', 'Text')), { code: 'STRUCTURE_PARSE_TIMED_OUT' });
    assert.equal(structures.status().workerRestarts, 2);
  } finally { await structures.close(); }
  const unavailable = new RetrievalStructureService({ workerFactory: () => { throw new Error('Synthetic startup failure'); } });
  await assert.rejects(unavailable.parse(source('missing', 'doc.md', 'Text')), { code: 'STRUCTURE_WORKER_FAILED' });
  assert.equal(unavailable.status().workerStarted, false);
  await unavailable.close();
});

test('parser worker exit permits a bounded fresh request while stale events and cancelled requests never replay', async () => {
  class RecoverableWorker extends EventEmitter {
    calls = [];
    terminated = 0;
    postMessage(value) { this.calls.push(value); }
    ref() {}
    unref() {}
    async terminate() { this.terminated++; this.emit('exit', 0); return 0; }
    complete() { this.emit('message', { id: this.calls.at(-1).id,
      result: { structure: { parseStatus: 'parsed' }, chunks: [] } }); }
  }
  const workers = [], structures = new RetrievalStructureService({ workerFactory: () => {
    const worker = new RecoverableWorker(); workers.push(worker); return worker;
  } });
  try {
    const old = structures.parse(source('old', 'old.md', 'Old original'));
    const rejected = assert.rejects(old, { code: 'STRUCTURE_WORKER_EXITED' });
    workers[0].emit('exit', 1); await rejected;
    const cancelled = new AbortController(); cancelled.abort();
    await assert.rejects(structures.parse(source('cancelled', 'cancelled.md', 'Cancelled'), { signal: cancelled.signal }), { name: 'AbortError' });
    assert.equal(workers.length, 1);
    const fresh = structures.parse(source('fresh', 'fresh.md', 'Fresh original'));
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(workers.length, 2); assert.equal(workers[0].terminated, 1);
    assert.equal(workers[0].calls.length, 1); assert.equal(workers[1].calls.length, 1);
    workers[0].emit('error', new Error('Late old error')); workers[0].complete();
    workers[1].complete(); await fresh;
    assert.equal(structures.status().state, 'idle'); assert.equal(structures.status().workerRestarts, 1);
    workers[1].emit('exit', 1);
    const second = structures.parse(source('second', 'second.md', 'Second recovery'));
    await new Promise(resolve => setImmediate(resolve));
    workers[2].complete(); await second;
    workers[2].emit('exit', 1);
    await assert.rejects(structures.parse(source('bounded', 'bounded.md', 'No further worker')), { code: 'STRUCTURE_WORKER_EXITED' });
    assert.equal(workers.length, 3); assert.equal(structures.status().workerRestarts, 2);
  } finally { await structures.close(); }
  await assert.rejects(structures.parse(source('closed', 'closed.md', 'No revival')), { code: 'STRUCTURE_SERVICE_CLOSED' });
});

test('a terminated real parser worker can parse a later source in a replacement process', async () => {
  const structures = new RetrievalStructureService();
  try {
    const first = await structures.parse(source('real-first', 'first.js', 'function first() { return 1; }'));
    assert.equal(first.structure.parseStatus, 'parsed');
    const oldWorker = structures.worker;
    await oldWorker.terminate();
    assert.equal(structures.status().state, 'unavailable');
    const second = await structures.parse(source('real-second', 'second.js', 'function second() { return 2; }'));
    assert.notEqual(structures.worker, oldWorker);
    assert.equal(second.structure.parseStatus, 'parsed');
    assert.ok(second.chunks.some(chunk => chunk.structure.symbolName === 'second'));
    assert.equal(structures.status().workerRestarts, 1);
  } finally { await structures.close(); }
});

function comparisonCorpus() {
  const cases = [
    ['JobIndex', 'CancelJob', 'cs', 'Kynxa.JobIndex.CancelJob'],
    ['SourceStore', 'ReadSource', 'mjs', 'SourceStore.ReadSource'],
    ['WorkspaceAgent', 'OpenWorkspace', 'ts', 'WorkspaceAgent.OpenWorkspace'],
    ['ReportPanel', 'RenderReport', 'tsx', 'RenderReport']
  ];
  const documents = [], queries = [];
  for (const [owner, name, extension, qualified] of cases) {
    const marker = `receipt-from-${owner}-${name}`, filename = `${owner}.${extension}`;
    const method = extension === 'cs' ? `public string ${name}() { return "${marker}"; }`
      : extension === 'tsx' ? `export function ${name}() { return <div>${marker}</div>; }`
        : `${name}() { return "${marker}"; }`;
    const content = extension === 'cs' ? `namespace Kynxa; class ${owner} { ${method} }`
      : extension === 'tsx' ? method : `export class ${owner} { ${method} }`;
    documents.push(source(owner, filename, content));
    // These distractors are real documentation mentions, not executable declarations.
    // 干扰资料是真实的文档提及，不把说明文字当作可执行声明；标签来自自造原文中的独立返回标记。
    for (let number = 0; number < 6; number++) documents.push(source(`guide-${owner}-${number}`, `${owner}-guide-${number}.md`,
      `# ${qualified}\n\n${qualified} ${name} ${owner} definition reference. ${qualified} ${qualified}.`));
    const gold = [{ sourceId: owner, evidence: [marker] }];
    queries.push({ queryId: `${owner}:definition`, query: `${qualified} definition`, symbol: qualified, gold, kind: 'code' });
    queries.push({ queryId: `${owner}:path`, query: `在 ${filename} 中找到 ${name} 定义`, path: filename, symbol: name, gold, kind: 'code' });
  }
  for (const [name, body] of [['取消作业', '取消前记录已经完成的批次，未提交的任务才停止。'],
    ['并发发布', '两个写入不能覆盖同一来源的版本。'], ['依赖安装', '安装前检查运行环境和依赖是否存在。'], ['任务恢复', '恢复之前先核验执行回执。']]) {
    const sourceId = `document-${name}`, marker = `${name}的独立证据`;
    documents.push(source(sourceId, `${name}.md`, `# ${name}\n\n${body}\n\n## 验证\n\n${marker}：${body}\n`));
    queries.push({ queryId: sourceId, query: `${name}的独立证据`, gold: [{ sourceId, evidence: [marker] }], kind: 'knowledge' });
  }
  const joinedSources = [['handover-owner', 'handover Owner receipt: Rowan.'],
    ['handover-condition', 'handover Required condition receipt: verified backup.']];
  for (const [sourceId, text] of joinedSources) documents.push(source(sourceId, `${sourceId}.md`, text));
  queries.push({ queryId: 'multi-source-evidence', query: 'handover receipt', kind: 'knowledge',
    gold: joinedSources.map(([sourceId, evidence]) => ({ sourceId, evidence: [evidence] })) });
  queries.push({ queryId: 'absent-evidence', query: 'zzragunavailabledevelopment20261008zz',
    kind: 'knowledge', gold: [], noAnswer: true });
  return { documents, queries };
}

test('fixed local comparison reports paired development evidence, no-answer results and observed latency', async t => {
  const { structures, legacy, upgraded } = await indexFixture(t), corpus = comparisonCorpus();
  const dataset = createRetrievalEvaluationDataset({ datasetId: 'controlled-structure', datasetVersion: 'development-v2',
    partition: 'development', sources: corpus.documents, queries: corpus.queries });
  const artifactDirectory = fileURLToPath(new URL('../../../artifacts/verification/rag-structure-20261008/', import.meta.url));
  await mkdir(artifactDirectory, { recursive: true });
  const output = await mkdtemp(join(artifactDirectory, 'development-'));
  const snapshot = JSON.stringify({ datasetId: dataset.identity.datasetId, datasetVersion: dataset.identity.datasetVersion,
    partition: dataset.identity.partition, sources: corpus.documents.map(({ sourceId, text }) => ({ sourceId, text })),
    queries: dataset.queries }, null, 2) + '\n';
  await writeFile(join(output, 'dataset.json'), snapshot, { flag: 'wx' });
  const started = performance.now(), prepared = [];
  for (const input of corpus.documents) {
    const parsedSource = await structures.parse(input);
    prepared.push({ ...input, ...parsedSource });
  }
  await Promise.all([legacy.upsertSources(corpus.documents), upgraded.upsertSources(prepared)]);
  const preparationMs = performance.now() - started;
  const score = async (index, structured) => {
    const observations = [];
    for (const gold of corpus.queries) {
      const started = performance.now();
      try {
        const result = await index.search({ query: gold.query, scopeKeys: ['user'], limit: 5,
          ...(structured ? { retrievalIntent: buildRetrievalIntent(gold.query,
            { domain: gold.kind, symbol: gold.symbol, path: gold.path }) } : {}) });
        observations.push({ queryId: gold.queryId, status: 'completed', durationMs: performance.now() - started,
          items: result.items.map(({ sourceId, excerpt }) => ({ sourceId, excerpt })),
          diagnosticCodes: result.degradedReason ? ['EVALUATION_SEARCH_DEGRADED'] : [] });
      } catch (error) {
        observations.push({ queryId: gold.queryId, status: 'failed', durationMs: performance.now() - started,
          diagnosticCodes: [typeof error.code === 'string' && error.code.trim() ? error.code : 'EVALUATION_SEARCH_FAILED'] });
      }
    }
    return observations;
  };
  const evaluation = retrievalEvaluationReport(dataset, { baseline: await score(legacy, false), structured: await score(upgraded, true) });
  const { baseline, structured } = evaluation.methods;
  await writeFile(join(output, 'retrieval-comparison.json'), JSON.stringify({
    benchmark: 'controlled-structure-regression-v2', scope: 'synthetic local development component comparison; not heldout/model/Agent performance',
    model: null, embeddings: 'disabled equally in both arms', vectorBackend: false,
    latencyMeasurement: 'One search per query and method, including failed attempts; cache state is not classified',
    corpusSources: corpus.documents.length, queries: corpus.queries.length,
    datasetSnapshot: { filename: 'dataset.json', sha256: createHash('sha256').update(snapshot).digest('hex') },
    gold: 'independent source IDs and literal evidence spans, including multi-source and no-answer cases', preparationMs, evaluation
  }, null, 2), { flag: 'wx' });
  assert.equal(structured.counts.completed, corpus.queries.length);
  assert.equal(structured.counts.failed, 0);
  assert.equal(structured.counts.skipped, 0);
  assert.equal(structured.metrics.allEvidenceAt5.value, 1, 'all independent code/doc spans must be retrievable');
  assert.ok(structured.metrics.evidenceHitAt1.value >= baseline.metrics.evidenceHitAt1.value);
  assert.equal(structured.noAnswerEmptyResult.value, 1);
});

import assert from 'node:assert/strict';
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

test('code queries reach automatic retrieval and exact file names are not mistaken for declarations', () => {
  for (const message of ['查找 CancelJob 方法的定义', '仓库中的 CancelJob 在哪里', '解释 `JobIndex.CancelJob`']) {
    assert.equal(retrievalPlan(message).shouldRetrieve, true, message);
    assert.equal(retrievalPlan(message).domain, 'code', message);
  }
  assert.deepEqual(buildRetrievalIntent('查看 `settings.json` 中的设置'), { domain: 'mixed', path: 'settings.json' });
  assert.deepEqual(buildRetrievalIntent('在 JobIndex.cs 查找 CancelJob 方法'), { domain: 'code', symbol: 'CancelJob', path: 'jobindex.cs' });
  assert.equal(retrievalPlan('你好').shouldRetrieve, false);
  assert.equal(retrievalPlan('运行 node 验证脚本').shouldRetrieve, false);
  assert.equal(retrievalPlan('然后它怎么处理取消', { history: [{ Role: 'user', Content: '仓库中 CancelJob 方法如何处理取消' }] }).shouldRetrieve, true);
});

test('real parser worker preserves declaration ranges, raw text and a bounded reusable derivation', async t => {
  const { structures } = await indexFixture(t);
  const text = 'namespace Kynxa;\r\nclass JobIndex {\r\n  public bool CancelJob() { return true; }\r\n}\r\n';
  const input = source('job-index', 'JobIndex.cs', text);
  const first = await structures.parse(input);
  assert.equal(first.structure.language, 'csharp');
  assert.equal(first.structure.parseStatus, 'parsed');
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

test('cancelled and timed-out parsing remains bounded until acknowledged; failures close without stale caches', async () => {
  class PausedWorker extends EventEmitter {
    calls = [];
    postMessage(value) { this.calls.push(value); }
    ref() {}
    unref() {}
    async terminate() { this.emit('exit', 0); return 0; }
  }
  const worker = new PausedWorker(), structures = new RetrievalStructureService({ workerFactory: () => worker, timeoutMs: 10 });
  try {
    for (let index = 0; index < 32; index++) {
      const controller = new AbortController();
      const pending = structures.parse(source(`cancel-${index}`, 'doc.md', 'Temporary text'), { signal: controller.signal });
      const rejected = assert.rejects(pending, { name: 'AbortError' });
      controller.abort(); await rejected;
    }
    assert.equal(worker.calls.length, 32);
    assert.equal(structures.status().pendingRequests, 0);
    assert.equal(structures.status().inFlightRequests, 32);
    await assert.rejects(structures.parse(source('blocked', 'doc.md', 'New text')), { code: 'STRUCTURE_QUEUE_FULL' });
    assert.equal(worker.calls.length, 32);
    for (const call of worker.calls) worker.emit('message', { id: call.id, result: { structure: { parseStatus: 'parsed' }, chunks: [] } });
    assert.equal(structures.status().cacheEntries, 0);
    await assert.rejects(structures.parse(source('timeout', 'doc.md', 'New text')), { code: 'STRUCTURE_PARSE_TIMED_OUT' });
    assert.equal(structures.status().inFlightRequests, 1);
    worker.emit('message', { id: worker.calls.at(-1).id, error: { code: 'ABORT_ERR', name: 'AbortError', message: 'Cancelled' } });
    assert.equal(structures.status().inFlightRequests, 0);
    const pending = structures.parse(source('failed', 'doc.md', 'Text'));
    const rejected = assert.rejects(pending, { code: 'STRUCTURE_WORKER_FAILED' });
    worker.emit('error', new Error('Synthetic worker failure')); await rejected;
    assert.equal(structures.status().state, 'unavailable');
    assert.equal(structures.status().cacheEntries, 0);
    await assert.rejects(structures.parse(source('later', 'doc.md', 'Text')), { code: 'STRUCTURE_WORKER_FAILED' });
  } finally { await structures.close(); }
  const unavailable = new RetrievalStructureService({ workerFactory: () => { throw new Error('Synthetic startup failure'); } });
  await assert.rejects(unavailable.parse(source('missing', 'doc.md', 'Text')), { code: 'STRUCTURE_WORKER_FAILED' });
  assert.equal(unavailable.status().workerStarted, false);
  await unavailable.close();
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
    queries.push({ query: `${qualified} definition`, symbol: qualified, targetId: owner, marker, kind: 'code' });
    queries.push({ query: `在 ${filename} 中找到 ${name} 定义`, path: filename, symbol: name, targetId: owner, marker, kind: 'code' });
  }
  for (const [name, body] of [['取消作业', '取消前记录已经完成的批次，未提交的任务才停止。'],
    ['并发发布', '两个写入不能覆盖同一来源的版本。'], ['依赖安装', '安装前检查运行环境和依赖是否存在。'], ['任务恢复', '恢复之前先核验执行回执。']]) {
    const sourceId = `document-${name}`, marker = `${name}的独立证据`;
    documents.push(source(sourceId, `${name}.md`, `# ${name}\n\n${body}\n\n## 验证\n\n${marker}：${body}\n`));
    queries.push({ query: `${name}的独立证据`, targetId: sourceId, marker, kind: 'knowledge' });
  }
  return { documents, queries };
}

test('fixed local comparison reports independent evidence hits and warm latency for old and structured pipelines', async t => {
  const { structures, legacy, upgraded } = await indexFixture(t), corpus = comparisonCorpus();
  const started = performance.now(), prepared = [];
  for (const input of corpus.documents) {
    const parsedSource = await structures.parse(input);
    prepared.push({ ...input, ...parsedSource });
  }
  await Promise.all([legacy.upsertSources(corpus.documents), upgraded.upsertSources(prepared)]);
  const preparationMs = performance.now() - started;
  const score = async (index, structured) => {
    let hits1 = 0, hits5 = 0;
    const latencies = [], rows = [];
    for (const gold of corpus.queries) {
      const started = performance.now();
      const result = await index.search({ query: gold.query, scopeKeys: ['user'], limit: 5,
        ...(structured ? { retrievalIntent: buildRetrievalIntent(gold.query,
          { domain: gold.kind, symbol: gold.symbol, path: gold.path }) } : {}) });
      latencies.push(performance.now() - started);
      const hit = item => item.sourceId === gold.targetId && item.excerpt.includes(gold.marker);
      const rank = result.items.findIndex(hit);
      if (rank === 0) hits1++;
      if (rank >= 0) hits5++;
      rows.push({ query: gold.query, expectedSourceId: gold.targetId, relevantEvidenceRank: rank < 0 ? null : rank + 1 });
    }
    latencies.sort((left, right) => left - right);
    return { queries: corpus.queries.length, evidenceHitAt1: hits1 / corpus.queries.length,
      evidenceHitAt5: hits5 / corpus.queries.length, p50Ms: latencies[Math.floor(latencies.length * 0.5)],
      p95Ms: latencies[Math.min(latencies.length - 1, Math.ceil(latencies.length * 0.95) - 1)], rows };
  };
  const baseline = await score(legacy, false), structured = await score(upgraded, true);
  assert.equal(structured.evidenceHitAt5, 1, 'all independent code/doc markers must be retrievable');
  assert.ok(structured.evidenceHitAt1 >= baseline.evidenceHitAt1);
  const artifactDirectory = fileURLToPath(new URL('../../../artifacts/verification/rag-structure-20261007/', import.meta.url));
  await mkdir(artifactDirectory, { recursive: true });
  await writeFile(join(artifactDirectory, 'retrieval-comparison.json'), JSON.stringify({
    benchmark: 'controlled-structure-regression-v1', scope: 'synthetic local component comparison; not a public benchmark or model/Agent success score',
    model: null, embeddings: 'disabled equally in both arms', vectorBackend: false,
    corpusSources: corpus.documents.length, queries: corpus.queries.length,
    gold: 'independent literal evidence markers in fixture source, not parser-produced labels', preparationMs, baseline, structured
  }, null, 2));
});

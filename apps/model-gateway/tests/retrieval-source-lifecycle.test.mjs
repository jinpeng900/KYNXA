import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, readdir, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { EventEmitter } from 'node:events';
import { Readable } from 'node:stream';
import { join, relative, resolve, sep } from 'node:path';
import { SourceLibrary } from '../data/retrieval/source-library.mjs';
import { RetrievalJobStore } from '../data/retrieval/job-store.mjs';
import { hashText } from '../data/retrieval/retrieval-contracts.mjs';
import { SourceIndexService } from '../orchestration/retrieval/source-manager.mjs';
import { SourceIndexer } from '../orchestration/retrieval/source-indexer.mjs';
import { handleRetrievalRoute } from '../orchestration/retrieval/http-routes.mjs';
import { atomicJson } from '../platform/atomic-json.mjs';
import { readSourceTree } from '../tools/retrieval/source-reader.mjs';
import { RetrievalIndex, chunkSource } from '../data/retrieval/index.mjs';

async function resumableFixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-index-recovery-'));
  const library = new SourceLibrary({ root, conversationStore: {
    describeProject: async projectId => ({ projectId, isArchived: false, isFolderlessWorkspace: false })
  } });
  const index = new RetrievalIndex({ root });
  const settings = { local: { enabled: true, semantic: 'on', embeddingProfileId: 'builtin-multilingual', indexing: { batchSize: 2 } },
    cache: { memoryLimitBytes: 16 * 1024 * 1024 }, projectIndexing: { mountedFolder: false, bindingRevision: 0 } };
  const services = [], entered = gate(), blocked = gate();
  let workspace = null;
  const createService = ({ blockAt = 0 } = {}) => {
    const counts = { parsed: 0, embedded: 0 };
    const structures = { version: () => 'synthetic-structure-v1', parse: async (source, { signal }) => {
      if (++counts.parsed === blockAt) { entered.release(); await blocked.promise; signal.throwIfAborted(); }
      return { structure: { domain: 'knowledge', language: 'text', parserVersion: 'synthetic-parser-v1',
        parseStatus: 'parsed', diagnosticCodes: [] }, parserVersion: 'synthetic-parser-v1',
        chunkerVersion: 'character-v1', embeddingInputVersion: 'source-context-v1', chunks: chunkSource(source) };
    } };
    const embeddings = { status: () => ({ state: 'ready', modelVersion: 'synthetic-embedding-v1',
      embeddingSpaceId: '1'.repeat(64), dimensions: 2 }), embedDocuments: async texts => {
      counts.embedded++;
      return { profileId: 'builtin-multilingual', modelVersion: 'synthetic-embedding-v1',
        embeddingSpaceId: '1'.repeat(64), dimensions: 2, vectors: texts.map(() => [1, 0]) };
    } };
    const jobs = new RetrievalJobStore(root);
    const service = new SourceIndexService({ library, jobs, index, structures, embeddings,
      getProject: async projectId => projectId ? { FolderPath: workspace } : null,
      effectiveSettings: async () => structuredClone(settings), serialize: operation => operation() });
    services.push(service);
    return { service, jobs, counts };
  };
  t.after(async () => {
    blocked.release();
    for (const service of services) await service.close();
    await index.close();
    const suffix = relative(resolve(tmpdir()), resolve(root));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(root, { recursive: true, force: true });
  });
  const added = await library.add(Array.from({ length: 5 }, (_, index) => ({ path: join(root, `source-${index}.md`),
    title: `Source ${index}`, text: `Evidence for the restart-safe source ${index}.` })));
  const first = createService({ blockAt: 3 });
  const pause = async options => {
    const job = await first.service.rebuild(options);
    await entered.promise;
    await first.service.close({ releaseInference: () => blocked.release() });
    return first.jobs.get(job.jobId);
  };
  return { root, library, index, settings, added, createService, first, pause, entered, blocked,
    setWorkspace: path => { workspace = path; } };
}

function gate() {
  let release;
  const promise = new Promise(resolveGate => { release = resolveGate; });
  return { promise, release };
}

async function fixture(t, overrides = {}) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-source-lifecycle-'));
  const library = new SourceLibrary({ root, conversationStore: {
    describeProject: async projectId => ({ projectId, isArchived: false, isFolderlessWorkspace: false })
  } });
  const jobs = new RetrievalJobStore(root);
  const publications = [];
  const index = { upsertSources: async (sources, { signal } = {}) => {
    signal?.throwIfAborted(); publications.push(...sources.map(source => source.sourceId));
    return { sources: sources.map(source => ({ sourceId: source.sourceId })) };
  }, removeSource: async () => {}, listSources: async () => [] };
  const settings = { local: { enabled: true, semantic: 'off', embeddingProfileId: 'builtin-multilingual' },
    cache: { memoryLimitBytes: 16 * 1024 * 1024 }, projectIndexing: { mountedFolder: false, bindingRevision: 0 } };
  let queue = Promise.resolve();
  const serialize = operation => { const pending = queue.catch(() => {}).then(operation); queue = pending; return pending; };
  const service = new SourceIndexService({ library, jobs, index,
    embeddings: { status: () => ({ state: 'unavailable' }) }, getProject: async () => null,
    effectiveSettings: async () => settings, serialize, ...overrides });
  t.after(async () => {
    await service.close();
    const suffix = relative(resolve(tmpdir()), resolve(root));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(root, { recursive: true, force: true });
  });
  return { root, library, jobs, service, index, publications, settings };
}

test('cancelled import approval never reads or registers source bytes', async t => {
  const entered = gate(), approved = gate();
  let reads = 0, approvalSignal;
  const f = await fixture(t, { authorizeImport: async (_, { signal }) => {
    approvalSignal = signal; entered.release(); await approved.promise;
  }, readTree: async () => { reads++; return []; } });
  const controller = new AbortController();
  const importing = f.service.import({ path: join(f.root, 'not-read.md') }, { signal: controller.signal, permissionMode: 'Ask' });
  await entered.promise;
  controller.abort();
  assert.equal(approvalSignal.aborted, true);
  approved.release();
  await assert.rejects(importing, { name: 'AbortError' });
  assert.equal(reads, 0);
  assert.deepEqual((await f.library.list()).sources, []);
  assert.deepEqual(await f.jobs.list(), []);
});

test('source registration cancellation removes only uncommitted owned snapshots', async t => {
  const f = await fixture(t);
  const existing = await f.library.add([{ path: join(f.root, 'existing.md'), title: 'Existing', text: '已经登记的资料。' }]);
  const controller = new AbortController();
  const originalTarget = f.library._target.bind(f.library);
  let targetChecks = 0;
  f.library._target = async (...args) => {
    const target = await originalTarget(...args);
    if (++targetChecks === 2) controller.abort();
    return target;
  };
  await assert.rejects(f.library.add([{ path: join(f.root, 'cancelled.md'), title: 'Cancelled', text: '不能晚到登记。' }],
    { signal: controller.signal }), { name: 'AbortError' });
  const catalog = await f.library.list();
  assert.deepEqual(catalog.sources.map(source => source.id), [existing.sources[0].id]);
  const directories = await readdir(f.library.folder, { withFileTypes: true });
  for (const directory of directories.filter(entry => entry.isDirectory())) {
    const files = await readdir(join(f.library.folder, directory.name, 'source'));
    assert.deepEqual(files, directory.name === existing.sources[0].id ? ['document.txt'] : []);
  }
});

test('atomic JSON cancellation preserves the previous committed document and leaves no temporary file', async t => {
  const f = await fixture(t);
  const path = join(f.root, 'atomic-test.json');
  await atomicJson(path, { committed: 1 });
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(atomicJson(path, { committed: 2 }, { signal: controller.signal }), { name: 'AbortError' });
  assert.deepEqual(JSON.parse(await readFile(path, 'utf8')), { committed: 1 });
  assert.deepEqual((await readdir(f.root)).filter(name => name.startsWith('atomic-test.json.')), []);
});

test('a failed atomic rename preserves the destination and cleans its owned temporary file', async t => {
  const f = await fixture(t);
  const path = join(f.root, 'existing-directory');
  await mkdir(path);
  await assert.rejects(atomicJson(path, { value: 'cannot replace a directory' }));
  assert.deepEqual(await readdir(path), []);
  assert.deepEqual((await readdir(f.root)).filter(name => name.startsWith('existing-directory.')), []);
});

test('cancellation after source registration preserves the committed receipt without starting indexing', async t => {
  const f = await fixture(t, { readTree: async path => [{ path, title: 'Committed', text: '正式提交之后不回滚。' }] });
  const controller = new AbortController();
  const add = f.library.add.bind(f.library);
  f.library.add = async (...args) => {
    const committed = await add(...args);
    controller.abort();
    return committed;
  };
  const result = await f.service.import({ path: join(f.root, 'committed.md') }, { signal: controller.signal });
  assert.equal(result.status, 'ready');
  assert.equal(result.indexingStatus, 'pending');
  assert.equal(result.jobId, undefined);
  assert.equal((await f.library.list()).sources[0].id, result.id);
  assert.deepEqual(await f.jobs.list(), []);
});

test('index cancellation drains pending inference and retains earlier committed batches and progress', async t => {
  const entered = gate(), inference = gate();
  let calls = 0;
  const embeddings = { status: () => ({ state: 'ready', modelVersion: 'fixture-v1', embeddingSpaceId: 'fixture-space' }),
    embedDocuments: async texts => {
      if (++calls === 5) { entered.release(); await inference.promise; }
      return { profileId: 'builtin-multilingual', modelVersion: 'fixture-v1', embeddingSpaceId: 'fixture-space',
        vectors: texts.map(() => [1, 0]) };
    } };
  const f = await fixture(t, { embeddings });
  f.settings.local.semantic = 'on';
  await f.library.add(Array.from({ length: 5 }, (_, index) => ({ path: join(f.root, `doc-${index}.md`),
    title: `Document ${index}`, text: `已经授权的第 ${index} 份资料。` })));
  const job = await f.service.rebuild();
  await entered.promise;
  assert.equal(f.publications.length, 4);
  let acknowledged = false;
  const cancelling = f.service.cancelJob(job.jobId).then(result => { acknowledged = true; return result; });
  await new Promise(resolveNext => setImmediate(resolveNext));
  assert.equal(acknowledged, false);
  inference.release();
  const stopped = await cancelling;
  assert.equal(stopped.status, 'cancelled');
  assert.equal(stopped.completedSources, 4);
  assert.equal(stopped.totalSources, 5);
  assert.equal(f.publications.length, 4);
  assert.equal(f.service.lifecycle.active.size, 0);
  assert.equal((await f.library.list()).sources.length, 5);
});

test('close releases pending inference, cancels source imports and drains owned work without closing foreign resources', async t => {
  const embedEntered = gate(), importEntered = gate(), pendingReads = gate();
  let reads = 0, closes = 0;
  const embeddings = { status: () => ({ state: 'ready', modelVersion: 'fixture-v1' }),
    embedDocuments: async texts => { embedEntered.release(); await pendingReads.promise; return { vectors: texts.map(() => [1, 0]) }; },
    close: async () => { closes++; } };
  const f = await fixture(t, { embeddings, readTree: async () => {
    reads++; importEntered.release(); await pendingReads.promise;
    return [{ path: 'synthetic.md', title: 'Synthetic', text: '不会提交的导入。' }];
  } });
  f.settings.local.semantic = 'on';
  await f.library.add([{ path: join(f.root, 'existing.md'), title: 'Existing', text: '现有资料。' }]);
  const job = await f.service.rebuild();
  await embedEntered.promise;
  const importing = f.service.import({ path: join(f.root, 'pending.md') });
  const rejectedImport = assert.rejects(importing, { name: 'AbortError' });
  await importEntered.promise;
  await f.service.close({ releaseInference: async () => { pendingReads.release(); } });
  await rejectedImport;
  assert.equal(reads, 1);
  assert.equal(closes, 0);
  assert.equal(f.publications.length, 0);
  assert.equal(f.service.lifecycle.active.size, 0);
  assert.equal(f.service.operations.size, 0);
  assert.equal((await f.jobs.get(job.jobId)).status, 'cancelled');
  assert.equal((await f.library.list()).sources.length, 1);
  await assert.rejects(f.service.rebuild(), { code: 'RETRIEVAL_CLOSED' });
});

test('concurrent rebuild admission deduplicates active jobs and terminal jobs cannot resume', async t => {
  const entered = gate(), inference = gate();
  const f = await fixture(t, { embeddings: {
    status: () => ({ state: 'ready', modelVersion: 'fixture-v1' }),
    embedDocuments: async texts => { entered.release(); await inference.promise; return { vectors: texts.map(() => [1, 0]) }; }
  } });
  f.settings.local.semantic = 'on';
  await f.library.add([{ path: join(f.root, 'one.md'), title: 'One', text: '被去重的资料。' }]);
  const [first, second] = await Promise.all([f.service.rebuild(), f.service.rebuild()]);
  assert.equal(first.jobId, second.jobId);
  assert.equal((await f.jobs.list()).length, 1);
  await entered.promise;
  inference.release();
  await f.service.lifecycle.active.get(first.jobId)?.promise;
  assert.equal((await f.service.cancelJob(first.jobId)).status, 'completed');
  await assert.rejects(f.jobs.update(first.jobId, { status: 'running' }), { code: 'RETRIEVAL_JOB_STATE_CONFLICT' });
});

test('startup recovery preserves completed batch counts and marks unfinished jobs interrupted without executing them', async t => {
  const f = await fixture(t);
  const interrupted = await f.jobs.create('synthetic-project');
  await f.jobs.update(interrupted.jobId, { status: 'running', totalSources: 10, completedSources: 4 });
  const queued = await f.jobs.create();
  const recovered = new RetrievalJobStore(f.root);
  await recovered.recover();
  assert.deepEqual({ status: (await recovered.get(interrupted.jobId)).status,
    count: (await recovered.get(interrupted.jobId)).completedSources,
    error: (await recovered.get(interrupted.jobId)).error }, { status: 'failed', count: 4, error: 'INDEX_JOB_INTERRUPTED' });
  assert.equal((await recovered.get(queued.jobId)).status, 'failed');
  assert.equal(f.publications.length, 0);
});

test('source metadata and parser versions invalidate fingerprints without changing original source identity', async t => {
  const f = await fixture(t);
  const source = { sourceId: 'synthetic-source', scopeKey: 'user', sourceType: 'document', title: 'Original', locator: {},
    text: '版本与源码哈希不同的派生。', contentHash: hashText('版本与源码哈希不同的派生。') };
  const indexer = new SourceIndexer({ library: f.library, index: f.index,
    embeddings: { status: () => ({ state: 'unavailable' }) }, serialize: operation => operation(),
    effectiveSettings: async () => f.settings, getProject: async () => null });
  await indexer.upsert([source], f.settings, undefined, undefined, { semantic: false });
  await indexer.upsert([source], f.settings, undefined, undefined, { semantic: false });
  assert.equal(f.publications.length, 1);
  await indexer.upsert([{ ...source, title: 'Renamed' }], f.settings, undefined, undefined, { semantic: false });
  await indexer.upsert([{ ...source, title: 'Renamed', parserVersion: 'fixture-parser-v2' }], f.settings, undefined, undefined, { semantic: false });
  assert.deepEqual(f.publications, [source.sourceId, source.sourceId, source.sourceId]);
});

test('pre-cancelled tree scan rejects before inspecting the requested path', async t => {
  const f = await fixture(t);
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(readSourceTree(join(f.root, 'does-not-exist'), { signal: controller.signal }), { name: 'AbortError' });
});

test('a later import remains usable after the previous read was cancelled', async t => {
  const entered = gate(), readGate = gate();
  let reads = 0;
  const f = await fixture(t, { readTree: async (path, { signal }) => {
    if (++reads === 1) { entered.release(); await readGate.promise; signal.throwIfAborted(); }
    return [{ path, title: 'Independent request', text: '取消不会污染后续来源。' }];
  } });
  const controller = new AbortController();
  const first = f.service.import({ path: join(f.root, 'one.md') }, { signal: controller.signal });
  await entered.promise;
  controller.abort(); readGate.release();
  await assert.rejects(first, { name: 'AbortError' });
  const imported = await f.service.import({ path: join(f.root, 'two.md') });
  await f.service.lifecycle.active.get(imported.jobId)?.promise;
  assert.equal(imported.status, 'ready');
  assert.equal((await f.jobs.get(imported.jobId)).status, 'completed');
  assert.equal((await f.library.list()).sources.length, 1);
});

test('importing another source during indexing schedules a fresh pass in the same owned job', async t => {
  const entered = gate(), inference = gate();
  const f = await fixture(t, { embeddings: {
    status: () => ({ state: 'ready', modelVersion: 'fixture-v1' }),
    embedDocuments: async texts => { entered.release(); await inference.promise; return { vectors: texts.map(() => [1, 0]) }; }
  } });
  f.settings.local.semantic = 'auto';
  const firstPath = join(f.root, 'a.md'), secondPath = join(f.root, 'b.md');
  await writeFile(firstPath, 'First registered document.', 'utf8');
  await writeFile(secondPath, 'Second registered document.', 'utf8');
  const first = await f.service.import({ path: firstPath });
  await entered.promise;
  const second = await f.service.import({ path: secondPath });
  assert.equal(first.jobId, second.jobId);
  inference.release();
  await f.service.lifecycle.active.get(first.jobId)?.promise;
  assert.deepEqual(new Set(f.publications), new Set([first.id, second.id]));
  const completed = await f.jobs.get(first.jobId);
  assert.equal(completed.status, 'completed');
  assert.equal(completed.completedSources, 2);
  assert.equal(completed.totalSources, 2);
  assert.equal((await f.library.list()).sources.length, 2);
});

test('mounted folder changes during inference are re-scanned without publishing the obsolete bytes', async t => {
  const entered = gate(), inference = gate();
  let workspace;
  const f = await fixture(t, { getProject: async projectId => projectId ? { FolderPath: workspace } : null,
    embeddings: {
      status: () => ({ state: 'ready', modelVersion: 'fixture-v1' }),
      embedDocuments: async texts => { entered.release(); await inference.promise; return { vectors: texts.map(() => [1, 0]) }; }
    } });
  f.settings.local.semantic = 'auto';
  f.settings.projectIndexing.mountedFolder = true;
  workspace = join(f.root, 'workspace');
  await mkdir(workspace);
  const path = join(workspace, 'guide.md');
  await writeFile(path, 'Older document bytes.', 'utf8');
  const contents = [];
  f.index.upsertSources = async sources => {
    contents.push(...sources.map(source => source.text));
    return { sources: sources.map(source => ({ sourceId: source.sourceId })) };
  };
  const job = await f.service.rebuild({ projectId: 'synthetic-project' });
  await entered.promise;
  await writeFile(path, 'Newer document bytes.', 'utf8');
  await f.service.sync.onFolderChanged('synthetic-project');
  inference.release();
  await f.service.lifecycle.active.get(job.jobId)?.promise;
  assert.deepEqual(contents, ['Newer document bytes.']);
  assert.equal((await f.jobs.get(job.jobId)).status, 'completed');
});

test('a cancelled dirty job never starts its queued follow-up pass', async t => {
  const entered = gate(), inference = gate();
  let embedCalls = 0;
  const f = await fixture(t, { embeddings: {
    status: () => ({ state: 'ready', modelVersion: 'fixture-v1' }),
    embedDocuments: async texts => { embedCalls++; entered.release(); await inference.promise; return { vectors: texts.map(() => [1, 0]) }; }
  } });
  f.settings.local.semantic = 'auto';
  await f.library.add([{ path: join(f.root, 'one.md'), title: 'One', text: 'Cancelled source.' }]);
  const job = await f.service.rebuild();
  await entered.promise;
  await f.service.rebuild({ dirty: true });
  const cancelling = f.service.cancelJob(job.jobId);
  inference.release();
  assert.equal((await cancelling).status, 'cancelled');
  assert.equal(embedCalls, 1);
  assert.equal(f.publications.length, 0);
});

test('closing after source commit but before job admission returns the committed receipt and pending indexing', async t => {
  const admitted = gate(), admissionQueue = gate();
  const f = await fixture(t, { readTree: async path => [{ path, title: 'Committed', text: 'Committed before shutdown.' }] });
  f.service.lifecycle.admissionQueue = admissionQueue.promise;
  const rebuild = f.service.rebuild.bind(f.service);
  f.service.rebuild = (...args) => { admitted.release(); return rebuild(...args); };
  const importing = f.service.import({ path: join(f.root, 'committed.md') });
  await admitted.promise;
  await f.service.close({ releaseInference: async () => admissionQueue.release() });
  const result = await importing;
  assert.equal(result.status, 'ready');
  assert.equal(result.indexingStatus, 'pending');
  assert.equal(result.jobId, undefined);
  assert.equal((await f.library.list()).sources[0].id, result.id);
  assert.deepEqual(await f.jobs.list(), []);
});

test('an unexpected job admission storage failure is still reported after a source commits', async t => {
  const f = await fixture(t, { readTree: async path => [{ path, title: 'Committed', text: 'The registry remains authoritative.' }] });
  f.service.lifecycle.jobs.create = async () => { throw Object.assign(new Error('Synthetic storage failure'), { code: 'SYNTHETIC_STORAGE_FAILURE' }); };
  await assert.rejects(f.service.import({ path: join(f.root, 'committed.md') }), { code: 'SYNTHETIC_STORAGE_FAILURE' });
  assert.equal((await f.library.list()).sources.length, 1);
});

test('dirty passes retain bounded earlier diagnostics without presenting a recovered current pass as failed', async t => {
  const entered = gate(), publication = gate();
  const f = await fixture(t);
  await f.library.add([{ path: join(f.root, 'one.md'), title: 'One', text: 'Recovery diagnostics belong to this job.' }]);
  let pass = 0;
  f.service.lifecycle.publishSources = async (sources, settings, signal, progress) => {
    const firstPass = ++pass === 1;
    if (firstPass) { entered.release(); await publication.promise; }
    const semantic = { requested: true, profileId: 'builtin-multilingual', state: firstPass ? 'unavailable' : 'complete',
      totalChunks: 1, vectorChunks: firstPass ? 0 : 1, cachedChunks: 0,
      diagnosticCodes: firstPass ? ['SYNTHETIC_EMBEDDING_FAILURE'] : [] };
    const coverage = { discovered: sources.length, lexical: sources.length, semantic: firstPass ? 0 : sources.length,
      failed: 0, skipped: 0, partial: firstPass ? sources.length : 0, complete: !firstPass, sources: [] };
    await progress(sources.length, { semantic, coverage });
    return { semantic, coverage };
  };
  const job = await f.service.rebuild();
  await entered.promise;
  await f.service.rebuild({ dirty: true });
  publication.release();
  await f.service.lifecycle.active.get(job.jobId)?.promise;
  const final = await f.jobs.get(job.jobId);
  assert.equal(final.status, 'completed');
  assert.equal(final.semantic.state, 'complete');
  assert.deepEqual(final.semantic.diagnosticCodes, []);
  assert.deepEqual(final.semantic.priorDiagnosticCodes, ['SYNTHETIC_EMBEDDING_FAILURE']);
  const independent = await f.service.rebuild();
  await f.service.lifecycle.active.get(independent.jobId)?.promise;
  assert.deepEqual((await f.jobs.get(independent.jobId)).semantic.priorDiagnosticCodes, []);
});

test('job progress keeps the last returned semantic publication when cancellation stops a later batch', async t => {
  const f = await fixture(t);
  const controller = new AbortController();
  await f.library.add([{ path: join(f.root, 'one.md'), title: 'One', text: 'Committed semantic receipts remain visible.' }]);
  f.service.lifecycle.publishSources = async (sources, settings, signal, progress) => {
    await progress(sources.length, { semantic: { requested: true, profileId: 'builtin-multilingual', state: 'partial',
      totalChunks: 2, vectorChunks: 1, cachedChunks: 0, diagnosticCodes: ['SYNTHETIC_PARTIAL_RESULT'] } });
    controller.abort();
    signal.throwIfAborted();
  };
  const job = await f.service.rebuild({ signal: controller.signal });
  await f.service.lifecycle.active.get(job.jobId)?.promise;
  const cancelled = await f.jobs.get(job.jobId);
  assert.equal(cancelled.status, 'cancelled');
  assert.equal(cancelled.completedSources, 1);
  assert.equal(cancelled.semantic.vectorChunks, 1);
  assert.equal(cancelled.semantic.state, 'partial');
});

test('durable job semantic progress rejects malformed counts, unbounded diagnostics and unknown fields', async t => {
  const f = await fixture(t);
  const job = await f.jobs.create();
  const valid = { requested: true, profileId: 'builtin-multilingual', state: 'partial',
    totalChunks: 2, vectorChunks: 1, cachedChunks: 0, diagnosticCodes: [] };
  for (const semantic of [{ ...valid, vectorChunks: -1 }, { ...valid, totalChunks: 'two' },
    { ...valid, diagnosticCodes: Array(9).fill('SYNTHETIC_ERROR') }, { ...valid, privatePath: 'synthetic-path' },
    { ...valid, diagnosticCodes: ['not a machine error code'] }])
    await assert.rejects(f.jobs.update(job.jobId, { semantic }), { code: 'INVALID_RETRIEVAL_JOB_UPDATE' });
  assert.equal((await f.jobs.get(job.jobId)).semantic, undefined);
  await f.jobs.update(job.jobId, { semantic: valid });
  assert.deepEqual((await f.jobs.get(job.jobId)).semantic, valid);
});

test('an unavailable selected profile retains lexical indexing and honest durable semantic status', async t => {
  const f = await fixture(t);
  f.settings.local.semantic = 'auto';
  f.settings.local.embeddingProfileId = `profile-${'x'.repeat(248)}`;
  await f.library.add([{ path: join(f.root, 'one.md'), title: 'One', text: 'Unavailable embeddings preserve lexical evidence.' }]);
  const job = await f.service.rebuild();
  await f.service.lifecycle.active.get(job.jobId)?.promise;
  const completed = await f.jobs.get(job.jobId);
  assert.equal(completed.status, 'partial');
  assert.equal(completed.coverage.lexical, 1);
  assert.equal(completed.coverage.semantic, 0);
  assert.equal(completed.coverage.complete, false);
  assert.equal(f.publications.length, 1);
  assert.equal(completed.semantic.state, 'unavailable');
  assert.equal(completed.semantic.profileId, f.settings.local.embeddingProfileId);
  assert.equal(completed.semantic.vectorChunks, 0);
  assert.ok(completed.semantic.diagnosticCodes.includes('EMBEDDING_PROFILE_UNAVAILABLE'));
});

test('cancelling after source commit while admission is queued never starts the background job', async t => {
  const admitted = gate(), admissionQueue = gate();
  const f = await fixture(t, { readTree: async path => [{ path, title: 'Committed', text: 'Committed before request cancellation.' }] });
  f.service.lifecycle.admissionQueue = admissionQueue.promise;
  const rebuild = f.service.rebuild.bind(f.service);
  f.service.rebuild = (...args) => { admitted.release(); return rebuild(...args); };
  const controller = new AbortController();
  const importing = f.service.import({ path: join(f.root, 'committed.md') }, { signal: controller.signal });
  await admitted.promise;
  controller.abort();
  admissionQueue.release();
  const result = await importing;
  assert.equal(result.status, 'ready');
  assert.equal(result.indexingStatus, 'pending');
  assert.equal(result.jobId, undefined);
  assert.equal((await f.library.list()).sources[0].id, result.id);
  assert.deepEqual(await f.jobs.list(), []);
});

test('successful import completion releases request cancellation without cancelling its background job', async t => {
  const entered = gate(), inference = gate();
  let backgroundSignal;
  const f = await fixture(t, { readTree: async path => [{ path, title: 'One', text: 'Independent background indexing.' }],
    embeddings: {
      status: () => ({ state: 'ready', modelVersion: 'fixture-v1' }),
      embedDocuments: async (texts, { signal }) => {
        backgroundSignal = signal;
        entered.release(); await inference.promise;
        return { vectors: texts.map(() => [1, 0]) };
      }
    } });
  f.settings.local.semantic = 'auto';
  const controller = new AbortController();
  const imported = await f.service.import({ path: join(f.root, 'one.md') }, { signal: controller.signal });
  await entered.promise;
  controller.abort();
  assert.equal(backgroundSignal.aborted, false);
  inference.release();
  await f.service.lifecycle.active.get(imported.jobId)?.promise;
  assert.equal((await f.jobs.get(imported.jobId)).status, 'completed');
  assert.equal(f.publications.length, 1);
});

test('enabling mounted indexing through settings during an active job triggers a fresh source pass', async t => {
  const entered = gate(), inference = gate();
  let workspace, effective;
  const f = await fixture(t, { getProject: async projectId => projectId ? { FolderPath: workspace } : null,
    effectiveSettings: async () => structuredClone(effective), embeddings: {
      status: () => ({ state: 'ready', modelVersion: 'fixture-v1' }),
      embedDocuments: async texts => { entered.release(); await inference.promise; return { vectors: texts.map(() => [1, 0]) }; }
    } });
  f.settings.local.semantic = 'auto';
  effective = f.settings;
  effective.projectId = 'synthetic-project';
  workspace = join(f.root, 'workspace');
  await mkdir(workspace);
  await writeFile(join(workspace, 'guide.md'), 'The newly enabled mounted source.', 'utf8');
  await f.library.add([{ path: join(f.root, 'one.md'), title: 'One', text: 'Already imported source.' }]);
  const contents = [];
  f.index.upsertSources = async sources => {
    contents.push(...sources.map(source => source.text));
    return { sources: sources.map(source => ({ sourceId: source.sourceId })) };
  };
  const job = await f.service.rebuild({ projectId: 'synthetic-project' });
  await entered.promise;
  const request = Readable.from([Buffer.from(JSON.stringify({ expectedRevision: 0,
    patch: { indexingSources: { mountedFolder: { enabled: true } } } }))]);
  request.method = 'PATCH';
  const response = new EventEmitter();
  response.writableEnded = false;
  response.writeHead = status => { assert.equal(status, 200); };
  response.end = () => { response.writableEnded = true; response.emit('close'); };
  const routing = { settings: { patchProject: async () => {
    effective.projectIndexing.mountedFolder = true;
    return { revision: 1 };
  } }, effective: async () => structuredClone(effective), rebuild: options => f.service.rebuild(options) };
  await handleRetrievalRoute(request, response, new URL('http://localhost/api/projects/synthetic-project/retrieval/settings'), routing);
  inference.release();
  await f.service.lifecycle.active.get(job.jobId)?.promise;
  assert.ok(contents.includes('The newly enabled mounted source.'));
  assert.equal((await f.jobs.get(job.jobId)).status, 'completed');
  assert.equal(request.listenerCount('aborted'), 0);
  assert.equal(response.listenerCount('close'), 0);
});

test('restart resumes only uncommitted derivations after verifying durable SQLite receipts and live source hashes', async t => {
  const f = await resumableFixture(t);
  const paused = await f.pause();
  assert.equal(paused.status, 'paused');
  assert.equal(paused.completedSources, 2);
  const committed = await f.index.listSources({ scopeKeys: ['user'] });
  assert.equal(committed.length, 2);
  assert.ok(committed.every(source => source.vectorChunks === source.chunkCount && source.vectorDimensions === 2));
  const log = await readFile(join(f.root, 'Retrieval', 'checkpoints', `${paused.jobId}.jsonl`), 'utf8');
  assert.equal(log.includes('Evidence for the restart-safe source'), false, 'checkpoint metadata never contains source bodies');
  assert.equal(log.includes('"vectors"'), false, 'checkpoint metadata never copies embedding arrays');
  const resumed = f.createService();
  await resumed.service.initialize();
  await resumed.service.lifecycle.active.get(paused.jobId)?.promise;
  const completed = await resumed.jobs.get(paused.jobId);
  assert.equal(completed.status, 'completed');
  assert.equal(completed.completedSources, 5);
  assert.equal(completed.error, undefined);
  assert.equal(resumed.counts.parsed, 3, 'already published unchanged sources bypass reparsing');
  assert.equal(resumed.counts.embedded, 3, 'already published unchanged sources bypass re-embedding');
  const current = await f.index.listSources({ scopeKeys: ['user'] });
  for (const source of committed)
    assert.equal(current.find(item => item.sourceId === source.sourceId).generation, source.generation,
      'verified committed generations are not rewritten during recovery');
  assert.equal(completed.semantic.cachedChunks, 2);
});

test('restart never restores revoked sources and derives a committed source again when its vectors disappeared', async t => {
  const f = await resumableFixture(t);
  const paused = await f.pause();
  const [removed, republished] = await f.index.listSources({ scopeKeys: ['user'] });
  await f.library.remove(removed.sourceId);
  const original = await f.library.readSource(republished.sourceId, { scopeKeys: ['user'] });
  await f.index.upsertSources([{ ...original, title: 'A changed derived title invalidates saved vector inputs' }]);
  assert.equal((await f.index.listSources({ scopeKeys: ['user'] })).find(source => source.sourceId === republished.sourceId).vectorChunks, 0);
  const resumed = f.createService();
  await resumed.service.initialize();
  await resumed.service.lifecycle.active.get(paused.jobId)?.promise;
  assert.equal((await resumed.jobs.get(paused.jobId)).status, 'completed');
  assert.equal(resumed.counts.parsed, 4, 'missing committed vectors cannot count as a semantic cache hit');
  const sources = await f.index.listSources({ scopeKeys: ['user'] });
  assert.equal(sources.some(source => source.sourceId === removed.sourceId), false, 'revoked source rows are pruned rather than resurrected');
  assert.ok(sources.every(source => source.vectorChunks === source.chunkCount));
});

test('checkpoint recovery rejects changed settings, project bindings and malformed complete journal records', async t => {
  for (const scenario of ['settings', 'binding', 'root', 'journal']) await t.test(scenario, async t => {
    const f = await resumableFixture(t);
    if (scenario === 'root') {
      const root = join(f.root, 'workspace');
      await mkdir(root);
      f.setWorkspace(root);
      f.settings.projectIndexing.mountedFolder = true;
    }
    const paused = await f.pause(scenario === 'root' ? { projectId: 'synthetic-project' } : undefined);
    if (scenario === 'settings') f.settings.local.embeddingProfileId = 'another-explicit-profile';
    else if (scenario === 'binding') f.settings.projectIndexing.bindingRevision++;
    else if (scenario === 'root') {
      const another = join(f.root, 'another-workspace');
      await mkdir(another); f.setWorkspace(another);
    } else {
      const path = join(f.root, 'Retrieval', 'checkpoints', `${paused.jobId}.jsonl`);
      await writeFile(path, '{"not":"a validated publication receipt"}\n', 'utf8');
    }
    const resumed = f.createService();
    await resumed.service.initialize();
    await resumed.service.lifecycle.active.get(paused.jobId)?.promise;
    const rejected = await resumed.jobs.get(paused.jobId);
    assert.equal(rejected.status, 'failed');
    assert.equal(rejected.error, scenario === 'journal' ? 'INVALID_RETRIEVAL_CHECKPOINT' : 'INDEX_CHECKPOINT_STALE');
    assert.equal(resumed.counts.parsed, 0);
    assert.equal(resumed.counts.embedded, 0);
    assert.equal((await f.index.listSources({ scopeKeys: ['user'] })).length, 2, 'failed recovery preserves committed evidence');
  });
});

test('a torn uncommitted checkpoint tail is repaired without discarding verified committed records', async t => {
  const f = await resumableFixture(t);
  const paused = await f.pause();
  const path = join(f.root, 'Retrieval', 'checkpoints', `${paused.jobId}.jsonl`);
  const prefix = await readFile(path, 'utf8');
  await writeFile(path, `${prefix}{"jobId":"interrupted`, 'utf8');
  const resumed = f.createService();
  await resumed.service.initialize();
  await resumed.service.lifecycle.active.get(paused.jobId)?.promise;
  assert.equal((await resumed.jobs.get(paused.jobId)).status, 'completed');
  assert.equal(resumed.counts.parsed, 3);
  const records = await resumed.jobs.checkpointSources(paused.jobId, (await resumed.jobs.get(paused.jobId)).checkpoint);
  assert.equal(records.length, 5);
});

test('explicit user cancellation of a paused pure index job prevents every automatic restart', async t => {
  const f = await resumableFixture(t);
  const paused = await f.pause();
  assert.equal((await f.first.service.cancelJob(paused.jobId)).status, 'cancelled');
  const resumed = f.createService();
  await resumed.service.initialize();
  assert.equal(resumed.service.lifecycle.active.size, 0);
  assert.equal(resumed.counts.parsed, 0);
  assert.equal((await resumed.jobs.get(paused.jobId)).status, 'cancelled');
});

test('a normal completed restart reuses verified derivations without reopening the old completed task', async t => {
  const f = await resumableFixture(t);
  const first = f.createService();
  const previous = await first.service.rebuild();
  await first.service.lifecycle.active.get(previous.jobId)?.promise;
  assert.equal(first.counts.parsed, 5);
  await first.service.close();
  const before = await f.index.listSources({ scopeKeys: ['user'] });
  const restarted = f.createService();
  await restarted.service.initialize();
  assert.equal(restarted.service.lifecycle.active.size, 0, 'a completed index job is not automatically executed again');
  const next = await restarted.service.rebuild();
  assert.notEqual(next.jobId, previous.jobId);
  await restarted.service.lifecycle.active.get(next.jobId)?.promise;
  assert.equal(restarted.counts.parsed, 0);
  assert.equal(restarted.counts.embedded, 0);
  assert.equal((await restarted.jobs.get(next.jobId)).semantic.cachedChunks, 5);
  assert.equal((await restarted.jobs.get(previous.jobId)).status, 'completed');
  assert.deepEqual((await f.index.listSources({ scopeKeys: ['user'] })).map(source => [source.sourceId, source.generation]),
    before.map(source => [source.sourceId, source.generation]));
});

test('shutdown cannot turn an explicit in-flight user cancellation into resumable paused work', async t => {
  const f = await resumableFixture(t);
  const job = await f.first.service.rebuild();
  await f.entered.promise;
  const cancelled = f.first.service.cancelJob(job.jobId);
  await f.first.service.close({ releaseInference: () => f.blocked.release() });
  assert.equal((await cancelled).status, 'cancelled');
  const restarted = f.createService();
  await restarted.service.initialize();
  assert.equal(restarted.service.lifecycle.active.size, 0);
  assert.equal((await restarted.jobs.get(job.jobId)).status, 'cancelled');
});

test('a parent signal cancelled after shutdown starts keeps the committed indexing task terminal', async t => {
  const f = await resumableFixture(t);
  const controller = new AbortController();
  const job = await f.first.service.rebuild({ signal: controller.signal });
  await f.entered.promise;
  await f.first.service.close({ releaseInference: () => { controller.abort(); f.blocked.release(); } });
  const cancelled = await f.first.jobs.get(job.jobId);
  assert.equal(cancelled.status, 'cancelled');
  assert.equal(cancelled.completedSources, 2, 'actual committed progress is retained through the cancellation race');
  const restarted = f.createService();
  await restarted.service.initialize();
  assert.equal(restarted.service.lifecycle.active.size, 0);
});

test('a source changed between snapshot and lazy read schedules a bounded fresh pass instead of aborting the whole index', async t => {
  const f = await resumableFixture(t);
  const workspace = join(f.root, 'changing-workspace');
  await mkdir(workspace);
  const path = join(workspace, 'guide.md');
  await writeFile(path, 'The initial mounted content.', 'utf8');
  f.setWorkspace(workspace); f.settings.projectIndexing.mountedFolder = true;
  const indexing = f.createService();
  const prepare = indexing.service.lifecycle.prepareSources.bind(indexing.service.lifecycle);
  let passes = 0;
  indexing.service.lifecycle.prepareSources = async (...args) => {
    const snapshot = await prepare(...args);
    if (++passes === 1) await writeFile(path, 'The updated mounted content.', 'utf8');
    return snapshot;
  };
  const job = await indexing.service.rebuild({ projectId: 'synthetic-project' });
  await indexing.service.lifecycle.active.get(job.jobId)?.promise;
  const completed = await indexing.jobs.get(job.jobId);
  assert.equal(completed.status, 'completed');
  assert.equal(passes, 2);
  assert.deepEqual(completed.semantic.diagnosticCodes, []);
  assert.ok(completed.semantic.priorDiagnosticCodes.includes('STALE_RETRIEVAL_SOURCE'));
  const mounted = (await f.index.listSources({ scopeKeys: ['project:synthetic-project'] })).find(source => source.sourceType === 'work-file');
  assert.equal(mounted.contentHash, hashText('The updated mounted content.'));
  assert.equal(indexing.counts.parsed, 6, 'five unchanged imports are not derived again during the fresh pass');
});

test('foreground corpus synchronization skips unchanged work while messages and real index epochs remain current', async t => {
  const f = await resumableFixture(t);
  const indexing = f.createService();
  let bodyLoads = 0, freshnessChecks = 0;
  const snapshot = async (message, memoryContent) => {
    const registered = await indexing.service.librarySnapshot(['user'], f.settings);
    const messages = message ? [{ sourceId: message.id, scopeKey: 'chat:synthetic-chat', sourceType: 'message',
      title: 'user', locator: { conversationId: 'synthetic-chat', messageId: message.id }, text: message.content,
      contentHash: hashText(message.content), sourceRevision: hashText(message.content) }] : [];
    const memories = memoryContent ? [{ sourceId: 'memory-one', scopeKey: 'user', sourceType: 'memory', title: 'Confirmed memory',
      locator: { memoryId: 'memory-one' }, text: memoryContent, contentHash: hashText(memoryContent), sourceRevision: hashText(memoryContent) }] : [];
    const sources = [...registered.sources, ...messages, ...memories];
    return { settings: f.settings, scopes: ['user', 'chat:synthetic-chat'], sources,
      identities: new Map(sources.map(source => [source.sourceId, source])),
      loadSource: async (source, signal) => {
        bodyLoads++;
        return registered.loadSource(source, signal);
      }, isCurrent: async (source, signal) => {
        freshnessChecks++;
        return source.sourceType === 'knowledge' ? registered.isCurrent(source, signal) : true;
      } };
  };
  await indexing.service.syncFormalSources(await snapshot({ id: 'message-one', content: 'The first formal chat message.' }, 'The first confirmed memory.'));
  assert.equal(bodyLoads, 5);
  const firstParses = indexing.counts.parsed;
  const initialVersion = await f.index.scopeVersion({ scopeKeys: ['user'] });
  const [vectorTarget] = await f.index.listSources({ scopeKeys: ['user'], sourceType: 'knowledge' });
  const original = await f.library.readSource(vectorTarget.sourceId, { scopeKeys: ['user'] });
  const chunks = chunkSource(original);
  await f.index.upsertSources([{ ...original, structure: vectorTarget.structure, parserVersion: vectorTarget.parserVersion,
    chunkerVersion: vectorTarget.chunkerVersion, tokenizerVersion: vectorTarget.tokenizerVersion,
    embeddingInputVersion: vectorTarget.embeddingInputVersion, chunks, vectors: chunks.map(() => [1, 0]),
    embeddingProfileId: 'builtin-multilingual', embeddingModelVersion: 'synthetic-embedding-v1|source-context-v1',
    embeddingSpaceId: '1'.repeat(64) }]);
  const vectorVersion = await f.index.scopeVersion({ scopeKeys: ['user'] });
  assert.ok(vectorVersion.scopes[0].generation > initialVersion.scopes[0].generation);
  assert.equal(vectorVersion.scopes[0].corpusGeneration, initialVersion.scopes[0].corpusGeneration,
    'a real vector-only publication does not change the corpus derivation proof');
  await indexing.service.syncFormalSources(await snapshot({ id: 'message-two', content: 'The changed formal chat message.' }, 'The changed confirmed memory.'));
  assert.equal(bodyLoads, 5, 'vector augmentation and new chat or memory content never reload the unchanged corpus');
  assert.equal(freshnessChecks, 0, 'an unchanged corpus does not stat every file during foreground synchronization');
  assert.equal(indexing.counts.parsed, firstParses + 2, 'the new chat message and changed memory still receive current derivations');
  assert.deepEqual((await f.index.listSources({ scopeKeys: ['chat:synthetic-chat'] })).map(source => source.sourceId), ['message-two']);
  const userSources = await f.index.listSources({ scopeKeys: ['user'] });
  const memory = userSources.find(source => source.sourceType === 'memory');
  assert.equal(memory.contentHash, hashText('The changed confirmed memory.'));
  const memoryVersion = await f.index.scopeVersion({ scopeKeys: ['user'] });
  assert.equal(memoryVersion.scopes[0].corpusGeneration, initialVersion.scopes[0].corpusGeneration,
    'user memory updates remain outside the corpus generation');
  const corpus = userSources.find(source => source.sourceType === 'knowledge');
  await f.index.removeSource(corpus.sourceId, { scopeKeys: ['user'], permanent: false });
  await indexing.service.syncFormalSources(await snapshot({ id: 'message-two', content: 'The changed formal chat message.' }));
  assert.equal(bodyLoads, 10, 'a real SQLite scope generation change forces validated corpus reconstruction');
  assert.equal((await f.index.listSources({ scopeKeys: ['user'] })).length, 5);
  assert.equal((await f.index.listSources({ scopeKeys: ['user'] })).some(source => source.sourceType === 'memory'), false,
    'memory removal is reflected even when corpus synchronization is cached');
  await f.library.remove(corpus.sourceId);
  await indexing.service.syncFormalSources(await snapshot({ id: 'message-two', content: 'The changed formal chat message.' }));
  assert.equal((await f.index.listSources({ scopeKeys: ['user'] })).some(source => source.sourceId === corpus.sourceId), false);
  assert.equal(indexing.service.corpusSyncCache.size, 1, 'chat identifiers never create extra whole-corpus cache slots');
});

test('the first foreground synchronization after a completed restart restores lexical proof without reparsing the corpus', async t => {
  const f = await resumableFixture(t);
  const completed = f.createService();
  const job = await completed.service.rebuild();
  await completed.service.lifecycle.active.get(job.jobId)?.promise;
  await completed.service.close();
  const restarted = f.createService();
  const registered = await restarted.service.librarySnapshot(['user'], f.settings);
  await restarted.service.syncFormalSources({ settings: f.settings, scopes: ['user'], sources: registered.sources,
    identities: new Map(registered.sources.map(source => [source.sourceId, source])),
    loadSource: registered.loadSource, isCurrent: registered.isCurrent });
  assert.equal(restarted.counts.parsed, 0);
  assert.equal(restarted.counts.embedded, 0);
  assert.equal((await restarted.jobs.get(job.jobId)).status, 'completed');
});

test('large cold corpus synchronization schedules a job without blocking chat or pruning incomplete discovery', async t => {
  const f = await fixture(t);
  let corpusGeneration = 1, admitted = 0;
  f.index.scopeVersion = async ({ scopeKeys }) => ({ indexEpoch: 'fixture',
    scopes: scopeKeys.map(scopeKey => ({ scopeKey, corpusGeneration })) });
  f.service.rebuild = async () => { admitted++; return { jobId: 'owned-job' }; };
  const corpus = Array.from({ length: 513 }, (_, number) => ({ sourceId: `large-${number}`, scopeKey: 'user',
    sourceType: 'knowledge', title: `File ${number}`, contentHash: hashText(`Body ${number}`),
    sourceRevision: 1, locator: { relativePath: `file-${number}.md` } }));
  const message = { sourceId: 'current-message', scopeKey: 'chat:test', sourceType: 'message',
    title: 'user', locator: { messageId: 'current-message' }, text: 'Current formal message.', sourceRevision: 1 };
  const snapshot = () => ({ sources: [...corpus, message], settings: f.settings,
    scopes: ['user', 'chat:test'], sourceScan: { backgroundPending: true }, loadSource: () => {
      throw new Error('Cold corpus bodies must not be loaded by foreground synchronization.');
    } });
  const prunedTypes = [], prune = f.service.indexer.prune.bind(f.service.indexer);
  f.service.indexer.prune = (input, signal, options) => { prunedTypes.push(...options.sourceTypes); return prune(input, signal, options); };
  const first = snapshot();
  await f.service.syncFormalSources(first);
  assert.equal(first.indexingPending, true);
  assert.equal(admitted, 1);
  assert.deepEqual(f.publications, ['current-message']);
  assert.equal(prunedTypes.includes('knowledge') || prunedTypes.includes('work-file'), false);
  // The completed background publication installs the same metadata/version proof used by foreground queries.
  // 后台提交完成后建立与前台相同的元信息和版本证明，避免每个查询重新调度整库。
  await f.service.lifecycle.finalizeSources({ sources: corpus, scopes: ['user'], settings: f.settings,
    preparationFailures: f.service.indexer.preparationFailures });
  const warm = snapshot();
  delete warm.sourceScan;
  await f.service.syncFormalSources(warm);
  assert.equal(admitted, 1);
  assert.equal(warm.indexingPending, undefined);
  corpusGeneration++;
  const changed = snapshot();
  await f.service.syncFormalSources(changed);
  assert.equal(changed.indexingPending, true);
  assert.equal(admitted, 2);
});

test('one actual-tokenizer oversize rejection preserves all valid peer embeddings without cutting source text', async t => {
  const calls = [];
  const f = await fixture(t, { embeddings: { status: () => ({ state: 'ready' }), embedDocuments: async texts => {
    calls.push([...texts]);
    const index = texts.findIndex(text => text.startsWith('oversized'));
    if (index >= 0) throw Object.assign(new Error('Actual tokenizer input exceeded 512 tokens.'), {
      code: 'EMBEDDING_INPUT_TOO_LONG', details: { index, tokenCount: 513, maxInputTokens: 512 } });
    return { profileId: 'fixture', vectors: texts.map(() => [1, 0]) };
  } } });
  const original = ['valid Chinese 中文', 'oversized 中文'.repeat(50), 'valid code Cobalt.Apply()', 'oversized raw'.repeat(50)];
  const diagnostics = [];
  const result = await f.service.indexer.embedBoundedDocuments(original, {}, code => diagnostics.push(code));
  assert.deepEqual(result.vectors, [[1, 0], null, [1, 0], null]);
  assert.deepEqual([...result.rejectedPositions], [1, 3]);
  assert.deepEqual(calls.at(-1), [original[0], original[2]]);
  assert.ok(calls.every(batch => batch.every(text => original.includes(text))));
  assert.deepEqual(diagnostics, ['EMBEDDING_INPUT_TOO_LONG', 'EMBEDDING_INPUT_TOO_LONG']);
  const empty = await f.service.indexer.embedBoundedDocuments([original[1]], {}, () => {});
  assert.deepEqual(empty.vectors, [null]);
});

test('a rebuilt SQLite epoch invalidates a warm corpus synchronization proof even when source descriptors did not change', async t => {
  const f = await resumableFixture(t);
  const indexing = f.createService();
  let bodyLoads = 0;
  const snapshot = async () => {
    const registered = await indexing.service.librarySnapshot(['user'], f.settings);
    return { settings: f.settings, scopes: ['user'], sources: registered.sources,
      identities: new Map(registered.sources.map(source => [source.sourceId, source])),
      loadSource: (source, signal) => { bodyLoads++; return registered.loadSource(source, signal); }, isCurrent: registered.isCurrent };
  };
  await indexing.service.syncFormalSources(await snapshot());
  const previousEpoch = (await f.index.scopeVersion({ scopeKeys: ['user'] })).indexEpoch;
  await f.index.close();
  await rm(join(f.root, 'Index', 'retrieval.sqlite'));
  const rebuilt = new RetrievalIndex({ root: f.root });
  const close = indexing.service.close.bind(indexing.service);
  indexing.service.close = async (...args) => { await close(...args); await rebuilt.close(); };
  indexing.service.index = rebuilt;
  indexing.service.indexer.index = rebuilt;
  assert.notEqual((await rebuilt.scopeVersion({ scopeKeys: ['user'] })).indexEpoch, previousEpoch);
  await indexing.service.syncFormalSources(await snapshot());
  assert.equal(bodyLoads, 10);
  assert.equal(indexing.counts.parsed, 10);
  assert.equal((await rebuilt.listSources({ scopeKeys: ['user'] })).length, 5);
});

test('transient parser fallback cannot become a permanent whole-corpus synchronization cache hit', async t => {
  const f = await resumableFixture(t);
  const indexing = f.createService();
  const parse = indexing.service.indexer.structures.parse;
  let unavailable = true;
  indexing.service.indexer.structures.parse = (source, options) => {
    if (unavailable) throw Object.assign(new Error('A synthetic temporary parser fault.'), { code: 'STRUCTURE_WORKER_FAILED' });
    return parse(source, options);
  };
  const snapshot = async () => {
    const registered = await indexing.service.librarySnapshot(['user'], f.settings);
    return { settings: f.settings, scopes: ['user'], sources: registered.sources,
      identities: new Map(registered.sources.map(source => [source.sourceId, source])),
      loadSource: registered.loadSource, isCurrent: registered.isCurrent };
  };
  await indexing.service.syncFormalSources(await snapshot());
  assert.equal(indexing.service.corpusSyncCache.size, 0, 'failed structural preparation is not a complete synchronization proof');
  unavailable = false;
  await indexing.service.syncFormalSources(await snapshot());
  assert.equal(indexing.counts.parsed, 5);
  assert.equal(indexing.service.corpusSyncCache.size, 1);
  await indexing.service.syncFormalSources(await snapshot());
  assert.equal(indexing.counts.parsed, 5, 'a validated recovered corpus may then use the bounded synchronization cache');
});

test('the background execution slot serializes independent projects and cancels queued work before inference', async t => {
  const entered = gate(), pending = gate();
  let calls = 0;
  const f = await fixture(t, { embeddings: {
    status: () => ({ state: 'ready', modelVersion: 'synthetic-model' }),
    embedDocuments: async texts => { calls++; entered.release(); await pending.promise; return { vectors: texts.map(() => [1, 0]) }; }
  } });
  f.settings.local.semantic = 'on';
  await f.library.add([{ path: join(f.root, 'queued.md'), title: 'Queued', text: 'One controlled index task.' }]);
  const running = await f.service.rebuild({ projectId: 'synthetic-one' });
  await entered.promise;
  const queued = await f.service.rebuild({ projectId: 'synthetic-two' });
  assert.equal((await f.jobs.get(queued.jobId)).status, 'queued');
  assert.equal(calls, 1);
  const cancelled = f.service.cancelJob(queued.jobId);
  assert.equal((await cancelled).status, 'cancelled');
  assert.equal((await f.jobs.get(running.jobId)).status, 'running', 'the unrelated running inference is still gated');
  assert.equal((await f.jobs.get(queued.jobId)).status, 'cancelled', 'queued cancellation is already durable before the execution slot is released');
  pending.release();
  await f.service.lifecycle.active.get(running.jobId)?.promise;
  assert.equal((await f.jobs.get(running.jobId)).status, 'completed');
  assert.equal(calls, 1, 'queued cancellation never enters the embedding implementation');
});

test('configured source batches preserve the actual SQLite atomic text budget for maximum-sized sources', async t => {
  const f = await fixture(t);
  const index = new RetrievalIndex({ root: f.root, vectorEnabled: false });
  const close = f.service.close.bind(f.service);
  f.service.close = async (...args) => { await close(...args); await index.close(); };
  const batchSizes = [], original = index.upsertSources.bind(index);
  index.upsertSources = (sources, options) => { batchSizes.push(sources.length); return original(sources, options); };
  const sourceText = 'x'.repeat(2 * 1024 * 1024);
  const sources = Array.from({ length: 5 }, (_, index) => ({ sourceId: `large-source-${index}`, scopeKey: 'user',
    sourceType: 'document', title: `Large ${index}`, locator: {}, text: sourceText, contentHash: hashText(sourceText) }));
  f.settings.local.indexing = { batchSize: 32 };
  const indexer = new SourceIndexer({ library: f.library, index, embeddings: { status: () => ({ state: 'unavailable' }) },
    structures: { version: () => 'synthetic-bounds-v1', parse: async source => ({ chunks: chunkSource(source, { maxChars: 8000 }) }) },
    serialize: operation => operation(), effectiveSettings: async () => f.settings, getProject: async () => null });
  await indexer.upsert(sources, f.settings, undefined, undefined, { semantic: false });
  assert.deepEqual(batchSizes, [4, 1]);
  assert.equal((await index.listSources({ scopeKeys: ['user'] })).length, 5);
});

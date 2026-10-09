import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { test } from 'node:test';
import { EmbeddingService } from '../models/retrieval/embedding-service.mjs';
import { RerankerService } from '../models/retrieval/reranker-service.mjs';
import { resolveRetrievalModelProfile } from '../models/retrieval/model-registry.mjs';

class ControlledWorker extends EventEmitter {
  messages = [];
  failDispatch = false;
  failCloseDispatch = false;
  autoClose = true;
  ref() {}
  unref() {}
  postMessage(message) {
    this.messages.push(message);
    if (this.failDispatch && ['embed', 'rerank'].includes(message.type)) throw new Error('Synthetic dispatch failure.');
    if (message.type === 'close' && this.failCloseDispatch) throw new Error('Synthetic IPC disconnect during natural retirement.');
    if (message.type === 'close' && this.autoClose) setImmediate(() => this.retire());
  }
  retire(disposed = true) {
    this.emit('message', { type: 'closed', disposed });
    this.emit('exit', 0);
  }
  reply(message) { this.emit('message', message); }
}

async function fixture(t, kind) {
  const profileId = kind === 'embedding' ? 'builtin-multilingual' : 'builtin-multilingual-reranker';
  const profile = resolveRetrievalModelProfile(kind, profileId);
  const modelRoot = await mkdtemp(join(tmpdir(), 'kynxa-model-lifecycle-'));
  for (const asset of profile.files) {
    await mkdir(dirname(join(modelRoot, asset.path)), { recursive: true });
    await writeFile(join(modelRoot, asset.path), 'Synthetic transport fixture; never loaded into ONNX.');
  }
  const worker = new ControlledWorker();
  let workerOptions;
  const Service = kind === 'embedding' ? EmbeddingService : RerankerService;
  const service = new Service({ modelRoot, profileId, workerFactory: (url, options) => {
    workerOptions = options;
    return worker;
  } });
  t.after(async () => { await service.close().catch(() => {}); await rm(modelRoot, { recursive: true, force: true }); });
  return { service, worker, profileId, get workerOptions() { return workerOptions; } };
}

const request = (kind, service, options = {}) => kind === 'embedding'
  ? service.embedQuery('A public synthetic source.', options)
  : service.rerank({ query: 'account', candidates: [{ sourceRef: 'first', excerpt: 'weather' },
    { sourceRef: 'second', excerpt: 'account recovery' }], ...options });
const successfulResult = (kind, id) => kind === 'embedding'
  ? { type: 'result', id, vectors: [Array(384).fill(0.125)] }
  : { type: 'result', id, scores: [0.1, 0.9], truncatedInputsCount: 0 };

for (const kind of ['embedding', 'reranker']) {
  test(`${kind} cancellation ignores late results and keeps the next request and profile isolated`, async t => {
    const setup = await fixture(t, kind);
    const { service, worker, profileId } = setup;
    const controller = new AbortController();
    const cancelled = request(kind, service, { signal: controller.signal, profileId });
    controller.abort();
    await assert.rejects(cancelled, { name: 'AbortError' });
    assert.equal(service.status().pendingRequests, 0);
    const next = request(kind, service, { profileId });
    assert.equal(setup.workerOptions.workerData.profileId, profileId);
    assert.deepEqual(setup.workerOptions.execArgv, []);
    worker.reply({ type: 'ready' });
    worker.reply({ type: 'result', id: 1, vectors: [[NaN]], scores: [NaN], truncatedInputsCount: 0 });
    assert.equal(service.status().pendingRequests, 1, 'late result belongs only to the cancelled request');
    worker.reply(successfulResult(kind, 2));
    const result = await next;
    assert.equal(result.profileId, profileId);
    if (kind === 'embedding') assert.equal(result.vector.length, 384);
    else assert.equal(result.items[0].sourceRef, 'second');
    assert.equal(service.status().loaded, true);
    assert.equal(service.status().assetVerification, 'verified');
  });

  test(`${kind} worker dispatch failure releases pending requests rather than leaking queue slots`, async t => {
    const { service, worker } = await fixture(t, kind);
    worker.failDispatch = true;
    await assert.rejects(request(kind, service), { code: kind === 'embedding' ? 'EMBEDDING_WORKER_FAILED' : 'RERANK_WORKER_FAILED' });
    assert.equal(service.status().pendingRequests, 0);
  });

  test(`${kind} fatal resource failure cannot be undone by a stale ready notification`, async t => {
    const { service, worker } = await fixture(t, kind);
    const pending = request(kind, service);
    const code = kind === 'embedding' ? 'EMBEDDING_ASSET_INVALID' : 'RERANK_ASSET_INVALID';
    worker.reply({ type: 'fatal', code, message: 'Synthetic fixed-asset verification failure.' });
    await assert.rejects(pending, { code });
    worker.reply({ type: 'ready' });
    assert.equal(service.status().state, 'error');
    assert.equal(service.status().loaded, false);
    assert.equal(service.status().assetVerification, 'failed');
    await assert.rejects(request(kind, service), { code });
  });

  test(`${kind} retirement rejects clients, waits for disposal, and never reopens on late events`, async t => {
    const { service, worker } = await fixture(t, kind);
    const pending = request(kind, service);
    worker.autoClose = false;
    const close = service.close();
    assert.equal(service.close(), close);
    await assert.rejects(pending, { code: kind === 'embedding' ? 'EMBEDDING_CLOSED' : 'RERANK_CLOSED' });
    worker.reply({ type: 'ready' });
    worker.reply({ type: 'fatal', code: 'SYNTHETIC_LATE_FAILURE', message: 'Late work cannot reopen retirement.' });
    worker.reply(successfulResult(kind, 1));
    assert.equal(service.status().loaded, false);
    assert.equal(service.status().state, 'closing');
    worker.retire();
    await close;
    assert.equal(service.status().workerPhase, 'stopped');
    assert.equal(service.status().state, 'unavailable');
    assert.equal(service.status().pendingRequests, 0);
  });

  test(`${kind} bounds admitted work and releases cancelled caller slots`, async t => {
    const { service } = await fixture(t, kind);
    const maximumRequests = kind === 'embedding' ? 32 : 8;
    const controllers = Array.from({ length: maximumRequests }, () => new AbortController());
    const admitted = controllers.map(controller => request(kind, service, { signal: controller.signal })
      .then(() => null, error => error));
    await assert.rejects(request(kind, service), { code: kind === 'embedding' ? 'EMBEDDING_BUSY' : 'RERANK_BUSY' });
    assert.equal(service.status().pendingRequests, maximumRequests);
    controllers.forEach(controller => controller.abort());
    assert.ok((await Promise.all(admitted)).every(error => error?.name === 'AbortError'));
    assert.equal(service.status().pendingRequests, 0);
  });

  test(`${kind} close observes an acknowledged natural exit after IPC has already disconnected`, async t => {
    const { service, worker } = await fixture(t, kind);
    const pending = request(kind, service);
    const rejected = assert.rejects(pending, { code: kind === 'embedding' ? 'EMBEDDING_CLOSED' : 'RERANK_CLOSED' });
    worker.failCloseDispatch = true;
    const close = service.close();
    setImmediate(() => worker.retire());
    await rejected;
    await close;
    assert.equal(service.status().state, 'unavailable');
    assert.equal(service.status().workerPhase, 'stopped');
  });
}

test('a reranker process exit rejects queued clients even after an earlier request set the runtime error state', async t => {
  const { service, worker } = await fixture(t, 'reranker');
  const first = request('reranker', service);
  const queued = request('reranker', service);
  const queuedOutcome = assert.rejects(queued, { code: 'RERANK_WORKER_FAILED' });
  worker.reply({ type: 'error', id: 1, code: 'RERANK_FAILED', message: 'Synthetic scoring failure.' });
  await assert.rejects(first, { code: 'RERANK_FAILED' });
  assert.equal(service.status().state, 'error');
  worker.emit('exit', 23);
  await queuedOutcome;
  assert.equal(service.status().pendingRequests, 0);
});

test('embedding rejects malformed worker vectors before returning indexable metadata', async t => {
  const { service, worker } = await fixture(t, 'embedding');
  const pending = service.embedDocuments(['first', 'second']);
  worker.reply({ type: 'result', id: 1, vectors: [Array(384).fill(0)] });
  await assert.rejects(pending, { code: 'EMBEDDING_INVALID_VECTOR' });
  assert.equal(service.status().pendingRequests, 0);
});

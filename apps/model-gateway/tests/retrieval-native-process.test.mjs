import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { test } from 'node:test';
import { EmbeddingService } from '../models/retrieval/embedding-service.mjs';
import { RerankerService } from '../models/retrieval/reranker-service.mjs';
import { resolveRetrievalModelProfile } from '../models/retrieval/model-registry.mjs';
import { createNativeInferenceProcess } from '../models/retrieval/native-inference-process.mjs';

const portModule = new URL('../models/retrieval/native-inference-port.mjs', import.meta.url).href;
const processModule = new URL('../models/retrieval/native-inference-process.mjs', import.meta.url).href;
const processFixtureSource = `
  import { openNativeInferencePort } from ${JSON.stringify(portModule)};
  const { parentPort, workerData } = await openNativeInferencePort();
  let timer;
  const respond = message => {
    parentPort.postMessage({ type: 'ready' });
    parentPort.postMessage(message.type === 'embed'
      ? { type: 'result', id: message.id, vectors: message.texts.map(() => Array(384).fill(0.125)) }
      : { type: 'result', id: message.id, scores: message.texts.map(() => 0.75), truncatedInputsCount: 0 });
    parentPort.postMessage({ type: 'idle', throughId: message.id });
  };
  parentPort.on('message', message => {
    if (message.type === 'close') {
      if (workerData.mode === 'hung-close') { timer = setInterval(() => {}, 1000); return; }
      clearInterval(timer);
      parentPort.postMessage({ type: 'closed', disposed: true });
      parentPort.close();
      return;
    }
    if (!['embed', 'rerank'].includes(message.type)) return;
    if (workerData.mode === 'crash') { process.exit(23); return; }
    if (workerData.mode === 'late-result') setTimeout(() => respond(message), 25);
    else respond(message);
  });
`;

async function fixture(t, kind, mode) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-native-process-'));
  const profileId = kind === 'embedding' ? 'builtin-multilingual' : 'builtin-multilingual-reranker';
  for (const asset of resolveRetrievalModelProfile(kind, profileId).files) {
    await mkdir(dirname(join(root, asset.path)), { recursive: true });
    await writeFile(join(root, asset.path), 'Synthetic assets: this fixture loads no native model.');
  }
  const modulePath = join(root, 'inference-fixture.mjs');
  await writeFile(modulePath, processFixtureSource);
  let inferenceProcess;
  const Service = kind === 'embedding' ? EmbeddingService : RerankerService;
  const service = new Service({ modelRoot: root, closeTimeoutMs: 25, workerFactory: (url, options) => {
    inferenceProcess = createNativeInferenceProcess(pathToFileURL(modulePath), {
      ...options, workerData: { ...options.workerData, mode },
    });
    return inferenceProcess;
  } });
  t.after(async () => {
    await service.close().catch(() => {});
    await inferenceProcess?.terminate();
    await rm(root, { recursive: true, force: true });
  });
  return { service, moduleUrl: pathToFileURL(modulePath), get inferenceProcess() { return inferenceProcess; } };
}

const request = (kind, service) => kind === 'embedding'
  ? service.embedQuery('Public synthetic password recovery guide.')
  : service.rerank({ query: 'password', candidates: [{ sourceRef: 'public-fixture', excerpt: 'password recovery guide' }] });

for (const kind of ['embedding', 'reranker']) {
  test(`${kind} native process failure rejects every admitted request and leaves the gateway alive`, { timeout: 5000 }, async t => {
    const setup = await fixture(t, kind, 'crash');
    const first = request(kind, setup.service);
    const second = request(kind, setup.service);
    assert.notEqual(setup.inferenceProcess.pid, process.pid);
    const results = await Promise.allSettled([first, second]);
    assert.ok(results.every(result => result.status === 'rejected'
      && result.reason.code === (kind === 'embedding' ? 'EMBEDDING_WORKER_FAILED' : 'RERANK_WORKER_FAILED')));
    assert.equal(setup.service.status().pendingRequests, 0);
    assert.equal(setup.service.status().state, 'error');
    assert.equal(setup.service.status().workerPhase, 'stopped');
    await assert.rejects(request(kind, setup.service), {
      code: kind === 'embedding' ? 'EMBEDDING_WORKER_FAILED' : 'RERANK_WORKER_FAILED',
    });
  });

  test(`${kind} a shutdown timeout reaps only its owned process before rejecting retirement`, { timeout: 5000 }, async t => {
    const setup = await fixture(t, kind, 'hung-close');
    await request(kind, setup.service);
    const ownedPid = setup.inferenceProcess.pid;
    const close = setup.service.close();
    assert.equal(setup.service.close(), close);
    await assert.rejects(close, { code: kind === 'embedding' ? 'EMBEDDING_CLOSE_TIMEOUT' : 'RERANK_CLOSE_TIMEOUT' });
    assert.equal(setup.service.status().workerPhase, 'stopped');
    assert.equal(setup.service.status().state, 'error');
    assert.equal(setup.service.status().pendingRequests, 0);
    assert.throws(() => process.kill(ownedPid, 0), { code: 'ESRCH' });
  });

  test(`${kind} IPC cancellation discards a late result and preserves the following request`, { timeout: 5000 }, async t => {
    const setup = await fixture(t, kind, 'late-result');
    const controller = new AbortController();
    const cancelled = kind === 'embedding'
      ? setup.service.embedQuery('Synthetic cancelled request.', { signal: controller.signal })
      : setup.service.rerank({ query: 'synthetic', candidates: [{ excerpt: 'cancelled request' }], signal: controller.signal });
    controller.abort();
    await assert.rejects(cancelled, { name: 'AbortError' });
    const next = await request(kind, setup.service);
    if (kind === 'embedding') assert.equal(next.vector.length, 384);
    else assert.equal(next.items[0].sourceRef, 'public-fixture');
    assert.equal(setup.service.status().pendingRequests, 0);
    assert.equal(setup.service.status().loaded, true);
    await setup.service.close();
    assert.equal(setup.service.status().workerPhase, 'stopped');
  });
}

test('an idle inference process retires after its owning parent exits', { timeout: 5000 }, async t => {
  const setup = await fixture(t, 'embedding', 'parent-loss');
  // The fixture path is passed as an argument, with no shell or product storage involved.
  // 夹具路径作为参数传递，不涉及 shell 或产品存储。
  const source = `
    import { createNativeInferenceProcess } from ${JSON.stringify(processModule)};
    const worker = createNativeInferenceProcess(new URL(process.argv[1]), { workerData: { mode: 'parent-loss' } });
    worker.on('error', error => { throw error; });
    worker.on('message', message => {
      if (message.type !== 'result') return;
      process.stdout.write(String(worker.pid));
      worker.unref();
    });
    worker.postMessage({ type: 'embed', id: 1, texts: ['Synthetic public fixture.'] });
  `;
  const owner = spawn(process.execPath, ['--input-type=module', '-e', source, setup.moduleUrl.href], {
    windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'],
  });
  let stdout = '', stderr = '', ownedPid;
  owner.stdout.on('data', chunk => { stdout += chunk; });
  owner.stderr.on('data', chunk => { stderr += chunk; });
  const isAlive = () => {
    try { process.kill(ownedPid, 0); return true; }
    catch (error) { if (error.code === 'ESRCH') return false; throw error; }
  };
  t.after(() => {
    if (owner.exitCode === null) owner.kill('SIGKILL');
    if (ownedPid && isAlive()) process.kill(ownedPid, 'SIGKILL');
  });
  const exitCode = await new Promise((resolveExit, rejectExit) => {
    owner.once('error', rejectExit);
    owner.once('close', resolveExit);
  });
  assert.equal(exitCode, 0, stderr);
  ownedPid = Number(stdout);
  assert.ok(Number.isInteger(ownedPid) && ownedPid > 0 && ownedPid !== process.pid);
  const deadline = Date.now() + 2000;
  while (isAlive() && Date.now() < deadline) await new Promise(resolve => setTimeout(resolve, 20));
  assert.equal(isAlive(), false, 'the IPC disconnect retires the owned idle process');
});

// Real model acceptance is opt-in and uses only synthetic text plus already pinned local assets.
// 真实模型验收显式启用，只使用合成文本和已固定的本地资产，不下载模型或连接用户数据。
test('native embedding and reranking alternate in separate processes without corrupting the embedding runtime',
  { skip: process.env.KYNXA_TEST_NATIVE_RETRIEVAL_ISOLATION !== '1', timeout: 90000 }, async t => {
    const pids = [];
    const workerFactory = (url, options) => {
      const inferenceProcess = createNativeInferenceProcess(url, options);
      pids.push(inferenceProcess.pid);
      return inferenceProcess;
    };
    const embeddings = new EmbeddingService({ workerFactory });
    const reranker = new RerankerService({ workerFactory });
    t.after(() => Promise.all([embeddings.close(), reranker.close()]));
    assert.equal(embeddings.status().state, 'ready', 'Embedding acceptance requires existing pinned local assets.');
    assert.equal(reranker.status().state, 'ready', 'Reranker acceptance requires existing pinned local assets.');
    const candidates = [
      { sourceRef: 'weather', excerpt: 'The weather is sunny and the temperature is twenty degrees Celsius.' },
      { sourceRef: 'password', excerpt: 'Reset your account password in Account Settings using the recovery email.' },
    ];
    const query = 'How do I reset my account password?';
    const before = await embeddings.embedQuery(query);
    assert.equal(before.vector.length, 384);
    assert.ok(Math.abs(before.vector.reduce((sum, value) => sum + value * value, 0) - 1) < 1e-4);
    for (let iteration = 0; iteration < 2; iteration += 1) {
      const ranked = await reranker.rerank({ query, candidates, limit: 2 });
      assert.equal(ranked.items[0].sourceRef, 'password');
      assert.ok(ranked.items.every(item => Number.isFinite(item.rerankScore)));
      const after = await embeddings.embedQuery(query);
      assert.equal(after.vector.length, 384);
      assert.ok(before.vector.every((value, index) => Math.abs(value - after.vector[index]) < 1e-5));
    }
    assert.equal(new Set(pids).size, 2);
    assert.ok(pids.every(pid => pid !== process.pid));
    assert.equal(embeddings.status().network, false);
    assert.equal(reranker.status().network, false);
    t.diagnostic(`Native alternation passed on ${process.platform} ${process.version}: 384 dimensions, 2 reranks, 3 embeddings, 2 owned processes.`);
  });

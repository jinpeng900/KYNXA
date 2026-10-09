import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { test } from 'node:test';

const serviceModule = new URL('../models/retrieval/embedding-service.mjs', import.meta.url).href;
const profileModule = new URL('../models/retrieval/embedding-profile.mjs', import.meta.url).href;
const processModule = new URL('../models/retrieval/native-inference-process.mjs', import.meta.url).href;

async function isolatedEmbeddingCheck(t, scenario) {
  const home = await mkdtemp(join(tmpdir(), 'kynxa-embedding-shutdown-'));
  t.after(async () => {
    const suffix = relative(resolve(tmpdir()), resolve(home));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(home, { recursive: true, force: true });
  });
  // A native lifecycle failure must be observed as the child exit, not take down the test runner.
  // 原生生命周期问题必须表现为测试子进程退出结果，不能拖垮整个测试运行器。
  const source = String.raw`
    import assert from 'node:assert/strict';
    import { mkdir, writeFile } from 'node:fs/promises';
    import { join } from 'node:path';
    import { EventEmitter } from 'node:events';
    const [serviceModule, profileModule, processModule, scenario, home] = process.argv.slice(1);
    const { EmbeddingService } = await import(serviceModule);
    const { BUILTIN_EMBEDDING_PROFILE } = await import(profileModule);
    const { createNativeInferenceProcess } = await import(processModule);
    let forcedTerminations = 0;
    const createService = (options = {}) => new EmbeddingService({ ...options, workerFactory: (url, workerOptions) => {
      const worker = createNativeInferenceProcess(url, workerOptions);
      const terminate = worker.terminate.bind(worker);
      worker.terminate = () => { forcedTerminations++; return terminate(); };
      return worker;
    } });
    const waitForPhase = async (service, phase) => {
      const deadline = Date.now() + 30000;
      while (service.status().workerPhase !== phase) {
        assert.ok(Date.now() < deadline, 'embedding worker reaches phase ' + phase);
        await new Promise(ready => setTimeout(ready, 1));
      }
    };
    const outcome = promise => promise.then(() => ({ completed: true }), error => ({ code: error.code }));
    let report;
    if (scenario === 'load-close') {
      const service = createService();
      const pending = outcome(service.embedQuery('A synthetic request closed during native model loading.'));
      await waitForPhase(service, 'loading-model');
      const close = service.close();
      assert.equal(service.close(), close, 'repeated close shares the same retirement promise');
      assert.equal((await pending).code, 'EMBEDDING_CLOSED');
      assert.equal(service.status().pendingRequests, 0);
      await close;
      assert.equal(service.status().workerPhase, 'stopped');
      assert.equal(service.status().state, 'unavailable');
      report = { scenario, safelyDrained: true, clientRejected: true };
    } else if (scenario === 'inference-close') {
      const service = createService();
      assert.equal((await service.embedQuery('Warm the real offline CPU model.')).vector.length, 384);
      const passage = 'A passage about local retrieval and account password recovery. '.repeat(30);
      const running = outcome(service.embedDocuments(Array(64).fill(passage)));
      await waitForPhase(service, 'inference');
      const queued = outcome(service.embedQuery('A queued request must not run after shutdown.'));
      const close = service.close();
      assert.equal(service.close(), close);
      assert.equal((await running).code, 'EMBEDDING_CLOSED');
      assert.equal((await queued).code, 'EMBEDDING_CLOSED');
      assert.equal(service.status().pendingRequests, 0);
      await close;
      assert.equal(service.status().workerPhase, 'stopped');
      assert.equal(service.status().loaded, false);
      await assert.rejects(service.embedQuery('Cannot reopen a retired service.'), { code: 'EMBEDDING_CLOSED' });
      report = { scenario, safelyDrained: true, runningAndQueuedRejected: true };
    } else if (scenario === 'cancel-verification') {
      const service = createService();
      const controller = new AbortController();
      const pending = outcome(service.embedQuery('Cancel while verifying pinned local assets.', { signal: controller.signal }));
      await waitForPhase(service, 'verifying-assets');
      controller.abort();
      assert.equal((await pending).code, 'EMBEDDING_CANCELLED');
      await waitForPhase(service, 'idle');
      assert.equal(service.status().loaded, false, 'a cancelled verifier does not continue loading ONNX');
      assert.equal((await service.embedQuery('A later request may still load normally.')).vector.length, 384);
      await service.close();
      report = { scenario, cancelledBeforeNativeLoad: true, laterRequestWorks: true };
    } else if (scenario === 'close-timeout') {
      // Enter a known owned-worker boundary; immediate close can precede asynchronous resource admission.
      // 先进入确定的自有 worker 边界；立即 close 可能早于异步资源准入，不能将未启动误当关闭超时。
      const root = join(home, 'synthetic-timeout-model');
      for (const asset of BUILTIN_EMBEDDING_PROFILE.files) {
        await mkdir(join(root, asset.path, '..'), { recursive: true });
        await writeFile(join(root, asset.path), 'synthetic timeout asset');
      }
      const worker = new EventEmitter();
      worker.ref = () => {}; worker.unref = () => {};
      worker.postMessage = message => {
        if (message.type === 'embed') worker.emit('message', { type: 'phase', phase: 'inference' });
      };
      worker.terminate = async () => { forcedTerminations++; worker.emit('exit', 0); };
      const resources = {
        acquire: async request => ({ ...request, status: 'granted', leaseId: 'owned-timeout-fixture' }),
        renew: async () => ({ status: 'renewed' }), release: async () => ({ status: 'released' }),
      };
      const service = new EmbeddingService({ modelRoot: root, closeTimeoutMs: 1, devicePreference: 'cpu',
        resourceService: resources, workerFactory: () => worker });
      const pending = outcome(service.embedQuery('A request closed before its worker finishes starting.'));
      await waitForPhase(service, 'inference');
      const close = service.close();
      assert.equal(service.close(), close);
      await assert.rejects(close, { code: 'EMBEDDING_CLOSE_TIMEOUT' });
      assert.equal((await pending).code, 'EMBEDDING_CLOSED');
      assert.equal(service.status().state, 'error');
      assert.equal(service.status().errorCode, 'EMBEDDING_CLOSE_TIMEOUT');
      await waitForPhase(service, 'stopped');
      assert.equal(service.close(), close, 'a previous timeout cannot become a false successful retirement');
      await assert.rejects(service.close(), { code: 'EMBEDDING_CLOSE_TIMEOUT' });
      report = { scenario, timeoutReported: true, ownedProcessReaped: true };
    } else if (scenario === 'asset-failure') {
      const root = join(home, 'synthetic-invalid-model');
      for (const asset of BUILTIN_EMBEDDING_PROFILE.files) {
        await mkdir(join(root, asset.path, '..'), { recursive: true });
        await writeFile(join(root, asset.path), 'synthetic invalid asset');
      }
      const service = createService({ modelRoot: root });
      await assert.rejects(service.embedQuery('The invalid model cannot load.'), { code: 'EMBEDDING_ASSET_INVALID' });
      await service.close();
      assert.equal(service.status().workerPhase, 'stopped');
      report = { scenario, failedWorkerNaturallyClosed: true };
    } else throw new Error('Unknown isolated embedding scenario.');
    assert.equal(forcedTerminations, scenario === 'close-timeout' ? 1 : 0);
    process.stdout.write(JSON.stringify({ ...report, forcedTerminations }));
  `;
  const child = spawn(process.execPath, ['--no-warnings', '--unhandled-rejections=strict', '--input-type=module', '-e', source,
    serviceModule, profileModule, processModule, scenario, home], {
    env: { ...process.env, USERPROFILE: home, HOME: home, KYNXA_DATA_HOME: join(home, 'Data'),
      KYNXA_MODEL_HOME: '', KYNXA_EXTENSION_HOME: join(home, 'Extensions') },
    windowsHide: true, stdio: ['ignore', 'pipe', 'pipe']
  });
  let stdout = '', stderr = '';
  child.stdout.setEncoding('utf8'); child.stderr.setEncoding('utf8');
  child.stdout.on('data', value => { stdout += value; }); child.stderr.on('data', value => { stderr += value; });
  const completed = new Promise((ready, reject) => {
    child.once('error', reject);
    child.once('close', (code, signal) => ready({ code, signal }));
  });
  const watchdog = setTimeout(() => child.kill(), 45000);
  t.after(async () => {
    clearTimeout(watchdog);
    if (child.exitCode === null && child.signalCode === null) child.kill();
    await completed;
  });
  const result = await completed;
  clearTimeout(watchdog);
  assert.equal(result.code, 0, `${scenario} exits normally: ${stderr}`);
  assert.equal(result.signal, null);
  assert.equal(stderr.trim(), '');
  const report = JSON.parse(stdout);
  assert.equal(report.scenario, scenario);
  assert.equal(report.forcedTerminations, scenario === 'close-timeout' ? 1 : 0);
  return report;
}

test('embedding shutdown safely drains native loading, inference, cancellation and failure paths', { timeout: 180000 }, async t => {
  for (const scenario of ['load-close', 'inference-close', 'cancel-verification', 'close-timeout', 'asset-failure'])
    await t.test(scenario, { timeout: 50000 }, childTest => isolatedEmbeddingCheck(childTest, scenario));
});

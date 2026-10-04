import assert from 'node:assert/strict';
import { test } from 'node:test';
import { access, mkdtemp, mkdir, readFile, stat, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { createServer } from 'node:net';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { storageMigrationActive } from '../storage-maintenance.mjs';
import { EXTENSION_LAYOUT_DIRECTORIES } from '../extension-storage.mjs';

test('gateway pauses writes during migration and reloads committed data root', async t => {
  const home = await mkdtemp(join(tmpdir(), 'kynxa-maintenance-'));
  const profile = join(home, '.kynxa');
  await mkdir(profile);
  const first = join(home, 'first'), second = join(home, 'second');
  for (const root of [first, second]) await mkdir(join(root, 'Models'), { recursive: true });
  await writeFile(join(profile, 'storage.json'), JSON.stringify({ dataRoot: first }));
  const socket = createServer();
  await new Promise(resolve => socket.listen(0, '127.0.0.1', resolve));
  const port = socket.address().port;
  await new Promise(resolve => socket.close(resolve));
  const child = spawn(process.execPath, [fileURLToPath(new URL('../server.mjs', import.meta.url))], {
    env: { ...process.env, USERPROFILE: home, HOME: home, KYNXA_MODEL_API_PORT: String(port), KYNXA_DATA_HOME: '', KYNXA_MODEL_HOME: '',
      KYNXA_EXTENSION_HOME: '', KYNXA_EXTENSION_POINTER: '' },
    windowsHide: true, stdio: 'ignore'
  });
  t.after(async () => { child.kill(); await new Promise(resolve => child.exitCode !== null ? resolve() : child.once('exit', resolve)); });
  const url = `http://127.0.0.1:${port}`;
  let health;
  for (let i = 0; i < 60; i++) {
    try { health = await (await fetch(`${url}/health`)).json(); break; } catch { await new Promise(resolve => setTimeout(resolve, 50)); }
  }
  assert.equal(health.storageProtocol, 1);
  assert.equal(health.modelDataHome, join(first, 'Models'));
  const lock = join(profile, 'storage-migration.lock');
  await writeFile(lock, JSON.stringify({ pid: process.pid }));
  assert.equal(storageMigrationActive(home), true);
  assert.equal((await fetch(`${url}/api/models`)).status, 503);
  health = await (await fetch(`${url}/health`)).json();
  assert.equal(health.migrating, true);
  assert.equal(health.activeRequests, 0);
  await writeFile(join(profile, 'storage.json'), JSON.stringify({ dataRoot: second }));
  await rm(lock);
  health = await (await fetch(`${url}/health`)).json();
  assert.equal(health.modelDataHome, join(second, 'Models'));
  assert.equal((await fetch(`${url}/api/models`)).status, 200);
});

test('gateway initial health respects migration and builds extensions only after release, with repairable layout errors', { timeout: 20000 }, async t => {
  const home = await mkdtemp(join(tmpdir(), 'kynxa-framework-activation-')), profile = join(home, '.kynxa');
  await mkdir(profile);
  const dataRoot = join(home, 'Data'), first = join(home, 'Extensions'), second = join(home, 'FutureExtensions');
  await writeFile(join(profile, 'storage.json'), JSON.stringify({ version: 1, dataRoot }));
  const pointer = join(profile, 'extensions.json'), lock = join(profile, 'storage-migration.lock');
  await writeFile(pointer, JSON.stringify({ version: 1, extensionRoot: first }));
  await writeFile(lock, JSON.stringify({ pid: process.pid }));
  const socket = createServer(); await new Promise(resolve => socket.listen(0, '127.0.0.1', resolve));
  const port = socket.address().port; await new Promise(resolve => socket.close(resolve));
  const child = spawn(process.execPath, [fileURLToPath(new URL('../server.mjs', import.meta.url))], {
    env: { ...process.env, USERPROFILE: home, HOME: home, KYNXA_MODEL_API_PORT: String(port), KYNXA_DATA_HOME: '', KYNXA_MODEL_HOME: '',
      KYNXA_EXTENSION_HOME: '', KYNXA_EXTENSION_POINTER: '' }, windowsHide: true, stdio: 'ignore'
  });
  t.after(async () => {
    child.kill(); await new Promise(resolve => child.exitCode !== null ? resolve() : child.once('exit', resolve));
    const suffix = relative(resolve(tmpdir()), home); assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(home, { recursive: true, force: true });
  });
  const url = `http://127.0.0.1:${port}`;
  let health;
  for (let i = 0; i < 100; i++) {
    try { health = await (await fetch(`${url}/health`)).json(); break; } catch { await new Promise(resolve => setTimeout(resolve, 50)); }
  }
  assert.equal(health?.migrating, true); assert.equal(health.activeRequests, 0);
  await assert.rejects(access(first), { code: 'ENOENT' });
  await rm(lock);
  health = await (await fetch(`${url}/health`)).json();
  assert.equal(health.migrating, false); assert.equal(health.extensionRoot, first); assert.equal(health.storageConfigError, undefined);
  for (const name of EXTENSION_LAYOUT_DIRECTORIES) assert.equal((await stat(join(first, name))).isDirectory(), true, name);
  assert.deepEqual(JSON.parse(await readFile(join(first, 'extension-layout.json'), 'utf8')), { version: 1 });
  await mkdir(second); await writeFile(join(second, 'extension-layout.json'), '{"version":99}');
  await writeFile(pointer, JSON.stringify({ version: 1, extensionRoot: second }));
  health = await (await fetch(`${url}/health`)).json();
  assert.equal(health.storageConfigError, 'UNSUPPORTED_EXTENSION_LAYOUT');
  assert.equal((await fetch(`${url}/api/agent/config`)).status, 503);
  await assert.rejects(access(join(second, 'Agent')), { code: 'ENOENT' });
  assert.equal(await readFile(join(second, 'extension-layout.json'), 'utf8'), '{"version":99}');
  await writeFile(join(second, 'extension-layout.json'), '{"version":1,"extra":"kept"}');
  health = await (await fetch(`${url}/health`)).json();
  assert.equal(health.storageConfigError, undefined); assert.equal(health.extensionRoot, second);
  assert.equal((await fetch(`${url}/api/agent/config`)).status, 200);
  assert.equal(await readFile(join(second, 'extension-layout.json'), 'utf8'), '{"version":1,"extra":"kept"}');
});

test('a retired runtime cleanup failure is redacted and prevents a second runtime or any further writes', { timeout: 20000 }, async t => {
  for (const mode of ['rejected-promise', 'synchronous-throw']) await t.test(mode, async childTest => {
    const home = await mkdtemp(join(tmpdir(), 'kynxa-runtime-retirement-'));
    childTest.after(async () => {
      const suffix = relative(resolve(tmpdir()), resolve(home));
      assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`), 'cleanup remains within the owned temporary profile');
      await rm(home, { recursive: true, force: true });
    });
    const first = join(home, 'first'), second = join(home, 'second');
    for (const root of [first, second]) await mkdir(join(root, 'Models'), { recursive: true });
    await writeFile(join(first, 'Models', 'connections.json'), JSON.stringify({ version: 1, providers: [] }));
    await writeFile(join(second, 'Models', 'connections.json'), JSON.stringify({ version: 1, providers: [{
      providerId: 'moved-synthetic', displayName: 'Synthetic migrated model', baseUrl: 'http://127.0.0.1:9', models: ['synthetic-model']
    }] }));
    const privateFailure = 'PRIVATE_RUNTIME_CLEANUP_FAILURE_DO_NOT_EXPOSE';
    // No server/store import may happen until this subprocess has its own profile and Data.
    // Strict rejection mode also proves that a listener cannot merely hide a fatal rejection.
    // 子进程先配置自身的用户目录和 Data，再导入服务器或存储模块；严格拒绝模式同时证明监听器不能仅掩盖致命拒绝。
    const source = `
      import assert from 'node:assert/strict';
      import { join } from 'node:path';
      const [first, second, mode, serverModule, runtimeModule, privateFailure] = process.argv.slice(1);
      let unhandledCount = 0, oldCloseCount = 0;
      process.on('unhandledRejection', () => { unhandledCount++; });
      const { ModelRuntime } = await import(runtimeModule);
      const originalClose = ModelRuntime.prototype.close;
      ModelRuntime.prototype.close = function () {
        if (this.store.dataHome === join(first, 'Models')) {
          oldCloseCount++;
          if (mode === 'synchronous-throw') throw new Error(privateFailure);
          return Promise.reject(new Error(privateFailure));
        }
        return originalClose.call(this);
      };
      const { createModelServer } = await import(serverModule);
      const server = createModelServer();
      await new Promise(ready => server.listen(0, '127.0.0.1', ready));
      const base = 'http://127.0.0.1:' + server.address().port;
      try {
        const before = await (await fetch(base + '/health')).json();
        assert.equal(before.modelDataHome, join(first, 'Models'));
        assert.equal(before.runtimeCleanupError, undefined);
        process.env.KYNXA_DATA_HOME = second;
        let after;
        for (let attempt = 0; attempt < 40; attempt++) {
          const response = await fetch(base + '/health');
          assert.equal(response.status, 200);
          after = await response.json();
          if (after.runtimeCleanupError) break;
          await new Promise(ready => setTimeout(ready, 10));
        }
        assert.equal(after.modelDataHome, join(first, 'Models'));
        assert.equal(after.runtimeCleanupError, 'RUNTIME_CLEANUP_FAILED');
        const modelsResponse = await fetch(base + '/api/models');
        const models = await modelsResponse.json();
        await new Promise(ready => setImmediate(ready));
        await new Promise(ready => setImmediate(ready));
        process.stdout.write(JSON.stringify({ beforeHome: before.modelDataHome, afterHome: after.modelDataHome,
          cleanupCode: after.runtimeCleanupError, modelsStatus: modelsResponse.status,
          errorCode: models.code, oldCloseCount, unhandledCount }));
      } finally {
        server.closeAllConnections();
        await new Promise((ready, reject) => server.close(error => error ? reject(error) : ready()));
        await server.shutdownModelRuntime();
      }
    `;
    const child = spawn(process.execPath, ['--no-warnings', '--unhandled-rejections=strict', '--input-type=module', '-e', source,
      first, second, mode, new URL('../server.mjs', import.meta.url).href, new URL('../runtime.mjs', import.meta.url).href, privateFailure], {
      env: { ...process.env, USERPROFILE: home, HOME: home, APPDATA: join(home, 'AppData'), LOCALAPPDATA: join(home, 'LocalAppData'),
        KYNXA_DATA_HOME: first, KYNXA_MODEL_HOME: '', KYNXA_EXTENSION_HOME: '', KYNXA_EXTENSION_POINTER: '',
        KYNXA_LEGACY_DESKTOP_HOME: join(home, 'LegacyDesktop') },
      windowsHide: true, stdio: ['ignore', 'pipe', 'pipe']
    });
    let stdout = '', stderr = '';
    child.stdout.setEncoding('utf8'); child.stderr.setEncoding('utf8');
    child.stdout.on('data', value => { stdout += value; }); child.stderr.on('data', value => { stderr += value; });
    const completed = new Promise((ready, reject) => {
      child.once('error', reject);
      child.once('close', (code, signal) => ready({ code, signal }));
    });
    const watchdog = setTimeout(() => child.kill(), 10000);
    childTest.after(async () => { clearTimeout(watchdog); if (child.exitCode === null && child.signalCode === null) child.kill(); await completed; });
    const result = await completed; clearTimeout(watchdog);
    assert.equal(result.code, 0, `isolated gateway exits normally: ${stderr}`);
    assert.equal(result.signal, null);
    const report = JSON.parse(stdout);
    assert.deepEqual(report, { beforeHome: join(first, 'Models'), afterHome: join(first, 'Models'),
      cleanupCode: 'RUNTIME_CLEANUP_FAILED', modelsStatus: 503, errorCode: 'RUNTIME_CLEANUP_FAILED', oldCloseCount: 1, unhandledCount: 0 });
    assert.deepEqual(stderr.trim().split(/\r?\n/), ['RUNTIME_CLEANUP_FAILED'], 'cleanup logs contain only the stable error code');
    assert.equal((stdout + stderr).includes(privateFailure), false, 'private cleanup details never reach health or logs');
  });
});

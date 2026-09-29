import assert from 'node:assert/strict';
import { test } from 'node:test';
import { mkdtemp, mkdir, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createServer } from 'node:net';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { storageMigrationActive } from '../storage-maintenance.mjs';

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
    env: { ...process.env, USERPROFILE: home, HOME: home, KYNXA_MODEL_API_PORT: String(port), KYNXA_DATA_HOME: '', KYNXA_MODEL_HOME: '' },
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

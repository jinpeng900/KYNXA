import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtemp, mkdir, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';
import { HostTerminalRunner } from '../../apps/model-gateway/tools/host-terminal-runner.mjs';

const directory = dirname(fileURLToPath(import.meta.url));

async function command(executable, args) {
  return new Promise((resolvePromise, reject) => {
    const child = spawn(executable, args, { windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
    let output = '';
    child.stdout.on('data', chunk => { output += chunk; });
    child.stderr.on('data', chunk => { output += chunk; });
    child.once('error', reject);
    child.once('close', code => code === 0 ? resolvePromise(output) : reject(new Error(output)));
  });
}

test('real native bridge handles incremental frames and lost terminal receipts without replay',
  { skip: process.platform !== 'win32', timeout: 90000 }, async t => {
    const root = await mkdtemp(join(tmpdir(), 'kynxa-terminal-transport-'));
    let runner;
    t.after(async () => {
      await runner?.close();
      const suffix = relative(resolve(tmpdir()), resolve(root));
      assert.ok(suffix && suffix !== '..' && !suffix.startsWith('..' + sep));
      await rm(root, { recursive: true, force: true });
    });
    const output = join(root, 'fixture');
    await command('dotnet', ['build', join(directory, 'FakeHost.csproj'), '-o', output,
      `-p:BaseIntermediateOutputPath=${join(root, 'obj')}${sep}`, '-v:q']);
    runner = new HostTerminalRunner({ toolHostPath: join(output, 'KYNXA.FakeTerminalHost.exe') });
    async function fixture(name) {
      const cwd = join(root, name); await mkdir(cwd);
      return { shell: 'cmd', script: name, cwd, timeoutMs: 10000 };
    }

    await t.test('effect followed by loss before started notification remains unknown', async () => {
      const request = await fixture('lost_before_started');
      const result = await runner.run(request);
      assert.equal(result.status, 'unknown'); assert.equal(result.value.completed, false);
      assert.equal(await readFile(join(request.cwd, 'effects.txt'), 'utf8'), 'before\n');
    });
    await t.test('explicit refusal before execution stays a known failure', async () => {
      const request = await fixture('not_started');
      await assert.rejects(runner.run(request), { code: 'HOST_TERMINAL_START_FAILED' });
      await assert.rejects(readFile(join(request.cwd, 'effects.txt')), { code: 'ENOENT' });
    });
    await t.test('fragmented UTF-8 progress survives loss of final receipt', async () => {
      const request = await fixture('partial_lost'), events = [];
      const result = await runner.run(request, undefined, event => events.push(event));
      assert.equal(result.status, 'unknown'); assert.equal(events.length, 1);
      assert.equal(events[0].text, '分段中文UTF8输出\n');
      assert.equal(result.value.stdout, events[0].text);
    });
    await t.test('output arrives before the command completes', async () => {
      const request = await fixture('completed_live');
      let notifyOutput;
      const firstOutput = new Promise(resolvePromise => { notifyOutput = resolvePromise; });
      let finished = false;
      const pending = runner.run(request, undefined, notifyOutput).then(value => { finished = true; return value; });
      assert.equal((await firstOutput).text, '分段中文UTF8输出\n'); assert.equal(finished, false);
      assert.equal(await readFile(join(request.cwd, 'effects.txt'), 'utf8'), 'before\n');
      const result = await pending;
      assert.equal(result.isError, false); assert.equal(result.value.stdout, '分段中文UTF8输出\n');
      assert.equal(await readFile(join(request.cwd, 'effects.txt'), 'utf8'), 'before\nafter\n');
    });
    await t.test('bounded replacement frames permit a session exceeding the per-frame transport limit', async () => {
      const request = await fixture('many_console_frames');
      let count = 0, total = 0, latest;
      const result = await runner.run({ ...request, visible: true, keepOpenMs: 0 }, undefined, event => {
        count++; total += event.text.length; latest = event;
      });
      assert.equal(count, 40); assert.ok(total > 2 * 1024 * 1024);
      assert.equal(latest.replace, true); assert.equal(latest.sequence, 40);
      assert.equal(result.isError, false); assert.equal(result.value.consoleText, 'final bounded screen');
    });
    await t.test('display failure stops subsequent effects and retains partial output', async () => {
      const request = await fixture('display_failure');
      const result = await runner.run(request, undefined, () => { throw new Error('synthetic display failure'); });
      assert.equal(result.status, 'unknown'); assert.equal(result.value.stdout, '分段中文UTF8输出\n');
      assert.equal(await readFile(join(request.cwd, 'effects.txt'), 'utf8'), 'before\n');
    });
  });

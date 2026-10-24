import { spawn } from 'node:child_process';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';

const [edgePath, fixtureUri, resultDirectory] = process.argv.slice(2);
if (!edgePath || !fixtureUri || !resultDirectory) {
  throw new Error('Usage: cdp-checks.mjs <edge executable> <fixture URI> <result directory>');
}

class BrowserProtocol {
  constructor(socket) {
    this.socket = socket;
    this.sequence = 0;
    this.pending = new Map();
    this.exceptions = [];
    socket.addEventListener('message', event => {
      const response = JSON.parse(event.data);
      if (response.method === 'Runtime.exceptionThrown') this.exceptions.push(response.params.exceptionDetails);
      const pending = this.pending.get(response.id);
      if (!pending) return;
      this.pending.delete(response.id);
      clearTimeout(pending.timeout);
      if (response.error) pending.reject(new Error(JSON.stringify(response.error)));
      else pending.resolve(response.result);
    });
    socket.addEventListener('close', () => {
      for (const pending of this.pending.values()) {
        clearTimeout(pending.timeout);
        pending.reject(new Error('Isolated browser connection closed.'));
      }
      this.pending.clear();
    });
  }

  command(method, params = {}, sessionId) {
    return new Promise((resolve, reject) => {
      const id = ++this.sequence;
      const timeout = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`Browser protocol timed out: ${method}`));
      }, 10000);
      this.pending.set(id, { resolve, reject, timeout });
      this.socket.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }));
    });
  }

  async evaluate(expression, sessionId) {
    const response = await this.command('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true }, sessionId);
    if (response.exceptionDetails) throw new Error(JSON.stringify(response.exceptionDetails));
    return response.result.value;
  }
}

async function connectBrowser(profileDirectory, browser) {
  const deadline = Date.now() + 15000;
  while (Date.now() < deadline) {
    if (browser.exitCode !== null) throw new Error(`Isolated browser exited before CDP startup: ${browser.exitCode}`);
    try {
      const [port, endpoint] = (await readFile(join(profileDirectory, 'DevToolsActivePort'), 'utf8')).trim().split(/\r?\n/);
      const socket = new WebSocket(`ws://127.0.0.1:${port}${endpoint}`);
      await new Promise((resolve, reject) => {
        socket.addEventListener('open', resolve, { once: true });
        socket.addEventListener('error', reject, { once: true });
      });
      return new BrowserProtocol(socket);
    } catch (error) {
      if (Date.now() >= deadline) throw error;
      await delay(50);
    }
  }
  throw new Error('Isolated browser did not publish a CDP endpoint.');
}

await mkdir(resultDirectory, { recursive: true });
const profileDirectory = join(resultDirectory, `edge-profile-${process.pid}-${Date.now()}`);
const browser = spawn(edgePath, [
  '--headless=new', '--no-first-run', '--no-default-browser-check', '--disable-extensions',
  '--disable-background-timer-throttling', '--disable-renderer-backgrounding',
  '--disable-backgrounding-occluded-windows', '--window-size=1280,1000',
  '--allow-file-access-from-files', '--remote-debugging-address=127.0.0.1', '--remote-debugging-port=0',
  `--user-data-dir=${profileDirectory}`, 'about:blank'
], { windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
let browserLog = '';
browser.stderr.on('data', value => { browserLog += value; });
browser.stdout.on('data', value => { browserLog += value; });
let browserError;
browser.on('error', error => { browserError = error; });
let protocol;
let sessionId;
let result = 'RUNNING';
let failure;
const started = Date.now();
try {
  protocol = await connectBrowser(profileDirectory, browser);
  const { targetId } = await protocol.command('Target.createTarget', { url: 'about:blank' });
  ({ sessionId } = await protocol.command('Target.attachToTarget', { targetId, flatten: true }));
  await protocol.command('Page.enable', {}, sessionId);
  await protocol.command('Runtime.enable', {}, sessionId);
  await protocol.command('Target.activateTarget', { targetId });
  await protocol.command('Page.navigate', { url: fixtureUri }, sessionId);
  const deadline = Date.now() + 30000;
  while (Date.now() < deadline) {
    // Use real time and native animation frames; the fixture keeps the production virtualizer intact.
    // 使用真实时钟和原生动画帧，测试不替换产品的虚拟化实现。
    try { result = await protocol.evaluate("document.getElementById('result')?.textContent || 'RUNNING'", sessionId); }
    catch (error) {
      if (!String(error).includes('context') && !String(error).includes('navigation')) throw error;
    }
    if (/^(PASS:|FAIL:)/.test(result)) break;
    await delay(25);
  }
  if (!result.startsWith('PASS:')) throw new Error(result.startsWith('FAIL:') ? result : 'DOM fixture did not complete in 30 seconds.');
} catch (error) {
  failure = error;
} finally {
  if (protocol && sessionId) {
    try { await writeFile(join(resultDirectory, 'dom.txt'), await protocol.evaluate('document.documentElement.outerHTML', sessionId)); }
    catch (error) { browserLog += `\nDOM capture failed: ${error}`; }
  }
  await writeFile(join(resultDirectory, 'browser.txt'), browserLog);
  await writeFile(join(resultDirectory, 'result.json'), JSON.stringify({
    passed: !failure, result, elapsedMs: Date.now() - started,
    exceptions: protocol?.exceptions ?? [], error: failure?.stack ?? browserError?.stack
  }, null, 2));
  // Close only this test's own profile and process tree; never stop a user's browser by name.
  // 只关闭本测试创建的独立进程树，不能按进程名结束用户的浏览器。
  if (protocol) {
    try { await protocol.command('Browser.close'); } catch { }
    protocol.socket.close();
  }
  for (let attempt = 0; attempt < 20 && browser.exitCode === null; attempt++) await delay(50);
  if (browser.exitCode === null && browser.pid) {
    await new Promise(resolve => {
      const stop = spawn('taskkill.exe', ['/PID', String(browser.pid), '/T', '/F'], { windowsHide: true, stdio: 'ignore' });
      stop.once('exit', resolve);
      stop.once('error', resolve);
    });
  }
}
if (failure) { console.error(failure.stack); process.exitCode = 1; }
else console.log(result);

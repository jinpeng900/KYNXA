import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { execFile, spawn } from 'node:child_process';
import { mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { promisify } from 'node:util';
import { McpToolClients } from '../../apps/model-gateway/tools/mcp-client.mjs';
import { inspectScreenshotImage } from '../../apps/model-gateway/tools/browser-artifacts.mjs';

// Explicit opt-in paths select already installed packages. No user browser profile,
// user conversation, cloud model, credential or dependency installer is used.
// 显式选择已安装的包路径；不使用用户浏览器配置、聊天、云模型、凭据或依赖安装器。
const playwright = process.env.KYNXA_LIVE_PLAYWRIGHT_ENTRY;
const devtools = process.env.KYNXA_LIVE_CHROME_DEVTOOLS_ENTRY;
const browser = process.env.KYNXA_LIVE_BROWSER_PATH;
const remoteCdp = process.argv.includes('--remote-cdp');
assert.ok(browser && (playwright || devtools), 'Supply an installed browser and at least one existing MCP package entry point.');
const root = await mkdtemp(join(tmpdir(), 'kynxa-browser-artifacts-live-'));
const http = createServer((_request, response) => {
  response.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' });
  response.end('<!doctype html><title>KYNXA isolated screenshot</title><style>body{font:28px sans-serif;background:#eee;padding:40px}</style><h1>Synthetic browser screenshot</h1><button id="probe">Local fixture only</button>');
});
await new Promise(resolve => http.listen(0, '127.0.0.1', resolve));
const url = `http://127.0.0.1:${http.address().port}/`;
const clients = new McpToolClients();
let checks = 0, browserProcess, browserError, endpoint;
async function closeOwnedBrowser() {
  if (!browserProcess || browserProcess.exitCode !== null || !browserProcess.pid) return;
  try {
    const metadata = await (await fetch(endpoint + '/json/version', { signal: AbortSignal.timeout(3000) })).json();
    const address = new URL(metadata.webSocketDebuggerUrl);
    assert.equal(address.hostname, '127.0.0.1'); assert.equal(address.port, new URL(endpoint).port);
    await new Promise(resolve => {
      const socket = new WebSocket(address.href); let settled = false;
      const finish = () => { if (settled) return; settled = true; clearTimeout(timer); socket.close(); resolve(); };
      const timer = setTimeout(finish, 3000);
      socket.addEventListener('open', () => socket.send(JSON.stringify({ id: 1, method: 'Browser.close' })));
      socket.addEventListener('message', event => { try { if (JSON.parse(event.data).id === 1) finish(); } catch { } });
      socket.addEventListener('close', finish); socket.addEventListener('error', finish);
    });
  } catch { /* Only this fixture's retained process handle can be used for the fallback.
   * 回退逻辑只能使用此测试夹具保留的自有进程句柄。
   */ }
  if (browserProcess.exitCode === null) await new Promise(resolve => {
    const timer = setTimeout(resolve, 3000);
    browserProcess.once('close', () => { clearTimeout(timer); resolve(); });
  });
  if (browserProcess.exitCode !== null) return;
  if (process.platform === 'win32') {
    await promisify(execFile)(join(process.env.SystemRoot, 'System32', 'taskkill.exe'),
      ['/PID', String(browserProcess.pid), '/T', '/F'], { windowsHide: true, shell: false, timeout: 5000 });
  } else browserProcess.kill();
}
try {
  if (remoteCdp) {
    const profile = join(root, 'owned-cdp-profile');
    browserProcess = spawn(browser, ['--headless=new', '--no-first-run', '--no-default-browser-check',
      '--disable-background-networking', '--remote-debugging-port=0', '--user-data-dir=' + profile,
      '--window-size=960,640', 'about:blank'], { stdio: 'ignore', windowsHide: true, shell: false });
    browserProcess.on('error', error => { browserError = error; });
    const deadline = Date.now() + 10000;
    while (!endpoint) {
      if (browserError) throw browserError;
      if (browserProcess.exitCode !== null) throw new Error('Owned CDP browser exited before becoming available.');
      try {
        const port = Number((await readFile(join(profile, 'DevToolsActivePort'), 'utf8')).split('\n')[0]);
        if (Number.isInteger(port) && port > 0 && port < 65536) endpoint = `http://127.0.0.1:${port}`;
      } catch (error) { if (error.code !== 'ENOENT') throw error; }
      if (Date.now() > deadline) throw new Error('Owned CDP fixture startup timed out.');
      if (!endpoint) await new Promise(resolve => setTimeout(resolve, 20));
    }
    checks++;
  }
  for (const [kind, entry] of [['playwright', playwright], ['devtools', devtools]]) {
    if (!entry) continue;
    let processArgs;
    if (remoteCdp) processArgs = [entry, kind === 'playwright' ? '--cdp-endpoint' : '--browser-url', endpoint];
    else processArgs = [entry, '--headless', '--isolated', '--executable-path', browser];
    if (kind === 'devtools') processArgs.push('--no-usage-statistics', '--no-performance-crux');
    const server = { id: 'isolated-' + kind, name: 'Isolated synthetic ' + kind, enabled: true, transport: 'stdio',
      protocolVersion: '2025-11-25', startupTimeoutMs: 30000, command: process.execPath, cwd: root,
      args: processArgs,
      env: kind === 'playwright' ? {} : { CHROME_DEVTOOLS_MCP_NO_UPDATE_CHECKS: '1' } };
    const tools = await clients.catalog({ mcpServers: [server] }, { workspaceRoot: root }, { connect: true });
    assert.ok(tools.length > 0, 'Pinned MCP connects using the production client.'); checks++;
    const call = async (name, args) => {
      const descriptor = tools.find(tool => tool.toolName === name);
      assert.ok(descriptor, 'Real browser tool is discovered: ' + name);
      const result = await clients.execute(descriptor, { arguments: args, policy: { reason: 'Verify an isolated local screenshot fixture.' } });
      assert.equal(result.isError, false, 'Real browser operation completed: ' + name); checks++;
      return result;
    };
    let pageId;
    if (kind === 'playwright') await call('browser_navigate', { url });
    else {
      const opened = await call('new_page', { url, timeout: 10000 });
      pageId = Number(/^([0-9]+): .*KYNXA|^([0-9]+): .*127\.0\.0\.1/m.exec(opened.content)?.slice(1).find(Boolean));
      assert.ok(Number.isSafeInteger(pageId) && pageId > 0, 'Chrome returns the identity of the fixture tab.'); checks++;
    }
    const snapshot = kind === 'playwright' ? await call('browser_snapshot', {}) : await call('take_snapshot', { pageId });
    assert.match(snapshot.content, /Synthetic browser screenshot/, 'Attached browser returns the fixture DOM accessibility text.'); checks++;
    const filename = kind + (kind === 'devtools' ? '.jpeg' : '.png');
    const result = kind === 'playwright' ? await call('browser_take_screenshot', { ...(remoteCdp ? {} : { filename }), scale: 'css' })
      : await call('take_screenshot', { pageId, ...(remoteCdp ? {} : { filePath: join(root, filename) }), format: 'jpeg' });
    assert.equal(result.screenshotStatus, 'available', 'Filename-only result is hydrated into a typed image.'); checks++;
    const block = result.canonical.content.find(block => block.type === 'image');
    assert.ok(block, 'Canonical archive contains typed screenshot bytes.'); checks++;
    const bytes = Buffer.from(block.data, 'base64'), dimensions = inspectScreenshotImage(bytes, block.mimeType);
    assert.ok(dimensions && dimensions.width > 100 && dimensions.height > 100, 'The real browser captured fixture pixels within bounds.'); checks++;
    if (remoteCdp) await writeFile(join(root, filename), bytes);
    else { assert.deepEqual(bytes, await readFile(join(root, filename)), 'Archived image equals the browser-written file.'); checks++; }
    assert.equal(result.content.includes(block.data), false, 'Text model receives no base64 pixels.'); checks++;
    assert.match(result.content, /No image pixels were sent/); checks++;
    await writeFile(join(root, kind + '-canonical.json'), JSON.stringify(result.canonical));
    await clients.disconnect(server.id);
  }
} finally {
  try { await clients.close(); }
  finally {
    try { await closeOwnedBrowser(); }
    finally { await new Promise(resolve => http.close(resolve)); }
  }
}
const mode = remoteCdp ? 'remote CDP protocol through an owned loopback fixture' : 'isolated local browser';
await writeFile(join(root, 'result.txt'), `PASS: ${checks} real browser artifact checks (${mode}).\n`);
process.stdout.write(`PASS: ${checks} real browser artifact checks (${mode}).\nFixture artifacts: ${root}\n`);

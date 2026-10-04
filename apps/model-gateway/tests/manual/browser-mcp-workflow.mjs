// Explicit live acceptance against pinned public packages and a disposable local page.
// No user configuration, logged-in profile, external website or paid model is touched.
// 使用固定版本的公开包和临时本地页面进行显式验收；不触及用户配置、登录资料、外部网站或付费模型。
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, readFile, writeFile, realpath } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join, resolve, relative } from 'node:path';
import { McpToolClients } from '../../mcp-client.mjs';

const [runtimeDirectory, chromeExecutable] = process.argv.slice(2);
if (!runtimeDirectory || !chromeExecutable) throw Error('Usage: node browser-mcp-workflow.mjs <temporary npm runtime directory> <Chrome executable>');
const runtime = await realpath(resolve(runtimeDirectory));
if (relative(await realpath(tmpdir()), runtime).startsWith('..')) throw Error('The package runtime must be isolated under the temporary directory.');
const root = await mkdtemp(join(tmpdir(), 'kynxa-browser-workflow-'));
const page = createServer((request, response) => {
  response.setHeader('Content-Type', 'text/html; charset=utf-8');
  if (request.url === '/frame') response.end('<!doctype html><title>Frame</title><button onclick="document.getElementById(\'status\').textContent=\'clicked-from-iframe\'">Frame Button</button><p id="status">not-clicked</p><input aria-label="Fixture text">');
  else response.end(`<!doctype html><title>KYNXA disposable browser fixture</title><h1>Background browser fixture</h1><iframe title="Cross-origin fixture" src="http://127.0.0.1:${page.address().port}/frame"></iframe><p>Public fixture only.</p>`);
});
await new Promise(resolveListen => page.listen(0, '127.0.0.1', resolveListen));
const url = `http://localhost:${page.address().port}/`, report = { root, checkedAt: new Date().toISOString(), results: [] };
const text = result => result.canonical.content.filter(block => block.type === 'text').map(block => block.text).join('\n');
const envelope = args => ({ arguments: args, policy: { reason: 'Operate the disposable local acceptance page.' } });
try {
  for (const engine of ['chrome-devtools', 'playwright']) {
    const work = join(root, engine, 'work');
    await mkdir(work, { recursive: true });
    const packageDirectory = engine === 'chrome-devtools' ? join(runtime, 'node_modules/chrome-devtools-mcp') : join(runtime, 'node_modules/@playwright/mcp');
    const metadata = JSON.parse(await readFile(join(packageDirectory, 'package.json'), 'utf8'));
    assert.equal(metadata.version, engine === 'chrome-devtools' ? '1.10.1' : '0.0.83');
    const cli = engine === 'chrome-devtools' ? join(packageDirectory, 'build/src/bin/chrome-devtools-mcp.js') : join(packageDirectory, 'cli.js');
    const server = { id: engine, name: engine, command: process.execPath, enabled: true, startupTimeoutMs: 120000,
      args: engine === 'chrome-devtools' ? [cli, '--headless', '--isolated', '--executablePath', chromeExecutable,
        '--no-usage-statistics', '--filesystem-root', work] : [cli, '--headless', '--isolated', '--executable-path', chromeExecutable, '--output-dir', work],
      cwd: work };
    const clients = new McpToolClients({ extensionRoot: join(root, engine, 'extensions') });
    const started = Date.now(), item = { engine, version: metadata.version };
    try {
      const tools = await clients.catalog({ mcpServers: [server] }, { workspaceRoot: work }, { connect: true });
      assert.ok(tools.length, JSON.stringify(clients.diagnostics()));
      const call = async (name, args = {}) => {
        const descriptor = tools.find(tool => tool.toolName === name); assert.ok(descriptor, `Missing pinned tool ${name}`);
        const input = await clients.prepareBrowserExecution(descriptor, envelope(args), { sessionId: 'fixture', background: true, allowForeground: false });
        const result = await clients.execute(descriptor, input, undefined, { sessionId: 'fixture', background: true, allowForeground: false });
        await writeFile(join(work, 'last-output.txt'), text(result));
        assert.equal(result.isError, false, result.content.slice(0, 400)); return { descriptor, input, result };
      };
      if (engine === 'chrome-devtools') {
        const opened = await call('new_page', { url }); assert.equal(opened.input.arguments.background, true);
        assert.ok(opened.result.browser.tabId); item.tabId = opened.result.browser.tabId;
        const selected = await call('select_page', { pageId: Number(item.tabId) }); assert.equal(selected.input.arguments.bringToFront, false);
      } else {
        // Start with a snapshot, not a hidden tabs RPC. An unbound snapshot stays
        // readable, while later reference actions require explicit tab discovery.
        // 从快照开始，不暗中查询标签页；未绑定快照仍可读取，后续引用操作需要显式发现标签页。
        const initial = await call('browser_snapshot');
        item.firstSnapshotUnbound = initial.result.browser.needsTabDiscovery === true || !initial.result.browser.tabId;
        await call('browser_tabs', { action: 'list' }); await call('browser_navigate', { url });
      }
      const snapshot = await call(engine === 'chrome-devtools' ? 'take_snapshot' : 'browser_snapshot');
      await writeFile(join(work, 'snapshot.txt'), text(snapshot.result));
      const button = engine === 'chrome-devtools' ? text(snapshot.result).match(/uid=(\S+)\s+button "Frame Button"/)?.[1]
        : text(snapshot.result).match(/button "Frame Button"\s+\[ref=([^\]]+)\]/)?.[1];
      assert.ok(button, `Cross-origin iframe button absent in ${engine} snapshot; see ${join(work, 'snapshot.txt')}`);
      const click = tools.find(tool => tool.toolName === (engine === 'chrome-devtools' ? 'click' : 'browser_click'));
      const refName = engine === 'chrome-devtools' ? 'uid' : click.originalInputSchema.properties.ref ? 'ref' : 'target';
      await call(click.toolName, { [refName]: button });
      const refreshed = await call(snapshot.descriptor.toolName); assert.match(text(refreshed.result), /clicked-from-iframe/);
      const shot = await call(engine === 'chrome-devtools' ? 'take_screenshot' : 'browser_take_screenshot',
        engine === 'chrome-devtools' ? { filePath: join(work, 'iframe-shot.png') } : { filename: 'iframe-shot.png' });
      assert.equal(shot.result.screenshotStatus, 'available'); assert.ok(shot.result.canonical.content.some(block => block.type === 'image'));
      assert.match(shot.result.content, /No image pixels were sent/);
      item.success = true; item.iframeClickVerified = true; item.screenshotArchived = true;
      item.identity = refreshed.result.browser; item.backgroundDefaults = engine === 'chrome-devtools';
    } catch (error) { item.success = false; item.code = error.code ?? error.name; item.message = error.message.slice(0, 500); }
    finally { await clients.close(); }
    item.elapsedMs = Date.now() - started; report.results.push(item); console.log(JSON.stringify(item));
  }
} finally { await new Promise(resolveClose => page.close(resolveClose)); }
await writeFile(join(root, 'report.json'), JSON.stringify(report, null, 2));
console.log(JSON.stringify({ report: join(root, 'report.json'), passed: report.results.filter(item => item.success).length, total: report.results.length }));
if (report.results.some(item => !item.success)) process.exitCode = 1;

import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { access, mkdtemp, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { randomUUID } from 'node:crypto';
import { DesktopRunner, invokeDesktopHost } from '../../apps/model-gateway/desktop-runner.mjs';

// Own isolated profiles and loopback pages only. No user's browser/profile is inspected.
const artifact = await mkdtemp(join(tmpdir(), 'kynxa-native-browser-'));
const host = process.env.KYNXA_DESKTOP_SMOKE_TOOL_HOST
  ?? resolve('apps/tool-host/bin/Debug/net10.0-windows/win-x64/KYNXA.ToolHost.exe');
const candidates = [
  join(process.env['ProgramFiles(x86)'] ?? '', 'Microsoft/Edge/Application/msedge.exe'),
  join(process.env.ProgramFiles ?? '', 'Google/Chrome/Application/chrome.exe'),
];
let checks = 0;
function check(value, label) { assert.ok(value, label); checks++; }
const runner = new DesktopRunner({ toolHostPath: host });
async function request(value) {
  const { operation: _operation, action, ...args } = value;
  const result = await runner.run(action, { ...args, reason: 'Synthetic isolated browser verification.' });
  assert.equal(result.isError, false, result.value?.message);
  const receipt = result.value ?? { ...result.canonical.structuredContent, data: result.canonical.content[0].data };
  check(receipt.boundary === 'host-desktop' && receipt.completed, 'Honest completed desktop receipt through production transport');
  return receipt;
}
const sleep = ms => new Promise(accept => setTimeout(accept, ms));
const server = createServer((incoming, response) => {
  const marker = incoming.url?.split('/').at(-1) ?? 'unknown';
  response.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', 'Content-Security-Policy': "default-src 'none'; style-src 'unsafe-inline'" });
  response.end(`<!doctype html><html lang="en"><title>KYNXA browser ${marker}</title><style>body{font:24px sans-serif;padding:30px;color:#123}</style><h1>KYNXA visible browser ${marker}</h1><p>中文浏览器内容 ${marker}</p><input aria-label="Synthetic browser input" value="Owned temporary page"><p><a href="http://127.0.0.1:${server.address().port}/${marker}">Synthetic loopback source</a></p><input type="password" value="SYNTHETIC_BROWSER_PASSWORD_NEVER_EXPORT">`);
});
await new Promise(accept => server.listen(0, '127.0.0.1', accept));
const report = [];
try {
  const capability = await runner.capabilities();
  check(capability.available === true, 'Real production desktop capability transport');
  for (const executable of candidates) {
    try { await access(executable); } catch { continue; }
    const marker = randomUUID(), profile = join(artifact, marker), url = `http://127.0.0.1:${server.address().port}/${marker}`;
    let ownedPid, blankPid;
    try {
      const blankStarted = Date.now();
      const blank = await request({ operation: 'desktop', action: 'launch', appPath: executable, background: true, args: [
        `--user-data-dir=${profile}-blank`, '--no-first-run', '--disable-sync', '--disable-background-networking', 'about:blank'
      ] });
      blankPid = blank.processId;
      check(blank.backgroundRequested === true && blank.backgroundMode === 'best-effort-no-activate', 'Browser startup uses an honest best-effort background hint');
      check(Date.now() - blankStarted < 8000, 'Production about:blank launch settles while its browser remains open');
      const blankCleanup = spawn('taskkill.exe', ['/pid', String(blank.processId), '/t', '/f'], { windowsHide: true, stdio: 'ignore' });
      await new Promise(accept => blankCleanup.once('exit', accept));
      blankPid = undefined;
      const launchStarted = Date.now();
      const launched = await request({ operation: 'desktop', action: 'launch', appPath: executable, background: true, args: [
        `--user-data-dir=${profile}`, '--force-renderer-accessibility', '--no-first-run', '--disable-sync',
        '--disable-backgrounding-occluded-windows',
        '--disable-background-networking', '--disable-component-update', '--disable-default-apps',
        '--window-position=80,80', '--window-size=1000,760', url,
      ] });
      ownedPid = launched.processId;
      check(Date.now() - launchStarted < 8000, 'Production page launch settles while its browser remains open');
      let target;
      for (let attempt = 0; attempt < 50; attempt++) {
        const listed = await request({ operation: 'desktop', action: 'windows', processId: ownedPid });
        check(listed.windows.every(window => window.processId === ownedPid), 'Only isolated browser process inspected');
        target = listed.windows.find(window => window.title.includes(marker));
        if (target) break;
        await sleep(200);
      }
      assert.ok(target, 'Isolated browser window discovered under launched PID');
      await sleep(500);
      const screenshot = await request({ operation: 'desktop', action: 'screenshot', windowId: target.windowId, processId: ownedPid });
      await writeFile(join(artifact, `${marker}-browser.png`), Buffer.from(screenshot.data, 'base64'));
      let read;
      for (let attempt = 0; attempt < 8; attempt++) {
        read = await request({ operation: 'desktop', action: 'read', windowId: target.windowId, processId: ownedPid, maxCharacters: 32000, maxElements: 400 });
        if (read.text.includes(`KYNXA visible browser ${marker}`) && read.accessibleUrls.includes(url)) break;
        await sleep(300); // Chromium can initialize its cross-process accessibility provider asynchronously.
      }
      await writeFile(join(artifact, `${marker}-read.json`), JSON.stringify(read, null, 2));
      check(read.source === 'uia-visible', 'Browser source is visible UI Automation');
      check(read.text.includes(`KYNXA visible browser ${marker}`), 'Actual rendered browser document text');
      check(read.text.includes(`中文浏览器内容 ${marker}`), 'Actual bilingual page text');
      check(read.accessibleUrls.includes(url), 'Actual loopback browser address is accessible');
      check(!JSON.stringify(read).includes('SYNTHETIC_BROWSER_PASSWORD_NEVER_EXPORT'), 'Browser password control excluded');
      const passwordElements = read.elements.filter(element => element.isPassword);
      check(passwordElements.length > 0 && passwordElements.every(element => element.name === '' && element.value === '' && typeof element.elementId === 'string'), 'Browser password controls retain location without name or value');
      const controller = new AbortController(), originalInvoke = runner.invoke;
      let rpcStarted = false;
      try {
        runner.invoke = (binary, request_, signal, timeoutMs) => invokeDesktopHost(binary, request_, signal, timeoutMs,
          (binary_, args_, options) => {
            const child = spawn(binary_, args_, options);
            child.once('spawn', () => { rpcStarted = true; controller.abort(); });
            return child;
          });
        const pending = runner.run('read', { windowId: target.windowId, processId: ownedPid, reason: 'Cancel only an owned fixture read.' }, controller.signal);
        try { const result = await pending; check(result.value?.completed === true, 'Late cancellation retains a real read completion'); }
        catch (error) { check(error.name === 'AbortError' || error.code === 'DESKTOP_CANCELLED', 'Real native read cancellation settles'); }
        check(rpcStarted, 'Cancellation occurred after actual native helper creation');
        check(runner.active.size === 0, 'Cancelled production operation is removed from active set');
      } finally { runner.invoke = originalInvoke; }
      const afterCancel = await request({ operation: 'desktop', action: 'windows', processId: ownedPid });
      check(afterCancel.windows.some(window => window.windowId === target.windowId), 'Cancellation preserves the independent owned browser');
      report.push({ executable, passed: true, marker, processId: ownedPid, windowId: target.windowId, source: read.source, url });
    } finally {
      for (const ownedBrowserPid of [ownedPid, blankPid].filter(pid => Number.isSafeInteger(pid) && pid > 0)) {
        // The root PID was returned for the brand-new isolated user-data-dir above.
        const cleanup = spawn('taskkill.exe', ['/pid', String(ownedBrowserPid), '/t', '/f'], { windowsHide: true, stdio: 'ignore' });
        await new Promise(accept => cleanup.once('exit', accept));
      }
    }
  }
  check(report.length > 0, 'At least one installed real browser verified');
  await writeFile(join(artifact, 'result.json'), JSON.stringify({ passed: true, checks, browsers: report }, null, 2));
  console.log(JSON.stringify({ passed: true, checks, browsers: report.map(item => item.executable), artifact }));
} catch (error) {
  await writeFile(join(artifact, 'result.json'), JSON.stringify({ passed: false, checks, browsers: report, error: error.stack }, null, 2));
  console.error(JSON.stringify({ passed: false, checks, error: error.message, artifact }));
  process.exitCode = 1;
} finally { await runner.close(); await new Promise(accept => server.close(accept)); }

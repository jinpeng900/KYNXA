import assert from 'node:assert/strict';
import { mkdtemp, mkdir, rm, realpath, symlink, link, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { archiveBrowserScreenshot, inspectScreenshotImage } from '../browser-artifacts.mjs';
import { McpToolClients } from '../mcp-client.mjs';
import { publicToolResult } from '../tool-result-store.mjs';

const png = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jR2kAAAAASUVORK5CYII=', 'base64');
const descriptor = (toolName = 'browser_take_screenshot') => ({ name: `mcp.custom-browser.${toolName}`, toolName, operation: 'tools/call' });
const upstream = text => ({ content: [{ type: 'text', text }], structuredContent: { preserved: true }, _meta: { vendor: 'preserve-me' }, isError: false });
async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-browser-artifacts-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const work = join(root, 'work'), other = join(root, 'other'); await mkdir(work); await mkdir(other);
  return { root, work, other, server: { transport: 'stdio', args: ['@playwright/mcp@0.0.83'] }, context: { workspaceRoot: work } };
}

test('Playwright filename output is archived from this connection cwd while preserving upstream fields and text', async t => {
  const f = await fixture(t); await writeFile(join(f.work, 'chrome-opened.png'), png);
  const original = upstream('[Screenshot of viewport](./chrome-opened.png)'), frozen = structuredClone(original);
  const receipt = await archiveBrowserScreenshot(original, { ...f, descriptor: descriptor(), args: { filename: 'chrome-opened.png' } });
  assert.equal(receipt.screenshotStatus, 'available'); assert.deepEqual(original, frozen);
  assert.deepEqual(receipt.canonical.content.slice(0, original.content.length), original.content);
  assert.deepEqual(receipt.canonical._meta, original._meta); assert.deepEqual(receipt.canonical.structuredContent, original.structuredContent);
  assert.deepEqual(receipt.canonical.content.at(-1), { type: 'image', mimeType: 'image/png', data: png.toString('base64') });
  const projection = publicToolResult(receipt.canonical);
  assert.equal(projection.content.at(-1).data, undefined); assert.equal(JSON.stringify(projection).includes(png.toString('base64')), false);
});

test('Chrome filePath and explicit process cwd override the conversation workspace without reading outside those roots', async t => {
  const f = await fixture(t); const path = join(f.other, 'shot.png'); await writeFile(path, png);
  const options = { ...f, descriptor: descriptor('take_screenshot'), args: { filePath: path } };
  assert.equal((await archiveBrowserScreenshot(upstream('Saved screenshot to ' + path), options)).screenshotStatus, 'unavailable');
  const receipt = await archiveBrowserScreenshot(upstream('Saved screenshot to ' + path), { ...options, server: { ...f.server, cwd: f.other } });
  assert.equal(receipt.screenshotStatus, 'available');
});

test('Chrome normalized output extension follows its saved-file receipt instead of reading an unrelated old requested file', async t => {
  const f = await fixture(t), requested = join(f.work, 'shot.jpg'), actual = join(f.work, 'shot.jpeg');
  await writeFile(requested, 'old unrelated file');
  // A PNG payload can still be validated by its bytes; the extension never supplies the MIME type.
  await writeFile(actual, png);
  const receipt = await archiveBrowserScreenshot(upstream('Saved screenshot to ' + actual + '.'),
    { ...f, descriptor: descriptor('take_screenshot'), args: { filePath: requested, format: 'jpeg' } });
  assert.equal(receipt.screenshotStatus, 'available'); assert.equal(receipt.canonical.content.at(-1).mimeType, 'image/png');
  const unrelated = join(f.work, 'unrelated.jpeg'); await writeFile(unrelated, png);
  const rejected = await archiveBrowserScreenshot(upstream('Saved screenshot to ' + unrelated + '.'),
    { ...f, descriptor: descriptor('take_screenshot'), args: { filePath: requested } });
  assert.equal(rejected.screenshotStatus, 'unavailable');
});

test('expanded Windows short-path receipts are accepted only through a previously validated configured root', async t => {
  const f = await fixture(t), requested = join(f.work, 'receipt.png'); await writeFile(requested, png);
  const expanded = await realpath(requested);
  const receipt = await archiveBrowserScreenshot(upstream('Saved screenshot to ' + expanded + '.'),
    { ...f, descriptor: descriptor('take_screenshot'), args: { filePath: requested } });
  assert.equal(receipt.screenshotStatus, 'available');
  const outside = join(f.other, 'outside.png'); await writeFile(outside, png);
  const rejected = await archiveBrowserScreenshot(upstream('Saved screenshot to ' + await realpath(outside) + '.'),
    { ...f, descriptor: descriptor('take_screenshot'), args: { filePath: outside } });
  assert.equal(rejected.screenshotStatus, 'unavailable');
});

test('generated Markdown paths are used only with an explicitly configured output directory, never arbitrary text links', async t => {
  const f = await fixture(t); const path = join(f.other, 'generated.png'); await writeFile(path, png);
  const original = upstream(`[Screenshot of viewport](${path})`);
  assert.equal((await archiveBrowserScreenshot(original, { ...f, descriptor: descriptor() })).screenshotStatus, 'unavailable');
  for (const server of [{ ...f.server, args: [...f.server.args, '--output-dir', f.other] },
    { ...f.server, env: { PLAYWRIGHT_MCP_OUTPUT_DIR: f.other } }]) {
    assert.equal((await archiveBrowserScreenshot(original, { ...f, server, descriptor: descriptor() })).screenshotStatus, 'available');
  }
  assert.equal((await archiveBrowserScreenshot(upstream(`[Read arbitrary document](${path})`), { ...f,
    server: { ...f.server, args: ['--output-dir', f.other] }, descriptor: descriptor() })).screenshotStatus, 'unavailable');
  const ordinary = await archiveBrowserScreenshot(original, { ...f, descriptor: descriptor('read_file'), args: { filename: path } });
  assert.equal(ordinary.screenshotStatus, undefined); assert.deepEqual(ordinary.canonical, original);
});

test('remote HTTP and nonlocal CDP browsers never interpret their returned filenames as local files', async t => {
  const f = await fixture(t); await writeFile(join(f.work, 'shot.png'), png);
  for (const server of [{ transport: 'streamable-http', url: 'https://browser.example.invalid/mcp' },
    { ...f.server, args: ['--cdp-endpoint', 'wss://browser.example.invalid/devtools'] },
    { ...f.server, args: ['--browserUrl=https://browser.example.invalid'] },
    { ...f.server, envRefs: { PLAYWRIGHT_MCP_CDP_ENDPOINT: 'SYNTHETIC_BROWSER_ENDPOINT' } }]) {
    const output = await archiveBrowserScreenshot(upstream('[Screenshot of viewport](shot.png)'), { ...f, server,
      descriptor: descriptor(), args: { filename: 'shot.png' } });
    assert.equal(output.screenshotCode, 'BROWSER_REMOTE_IMAGE_REQUIRED'); assert.equal(output.canonical.content.length, 1);
  }
});

test('remote standard image and embedded image resource are retained for preview without fetching URLs', async t => {
  const f = await fixture(t), server = { transport: 'streamable-http', url: 'https://browser.example.invalid/mcp' };
  const image = { type: 'image', mimeType: 'image/png', data: png.toString('base64') };
  const original = { ...upstream('Remote screenshot.'), content: [image] };
  const direct = await archiveBrowserScreenshot(original, { ...f, server, descriptor: descriptor() });
  assert.equal(direct.screenshotStatus, 'available'); assert.deepEqual(direct.canonical, original);
  const embedded = { type: 'resource', resource: { uri: 'https://browser.example.invalid/shot.png', mimeType: 'image/png', blob: image.data } };
  const source = { ...upstream('Embedded screenshot.'), content: [embedded] };
  const converted = await archiveBrowserScreenshot(source, { ...f, server, descriptor: descriptor() });
  assert.equal(converted.screenshotStatus, 'available'); assert.deepEqual(converted.canonical.content, [embedded, image]);
  const link = { ...upstream('Link-only screenshot.'), content: [{ type: 'resource_link', uri: embedded.resource.uri, mimeType: 'image/png', name: 'shot' }] };
  assert.deepEqual((await archiveBrowserScreenshot(link, { ...f, server, descriptor: descriptor() })).canonical, link);
});

test('traversal, cross-root absolute paths, links, hard links and malformed images cannot become screenshots', async t => {
  const f = await fixture(t); await writeFile(join(f.other, 'outside.png'), png); await writeFile(join(f.work, 'invalid.png'), 'not an image');
  await writeFile(join(f.work, 'large.png'), Buffer.alloc(4 * 1024 * 1024 + 1));
  await link(join(f.other, 'outside.png'), join(f.work, 'hardlink.png'));
  const candidates = ['../other/outside.png', join(f.other, 'outside.png'), 'invalid.png', 'large.png', 'hardlink.png', 'https://example.invalid/shot.png'];
  await symlink(f.other, join(f.work, 'linked'), process.platform === 'win32' ? 'junction' : 'dir'); candidates.push('linked/outside.png');
  for (const filename of candidates) {
    const result = await archiveBrowserScreenshot(upstream('[Screenshot of viewport](' + filename + ')'), { ...f, descriptor: descriptor(), args: { filename } });
    assert.equal(result.screenshotStatus, 'unavailable', filename); assert.equal(result.canonical.content.length, 1);
  }
});

test('PNG/JPEG dimensions and canonical base64 are bounded before publishing screenshot data', () => {
  assert.deepEqual(inspectScreenshotImage(png), { mimeType: 'image/png', width: 1, height: 1 });
  const huge = Buffer.from(png); huge.writeUInt32BE(16000001, 16); assert.equal(inspectScreenshotImage(huge), null);
  assert.equal(inspectScreenshotImage(png, 'image/jpeg'), null); assert.equal(inspectScreenshotImage(png.subarray(0, png.length - 1)), null);
  const jpeg = Buffer.from([255, 216, 255, 192, 0, 11, 8, 0, 2, 0, 3, 1, 1, 17, 0, 255, 217]);
  assert.deepEqual(inspectScreenshotImage(jpeg, 'image/jpeg'), { mimeType: 'image/jpeg', width: 3, height: 2 });
  assert.equal(inspectScreenshotImage(Buffer.from([255, 216, 255, 217])), null);
});

test('MCP execution archives the browser artifact and keeps a text-only honest projection with exactly one RPC', async t => {
  const f = await fixture(t); await writeFile(join(f.work, 'shot.png'), png);
  const clients = new McpToolClients({ fetch: () => assert.fail('No image or URL fetch is allowed.') });
  let calls = 0;
  const tool = { ...descriptor(), key: 'synthetic', originalInputSchema: { type: 'object' } };
  clients.connections.set(tool.key, Promise.resolve({ closed: false, tools: [tool], artifactContext: { server: f.server, context: f.context },
    client: { callTool: async () => { calls++; return upstream('[Screenshot of viewport](./shot.png)'); } } }));
  const receipt = await clients.execute(tool, { arguments: { filename: 'shot.png' }, policy: { reason: 'Synthetic browser screenshot.' } });
  assert.equal(calls, 1); assert.equal(receipt.screenshotStatus, 'available');
  assert.equal(receipt.canonical.content.at(-1).type, 'image'); assert.equal(receipt.content.includes(png.toString('base64')), false);
  assert.match(receipt.content, /No image pixels were sent/); assert.equal(receipt.isError, false);
});

test('cancelled preview hydration preserves a completed upstream receipt instead of discarding it', async t => {
  const f = await fixture(t), controller = new AbortController(); controller.abort();
  const original = upstream('[Screenshot of viewport](shot.png)');
  const result = await archiveBrowserScreenshot(original, { ...f, descriptor: descriptor(), args: { filename: 'shot.png' }, signal: controller.signal });
  assert.deepEqual(result.canonical, original); assert.equal(result.screenshotCode, 'BROWSER_SCREENSHOT_PREVIEW_CANCELLED');
});

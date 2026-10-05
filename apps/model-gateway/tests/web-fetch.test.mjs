import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import vm from 'node:vm';
import test from 'node:test';
import { WebFetchTool } from '../tools/web-fetch.mjs';
import { toolFixture, pendingApproval, approve, parsed } from './tool-fixture.mjs';
import { searchTools } from '../tools/tool-discovery.mjs';
import { builtinDescriptors } from '../official-tools/Tools/catalog.mjs';
import { canRunInParallel } from '../tools/tool-scheduling.mjs';

const sourceUrl = 'https://www.example.com/original';
const finalUrl = 'https://www.example.com/docs/current/page';
const response = (content, contentType = 'text/html; charset=utf-8') => ({ url: finalUrl, status: 200,
  headers: { contentType }, bytes: Buffer.isBuffer(content) ? content : Buffer.from(content), redirects: [sourceUrl] });

test('public HTML extraction keeps title, Unicode, tables and absolute source links without scripts or form values', async () => {
  const reader = new WebFetchTool({ fetchPage: async () => response('<html><head><title>中文 &amp; English</title><style>fixture style</style></head>' +
    '<body><h1>证据😀</h1><p>English &lt;source&gt;</p><a href="../source?q=1">Source</a>' +
    '<a href="javascript:alert(1)">Unsafe link</a><table><tr><th>项目</th><th>值</th></tr><tr><td>已核查</td><td>12</td></tr></table>' +
    '<script>fixture secret script</script><input value="fixture form secret"><textarea>fixture textarea secret</textarea>' +
    '<div hidden>fixture hidden secret</div></body></html>') });
  const result = await reader.run({ url: sourceUrl, reason: 'Read synthetic public evidence.' });
  const value = JSON.parse(result.content);
  assert.equal(value.title, '中文 & English'); assert.equal(value.url, finalUrl); assert.equal(value.javascriptExecuted, false);
  assert.match(value.content, /证据😀/); assert.match(value.content, /English <source>/); assert.match(value.content, /已核查/);
  assert.match(value.content, /https:\/\/www.example.com\/docs\/source\?q=1/);
  assert.doesNotMatch(value.content, /fixture (?:secret|style|form|textarea|hidden)|javascript:/);
  assert.equal(result.canonical.content[0].uri, finalUrl);
  assert.equal(result.canonical.structuredContent.content, value.content);
});

test('short live pages preserve complete bounded extracted text in the archive and Unicode paging makes progress', async () => {
  const full = '中文😀English\n'.repeat(8000);
  const reader = new WebFetchTool({ fetchPage: async () => response(full, 'text/plain') });
  let offset = 0, restored = '';
  do {
    const value = JSON.parse((await reader.run({ url: sourceUrl, offset, limit: 15999 })).content);
    assert.equal(value.offset, offset); assert.ok(value.content.isWellFormed());
    restored += value.content; offset = value.nextOffset;
    if (!value.hasMore) break;
  } while (true);
  assert.equal(restored, full);
  const emoji = new WebFetchTool({ fetchPage: async () => response('😀', 'text/plain') });
  const one = JSON.parse((await emoji.run({ url: sourceUrl, limit: 1 })).content);
  assert.equal(one.content, '😀'); assert.equal(one.hasMore, false); assert.equal(one.nextOffset, 2);
  await assert.rejects(emoji.run({ url: sourceUrl, offset: 1 }), { code: 'WEB_INVALID_OFFSET' });
  await assert.rejects(emoji.run({ url: sourceUrl, offset: 3 }), { code: 'WEB_INVALID_OFFSET' });
  const long = new WebFetchTool({ fetchPage: async () => response('x'.repeat(262150), 'text/plain') });
  const limited = await long.run({ url: sourceUrl });
  assert.equal(limited.canonical.structuredContent.content.length, 262144);
  assert.equal(JSON.parse(limited.content).extractionTruncated, true);
});

test('declared encodings decode correctly and unsupported, malformed or binary text is not reported as success', async () => {
  const chinese = new WebFetchTool({ fetchPage: async () => response(Buffer.from([0xd6, 0xd0, 0xce, 0xc4]), 'text/plain; charset=gbk') });
  assert.equal(JSON.parse((await chinese.run({ url: sourceUrl })).content).content, '中文');
  for (const [bytes, type, code] of [[Buffer.from('abc'), 'text/plain; charset=fixture-encoding', 'WEB_UNSUPPORTED_ENCODING'],
    [Buffer.from([0xff]), 'text/plain; charset=utf-8', 'WEB_INVALID_ENCODING'],
    [Buffer.from([0]), 'text/plain', 'WEB_UNSUPPORTED_CONTENT']]) {
    const reader = new WebFetchTool({ fetchPage: async () => response(bytes, type) });
    await assert.rejects(reader.run({ url: sourceUrl }), { code });
  }
});

test('broker exposes public reading with no MCP connection, enforces reason/approval and preserves exact approved arguments', async t => {
  const calls = [];
  const reader = new WebFetchTool({ fetchPage: async (url, options) => { calls.push({ url, options }); return response('<p>Synthetic evidence</p>'); } });
  const f = await toolFixture(t, { officialTools: true, webFetcher: reader });
  const context = await f.context('ask');
  const catalog = await f.service.catalog(context);
  assert.ok(catalog.some(item => item.name === 'web.fetch'));
  assert.equal(f.service.mcp.connections.size, 0);
  for (const args of [{ url: sourceUrl }, { url: sourceUrl, reason: ' ' }, { url: 'file:///fixture', reason: 'Fixture' }]) {
    const denied = await f.run(context, 'web.fetch', args, { interactive: false });
    assert.equal(denied.isError, true);
  }
  const call = f.call('web.fetch', { url: sourceUrl, reason: 'Synthetic public source verification.' });
  const waiting = await pendingApproval(f.service, context, call);
  assert.equal(waiting.event.tool.outsideWorkspace, true); assert.equal(calls.length, 0);
  waiting.event.tool.arguments.url = 'https://www.example.com/mutated';
  call.arguments.url = 'https://www.example.com/also-mutated';
  approve(f.service, context, waiting.event.tool);
  const completed = await waiting.result;
  assert.equal(calls.length, 1); assert.equal(calls[0].url, sourceUrl);
  assert.equal(completed.status, 'completed'); assert.equal(completed.outsideWorkspace, true);
  assert.ok(completed.resultRef);
  assert.equal(parsed(completed).url, finalUrl);
  assert.equal((await f.service.results.get(context, completed.resultRef.id)).structuredContent.content, 'Synthetic evidence');
  assert.equal(f.service.mcp.connections.size, 0);
});

test('full mode calls the read directly; independent reads are parallel, discoverable in Chinese, and result paging stays complete', async t => {
  const full = 'Complete public fixture source.\n'.repeat(3000);
  const f = await toolFixture(t, { webFetcher: new WebFetchTool({ fetchPage: async () => response(full, 'text/plain') }) });
  const context = await f.context('full'); await f.service.catalog(context);
  const result = await f.run(context, 'web.fetch', { url: sourceUrl, limit: 100, reason: 'Synthetic source.' }, { interactive: false });
  assert.equal(parsed(result).content.length, 100); assert.equal(parsed(result).hasMore, true);
  let offset = 0, archived = '';
  do {
    const page = parsed(await f.run(context, 'tool.result.read', { id: result.resultRef.id, offset }, { interactive: false }));
    archived += page.text; offset = page.nextOffset;
    if (!page.truncated) break;
  } while (true);
  assert.equal(JSON.parse(archived).structuredContent.content, full);
  assert.equal(canRunInParallel(f.call('web.fetch', { url: sourceUrl })), true);
  assert.equal(searchTools(builtinDescriptors, '读取网页')[0].name, 'web.fetch');
});

test('reader teardown and request cancellation stop pending work and never report a completed page', async () => {
  const fetchPage = (url, { signal }) => new Promise((resolve, reject) => {
    signal.addEventListener('abort', () => reject(signal.reason), { once: true });
  });
  const first = new WebFetchTool({ fetchPage }), cancelled = new AbortController();
  const pending = first.run({ url: sourceUrl }, cancelled.signal);
  cancelled.abort(); await assert.rejects(pending, { name: 'AbortError' });
  assert.equal(first.active.size, 0);
  const second = new WebFetchTool({ fetchPage }), closing = second.run({ url: sourceUrl });
  second.close(); await assert.rejects(closing, { name: 'AbortError' });
  assert.equal(second.active.size, 0);
  await assert.rejects(second.run({ url: sourceUrl }), { code: 'TOOL_SERVICE_CLOSED' });
});

test('builtin webpage activity shows a friendly label and direct source URL without exposing result JSON', async () => {
  const window = {}, sandbox = vm.createContext({ window, URL });
  for (const name of ['tool-web-links.js', 'tool-presentation.js']) {
    const code = await readFile(new URL(`../../desktop/Resources/Transcript/${name}`, import.meta.url), 'utf8');
    vm.runInContext(code, sandbox);
  }
  const tool = { name: 'web.fetch', arguments: { url: sourceUrl, reason: 'Read a source.' }, status: 'completed',
    result: JSON.stringify({ url: finalUrl, content: 'Synthetic evidence' }) };
  const presentation = window.KynxaToolPresentation.describe(tool);
  assert.equal(presentation.titleKey, 'toolReadWeb'); assert.equal(presentation.website, true); assert.equal(presentation.action, '');
  assert.ok(window.KynxaToolWebLinks.extract(tool).includes(sourceUrl));
});

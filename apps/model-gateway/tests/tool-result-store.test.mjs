import assert from 'node:assert/strict';
import { createHash, randomUUID } from 'node:crypto';
import { link, mkdir, mkdtemp, readFile, readdir, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { test } from 'node:test';
import { ConversationStore } from '../data/conversations.mjs';
import { MAX_TOOL_RESULT_BYTES, previewToolResult, publicToolResult, ToolResultStore } from '../data/tool-result-store.mjs';
import { normalizeToolExecutionEnvironment } from '../platform/tool-execution-environment.mjs';

const hostEnvironment = { schemaVersion: 1, executorKind: 'host-terminal', executorLocation: 'gateway-host',
  operationLocation: 'gateway-host', locationScope: 'builtin-execution-policy',
  gatewayHostMeaning: 'machine-running-the-gateway', userDeviceRelationship: 'unverified', grantsPermission: false,
  network: { requestOrigin: 'gateway-host', egress: 'unknown', proxy: 'unknown', sameEgressDoesNotProveSameMachine: true } };

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-tool-results-'));
  t.after(async () => {
    const suffix = relative(resolve(tmpdir()), resolve(root));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(root, { recursive: true, force: true });
  });
  const dataHome = join(root, 'Models');
  const conversations = new ConversationStore({ dataHome, legacyDesktopDirectory: null });
  const chat = id => ({ Id: id, Title: 'Synthetic results chat', Messages: [{ Id: randomUUID(), Role: 'user', Content: 'synthetic' }] });
  await conversations.saveCatalog({ ...(await conversations.catalog()), Projects: [{ Id: 'project-a', Name: 'Synthetic work', Chats: [chat('chat-work')] }],
    Chats: [chat('chat-a'), chat('chat-b')] });
  const results = new ToolResultStore({ conversationStore: conversations });
  const context = (conversationId = 'chat-a', projectId = null) => ({ conversationId, projectId, requestId: randomUUID() });
  const call = () => ({ id: randomUUID(), name: 'mcp.synthetic.inspect' });
  const path = (id, chatId = 'chat-a') => join(root, 'Chats', chatId, 'tool-results', `${id}.json`);
  return { root, dataHome, conversations, results, context, call, path };
}

test('complete typed results are atomically scoped and metadata/media have separate public projections', async t => {
  const f = await fixture(t), ctx = f.context(), call = f.call();
  const canonical = { content: [{ type: 'text', text: 'preview' }, { type: 'image', data: 'ZmFrZQ==', mimeType: 'image/png' },
    { type: 'audio', data: 'ZmFrZQ==', mimeType: 'audio/wav' }, { type: 'resource_link', uri: 'file:///synthetic/report.json', name: 'report' },
    { type: 'resource', resource: { uri: 'file:///synthetic/binary', mimeType: 'application/octet-stream', blob: 'ZmFrZQ==' } }],
    structuredContent: { answer: 42, business: { type: 'image', data: 'business field', blob: 'business blob' },
      nested: { _meta: { hidden: 'private nested' }, value: 'public' } }, _meta: { hidden: 'private SDK value' } };
  const ref = await f.results.save(ctx, call, canonical);
  assert.match(ref.id, /^[a-f0-9-]{36}$/); assert.match(ref.sha256, /^[a-f0-9]{64}$/);
  assert.equal(ref.bytes, Buffer.byteLength(JSON.stringify(canonical)));
  const disk = JSON.parse(await readFile(f.path(ref.id), 'utf8'));
  assert.deepEqual(disk.canonical, canonical); assert.equal(disk.requestId, ctx.requestId); assert.equal(disk.toolCallId, call.id);
  assert.deepEqual(await readdir(join(f.root, 'Chats', 'chat-a', 'tool-results')), [`${ref.id}.json`]);
  const projected = JSON.parse((await f.results.read(ctx, ref.id)).text);
  assert.equal(projected.content[0].text, 'preview'); assert.equal(projected.structuredContent.answer, 42);
  assert.deepEqual(projected.structuredContent.business, canonical.structuredContent.business);
  assert.equal(projected.content[3].uri, 'file:///synthetic/report.json');
  assert.equal(projected.content[1].data, undefined); assert.equal(projected.content[1].media.bytes, 4);
  assert.equal(projected.content[1].media.resultRef.id, ref.id); assert.equal(projected.content[1].media.resultRef.pointer, '/content/1/data');
  assert.equal(projected.content[4].resource.blob, undefined); assert.equal(projected.content[4].resource.media.resultRef.pointer, '/content/4/resource/blob');
  assert.equal(projected._meta, undefined); assert.equal(projected.structuredContent.nested._meta, undefined);
  const local = await f.results.get({ conversationId: 'chat-a' }, ref.id);
  assert.equal(local.content[1].data, 'ZmFrZQ=='); assert.equal(local.content[4].resource.blob, 'ZmFrZQ=='); assert.equal(local._meta, undefined);
  const reopened = new ToolResultStore({ conversationStore: new ConversationStore({ dataHome: f.dataHome, legacyDesktopDirectory: null }) });
  assert.deepEqual(await reopened.get({ conversationId: 'chat-a' }, ref.id), local);
});

test('large public results are queryable across requests in bounded JSON pages without losing content', async t => {
  const f = await fixture(t), original = { content: [], structuredContent: { stdout: ('quoted " slash \\ tab \t emoji 😀\n').repeat(4500), exitCode: 0 } };
  const ref = await f.results.save(f.context(), f.call(), original), query = f.context();
  let offset = 0, recovered = '', pages = 0;
  do {
    const page = await f.results.read(query, ref.id, { offset, limit: 16000 });
    assert.equal(page.offset, offset); assert.ok(page.text.length <= 16000); assert.ok(JSON.stringify(page).length <= 65536);
    assert.equal(page.resultRef.id, ref.id); recovered += page.text; offset = page.nextOffset; pages++;
    if (!page.truncated) { assert.equal(offset, page.totalCharacters); break; }
  } while (pages < 100);
  assert.ok(pages > 2); assert.deepEqual(JSON.parse(recovered), original);
  assert.equal((await f.results.read(query, ref.id, { offset })).text, '');
  await assert.rejects(f.results.read(query, ref.id, { offset: offset + 1 }), { code: 'INVALID_TOOL_RESULT_OFFSET' });
  await assert.rejects(f.results.read(query, ref.id, { projection: 'raw' }), { code: 'INVALID_TOOL_RESULT_PROJECTION' });
});

test('previews preserve small JSON and escaped long outputs remain valid bounded envelopes with references', () => {
  const small = { stdout: 'small', exitCode: 0 };
  assert.equal(previewToolResult(small), JSON.stringify(small));
  for (const large of [{ stdout: '\t"\\'.repeat(64000) }, { stdout: '😀'.repeat(50000) }, { stdout: '\u0000'.repeat(20000) }]) {
    const ref = { id: randomUUID(), bytes: 100000, sha256: 'a'.repeat(64) };
    const text = previewToolResult(large, { resultRef: ref, status: 'completed' });
    assert.ok(text.length <= 65536);
    const parsed = JSON.parse(text);
    assert.equal(parsed.status, 'completed'); assert.equal(parsed.truncated, true); assert.deepEqual(parsed.resultRef, ref);
    assert.equal(parsed.totalCharacters, JSON.stringify(large).length); assert.ok(parsed.preview.length);
    assert.ok(JSON.stringify(large).startsWith(parsed.preview)); assert.doesNotMatch(parsed.preview, /[\uD800-\uDBFF]$/);
  }
  assert.throws(() => previewToolResult({ data: 'a'.repeat(1000) }, { maximumCharacters: 256, resultRef: { id: 'a'.repeat(400) } }),
    { code: 'INVALID_TOOL_RESULT_REFERENCE' });
});

test('result ownership cannot cross conversations even if an attacker copies a result file', async t => {
  const f = await fixture(t), ref = await f.results.save(f.context(), f.call(), { content: [], structuredContent: { value: 'chat-a only' } });
  await assert.rejects(f.results.read(f.context('chat-b'), ref.id), { code: 'TOOL_RESULT_NOT_FOUND' });
  const destination = f.path(ref.id, 'chat-b');
  await mkdir(join(f.root, 'Chats', 'chat-b', 'tool-results'));
  await writeFile(destination, await readFile(f.path(ref.id)));
  await assert.rejects(f.results.get({ conversationId: 'chat-b' }, ref.id), { code: 'CORRUPT_TOOL_RESULT' });
  for (const id of ['../file', 'C:\\private', 'not-a-uuid', `${ref.id}/x`])
    await assert.rejects(f.results.read(f.context(), id), { code: 'INVALID_TOOL_RESULT_REFERENCE' });
});

test('current catalog ownership controls moved results and completed receipts follow the same chat', async t => {
  const f = await fixture(t), oldContext = f.context(), first = await f.results.save(oldContext, f.call(), { content: [], structuredContent: { saved: 'before move' } });
  let catalog = await f.conversations.catalog();
  const chat = catalog.Chats.find(item => item.Id === 'chat-a');
  catalog.Chats = catalog.Chats.filter(item => item.Id !== 'chat-a'); catalog.Projects[0].Chats.push(chat);
  await f.conversations.saveCatalog(catalog);
  await assert.rejects(f.results.read(oldContext, first.id), { code: 'WORKSPACE_CHANGED' });
  const current = f.context('chat-a', 'project-a');
  assert.equal((await f.results.get(current, first.id)).structuredContent.saved, 'before move');
  const late = await f.results.save(oldContext, f.call(), { content: [], structuredContent: { saved: 'late completed result' } });
  const movedPath = join(f.root, 'Projects', 'project-a', 'Sessions', 'chat-a', 'tool-results', `${late.id}.json`);
  const document = JSON.parse(await readFile(movedPath, 'utf8'));
  assert.equal(document.savedProjectId, 'project-a'); assert.equal(document.originalProjectId, null);
  assert.equal((await f.results.get(current, late.id)).structuredContent.saved, 'late completed result');
  catalog = await f.conversations.catalog(); const moved = catalog.Projects[0].Chats.find(item => item.Id === 'chat-a');
  catalog.Projects[0].Chats = catalog.Projects[0].Chats.filter(item => item.Id !== 'chat-a'); catalog.Chats.push(moved);
  await f.conversations.saveCatalog(catalog);
  assert.equal((await f.results.get(f.context(), late.id)).structuredContent.saved, 'late completed result');
});

test('archive preserves completion receipts, model retrieval stops, and delete/trash/undo never resurrect through result APIs', async t => {
  const f = await fixture(t), ctx = f.context();
  let catalog = await f.conversations.catalog(); catalog.Chats.find(item => item.Id === 'chat-a').IsArchived = true;
  await f.conversations.saveCatalog(catalog);
  const ref = await f.results.save(ctx, f.call(), { content: [], structuredContent: { saved: 'completed while archived' } });
  await assert.rejects(f.results.read(ctx, ref.id), { code: 'TOOL_RESULT_ARCHIVED' });
  assert.equal((await f.results.get({ conversationId: 'chat-a' }, ref.id)).structuredContent.saved, 'completed while archived');
  catalog = await f.conversations.catalog(); const archived = catalog.Chats.find(item => item.Id === 'chat-a');
  catalog.Chats = catalog.Chats.filter(item => item.Id !== 'chat-a'); await f.conversations.saveCatalog(catalog);
  await assert.rejects(f.results.get(ctx, ref.id), { code: 'CONVERSATION_DELETED' });
  await assert.rejects(f.results.save(ctx, f.call(), { content: [] }), { code: 'CONVERSATION_DELETED' });
  const trash = JSON.parse(await readFile(join(f.root, 'Trash', 'chat-a', 'tool-results', `${ref.id}.json`), 'utf8'));
  assert.equal(trash.id, ref.id);
  catalog = await f.conversations.catalog(); archived.IsArchived = false; catalog.Chats.push(archived);
  await f.conversations.saveCatalog(catalog);
  assert.equal((await f.results.get(f.context(), ref.id)).structuredContent.saved, 'completed while archived');
});

test('explicit local history paging allows archived chats and projects while model paging, scope and deletion guards remain', async t => {
  const f = await fixture(t);
  for (const kind of ['chat', 'project']) {
    const conversationId = kind === 'chat' ? 'chat-a' : 'chat-work';
    const ctx = f.context(conversationId, kind === 'chat' ? null : 'project-a');
    const canonical = { content: [{ type: 'image', data: 'ZmFrZQ==', mimeType: 'image/png' }],
      structuredContent: { text: 'public history '.repeat(4000) }, _meta: { hidden: 'PRIVATE_METADATA' } };
    const ref = await f.results.save(ctx, f.call(), canonical);
    let catalog = await f.conversations.catalog();
    if (kind === 'chat') catalog.Chats.find(item => item.Id === conversationId).IsArchived = true;
    else catalog.Projects[0].IsArchived = true;
    await f.conversations.saveCatalog(catalog);
    for (const allowArchived of [undefined, false, 'true', 1])
      await assert.rejects(f.results.read(ctx, ref.id, { allowArchived }), { code: 'TOOL_RESULT_ARCHIVED' });
    const local = { conversationId };
    let recovered = '', offset = 0;
    while (true) {
      const page = await f.results.read(local, ref.id, { offset, limit: 16000, allowArchived: true });
      recovered += page.text; offset = page.nextOffset;
      if (!page.truncated) break;
    }
    const publicResult = JSON.parse(recovered);
    assert.equal(publicResult.structuredContent.text, canonical.structuredContent.text);
    assert.equal(publicResult._meta, undefined); assert.equal(publicResult.content[0].data, undefined);
    assert.equal(publicResult.content[0].media.resultRef.id, ref.id);
    assert.equal((await f.results.get(local, ref.id)).content[0].data, 'ZmFrZQ==');
    await assert.rejects(f.results.read({ conversationId, projectId: 'different-project' }, ref.id, { allowArchived: true }),
      { code: 'WORKSPACE_CHANGED' });
    await assert.rejects(f.results.read({ conversationId: 'chat-b' }, ref.id, { allowArchived: true }), { code: 'TOOL_RESULT_NOT_FOUND' });
    catalog = await f.conversations.catalog();
    if (kind === 'chat') catalog.Chats = catalog.Chats.filter(item => item.Id !== conversationId);
    else catalog.Projects[0].Chats = catalog.Projects[0].Chats.filter(item => item.Id !== conversationId);
    await f.conversations.saveCatalog(catalog);
    await assert.rejects(f.results.read(local, ref.id, { allowArchived: true }), { code: 'CONVERSATION_DELETED' });
  }
});

test('linked directories, linked files and modified results are rejected without reading another file', async t => {
  const f = await fixture(t), ctx = f.context(), ref = await f.results.save(ctx, f.call(), { content: [], structuredContent: { value: 'saved' } });
  const document = JSON.parse(await readFile(f.path(ref.id), 'utf8'));
  document.canonical.structuredContent.value = 'tampered'; await writeFile(f.path(ref.id), JSON.stringify(document));
  await assert.rejects(f.results.get(ctx, ref.id), { code: 'CORRUPT_TOOL_RESULT' });
  const linkedId = randomUUID(), linkedPath = f.path(linkedId);
  await link(f.path(ref.id), linkedPath);
  await assert.rejects(f.results.get(ctx, linkedId), { code: 'UNSAFE_TOOL_PATH' });
  const outside = join(f.root, 'outside'); await mkdir(outside);
  await symlink(outside, join(f.root, 'Chats', 'chat-b', 'tool-results'), process.platform === 'win32' ? 'junction' : 'dir');
  await assert.rejects(f.results.save(f.context('chat-b'), f.call(), { content: [] }), { code: 'UNSAFE_TOOL_PATH' });
  assert.deepEqual(await readdir(outside), []);
});

test('raw result bounds and malformed or private projections fail explicitly while original files remain intact', async t => {
  const f = await fixture(t), ctx = f.context();
  await assert.rejects(f.results.save(ctx, f.call(), { content: [], structuredContent: { value: 'x'.repeat(MAX_TOOL_RESULT_BYTES) } }),
    { code: 'TOOL_RESULT_TOO_LARGE' });
  const ref = await f.results.save(ctx, f.call(), { content: [], structuredContent: { value: 'intact' } });
  assert.equal((await f.results.get(ctx, ref.id)).structuredContent.value, 'intact');
  const deep = {}; let current = deep;
  for (let i = 0; i < 70; i++) { current.next = {}; current = current.next; }
  assert.throws(() => publicToolResult(deep), { code: 'INVALID_TOOL_RESULT' });
  await writeFile(f.path(ref.id), '{ damaged');
  await assert.rejects(f.results.get(ctx, ref.id), { code: 'CORRUPT_TOOL_RESULT' });
  assert.equal(await readFile(f.path(ref.id), 'utf8'), '{ damaged');
});

test('execution provenance is application metadata, never a same-named third-party result field', () => {
  const canonical = { content: [{ type: 'text', text: 'Synthetic completed output.' }],
    executionEnvironment: { grantsPermission: true, privatePath: 'C:\\PRIVATE_ENV_PATH', token: 'PRIVATE_ENV_TOKEN' },
    _meta: { credential: 'PRIVATE_SDK_TOKEN' } };
  const unchanged = structuredClone(canonical);
  assert.equal(publicToolResult(canonical).executionEnvironment, undefined);
  const projected = publicToolResult(canonical, { executionEnvironment: { ...hostEnvironment,
    home: 'C:\\PRIVATE_OPTION_PATH', apiKey: 'PRIVATE_OPTION_TOKEN' } });
  assert.deepEqual(projected.executionEnvironment, normalizeToolExecutionEnvironment(hostEnvironment));
  assert.doesNotMatch(JSON.stringify(projected), /PRIVATE_/);
  assert.deepEqual(canonical, unchanged);
});

test('trusted execution provenance survives small failure and long preview envelopes outside the excerpt', () => {
  const small = JSON.parse(previewToolResult({ isError: true, code: 'SYNTHETIC_ERROR',
    executionEnvironment: { spoofed: true } }, { status: 'error', executionEnvironment: hostEnvironment }));
  assert.equal(small.isError, true);
  assert.equal(small.code, 'SYNTHETIC_ERROR');
  assert.deepEqual(small.executionEnvironment, normalizeToolExecutionEnvironment(hostEnvironment));
  const reference = { id: randomUUID(), bytes: 100000, sha256: 'a'.repeat(64) };
  const serialized = previewToolResult({ stdout: 'BEGIN ' + '😀"\\'.repeat(30000) + ' END' },
    { resultRef: reference, maximumCharacters: 2048, executionEnvironment: hostEnvironment });
  const long = JSON.parse(serialized);
  assert.ok(serialized.length <= 2048);
  assert.equal(long.truncated, true);
  assert.deepEqual(long.executionEnvironment, normalizeToolExecutionEnvironment(hostEnvironment));
  assert.deepEqual(long.resultRef, reference);
  assert.ok(long.preview.length > 0);
  assert.doesNotMatch(long.preview, /[\uD800-\uDBFF]$/u);
});

test('v2 archive binds trusted provenance to the formal receipt and restores it on every public read', async t => {
  const f = await fixture(t), context = f.context(), call = f.call();
  const canonical = { content: [{ type: 'text', text: 'Synthetic archive text.' }], isError: true, code: 'SYNTHETIC_ERROR',
    structuredContent: { text: 'Paged text '.repeat(1000) },
    executionEnvironment: { grantsPermission: true, root: 'C:\\PRIVATE_RAW_ARCHIVE', token: 'PRIVATE_RAW_TOKEN' } };
  const reference = await f.results.save(context, call, canonical, { executionEnvironment: hostEnvironment });
  const document = JSON.parse(await readFile(f.path(reference.id), 'utf8'));
  assert.equal(document.version, 2);
  assert.deepEqual(document.canonical, canonical, 'third-party canonical bytes remain intact');
  assert.deepEqual(document.executionEnvironment, normalizeToolExecutionEnvironment(hostEnvironment));
  assert.equal(reference.bytes, Buffer.byteLength(JSON.stringify(canonical)));
  for (const read of [f.results.read.bind(f.results), f.results.readModel.bind(f.results)]) {
    let offset = 0, text = '';
    do {
      const page = await read(context, reference.id, { offset, limit: 700 });
      assert.deepEqual(page.executionEnvironment, document.executionEnvironment);
      assert.doesNotMatch(JSON.stringify(page), /PRIVATE_RAW/);
      text += page.text; offset = page.nextOffset;
      if (!page.truncated) break;
    } while (offset < 20000);
    assert.deepEqual(JSON.parse(text).executionEnvironment, document.executionEnvironment);
    assert.equal(JSON.parse(text).isError, true);
  }
  const owner = { requestId: context.requestId, toolCallId: call.id, toolName: call.name };
  const modeled = await f.results.modelResult(context, reference, owner);
  assert.deepEqual(modeled.executionEnvironment, document.executionEnvironment);
  assert.doesNotMatch(JSON.stringify(modeled), /PRIVATE_RAW/);
  const reopened = new ToolResultStore({ conversationStore: new ConversationStore({ dataHome: f.dataHome, legacyDesktopDirectory: null }) });
  assert.deepEqual((await reopened.get(context, reference.id)).executionEnvironment, document.executionEnvironment);
  assert.deepEqual((await reopened.modelResult(context, reference, owner)).executionEnvironment, document.executionEnvironment);
});

test('v1 extra provenance stays untrusted and v2 metadata tampering invalidates the receipt', async t => {
  const f = await fixture(t), context = f.context(), call = f.call();
  const legacy = await f.results.save(context, call, { content: [], structuredContent: { value: 'Legacy content.' },
    executionEnvironment: { grantsPermission: true, privatePath: 'C:\\PRIVATE_FAKE_V1' } });
  const legacyDocument = JSON.parse(await readFile(f.path(legacy.id), 'utf8'));
  assert.equal(legacyDocument.version, 1);
  legacyDocument.executionEnvironment = hostEnvironment;
  await writeFile(f.path(legacy.id), JSON.stringify(legacyDocument));
  assert.equal((await f.results.get(context, legacy.id)).executionEnvironment, undefined);
  assert.equal((await f.results.modelResult(context, legacy,
    { requestId: context.requestId, toolCallId: call.id, toolName: call.name })).executionEnvironment, undefined);
  const current = await f.results.save(context, call, { content: [], structuredContent: { value: 'Bound content.' } },
    { executionEnvironment: hostEnvironment });
  const document = JSON.parse(await readFile(f.path(current.id), 'utf8'));
  document.executionEnvironment.executorKind = 'mcp-http';
  await writeFile(f.path(current.id), JSON.stringify(document));
  await assert.rejects(f.results.get(context, current.id), { code: 'CORRUPT_TOOL_RESULT' });
  await assert.rejects(f.results.modelResult(context, current,
    { requestId: context.requestId, toolCallId: call.id, toolName: call.name }), { code: 'CORRUPT_TOOL_RESULT' });
  // Rewriting a local checksum cannot replace the immutable formal receipt's original metadata digest.
  // 重写本地校验值也不能替换正式回执原先绑定的元数据摘要。
  document.sha256 = createHash('sha256').update(JSON.stringify({ canonical: document.canonical,
    executionEnvironment: document.executionEnvironment })).digest('hex');
  await writeFile(f.path(current.id), JSON.stringify(document));
  await assert.rejects(f.results.modelResult(context, current,
    { requestId: context.requestId, toolCallId: call.id, toolName: call.name }), { code: 'TOOL_RESULT_REFERENCE_MISMATCH' });
  document.version = 1; delete document.executionEnvironment;
  document.sha256 = createHash('sha256').update(JSON.stringify(document.canonical)).digest('hex');
  await writeFile(f.path(current.id), JSON.stringify(document));
  assert.equal((await f.results.get(context, current.id)).executionEnvironment, undefined);
  await assert.rejects(f.results.modelResult(context, current,
    { requestId: context.requestId, toolCallId: call.id, toolName: call.name }), { code: 'TOOL_RESULT_REFERENCE_MISMATCH' });
});

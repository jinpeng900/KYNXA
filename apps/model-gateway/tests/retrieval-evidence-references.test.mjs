import assert from 'node:assert/strict';
import { createHash, randomUUID } from 'node:crypto';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { test } from 'node:test';
import { ConversationStore } from '../data/conversations.mjs';
import { ToolResultStore } from '../data/tool-result-store.mjs';
import { EvidenceReferenceStore, allocateEvidenceArchiveId, evidenceSourceRef, parseEvidenceSourceRef,
  projectEvidenceSearchResult, projectRetrievalModelView } from '../data/retrieval/evidence-references.mjs';
import { hashText, sourceReference } from '../data/retrieval/retrieval-contracts.mjs';

function item(scopeKey = 'user', sourceId = 'synthetic-document') {
  const source = { sourceId, scopeKey, sourceRevision: 1, contentHash: hashText('current public original') };
  const chunk = { chunkId: 'synthetic-chunk', chunkHash: hashText('public original') };
  return { ...source, ...chunk, sourceRef: sourceReference(source, chunk), title: 'Synthetic public document', sourceType: 'document',
    excerpt: 'public original', locator: { relativePath: 'document.md', startOffset: 8, endOffset: 23, startLine: 2 },
    score: 0.016, lexicalRank: 1, lexicalScore: -0.75 };
}

const canonical = items => {
  const structuredContent = { items, indexSnapshotId: 'long-snapshot-id', scopeSnapshots: [{ scopeKey: 'user', snapshotId: 'long-id' }],
    evidenceAssessment: { strength: 'usable' }, acquisition: { action: 'read-source' }, rerankDiagnostic: { code: 'DISABLED' } };
  return { content: [{ type: 'text', text: JSON.stringify(structuredContent) }], structuredContent, _meta: { private: 'synthetic' } };
};

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-evidence-refs-'));
  t.after(async () => {
    const suffix = relative(resolve(tmpdir()), resolve(root));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(root, { recursive: true, force: true });
  });
  const dataHome = join(root, 'Models'), conversations = new ConversationStore({ dataHome, legacyDesktopDirectory: null });
  const chat = id => ({ Id: id, Title: 'Synthetic evidence chat', Messages: [{ Id: randomUUID(), Role: 'user', Content: 'synthetic' }] });
  await conversations.saveCatalog({ ...(await conversations.catalog()), Projects: [{ Id: 'work-a', Name: 'Synthetic work', Chats: [chat('work-chat')] }],
    Chats: [chat('chat-a'), chat('chat-b')] });
  const results = new ToolResultStore({ conversationStore: conversations });
  const evidence = new EvidenceReferenceStore({ conversationStore: conversations, resultStore: results });
  const context = { conversationId: 'chat-a', projectId: null, requestId: randomUUID() };
  const call = { id: `retrieval:${context.requestId}`, name: 'knowledge.search' };
  const owner = { requestId: context.requestId, toolCallId: call.id, toolName: call.name };
  const register = ref => conversations.upsertMessage('chat-a', { Id: context.requestId, Role: 'assistant', Content: '', RetrievalResultRef: ref });
  return { root, dataHome, conversations, results, evidence, context, call, owner, register };
}

test('compact evidence handles have fixed length, strict reversible IDs and bounded reference numbers', () => {
  const id = allocateEvidenceArchiveId();
  for (const referenceNumber of [1, 9, 35, 36, 60]) {
    const compact = evidenceSourceRef(id, referenceNumber);
    assert.equal(compact.length, 29); assert.deepEqual(parseEvidenceSourceRef(compact), { archiveId: id, referenceNumber });
  }
  for (const value of ['ev1:r1:01', `${evidenceSourceRef(id, 1)}=`, evidenceSourceRef(id, 1).slice(0, -2) + '00',
    evidenceSourceRef(id, 1).slice(0, -2) + '1p', 'ev1:AAAAAAAAAAAAAAAAAAAAAA:01'])
    assert.throws(() => parseEvidenceSourceRef(value), { code: 'INVALID_EVIDENCE_REFERENCE' });
  assert.throws(() => evidenceSourceRef(id, 61), { code: 'INVALID_EVIDENCE_REFERENCE' });
});

test('model projections preserve ordered evidence and diagnostics while canonical archives remain complete', () => {
  const id = allocateEvidenceArchiveId(), first = item(), second = item('user', 'other-document');
  first.modelSourceRef = evidenceSourceRef(id, 33);
  const full = canonical([first, second]), original = structuredClone(full), model = projectEvidenceSearchResult(full, id);
  assert.deepEqual(full, original);
  assert.deepEqual(model.structuredContent.items.map(value => [value.reference, value.sourceRef]), [[1, evidenceSourceRef(id, 1)], [2, evidenceSourceRef(id, 2)]]);
  for (const key of ['sourceId', 'contentHash', 'sourceRevision', 'chunkId', 'chunkHash', 'modelSourceRef'])
    assert.equal(model.structuredContent.items[0][key], undefined);
  assert.deepEqual(model.structuredContent.items[0].locator, first.locator);
  assert.equal(model.structuredContent.items[0].lexicalScore, first.lexicalScore);
  assert.deepEqual(model.structuredContent.evidenceAssessment, original.structuredContent.evidenceAssessment);
  assert.deepEqual(model.structuredContent.acquisition, original.structuredContent.acquisition);
  assert.deepEqual(model.structuredContent.rerankDiagnostic, original.structuredContent.rerankDiagnostic);
  assert.equal(model.structuredContent.scopeSnapshots, undefined);
  assert.doesNotMatch(model.content[0].text, /rag1:|contentHash|modelSourceRef/);
  assert.deepEqual(JSON.parse(model.content[0].text), model.structuredContent);
  assert.deepEqual(projectEvidenceSearchResult(full.structuredContent, id), model.structuredContent);
  assert.throws(() => projectEvidenceSearchResult({ items: [{ ...first, contentHash: 'a'.repeat(64) }] }, id), { code: 'INVALID_EVIDENCE_REFERENCE' });
  assert.throws(() => projectEvidenceSearchResult({ items: [{ ...first, sourceRef: 'rag1:fake' }] }, id), { code: 'INVALID_EVIDENCE_REFERENCE' });
});

test('formal receipts survive fresh store instances; model history is compact and public/disk views remain full', async t => {
  const f = await fixture(t), id = allocateEvidenceArchiveId(), full = canonical([item()]);
  const ref = await f.results.save(f.context, f.call, full, { id }), compact = evidenceSourceRef(id, 1);
  assert.equal(ref.id, id);
  await assert.rejects(f.evidence.resolve(f.context, compact), { code: 'EVIDENCE_REFERENCE_NOT_FOUND' });
  const trusted = await f.evidence.resolveTrusted(f.context, compact, { receipt: ref, owner: f.owner, scopeKeys: ['user'] });
  assert.equal(trusted.canonicalSourceRef, full.structuredContent.items[0].sourceRef); assert.equal(trusted.excerpt, undefined);
  await f.register(ref);
  const reopened = new ConversationStore({ dataHome: f.dataHome, legacyDesktopDirectory: null });
  const results = new ToolResultStore({ conversationStore: reopened });
  const evidence = new EvidenceReferenceStore({ conversationStore: reopened, resultStore: results });
  const fresh = await evidence.resolve({ ...f.context, requestId: randomUUID() }, compact, { scopeKeys: ['user'] });
  assert.equal(fresh.canonicalSourceRef, full.structuredContent.items[0].sourceRef); assert.equal(fresh.modelSourceRef, compact);
  const model = await results.modelResult(f.context, ref, f.owner);
  assert.equal(model.structuredContent.items[0].sourceRef, compact); assert.equal(model._meta, undefined);
  assert.equal((await results.get(f.context, id)).structuredContent.items[0].sourceRef, full.structuredContent.items[0].sourceRef);
  assert.equal(JSON.parse((await results.read(f.context, id)).text).structuredContent.items[0].sourceRef, full.structuredContent.items[0].sourceRef);
  const path = join(f.root, 'Chats', 'chat-a', 'tool-results', `${id}.json`), before = await readFile(path, 'utf8');
  assert.deepEqual(JSON.parse(before).canonical, full);
  await assert.rejects(results.save(f.context, f.call, canonical([]), { id }), { code: 'TOOL_RESULT_CONFLICT' });
  assert.equal(await readFile(path, 'utf8'), before);
});

test('nested retrieval paths and errors are model-only projections while useful evidence stays intact', async t => {
  const f = await fixture(t), evidence = item();
  evidence.title = 'Z:\\Synthetic user\\Notes\\document.md';
  evidence.locator.path = 'Z:\\Synthetic user\\Notes\\document.md';
  evidence.locator.root = 'Z:\\Synthetic user\\Notes';
  const full = canonical([evidence]);
  full.structuredContent.diagnostic = { details: [{ filename: '/synthetic/private/cache.sqlite',
    message: "Failed to read '/synthetic/private/cache.sqlite'; retry relative document.md",
    stack: 'Error\n at read (Z:\\Synthetic user\\Internal\\read.mjs:12:2)' }] };
  full.content[0].text = JSON.stringify(full.structuredContent);
  const ref = await f.results.save(f.context, f.call, full);
  const model = await f.results.modelResult(f.context, ref, f.owner);
  assert.equal(model.structuredContent.items[0].locator.path, undefined);
  assert.equal(model.structuredContent.items[0].locator.root, undefined);
  assert.equal(model.structuredContent.items[0].title, 'document.md');
  assert.equal(model.structuredContent.items[0].locator.relativePath, 'document.md');
  assert.equal(model.structuredContent.items[0].excerpt, evidence.excerpt);
  assert.equal(model.structuredContent.items[0].locator.startOffset, evidence.locator.startOffset);
  assert.equal(model.structuredContent.diagnostic.details[0].filename, undefined);
  assert.match(model.structuredContent.diagnostic.details[0].message, /\[local-path\]/);
  assert.doesNotMatch(JSON.stringify(model), /Synthetic user|synthetic\/private/);
  assert.deepEqual((await f.results.get(f.context, ref.id)).structuredContent, full.structuredContent);
  assert.equal(JSON.parse((await f.results.read(f.context, ref.id)).text).structuredContent.items[0].locator.path, evidence.locator.path);
  const view = projectRetrievalModelView({ content: [{ type: 'text', text: "ENOENT: open 'Z:\\Synthetic user\\missing.md'" }],
    structuredContent: { url: 'https://example.test/path', relativePath: 'docs/source.md', excerpt: 'Document mentions /usr/bin/node.' }, isError: true });
  assert.doesNotMatch(view.content[0].text, /Synthetic user/);
  assert.equal(view.structuredContent.url, 'https://example.test/path');
  assert.equal(view.structuredContent.relativePath, 'docs/source.md');
  assert.equal(view.structuredContent.excerpt, 'Document mentions /usr/bin/node.');
});

test('knowledge read history removes private locators without changing full source receipts', async t => {
  const f = await fixture(t), call = { id: 'read-receipt', name: 'knowledge.read' };
  const full = { content: [{ type: 'text', text: JSON.stringify({ text: 'Current original', offset: 2,
    nextOffset: 18, offsetUnit: 'utf16-code-units', locator: { path: '/synthetic/private/file.md', relativePath: 'file.md' } }) }], isError: false };
  const reference = await f.results.save(f.context, call, full);
  const model = await f.results.modelResult(f.context, reference, { ...f.owner, toolCallId: call.id, toolName: call.name });
  const parsed = JSON.parse(model.content[0].text);
  assert.deepEqual(parsed.locator, { relativePath: 'file.md' });
  assert.equal(parsed.offset, 2); assert.equal(parsed.offsetUnit, 'utf16-code-units');
  assert.equal(parsed.text, 'Current original');
  assert.deepEqual(await f.results.get(f.context, reference.id), full);
});

test('model archive paging projects a whole retrieval receipt before slicing with its own offsets', async t => {
  const f = await fixture(t), evidence = item();
  evidence.locator.path = 'Z:\\Synthetic user\\Private corpus\\document.md';
  evidence.locator.root = 'Z:\\Synthetic user\\Private corpus';
  evidence.excerpt = 'Public 😀 original evidence repeated. '.repeat(30);
  const full = canonical([evidence]), reference = await f.results.save(f.context, f.call, full);
  const expected = JSON.stringify(projectEvidenceSearchResult({ ...full, _meta: undefined }, reference.id));
  let offset = 0, text = '', characters;
  do {
    const page = await f.results.readModel(f.context, reference.id, { offset, limit: 37 });
    assert.equal(page.projection, 'model'); assert.equal(page.offsetUnit, 'utf16-code-units');
    assert.equal(page.resultRefBasis, 'canonical-archive'); assert.equal(page.resultRef.sha256, reference.sha256);
    assert.equal(page.offset, offset); assert.ok(page.nextOffset > offset);
    assert.equal(page.text, expected.slice(page.offset, page.nextOffset));
    assert.doesNotMatch(page.text, /^[\uDC00-\uDFFF]|[\uD800-\uDBFF]$/u);
    characters ??= page.totalCharacters;
    assert.equal(page.totalCharacters, characters);
    text += page.text; offset = page.nextOffset;
    if (!page.truncated) break;
  } while (offset < characters);
  assert.equal(text, expected); assert.doesNotMatch(text, /Synthetic user|Private corpus|rag1:/u);
  assert.equal(JSON.parse(text).structuredContent.items[0].excerpt, evidence.excerpt);
  const raw = JSON.parse((await f.results.read(f.context, reference.id)).text);
  assert.equal(raw.structuredContent.items[0].locator.path, evidence.locator.path);
  await assert.rejects(f.results.readModel({ ...f.context, conversationId: 'chat-b' }, reference.id), { code: 'TOOL_RESULT_NOT_FOUND' });
});

test('a failed search archive remains readable to the model without inventing evidence references', async t => {
  const f = await fixture(t);
  const full = { content: [{ type: 'text', text: "Read failed for 'Z:\\Synthetic user\\missing.md'" }], isError: true,
    code: 'STALE_RETRIEVAL_SOURCE', structuredContent: { error: { message: "Read failed for 'Z:\\Synthetic user\\missing.md'" } } };
  const reference = await f.results.save(f.context, f.call, full);
  const page = await f.results.readModel(f.context, reference.id);
  const projected = JSON.parse(page.text);
  assert.equal(projected.isError, true); assert.equal(projected.code, 'STALE_RETRIEVAL_SOURCE');
  assert.equal(projected.structuredContent.items, undefined);
  assert.doesNotMatch(page.text, /Synthetic user|ev1:/u);
  const history = await f.results.modelResult(f.context, reference, f.owner);
  assert.equal(history.code, 'STALE_RETRIEVAL_SOURCE');
  assert.deepEqual(await f.results.get(f.context, reference.id), full);
});

test('unquoted Windows retrieval error paths are projected in nested and historical receipts while originals stay complete', async t => {
  const f = await fixture(t);
  const privatePath = 'C:\\Synthetic Private Home\\Index\\source.sqlite';
  const privateShare = '\\\\synthetic-server\\Synthetic Private Share\\source.md';
  const full = { isError: true, code: 'RETRIEVAL_SOURCE_UNAVAILABLE',
    content: [{ type: 'text', text: `Index unavailable at ${privatePath}; inspect docs/source.md` },
      { type: 'text', text: JSON.stringify(`Cannot open ${privateShare}; metadata unavailable`) }],
    structuredContent: { error: { message: `Could not open ${privatePath}; retry only this source`,
      nested: [{ diagnostic: `Access denied ${privateShare}; do not replay mutations` }] },
      url: 'https://example.test/A:/public-release', relativePath: 'docs/source.md',
      excerpt: `The manual gives an example path ${privatePath}.` } };
  const unchanged = structuredClone(full);
  const reference = await f.results.save(f.context, f.call, full);
  const history = await f.results.modelResult(f.context, reference, f.owner);
  const immediate = projectEvidenceSearchResult(full, reference.id);
  const page = JSON.parse((await f.results.readModel(f.context, reference.id)).text);
  for (const projected of [history, immediate, page]) {
    assert.match(projected.content[0].text, /\[local-path\].*docs\/source\.md/u);
    assert.doesNotMatch(JSON.stringify(projected.content), /Synthetic Private Home|Synthetic Private Share/u);
    assert.doesNotMatch(JSON.stringify(projected.structuredContent.error), /Synthetic Private Home|Synthetic Private Share/u);
    assert.equal(projected.structuredContent.url, full.structuredContent.url);
    assert.equal(projected.structuredContent.relativePath, full.structuredContent.relativePath);
    assert.equal(projected.structuredContent.excerpt, full.structuredContent.excerpt, 'original evidence text is not indiscriminately redacted');
    assert.equal(projected.code, full.code);
  }
  assert.deepEqual(full, unchanged);
  assert.deepEqual(await f.results.get(f.context, reference.id), full);
  assert.deepEqual(JSON.parse((await f.results.read(f.context, reference.id)).text), full);
});

test('short references reject cross-chat, forged index, scope expansion and unbound internal ownership', async t => {
  const f = await fixture(t), ref = await f.results.save(f.context, f.call, canonical([item()]));
  await f.register(ref);
  await assert.rejects(f.evidence.resolve({ ...f.context, conversationId: 'chat-b' }, evidenceSourceRef(ref.id, 1)), { code: 'EVIDENCE_REFERENCE_NOT_FOUND' });
  await assert.rejects(f.evidence.resolve(f.context, evidenceSourceRef(ref.id, 2)), { code: 'EVIDENCE_REFERENCE_NOT_FOUND' });
  await assert.rejects(f.evidence.resolve(f.context, evidenceSourceRef(ref.id, 1), { scopeKeys: ['project:unrelated'] }), { code: 'RETRIEVAL_SCOPE_REQUIRED' });
  await assert.rejects(f.evidence.resolve(f.context, evidenceSourceRef(allocateEvidenceArchiveId(), 1)), { code: 'EVIDENCE_REFERENCE_NOT_FOUND' });
  assert.throws(() => f.evidence.resolveTrusted(f.context, evidenceSourceRef(ref.id, 1), { receipt: ref,
    owner: { ...f.owner, requestId: randomUUID() } }), { code: 'INVALID_EVIDENCE_REFERENCE' });
  const controller = new AbortController(); controller.abort();
  await assert.rejects(f.evidence.resolve(f.context, evidenceSourceRef(ref.id, 1), { signal: controller.signal }), { name: 'AbortError' });
});

test('formal receipts detect rewritten archives even when an attacker recalculates file hashes', async t => {
  const f = await fixture(t), ref = await f.results.save(f.context, f.call, canonical([item()]));
  await f.register(ref);
  const path = join(f.root, 'Chats', 'chat-a', 'tool-results', `${ref.id}.json`), before = await readFile(path, 'utf8');
  const document = JSON.parse(before); document.canonical = canonical([item('user', 'forged-source')]);
  const serialized = JSON.stringify(document.canonical);
  document.sha256 = createHash('sha256').update(serialized).digest('hex'); document.bytes = Buffer.byteLength(serialized);
  await writeFile(path, JSON.stringify(document));
  await assert.rejects(f.evidence.resolve(f.context, evidenceSourceRef(ref.id, 1)), { code: 'TOOL_RESULT_REFERENCE_MISMATCH' });
  await writeFile(path, before);
  await f.register({ ...ref, sha256: 'a'.repeat(64) });
  await assert.rejects(f.evidence.resolve(f.context, evidenceSourceRef(ref.id, 1)), { code: 'TOOL_RESULT_REFERENCE_MISMATCH' });
});

test('tool activity receipts authorize their exact call; moving a chat cannot keep its old project scope', async t => {
  const f = await fixture(t), call = { id: 'synthetic-search-call', name: 'knowledge.search' };
  const ref = await f.results.save(f.context, call, canonical([item('project:work-a')]));
  await f.conversations.upsertMessage('chat-a', { Id: f.context.requestId, Role: 'assistant', Content: '',
    ToolActivities: [{ toolCallId: call.id, name: call.name, status: 'completed', resultRef: ref }] });
  await assert.rejects(f.evidence.resolve(f.context, evidenceSourceRef(ref.id, 1)), { code: 'RETRIEVAL_SOURCE_NOT_FOUND' });
  let catalog = await f.conversations.catalog(), chat = catalog.Chats.find(value => value.Id === 'chat-a');
  catalog.Chats = catalog.Chats.filter(value => value.Id !== 'chat-a'); catalog.Projects[0].Chats.push(chat);
  await f.conversations.saveCatalog(catalog);
  await assert.rejects(f.evidence.resolve(f.context, evidenceSourceRef(ref.id, 1)), { code: 'WORKSPACE_CHANGED' });
  const moved = { ...f.context, projectId: 'work-a' };
  assert.equal((await f.evidence.resolve(moved, evidenceSourceRef(ref.id, 1))).scopeKey, 'project:work-a');
  catalog = await f.conversations.catalog(); chat = catalog.Projects[0].Chats.find(value => value.Id === 'chat-a');
  catalog.Projects[0].Chats = catalog.Projects[0].Chats.filter(value => value.Id !== 'chat-a'); catalog.Chats.push(chat);
  await f.conversations.saveCatalog(catalog);
  await assert.rejects(f.evidence.resolve(f.context, evidenceSourceRef(ref.id, 1)), { code: 'RETRIEVAL_SOURCE_NOT_FOUND' });
  catalog = await f.conversations.catalog(); catalog.Chats.find(value => value.Id === 'chat-a').IsArchived = true;
  await f.conversations.saveCatalog(catalog);
  await assert.rejects(f.evidence.resolve(f.context, evidenceSourceRef(ref.id, 1)), { code: 'TOOL_RESULT_ARCHIVED' });
});

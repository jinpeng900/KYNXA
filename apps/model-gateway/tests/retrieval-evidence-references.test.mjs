import assert from 'node:assert/strict';
import { createHash, randomUUID } from 'node:crypto';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { test } from 'node:test';
import { ConversationStore } from '../data/conversations.mjs';
import { ToolResultStore } from '../data/tool-result-store.mjs';
import { EvidenceReferenceStore, allocateEvidenceArchiveId, evidenceSourceRef, parseEvidenceSourceRef,
  projectEvidenceSearchResult } from '../data/retrieval/evidence-references.mjs';
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

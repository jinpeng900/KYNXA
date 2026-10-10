import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { test } from 'node:test';
import { ConversationStore } from '../data/conversations.mjs';
import { MemoryService } from '../data/memory-service.mjs';
import { memoryEntryIdentity } from '../data/memory-contracts.mjs';

const user = (Id, Content) => ({ Id, Role: 'user', Content, Status: 'completed' });

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-memory-proposals-'));
  t.after(async () => {
    const suffix = relative(resolve(tmpdir()), resolve(root));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(root, { recursive: true, force: true });
  });
  const conversations = new ConversationStore({ dataHome: join(root, 'Models'), legacyDesktopDirectory: null });
  const catalog = await conversations.catalog();
  const chat = Id => ({ Id, Title: Id, Messages: [user(`initial-${Id}`, 'Synthetic fixture user message')] });
  await conversations.saveCatalog({ ...catalog, Projects: [
    { Id: 'work-a', Name: 'Work A', Chats: [chat('chat-a'), chat('chat-sibling')] },
    { Id: 'work-b', Name: 'Work B', Chats: [chat('chat-b')] },
    { Id: 'folderless', Name: 'Folderless', IsFolderlessWorkspace: true, Chats: [chat('chat-f')] }
  ], Chats: [chat('chat-plain')] });
  const memory = new MemoryService({ conversationStore: conversations });
  const quote = async (text, messageId = 'proposal-source', conversationId = 'chat-a') => {
    await conversations.upsertMessage(conversationId, user(messageId, text));
    return [{ messageId, text }];
  };
  const target = async (id, scope = 'chat', conversationId = 'chat-a') =>
    (await memory.readForModel(conversationId, { scopes: [scope] })).entries.find(entry => entry.id === id).target;
  const confirm = result => memory.update('chat-a', result.entry.id,
    { scope: result.scope, status: 'confirmed', expectedRevision: result.revision,
      ...(result.entry.proposal?.target ? { proposalAction: result.action } : {}) });
  return { root, conversations, memory, quote, target, confirm };
}

test('model semantic additions preserve canonical quotes as inactive drafts until user CAS confirmation', async t => {
  const f = await fixture(t), raw = '  我希望今后的代码说明先用中文，然后补充英文。  ';
  const quotes = await f.quote(raw);
  const eventsFile = join(f.root, 'Projects', 'work-a', 'Sessions', 'chat-a', 'events.jsonl');
  const transcript = await readFile(eventsFile);
  const result = await f.memory.propose('chat-a', { action: 'add', content: '代码说明采用中文和英文。', kind: 'preference',
    quotes, reason: '从用户对说明语言的要求提炼。', isInference: true });
  assert.equal(result.created, true); assert.equal(result.requiresConfirmation, true);
  assert.equal(result.sourceVerified, true); assert.equal(result.correctnessCertified, false);
  assert.equal(result.scope, 'chat'); assert.equal(result.entry.status, 'draft');
  assert.deepEqual(result.entry.proposal.quotes, quotes); assert.equal(result.entry.proposal.isInference, true);
  assert.deepEqual((await f.memory.contextFor('chat-a')).entries, []);
  await assert.rejects(f.memory.update('chat-a', result.entry.id, { scope: 'chat', status: 'confirmed' }), { code: 'INVALID_MEMORY' });
  const confirmed = await f.confirm(result);
  assert.equal(confirmed.entries[0].id, result.entry.id); assert.equal(confirmed.entries[0].status, 'confirmed');
  assert.equal((await f.memory.contextFor('chat-a')).entries[0].content, '代码说明采用中文和英文。');
  assert.deepEqual((await f.memory.contextFor('chat-sibling')).entries, []);
  assert.deepEqual(await readFile(eventsFile), transcript);
  const reopened = new MemoryService({ conversationStore: f.conversations });
  assert.deepEqual((await reopened.contextFor('chat-a')).entries[0].proposal, confirmed.entries[0].proposal);
});

test('semantic suggestions cannot fabricate quotes, use assistant authority, confirm themselves or cross work boundaries', async t => {
  const f = await fixture(t), quotes = await f.quote('用户正式原话');
  const input = { action: 'add', content: '提炼后的内容', quotes, reason: '保留核验边界', isInference: true };
  await assert.rejects(f.memory.propose('chat-a', { ...input, status: 'confirmed' }), { code: 'INVALID_MEMORY' });
  await assert.rejects(f.memory.propose('chat-a', { ...input, quotes: [{ messageId: quotes[0].messageId, text: '伪造原话' }] }),
    { code: 'MEMORY_SOURCE_UNAVAILABLE' });
  await f.conversations.upsertMessage('chat-a', { Id: 'assistant-only', Role: 'assistant', Content: '助手声称已核验', Status: 'completed' });
  await assert.rejects(f.memory.propose('chat-a', { ...input, quotes: [{ messageId: 'assistant-only', text: '助手声称已核验' }] }),
    { code: 'MEMORY_SOURCE_UNAVAILABLE' });
  await assert.rejects(f.memory.propose('chat-a', { ...input, scope: 'project', scopeId: 'work-b' }), { code: 'MEMORY_SCOPE_CHANGED' });
  const folderlessQuotes = await f.quote('无文件夹工作原话', 'folderless-source', 'chat-f');
  await assert.rejects(f.memory.propose('chat-f', { ...input, scope: 'project', quotes: folderlessQuotes }), { code: 'MEMORY_SCOPE_CHANGED' });
  assert.ok((await f.memory.repository.readFor('chat-a')).scopes.every(document => !document.entries.length));
});

test('memory read returns selectively visible target bindings without sharing chat or other-work memory', async t => {
  const f = await fixture(t);
  for (const scope of ['chat', 'project', 'user']) await f.memory.create('chat-a', { scope, content: `${scope} 约定` });
  await f.memory.create('chat-b', { scope: 'project', content: '其他工作私有约定' });
  const read = await f.memory.readForModel('chat-a', { scopes: ['project'], query: '约定', limit: 1 });
  assert.deepEqual(read.scopes.map(document => document.scope), ['project']); assert.equal(read.entries.length, 1);
  assert.equal(read.entries[0].target.scopeId, 'work-a'); assert.equal(read.entries[0].target.expectedRevision, read.scopes[0].revision);
  assert.equal(read.entries[0].target.identity, memoryEntryIdentity(read.entries[0]));
  assert.deepEqual((await f.memory.readForModel('chat-sibling')).entries.map(entry => entry.scope).sort(), ['project', 'user']);
  assert.deepEqual((await f.memory.readForModel('chat-f')).entries.map(entry => entry.scope), ['user']);
  await assert.rejects(f.memory.readForModel('chat-f', { scopes: ['project'] }), { code: 'MEMORY_SCOPE_CHANGED' });
  await assert.rejects(f.memory.readForModel('chat-a', { limit: 51 }), { code: 'INVALID_MEMORY' });
  await assert.rejects(f.memory.readForModel('chat-a', { scopes: ['chat', 'chat'] }), { code: 'INVALID_MEMORY' });
  await writeFile(join(f.root, 'Memory', 'entries.json'), '{broken-global-memory');
  assert.equal((await f.memory.readForModel('chat-a', { scopes: ['project'] })).entries[0].content, 'project 约定',
    'selective reads do not touch an unrequested scope or fail because its file is corrupt');
  await assert.rejects(f.memory.readForModel('chat-a'), { code: 'CORRUPT_MEMORY' });
});

test('memory read pages beyond fifty across scopes with stable ordering, unique records and revision-bound targets', async t => {
  const f = await fixture(t), expectedIds = [];
  for (const scope of ['chat', 'project', 'user']) {
    await f.memory.repository.mutate('chat-a', scope, 0, document => {
      for (let index = 0; index < 30; index++) {
        const id = `paged-${scope}-${index.toString().padStart(3, '0')}`;
        expectedIds.push(id);
        document.entries.push({ id, scope, scopeId: document.scopeId, content: `shared fixture ${scope} ${index}`,
          kind: 'fact', status: 'confirmed', source: { type: 'manual', role: 'user' }, revision: 1,
          createdAt: '2026-01-01T00:00:00.000Z', updatedAt: '2026-01-01T00:00:00.000Z' });
      }
    });
  }
  await f.memory.create('chat-b', { scope: 'project', content: 'shared other-work secret' });
  await f.memory.create('chat-sibling', { scope: 'chat', content: 'shared sibling-chat secret' });
  const identities = new Map((await f.memory.repository.readFor('chat-a')).scopes.flatMap(document =>
    document.entries.map(entry => [entry.id, memoryEntryIdentity(entry)])));
  const readAllPages = async scopes => {
    const entries = [];
    let offset = 0, page;
    do {
      page = await f.memory.readForModel('chat-a', { scopes, query: 'shared', limit: 50, offset });
      assert.equal(page.offset, offset); assert.equal(page.matched, 90);
      assert.equal(page.hasMore, page.nextOffset !== null);
      for (const entry of page.entries) {
        assert.equal(entry.target.expectedRevision, page.scopes.find(document => document.scope === entry.scope).revision);
        assert.equal(entry.target.identity, identities.get(entry.id));
      }
      entries.push(...page.entries); offset = page.nextOffset;
    } while (page.hasMore);
    return entries;
  };
  const first = await f.memory.readForModel('chat-a', { query: 'shared', limit: 50 });
  assert.equal(first.entries.length, 50); assert.equal(first.offset, 0); assert.equal(first.nextOffset, 50); assert.equal(first.hasMore, true);
  assert.ok(new Set(first.entries.map(entry => entry.scope)).size > 1);
  const final = await f.memory.readForModel('chat-a', { query: 'shared', limit: 50, offset: 50 });
  assert.equal(final.entries.length, 40); assert.equal(final.nextOffset, null); assert.equal(final.hasMore, false);
  const records = await readAllPages(['chat', 'project', 'user']);
  assert.deepEqual(records.map(entry => entry.id), expectedIds.sort());
  assert.equal(new Set(records.map(entry => `${entry.scope}:${entry.scopeId}:${entry.id}`)).size, 90);
  assert.deepEqual((await readAllPages(['user', 'project', 'chat'])).map(entry => entry.id), records.map(entry => entry.id));
  assert.equal((await f.memory.readForModel('chat-a', { query: '', limit: 50 })).matched, 90);
  assert.equal((await f.memory.readForModel('chat-a', { query: '   ', limit: 50 })).matched, 90);
  assert.equal((await f.memory.readForModel('chat-a', { scopes: ['user'], limit: 50 })).matched, 30);
  assert.equal((await f.memory.readForModel('chat-sibling', { limit: 50 })).matched, 61);
  assert.equal((await f.memory.readForModel('chat-f', { limit: 50 })).matched, 30);
  const empty = await f.memory.readForModel('chat-a', { offset: 3000 });
  assert.deepEqual(empty.entries, []); assert.equal(empty.hasMore, false); assert.equal(empty.nextOffset, null);
  for (const offset of [-1, 3001, 0.5, '50', null])
    await assert.rejects(f.memory.readForModel('chat-a', { offset }), { code: 'INVALID_MEMORY' });
  await assert.rejects(f.memory.readForModel('chat-a', { query: null }), { code: 'INVALID_MEMORY' });
});

test('shared model views hide other-chat drafts and expose only confirmed content without private original quotations', async t => {
  for (const { scope, mode } of [
    { scope: 'project', mode: 'proposal' }, { scope: 'user', mode: 'proposal' }, { scope: 'project', mode: 'candidate' }
  ]) {
    const f = await fixture(t), marker = `PRIVATE-ORIGINAL-${scope}-${mode}`;
    const quotes = await f.quote(`工作决定：用户私有来源 ${marker}，共享约定只保留精炼结论。`);
    let draft;
    if (mode === 'proposal') {
      draft = await f.memory.propose('chat-a', { action: 'add', scope, content: '可共享的精炼结论。', quotes,
        reason: `推理过程也包含 ${marker}`, isInference: true, conflicts: [`原始细节 ${marker}`] });
    } else {
      await f.memory.scheduleCandidates('chat-a', { phase: 'user', userMessageId: quotes[0].messageId });
      const document = await f.memory.repository.read('chat-a', scope);
      draft = { scope, entry: document.entries[0], revision: document.revision };
    }
    assert.equal((await f.memory.readForModel('chat-a', { scopes: [scope] })).entries[0].id, draft.entry.id);
    assert.deepEqual((await f.memory.readForModel('chat-sibling', { scopes: [scope] })).entries, []);
    assert.ok(JSON.stringify(await f.memory.listScope(scope, scope === 'project' ? 'work-a' : 'user')).includes(marker),
      'management APIs retain complete provenance for user review');
    const confirmed = await f.memory.update('chat-a', draft.entry.id, { scope, content: '可共享的精炼结论。',
      status: 'confirmed', expectedRevision: draft.revision });
    const shared = await f.memory.readForModel('chat-sibling', { scopes: [scope] });
    assert.equal(shared.entries.length, 1); assert.equal(shared.entries[0].content, '可共享的精炼结论。');
    assert.equal(shared.entries[0].status, 'confirmed'); assert.equal(shared.entries[0].source, undefined);
    assert.equal(shared.entries[0].candidate, undefined); assert.equal(shared.entries[0].proposal, undefined);
    assert.equal(shared.entries[0].target.identity, memoryEntryIdentity(confirmed.entries[0]));
    assert.equal(shared.entries[0].target.expectedRevision, confirmed.revision);
    assert.equal(JSON.stringify(shared).includes(marker), false);
    if (mode === 'proposal') assert.equal(shared.entries[0].isInference, true);
    const unrelated = await f.memory.readForModel('chat-b', { scopes: [scope] });
    assert.equal(unrelated.entries.length, scope === 'user' ? 1 : 0);
    assert.equal(JSON.stringify(unrelated).includes(marker), false);
  }
});

test('proposal cancellation after initialization, source verification or path preparation never saves or publishes a draft', async t => {
  for (const boundary of ['initialization', 'source', 'before-commit']) {
    const f = await fixture(t), controller = new AbortController(), changes = [];
    const quotes = await f.quote(`取消边界 ${boundary} 的用户来源`);
    f.memory.onChange(change => { changes.push(change); });
    if (boundary === 'initialization') {
      const initialize = f.memory.initializeCandidates.bind(f.memory);
      t.mock.method(f.memory, 'initializeCandidates', async () => { await initialize(); controller.abort(); });
    } else if (boundary === 'source') {
      const validate = f.memory._validateStoredSource.bind(f.memory);
      t.mock.method(f.memory, '_validateStoredSource', async (...args) => { await validate(...args); controller.abort(); });
    } else {
      const safe = f.memory.repository._safe.bind(f.memory.repository);
      t.mock.method(f.memory.repository, '_safe', async (path, options) => {
        const result = await safe(path, options);
        if (options?.create) controller.abort();
        return result;
      });
    }
    await assert.rejects(f.memory.propose('chat-a', { action: 'add', content: '不能保存的取消提案', quotes,
      reason: '取消后停止落盘', isInference: true }, { signal: controller.signal }), { name: 'AbortError' });
    const document = await f.memory.repository.read('chat-a', 'chat');
    assert.equal(document.entries.length, 0); assert.equal(document.revision, 0); assert.deepEqual(changes, []);
  }
});

test('a cancelled proposal waiting for the storage lock never writes or publishes when that lock releases', { timeout: 5000 }, async t => {
  const f = await fixture(t), controller = new AbortController(), changes = [];
  const quotes = await f.quote('等待锁期间取消的用户来源');
  let notifyEntered, release, blocker;
  const entered = new Promise(resolve => { notifyEntered = resolve; }), pause = new Promise(resolve => { release = resolve; });
  f.memory.onChange(change => { changes.push(change); });
  const mutate = f.memory.repository.mutate.bind(f.memory.repository);
  t.mock.method(f.memory.repository, 'mutate', (...args) => {
    blocker = f.conversations.withConversationStorage('chat-a', async () => { notifyEntered(); await pause; });
    return mutate(...args);
  });
  const proposed = f.memory.propose('chat-a', { action: 'add', content: '等待锁的取消提案', quotes,
    reason: '锁释放后仍检查取消', isInference: true }, { signal: controller.signal });
  const rejected = assert.rejects(proposed, { name: 'AbortError' });
  await entered;
  controller.abort(); release();
  await rejected; await blocker;
  assert.equal((await f.memory.repository.read('chat-a', 'chat')).entries.length, 0); assert.deepEqual(changes, []);
});

test('cancellation after an atomic proposal commit keeps the stored draft and its actual completion receipt', async t => {
  const f = await fixture(t), controller = new AbortController(), quotes = await f.quote('已完成提交的用户来源');
  f.memory.onChange(() => { controller.abort(); });
  const result = await f.memory.propose('chat-a', { action: 'add', content: '真实已保存草稿', quotes,
    reason: '提交后返回实际回执', isInference: true }, { signal: controller.signal });
  assert.equal(controller.signal.aborted, true); assert.equal(result.created, true);
  assert.equal((await f.memory.repository.read('chat-a', 'chat')).entries[0].id, result.entry.id);
});

test('model memory reads stop at source I/O boundaries when cancelled and preserve their stored data', async t => {
  const f = await fixture(t), quotes = await f.quote('读取取消核验来源');
  await f.memory.propose('chat-a', { action: 'add', content: '读取中的草稿', quotes, reason: '来源读取边界', isInference: true });
  const before = await f.memory.repository.read('chat-a', 'chat'), controller = new AbortController();
  const read = f.conversations.readMessages.bind(f.conversations);
  t.mock.method(f.conversations, 'readMessages', async (...args) => { const messages = await read(...args); controller.abort(); return messages; });
  await assert.rejects(f.memory.readForModel('chat-a', { scopes: ['chat'] }, { signal: controller.signal }), { name: 'AbortError' });
  assert.deepEqual(await f.memory.repository.read('chat-a', 'chat'), before);
});

test('update proposals preserve the old confirmed target until user confirmation then retain its stable ID', async t => {
  const f = await fixture(t), old = await f.memory.create('chat-a', { scope: 'project', content: '输入上限为旧配置。' });
  const entry = old.entries[0], target = await f.target(entry.id, 'project');
  const quotes = await f.quote('纠正：输入上限现在从当前配置读取，请覆盖旧约定。');
  const result = await f.memory.propose('chat-a', { action: 'update', scope: 'project', content: '输入上限从当前配置读取。',
    quotes, reason: '用户更正了旧约定。', isInference: true, conflicts: ['旧值已不再适用。'], target });
  assert.equal((await f.memory.contextFor('chat-sibling')).entries[0].content, entry.content);
  assert.equal((await f.memory.listFor('chat-a')).scopes.find(document => document.scope === 'project').entries[1].proposalTargetCurrent, true);
  const changes = [];
  f.memory.onChange(async change => { changes.push(change); await f.memory.listFor('chat-a'); });
  const document = await f.memory.updateScope('project', 'work-a', result.entry.id,
    { status: 'confirmed', expectedRevision: result.revision, proposalAction: 'update' });
  assert.equal(document.entries.length, 1); assert.equal(document.entries[0].id, entry.id);
  assert.equal(document.entries[0].revision, entry.revision + 1); assert.equal(document.entries[0].proposal.action, 'update');
  assert.ok(document.entries[0].proposal.appliedAt); assert.equal(document.entries[0].source.messageId, quotes[0].messageId);
  assert.equal((await f.memory.contextFor('chat-sibling')).entries[0].content, '输入上限从当前配置读取。');
  assert.deepEqual(changes.map(change => change.operation).sort(), ['delete', 'update']);
  assert.equal((await f.memory.propose('chat-a', { action: 'update', scope: 'project', content: '输入上限从当前配置读取。',
    quotes, reason: '重试', isInference: true, target })).reason, 'dismissed');
});

test('deletion proposals are retractable drafts and only confirmed deletion removes the exact target', async t => {
  const f = await fixture(t), old = await f.memory.create('chat-a', { scope: 'chat', content: '旧约定' });
  const target = await f.target(old.entries[0].id), quotes = await f.quote('请删除这条旧约定。');
  const input = { action: 'delete', target, quotes, reason: '用户要求移除旧约定。', isInference: false };
  const draft = await f.memory.propose('chat-a', input);
  assert.equal((await f.memory.contextFor('chat-a')).entries.length, 1);
  const discarded = await f.memory.delete('chat-a', draft.entry.id, { scope: 'chat', expectedRevision: draft.revision });
  assert.equal(discarded.entries[0].id, old.entries[0].id);
  assert.equal((await f.memory.propose('chat-a', input)).reason, 'dismissed');
  const freshQuotes = await f.quote('我确认需要建议删除同一旧约定。', 'new-deletion');
  const fresh = await f.memory.propose('chat-a', { ...input, target: await f.target(old.entries[0].id), quotes: freshQuotes });
  const deleted = await f.confirm(fresh);
  assert.equal(deleted.entries.length, 0); assert.deepEqual((await f.memory.contextFor('chat-a')).entries, []);
  const reopened = new MemoryService({ conversationStore: f.conversations });
  // A changed paraphrase cannot resurrect a proposal dismissed from the same original source.
  // 同一原话的建议被丢弃后，换一种提炼措辞也不能使其复活。
  assert.equal((await reopened.propose('chat-a', { action: 'add', content: '改写后的旧约定', quotes,
    reason: '重新提炼', isInference: true })).reason, 'dismissed');
  const renewedQuotes = await f.quote('我重新提出旧约定供确认。', 'renewed-source');
  const renewed = await reopened.propose('chat-a', { action: 'add', content: '旧约定', quotes: renewedQuotes,
    reason: '新的用户来源可以重新提出，仍需用户确认。', isInference: true });
  assert.equal(renewed.created, true); assert.equal(renewed.entry.status, 'draft');
});

test('target scope, version, exact content and source identity are checked before proposing or confirming changes', async t => {
  const f = await fixture(t), old = await f.memory.create('chat-a', { scope: 'chat', content: '保留原条目' });
  const id = old.entries[0].id, target = await f.target(id), quotes = await f.quote('建议更新此条目');
  const input = { action: 'update', content: '建议新内容', quotes, reason: '用户更正', isInference: true, target };
  await assert.rejects(f.memory.propose('chat-a', { ...input, scope: 'user' }), { code: 'MEMORY_SCOPE_CHANGED' });
  await assert.rejects(f.memory.propose('chat-a', { ...input, target: { ...target, identity: 'a'.repeat(64) } }), { code: 'MEMORY_TARGET_CHANGED' });
  await assert.rejects(f.memory.propose('chat-a', { ...input, target: { ...target, expectedRevision: 0 } }), { code: 'MEMORY_CONFLICT' });
  const draft = await f.memory.propose('chat-a', input);
  const edited = await f.memory.update('chat-a', id, { scope: 'chat', content: '用户后来直接编辑的内容', expectedRevision: draft.revision });
  await assert.rejects(f.confirm(draft), { code: 'MEMORY_CONFLICT' });
  await assert.rejects(f.memory.update('chat-a', draft.entry.id,
    { scope: 'chat', status: 'confirmed', expectedRevision: edited.revision, proposalAction: 'update' }),
    { code: 'MEMORY_TARGET_CHANGED' });
  assert.equal((await f.memory.listFor('chat-a')).scopes[0].entries.find(entry => entry.id === draft.entry.id).proposalTargetCurrent, false);
  assert.equal((await f.memory.contextFor('chat-a')).entries[0].content, '用户后来直接编辑的内容');
});

test('legacy draft content confirmation cannot apply an update or deletion without the matching explicit proposal action', async t => {
  for (const action of ['update', 'delete']) {
    const f = await fixture(t), scope = action === 'update' ? 'project' : 'chat';
    const original = await f.memory.create('chat-a', { scope, content: '旧界面必须保留的确认内容' });
    const target = await f.target(original.entries[0].id, scope), quotes = await f.quote(`用户请求的 ${action} 建议`);
    const draft = await f.memory.propose('chat-a', { action, scope, target, quotes,
      content: '待用户明确动作确认的内容', reason: '动作与正文确认须分开表达。', isInference: true });
    const before = await f.memory.repository.read('chat-a', scope);
    const update = input => scope === 'project' ? f.memory.updateScope('project', 'work-a', draft.entry.id, input) :
      f.memory.update('chat-a', draft.entry.id, { scope, ...input });
    const legacyConfirmation = { status: 'confirmed', expectedRevision: draft.revision };
    await assert.rejects(update(legacyConfirmation), { code: 'MEMORY_PROPOSAL_CONFIRMATION_REQUIRED', statusCode: 409 });
    await assert.rejects(update({ ...legacyConfirmation, proposalAction: action === 'update' ? 'delete' : 'update' }),
      { code: 'MEMORY_PROPOSAL_CONFIRMATION_REQUIRED', statusCode: 409 });
    assert.deepEqual(await f.memory.repository.read('chat-a', scope), before);
    const confirmed = await update({ ...legacyConfirmation, proposalAction: action });
    assert.equal(confirmed.entries.length, action === 'update' ? 1 : 0);
    if (action === 'update') {
      assert.equal(confirmed.entries[0].id, original.entries[0].id);
      assert.equal(confirmed.entries[0].content, '待用户明确动作确认的内容');
    }
  }
});

test('source edits, deletion and work moves invalidate semantic drafts at confirmation', async t => {
  for (const change of ['edit', 'delete', 'move']) {
    const f = await fixture(t), quotes = await f.quote(`用户原始约束 ${change}`);
    const draft = await f.memory.propose('chat-a', { action: 'add', scope: 'project', content: '提炼后的约束', quotes,
      reason: '引用工作约束', isInference: true });
    if (change === 'edit') await f.conversations.upsertMessage('chat-a', user(quotes[0].messageId, '后来改变的原话'));
    else {
      const catalog = await f.conversations.catalog(), chat = catalog.Projects[0].Chats.shift();
      if (change === 'move') catalog.Projects[1].Chats.push(chat);
      await f.conversations.saveCatalog(catalog);
    }
    await assert.rejects(f.memory.updateScope('project', 'work-a', draft.entry.id,
      { status: 'confirmed', expectedRevision: draft.revision }), { code: 'MEMORY_SOURCE_UNAVAILABLE' });
    assert.equal((await f.memory.listScope('project', 'work-a')).entries[0].active, false);
  }
});

test('target identity rejects content or source changes even if an external edit fails to increment versions', async t => {
  for (const change of ['content', 'source']) {
    const f = await fixture(t), old = await f.memory.create('chat-a', { scope: 'chat', content: '目标原内容' });
    const target = await f.target(old.entries[0].id), quotes = await f.quote(`删除建议来源 ${change}`);
    const draft = await f.memory.propose('chat-a', { action: 'delete', target, quotes, reason: '精确核验目标', isInference: false });
    const file = join(f.root, 'Projects', 'work-a', 'Sessions', 'chat-a', 'Memory', 'entries.json');
    const document = JSON.parse(await readFile(file, 'utf8'));
    if (change === 'content') document.entries[0].content = '外部修改后的目标内容';
    else document.entries[0].source.conversationId = 'chat-b';
    await writeFile(file, JSON.stringify(document));
    await assert.rejects(f.confirm(draft), { code: 'MEMORY_TARGET_CHANGED' });
    assert.equal((await f.memory.repository.read('chat-a', 'chat')).entries.length, 2);
  }
});

test('source verification runs again inside the storage transaction after an earlier successful availability check', async t => {
  const f = await fixture(t), quotes = await f.quote('当前有效原话');
  const draft = await f.memory.propose('chat-a', { action: 'add', content: '提炼后的内容', quotes, reason: '引用原话', isInference: true });
  const update = f.memory.repository.update.bind(f.memory.repository);
  t.mock.method(f.memory.repository, 'update', async (...args) => {
    await f.conversations.upsertMessage('chat-a', user(quotes[0].messageId, '校验与确认之间改变的原话'));
    return update(...args);
  });
  await assert.rejects(f.confirm(draft), { code: 'MEMORY_SOURCE_UNAVAILABLE' });
  assert.equal((await f.memory.repository.read('chat-a', 'chat')).revision, draft.revision);
  assert.deepEqual((await f.memory.contextFor('chat-a')).entries, []);
});

test('equal or similar confirmed content produces explicit candidate conflicts without automatic merge or replacement', async t => {
  const f = await fixture(t), content = '说明使用中文和英文。';
  const existing = await f.memory.create('chat-a', { scope: 'chat', content });
  const quotes = await f.quote('我的说明需要中文，也需要英文。');
  const draft = await f.memory.propose('chat-a', { action: 'add', content, quotes, reason: '可能和原约定重复，留给用户核验。', isInference: true });
  assert.equal(draft.created, true); assert.deepEqual(draft.entry.proposal.conflicts, [`exact-content:${existing.entries[0].id}`]);
  assert.equal(draft.entry.status, 'draft'); assert.equal((await f.memory.contextFor('chat-a')).entries.length, 1);
  const similarQuotes = await f.quote('请保留双语说明习惯。', 'similar-source');
  const similar = await f.memory.propose('chat-a', { action: 'add', content: '保留双语说明习惯。', quotes: similarQuotes,
    reason: '语义相近仍只是单独建议。', isInference: true });
  assert.equal(similar.created, true); assert.equal((await f.memory.repository.read('chat-a', 'chat')).entries.length, 3);
});

test('concurrent suggestions deduplicate by the complete proposal fingerprint; no-op and disabled extraction write nothing', async t => {
  const f = await fixture(t), quotes = await f.quote('并发提炼使用同一来源。');
  const input = { action: 'add', content: '同一提案内容', quotes, reason: '并发提案', isInference: true };
  const other = new MemoryService({ conversationStore: f.conversations });
  const results = await Promise.all([f.memory.propose('chat-a', input), other.propose('chat-a', input)]);
  assert.equal(results.filter(result => result.created).length, 1); assert.equal(results[0].entry.id, results[1].entry.id);
  const before = await f.memory.repository.read('chat-a', 'chat');
  assert.equal((await f.memory.propose('chat-a', { action: 'noop' })).reason, 'no-change');
  f.memory.configureCandidates({ enabled: false });
  assert.equal((await f.memory.propose('chat-a', input)).reason, 'disabled');
  assert.deepEqual(await f.memory.repository.read('chat-a', 'chat'), before);
});

test('tampered proposal identity is rejected without overwriting the canonical memory document', async t => {
  const f = await fixture(t), quotes = await f.quote('来源内容');
  await f.memory.propose('chat-a', { action: 'add', content: '提炼后的内容', quotes, reason: '提炼', isInference: true });
  const file = join(f.root, 'Projects', 'work-a', 'Sessions', 'chat-a', 'Memory', 'entries.json');
  const document = JSON.parse(await readFile(file, 'utf8')); document.entries[0].proposal.fingerprint = 'f'.repeat(64);
  const tampered = JSON.stringify(document); await writeFile(file, tampered);
  await assert.rejects(f.memory.repository.read('chat-a', 'chat'), { code: 'CORRUPT_MEMORY' });
  await assert.rejects(f.memory.create('chat-a', { scope: 'chat', content: '不能覆盖' }), { code: 'CORRUPT_MEMORY' });
  assert.equal(await readFile(file, 'utf8'), tampered);
});

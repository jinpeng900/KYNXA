import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { test } from 'node:test';
import { ConversationStore } from '../data/conversations.mjs';
import { MemoryService } from '../data/memory-service.mjs';
import { extractMemoryCandidates, memoryCandidateSettings } from '../data/memory-candidates.mjs';
import { memorySourceId, validateMemoryDocument } from '../data/memory-contracts.mjs';

const user = (Id, Content) => ({ Id, Role: 'user', Content, Status: 'completed' });
const assistant = (Id, Content = 'Synthetic completed response', Status = 'completed') => ({ Id, Role: 'assistant', Content, Status });
const rounds = (count, length = 20) => Array.from({ length: count }, (_, index) => [
  user(`user-${index}`, `我偏好：第 ${index} 项采用明确配置。${'甲'.repeat(length)}`), assistant(`assistant-${index}`)]).flat();

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-memory-candidates-'));
  t.after(async () => {
    const suffix = relative(resolve(tmpdir()), resolve(root));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(root, { recursive: true, force: true });
  });
  const conversations = new ConversationStore({ dataHome: join(root, 'Models'), legacyDesktopDirectory: null });
  const catalog = await conversations.catalog();
  const chat = Id => ({ Id, Title: Id, Messages: [user(`initial-${Id}`, 'hello')] });
  await conversations.saveCatalog({ ...catalog, Projects: [
    { Id: 'work-a', Name: 'Work A', Chats: [chat('chat-a'), chat('chat-sibling')] },
    { Id: 'work-b', Name: 'Work B', Chats: [chat('chat-b')] },
    { Id: 'folderless', Name: 'Folderless', IsFolderlessWorkspace: true, Chats: [chat('chat-f')] }
  ], Chats: [chat('chat-plain')] });
  const memory = new MemoryService({ conversationStore: conversations });
  const saveUser = async (text, id = 'trigger', conversationId = 'chat-a') => {
    await conversations.upsertMessage(conversationId, user(id, text));
    return memory.scheduleCandidates(conversationId, { phase: 'user', userMessageId: id });
  };
  return { root, memory, conversations, saveUser };
}

test('automatic triggers preserve user quotations as inactive drafts and require an explicit revisioned confirmation', async t => {
  const f = await fixture(t), raw = '  纠正：我们的输出要求保留完整原话。\n第二行仍是用户正文。  ';
  assert.equal((await f.saveUser(raw)).created, 1);
  let document = await f.memory.repository.read('chat-a', 'chat');
  const entry = document.entries[0];
  assert.equal(entry.status, 'draft'); assert.equal(entry.candidate.trigger, 'correction');
  assert.equal(entry.candidate.quotes[0].text, raw); assert.equal(entry.content, raw.trim());
  assert.deepEqual((await f.memory.contextFor('chat-a')).entries, []);
  assert.equal((await f.memory.listFor('chat-a')).scopes[0].entries[0].active, false);
  await assert.rejects(f.memory.update('chat-a', entry.id, { scope: 'chat', status: 'confirmed' }), { code: 'INVALID_MEMORY' });
  document = await f.memory.update('chat-a', entry.id, { scope: 'chat', status: 'confirmed', expectedRevision: document.revision });
  assert.equal(document.entries[0].status, 'confirmed'); assert.equal(document.entries[0].revision, 2);
  assert.equal((await f.memory.contextFor('chat-a')).entries[0].content, raw.trim());
  assert.deepEqual((await f.memory.contextFor('chat-sibling')).entries, []);
  await assert.rejects(f.memory.update('chat-a', entry.id, { scope: 'chat', status: 'draft', expectedRevision: document.revision }), { code: 'INVALID_MEMORY' });
});

test('the existing whole-message remember command stays immediately confirmed and automatic capture excludes its source', async t => {
  const f = await fixture(t), raw = '记住：正式兼容约定';
  await f.conversations.upsertMessage('chat-a', user('explicit', raw));
  const confirmed = await f.memory.captureExplicit('chat-a', 'explicit', raw);
  assert.equal(confirmed.status, 'confirmed'); assert.equal(confirmed.scope, 'project');
  assert.equal((await f.memory.scheduleCandidates('chat-a', { phase: 'user', userMessageId: 'explicit' })).created, 0);
  assert.equal((await f.memory.contextFor('chat-sibling')).entries.length, 1);
});

test('scope prefixes alone authorize work and global candidates; default and folderless candidates stay in their own chat', async t => {
  const f = await fixture(t);
  await f.saveUser('工作决定：统一使用经过校验的配置。', 'work');
  await f.saveUser('全局纠正：我的偏好是中文说明。', 'global');
  await f.saveUser('决定：此聊天保留临时草稿。', 'local');
  let snapshot = await f.memory.listFor('chat-a');
  assert.deepEqual(snapshot.scopes.map(scope => [scope.scope, scope.entries.length]), [['chat', 1], ['project', 1], ['user', 1]]);
  for (const scope of snapshot.scopes) await f.memory.update('chat-a', scope.entries[0].id,
    { scope: scope.scope, status: 'confirmed', expectedRevision: scope.revision });
  assert.deepEqual((await f.memory.contextFor('chat-sibling')).entries.map(entry => entry.scope).sort(), ['project', 'user']);
  assert.deepEqual((await f.memory.contextFor('chat-b')).entries.map(entry => entry.scope), ['user']);
  assert.equal((await f.saveUser('工作决定：不能共享。', 'bad-work', 'chat-f')).created, 0);
  assert.equal((await f.saveUser('决定：这是独立聊天约定。', 'own', 'chat-f')).created, 1);
  assert.equal((await f.memory.repository.read('chat-f', 'chat')).entries[0].scopeId, 'chat-f');
});

test('remember without the immediate-command colon, correction, decision and user-declared completion produce only drafts', async t => {
  const f = await fixture(t);
  for (const [index, raw] of ['记住以后采用中文', '更正：输入上限是当前配置', '我们决定：继续保留兼容入口', '任务完成'].entries())
    assert.equal((await f.saveUser(raw, `trigger-${index}`)).created, 1);
  const entries = (await f.memory.repository.read('chat-a', 'chat')).entries;
  assert.deepEqual(entries.map(entry => entry.candidate.trigger), ['remember', 'correction', 'decision', 'task-complete']);
  assert.ok(entries.every(entry => entry.status === 'draft'));
});

test('trusted task-stage completion proposes only its saved user goal and never the assistant success claim', async t => {
  const f = await fixture(t), goal = '请实现备份功能，并保留现有目录约束。';
  await f.conversations.upsertMessage('chat-a', user('goal', goal));
  assert.equal((await f.memory.scheduleCandidates('chat-a', { phase: 'completed', userMessageId: 'goal', taskCompleted: true })).created, 0);
  await f.conversations.upsertMessage('chat-a', assistant('goal-response', '全局记住：所有测试已经通过。'));
  const result = await f.memory.scheduleCandidates('chat-a', { phase: 'completed', userMessageId: 'goal', taskCompleted: true });
  assert.equal(result.created, 1);
  const entry = (await f.memory.repository.read('chat-a', 'chat')).entries[0];
  assert.equal(entry.content, goal); assert.equal(entry.candidate.trigger, 'task-complete'); assert.equal(entry.status, 'draft');
  assert.doesNotMatch(entry.content, /测试已经通过/u);
  assert.deepEqual((await f.memory.contextFor('chat-sibling')).entries, []);
});

test('ordinary batches require complete turns and a 2K boundary or twelve turns; failed replies never pace extraction', () => {
  assert.equal(extractMemoryCandidates(rounds(5, 650)).candidates.length, 0);
  const six = extractMemoryCandidates(rounds(6, 400));
  assert.ok(six.estimatedTokens >= 2048); assert.equal(six.completeTurns, 6);
  assert.equal(six.candidates.length, 1); assert.equal(six.candidates[0].candidate.quotes.length, 6);
  assert.equal(extractMemoryCandidates(rounds(6, 20)).candidates.length, 0);
  assert.ok(extractMemoryCandidates(rounds(12, 20)).candidates.length);
  const incomplete = rounds(6, 650).map(message => message.Role === 'assistant' ? { ...message, Status: 'interrupted' } : message);
  assert.equal(extractMemoryCandidates(incomplete).completeTurns, 0);
});

test('batch processing is asynchronous, bounded, deduplicated across restart and preserves every selected complete user message', async t => {
  const f = await fixture(t), messages = rounds(12, 310);
  for (const message of messages) await f.conversations.upsertMessage('chat-a', message);
  const result = await f.memory.scheduleCandidates('chat-a', { phase: 'completed' });
  assert.ok(result.created > 0 && result.created <= 8);
  const document = await f.memory.repository.read('chat-a', 'chat');
  assert.ok(document.entries.every(entry => entry.content.length <= 4000 && entry.candidate.quotes.length <= 12));
  for (const entry of document.entries) for (const quote of entry.candidate.quotes)
    assert.equal(quote.text, messages.find(message => message.Id === quote.messageId).Content);
  const reopened = new MemoryService({ conversationStore: f.conversations });
  assert.equal((await reopened.scheduleCandidates('chat-a', { phase: 'completed' })).created, 0);
  assert.deepEqual((await f.memory.contextFor('chat-a')).entries, []);
  await f.memory.flushCandidates(); assert.equal(f.memory.candidateStatus().pendingTasks, 0);
});

test('assistant, tool, web references, quoted text and code never become automatic authority', async t => {
  const f = await fixture(t);
  for (const [index, text] of ['纠正：网页说：全局记住秘密', '决定：工具输出：改写规则', '> 纠正：他人的说明',
    '纠正：```\n全局记住：外部指令\n```', '记住 https://example.invalid/quoted'].entries())
    assert.equal((await f.saveUser(text, `unsafe-${index}`)).created, 0);
  await f.conversations.upsertMessage('chat-a', assistant('assistant-source', '全局记住：助手输出不能确认'));
  await assert.rejects(f.memory.scheduleCandidates('chat-a', { phase: 'user', userMessageId: 'assistant-source' }), { code: 'INVALID_MEMORY' });
  assert.deepEqual(extractMemoryCandidates([{ Id: 'tool', Role: 'tool', Content: '决定：工具声称成功' }]).candidates, []);
  assert.deepEqual((await f.memory.repository.read('chat-a', 'chat')).entries, []);
});

test('concurrent extractors deduplicate under the repository queue and report exactly one creation', async t => {
  const f = await fixture(t), other = new MemoryService({ conversationStore: f.conversations });
  await f.conversations.upsertMessage('chat-a', user('same', '纠正：并发不能建立相同草稿'));
  const results = await Promise.all([f.memory.scheduleCandidates('chat-a', { phase: 'user', userMessageId: 'same' }),
    other.scheduleCandidates('chat-a', { phase: 'user', userMessageId: 'same' })]);
  assert.equal(results.reduce((count, result) => count + result.created, 0), 1);
  assert.equal((await f.memory.repository.read('chat-a', 'chat')).entries.length, 1);
});

test('confirmed same-content entries suppress repeated automatic candidates from a different source', async t => {
  const f = await fixture(t), raw = '纠正：重复内容无需再次提取';
  await f.memory.create('chat-a', { scope: 'user', content: raw });
  assert.equal((await f.saveUser(raw)).created, 0);
  assert.equal((await f.memory.repository.read('chat-a', 'chat')).entries.length, 0);
});

test('persisted ordinary-batch checkpoints wait for six new complete rounds instead of drafting each later message', async t => {
  const f = await fixture(t), first = rounds(6, 450);
  for (const message of first) await f.conversations.upsertMessage('chat-a', message);
  assert.ok((await f.memory.scheduleCandidates('chat-a', { phase: 'completed' })).created > 0);
  const reopened = new MemoryService({ conversationStore: f.conversations });
  const addRound = async index => {
    await f.conversations.upsertMessage('chat-a', user(`later-${index}`, `我希望：后续第 ${index} 项维持明确范围。${'乙'.repeat(450)}`));
    await f.conversations.upsertMessage('chat-a', assistant(`later-response-${index}`));
  };
  for (let index = 0; index < 5; index++) {
    await addRound(index);
    assert.equal((await reopened.scheduleCandidates('chat-a', { phase: 'completed' })).created, 0);
  }
  await addRound(5);
  assert.ok((await reopened.scheduleCandidates('chat-a', { phase: 'completed' })).created > 0);
});

test('deleting drafts retains revocations for all originals and their fingerprint without resurrecting repeated text', async t => {
  const f = await fixture(t), raw = '决定：删除候选不应重新生成';
  await f.saveUser(raw);
  const document = await f.memory.repository.read('chat-a', 'chat');
  const deleted = await f.memory.delete('chat-a', document.entries[0].id, { scope: 'chat', expectedRevision: document.revision });
  assert.equal(deleted.dismissedSources[0].candidateFingerprint, document.entries[0].candidate.fingerprint);
  assert.equal((await f.saveUser(raw, 'same-new-message')).created, 0);
  const reopened = new MemoryService({ conversationStore: f.conversations });
  assert.equal((await reopened.scheduleCandidates('chat-a', { phase: 'user', userMessageId: 'trigger' })).created, 0);
});

test('candidate original edits and work moves invalidate confirmation rather than rebinding its authority', async t => {
  const f = await fixture(t), raw = '工作决定：此约定仅属于原工作';
  await f.saveUser(raw);
  const original = await f.memory.repository.read('chat-a', 'project'), entry = original.entries[0];
  await f.conversations.upsertMessage('chat-a', user('trigger', '工作决定：原话已修改'));
  await assert.rejects(f.memory.updateScope('project', 'work-a', entry.id, { status: 'confirmed', expectedRevision: original.revision }), { code: 'MEMORY_SOURCE_UNAVAILABLE' });
  await f.conversations.upsertMessage('chat-a', user('trigger', raw));
  const catalog = await f.conversations.catalog();
  catalog.Projects[1].Chats.push(catalog.Projects[0].Chats.shift()); await f.conversations.saveCatalog(catalog);
  await assert.rejects(f.memory.updateScope('project', 'work-a', entry.id, { status: 'confirmed', expectedRevision: original.revision }), { code: 'MEMORY_SOURCE_UNAVAILABLE' });
  assert.deepEqual((await f.memory.repository.read('chat-a', 'project')).entries, []);
});

test('queued work candidates cannot silently move into another work before their write', async t => {
  const f = await fixture(t);
  let entered, resume;
  const started = new Promise(resolve => { entered = resolve; }), pause = new Promise(resolve => { resume = resolve; });
  const originalCreate = f.memory.repository.createCandidate.bind(f.memory.repository);
  f.memory.repository.createCandidate = async (...args) => { entered(); await pause; return originalCreate(...args); };
  await f.conversations.upsertMessage('chat-a', user('move', '工作决定：排队期间也保持范围'));
  const task = f.memory.scheduleCandidates('chat-a', { phase: 'user', userMessageId: 'move' });
  await started;
  const catalog = await f.conversations.catalog();
  catalog.Projects[1].Chats.push(catalog.Projects[0].Chats.shift()); await f.conversations.saveCatalog(catalog);
  resume(); await assert.rejects(task, { code: 'MEMORY_SCOPE_CHANGED' });
  assert.deepEqual((await f.memory.repository.read('chat-a', 'project')).entries, []);
});

test('lifecycle notifications release catalog locks and identify the exact old/new indexed state without raw text', async t => {
  const f = await fixture(t), events = [];
  const unsubscribe = f.memory.onChange(async event => { await f.memory.contextFor('chat-a'); events.push(event); });
  await f.saveUser('纠正：选择性增量生命周期');
  let document = await f.memory.repository.read('chat-a', 'chat');
  const id = document.entries[0].id;
  document = await f.memory.update('chat-a', id, { scope: 'chat', status: 'confirmed', expectedRevision: document.revision });
  document = await f.memory.update('chat-a', id, { scope: 'chat', content: '用户人工更新后的内容', expectedRevision: document.revision });
  await f.memory.delete('chat-a', id, { scope: 'chat', expectedRevision: document.revision });
  assert.deepEqual(events.map(event => [event.operation, event.previousStatus, event.status]),
    [['create', null, 'draft'], ['confirm', 'draft', 'confirmed'], ['update', 'confirmed', 'confirmed'], ['delete', 'confirmed', null]]);
  assert.ok(events.every(event => event.sourceId === memorySourceId('chat', 'chat-a', id) && !Object.hasOwn(event, 'content')));
  assert.deepEqual(events.map(event => event.revision), [1, 2, 3, 4]);
  assert.equal(events[2].previousEntryRevision, 2); assert.equal(events[2].entryRevision, 3);
  unsubscribe(); await f.memory.create('chat-a', { scope: 'user', content: 'Confirmed independent entry' }); assert.equal(events.length, 4);
});

test('manual confirmed creation and draft deletion both publish lifecycle transitions', async t => {
  const f = await fixture(t), events = [];
  f.memory.onChange(event => { events.push(event); });
  const manual = await f.memory.createScope('user', 'user', { content: 'Manual confirmed preference' });
  assert.equal(events[0].status, 'confirmed'); assert.equal(events[0].scopeKey, 'user');
  await f.memory.updateScope('user', 'user', manual.entries[0].id, { kind: 'decision', expectedRevision: manual.revision });
  await f.saveUser('纠正：拒绝的草稿应通知清理');
  const draft = await f.memory.repository.read('chat-a', 'chat');
  await f.memory.delete('chat-a', draft.entries[0].id, { scope: 'chat', expectedRevision: draft.revision });
  assert.deepEqual(events.at(-1).previousStatus, 'draft'); assert.equal(events.at(-1).status, null);
});

test('disabled settings, invalid limits and failed subscribers remain explicit without losing committed memory', async t => {
  const f = await fixture(t);
  f.memory.configureCandidates({ enabled: false });
  assert.equal((await f.saveUser('纠正：禁用时不生成')).reason, 'disabled');
  assert.deepEqual((await f.memory.repository.read('chat-a', 'chat')).entries, []);
  assert.throws(() => memoryCandidateSettings({ minTurns: 5 }), { code: 'INVALID_MEMORY' });
  assert.throws(() => memoryCandidateSettings({ maxTokens: 4097 }), { code: 'INVALID_MEMORY' });
  assert.throws(() => memoryCandidateSettings({ unknown: true }), { code: 'INVALID_MEMORY' });
  f.memory.configureCandidates({ enabled: true });
  f.memory.onChange(() => { throw Object.assign(new Error('Synthetic subscriber failure'), { code: 'INDEX_UNAVAILABLE' }); });
  assert.equal((await f.saveUser('纠正：通知失败仍保留已提交原文', 'enabled')).created, 1);
  assert.equal(f.memory.candidateStatus().changeNotificationError.code, 'INDEX_UNAVAILABLE');
});

test('legacy confirmed entries remain readable alongside drafts and malformed provenance never overwrites the file', async t => {
  const f = await fixture(t);
  await f.memory.create('chat-a', { scope: 'chat', content: 'Legacy confirmed fact' });
  await f.saveUser('纠正：新候选与旧数据兼容');
  const sessionDirectory = await f.conversations.resolveSessionDirectory('chat-a'), file = join(sessionDirectory, 'Memory', 'entries.json');
  const document = JSON.parse(await readFile(file, 'utf8'));
  assert.equal(validateMemoryDocument(document, document).entries.length, 2);
  document.entries[1].candidate.fingerprint = '0'.repeat(64);
  const corrupt = JSON.stringify(document); await writeFile(file, corrupt);
  await assert.rejects(f.memory.create('chat-a', { scope: 'chat', content: 'Must not overwrite' }), { code: 'CORRUPT_MEMORY' });
  assert.equal(await readFile(file, 'utf8'), corrupt);
  await assert.rejects(f.memory.createScope('user', 'user', { status: 'draft', content: 'Forged automatic draft' }), { code: 'INVALID_MEMORY' });
});

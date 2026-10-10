import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { link, mkdir, readFile, readdir, realpath, rename, stat, symlink, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from 'node:test';
import { AgentConfigRepository } from '../tools/agent-config.mjs';
import { ToolService } from '../tools/tool-service.mjs';
import { estimateTokens } from '../models/context.mjs';
import { approve, parsed, pendingApproval, toolFixture } from './tool-fixture.mjs';

test('ongoing work binds one directory, schedules preparation and continues without expanding the current scope', async t => {
  const f = await toolFixture(t), root = join(f.root, 'ongoing-work');
  await mkdir(root);
  await writeFile(join(root, 'source.txt'), 'current work');
  const unmounted = await f.conversations.catalog();
  unmounted.Projects[0].FolderPath = null;
  await f.conversations.saveCatalog(unmounted);
  const context = await f.context('full'), staleContext = await f.context('full');
  await writeFile(join(context.workspaceRoot, 'original.txt'), 'original scope');
  const originalWorkspace = context.workspaceRoot;
  const before = await f.conversations.catalog();
  let schedules = 0;
  f.service.retrieval = { scheduleMountedProjects: () => { schedules++; } };
  const binding = parsed(await f.run(context, 'work.folder.bind', { path: root, reason: 'Continue working in this folder' }));
  assert.equal(binding.folderPath, root);
  assert.equal(binding.currentTurnScopeChanged, false);
  assert.equal(context.workspaceRoot, originalWorkspace);
  assert.equal(schedules, 1);
  const current = await f.conversations.catalog();
  assert.equal(current.Projects[0].FolderPath, root);
  assert.deepEqual(current.Projects[0].Chats, before.Projects[0].Chats);
  assert.deepEqual(current.Chats, before.Chats);
  assert.equal(parsed(await f.run(context, 'filesystem.read', { path: 'original.txt' })).content, 'original scope');
  assert.equal(parsed(await f.run(context, 'filesystem.read', { path: join(root, 'source.txt'), reason: 'Read ongoing work' })).content, 'current work');
  assert.equal((await f.run(staleContext, 'filesystem.read', { path: 'original.txt' })).code, 'WORKSPACE_CHANGED');
  parsed(await f.run(context, 'work.folder.bind', { path: root, reason: 'Same folder' }));
  assert.equal((await f.conversations.catalog()).Revision, current.Revision, 'same binding is idempotent');
  const next = await f.context('full');
  assert.equal(next.workspaceRoot, root);
  assert.equal(parsed(await f.run(next, 'filesystem.read', { path: 'source.txt' })).content, 'current work');
});

test('directory association requires existing directories and bound approval; temporary reads never bind', async t => {
  const f = await toolFixture(t), root = join(f.root, 'requested-work');
  await mkdir(root);
  const context = await f.context('ask'), before = await f.conversations.catalog();
  parsed(await f.run(context, 'filesystem.list', { path: '.' }));
  assert.deepEqual(await f.conversations.catalog(), before);
  const denied = await pendingApproval(f.service, context,
    f.call('work.folder.bind', { path: root, reason: 'Ongoing folder work' }));
  approve(f.service, context, denied.event.tool, false);
  assert.equal((await denied.result).code, 'TOOL_DENIED');
  assert.deepEqual(await f.conversations.catalog(), before);
  assert.equal((await f.run(await f.context('full'), 'work.folder.bind', { path: '.', reason: 'ambiguous relative root' })).code,
    'WORK_BINDING_PATH_REQUIRED');
  assert.equal((await f.run(await f.context('full', f.standaloneId), 'work.folder.bind', { path: root, reason: 'No work owner' })).code,
    'WORK_REQUIRED');
});

test('unknown dispatch proof comes from the owned terminal registry, never observation text or another chat', async t => {
  const f = await toolFixture(t, { hostTerminalRunner: { run: async () => assert.fail('state verification cannot dispatch a command') } }),
    context = await f.context('full');
  const call = f.call('terminal.host.start', { command: 'synthetic-once' });
  const records = [{ call, result: { status: 'unknown', recoveryHandle: { kind: 'host-terminal-job', jobId: 'owned-job' } } }];
  const observation = { call: f.call('terminal.host.read', { jobId: 'owned-job' }),
    result: { isError: false, content: '{"processId":123,"status":"completed"}' } };
  assert.deepEqual(await f.service.verifyUnknownEffects(context, records, observation), [], 'model-visible JSON cannot prove a missing job');
  f.service.hostTerminalJobs.jobs.set('owned-job', { id: 'owned-job', conversationId: context.conversationId,
    status: 'completed', processId: 123, output: '', totalCharacters: 0, baseOffset: 0,
    result: { isError: true, value: { completed: true, exitCode: 1 } } });
  const proofs = await f.service.verifyUnknownEffects(context, records, observation);
  assert.equal(proofs.length, 1);
  assert.equal(proofs[0].outcome, 'dispatch-confirmed');
  const canonical = await f.service.results.get(context, proofs[0].result.resultRef.id);
  assert.equal(canonical.structuredContent.taskSuccessCertified, false);
  assert.equal(canonical.structuredContent.receipt.exitCode, 1, 'started successfully does not mean the command succeeded');
  assert.deepEqual(await f.service.verifyUnknownEffects(await f.context('full', f.standaloneId), records, observation), []);
  assert.deepEqual(await f.service.verifyUnknownEffects(context, records,
    { ...observation, call: f.call('terminal.host.read', { jobId: 'different-job' }) }), []);
  f.service.hostTerminalJobs.jobs.get('owned-job').status = 'unknown';
  assert.deepEqual(await f.service.verifyUnknownEffects(context, records, observation), []);
});

test('malformed tool envelopes return a bounded validation receipt without dispatching', async t => {
  const f = await toolFixture(t), context = await f.context('full');
  f.service.mcp.execute = async () => assert.fail('invalid inputs must not dispatch');
  for (const call of [null, {}, { id: 'malformed', name: 10, arguments: {} },
    { id: 'malformed', name: 'filesystem.read', arguments: [] },
    { id: 'malformed', name: 'filesystem.read', arguments: null }]) {
    const result = await f.service.execute(context, call);
    assert.equal(result.isError, true);
    assert.equal(result.code, 'INVALID_TOOL_ARGUMENTS');
    assert.equal(result.resultRef, undefined);
    assert.equal(result.status, undefined);
  }
});

test('scoped UTF-8 reads, atomic writes, unique edits and hashes preserve the formal catalog', async t => {
  const f = await toolFixture(t), ctx = await f.context('smart'), before = await f.conversations.catalog();
  const created = parsed(await f.run(ctx, 'filesystem.write', { path: 'note.txt', content: '\uFEFF你好\r\nonly needle\r\n', expectedHash: null }));
  assert.match(created.sha256, /^[a-f0-9]{64}$/);
  const read = parsed(await f.run(ctx, 'filesystem.read', { path: 'note.txt' }));
  assert.equal(read.content, '\uFEFF你好\r\nonly needle\r\n');
  assert.equal(parsed(await f.run(ctx, 'filesystem.stat', { path: 'note.txt' })).sha256, read.sha256);
  const changed = parsed(await f.run(ctx, 'filesystem.edit', { path: 'note.txt', oldText: 'needle', newText: '替换', expectedHash: read.sha256 }));
  assert.notEqual(changed.sha256, read.sha256);
  assert.equal(await readFile(join(f.workspace, 'note.txt'), 'utf8'), '\uFEFF你好\r\nonly 替换\r\n');
  assert.equal((await f.run(ctx, 'filesystem.write', { path: 'note.txt', content: 'stale', expectedHash: read.sha256 })).code, 'TOOL_FILE_CONFLICT');
  assert.equal((await f.run(ctx, 'filesystem.write', { path: 'note.txt', content: 'new collision', expectedHash: null })).code, 'TOOL_FILE_CONFLICT');
  parsed(await f.run(ctx, 'filesystem.write', { path: 'duplicate.txt', content: 'aaaa', expectedHash: null }));
  const dup = parsed(await f.run(ctx, 'filesystem.read', { path: 'duplicate.txt' }));
  assert.equal((await f.run(ctx, 'filesystem.edit', { path: 'duplicate.txt', oldText: 'aa', newText: 'b', expectedHash: dup.sha256 })).code, 'TOOL_EDIT_NOT_UNIQUE');
  assert.equal(await readFile(join(f.workspace, 'duplicate.txt'), 'utf8'), 'aaaa');
  assert.equal((await readdir(f.workspace)).some(name => name.startsWith('.kynxa-write-')), false);
  assert.deepEqual(await f.conversations.catalog(), before);
});

test('Ask approvals are bound, single use, immutable and denied calls have no effects', async t => {
  const f = await toolFixture(t), ctx = await f.context('ask');
  assert.equal((await f.run(ctx, 'filesystem.write', { path: 'missing.txt', content: 'no', expectedHash: null }, { interactive: false })).code, 'TOOL_APPROVAL_REQUIRED');
  const call = f.call('filesystem.write', { path: 'safe.txt', content: 'original', expectedHash: null });
  const { result, event } = await pendingApproval(f.service, ctx, call);
  assert.equal(event.type, 'approval_required');
  assert.equal(event.tool.status, 'approval-required');
  await assert.rejects(stat(join(f.workspace, 'safe.txt')), { code: 'ENOENT' });
  assert.throws(() => f.service.approve({ conversationId: ctx.conversationId, requestId: randomUUID(),
    toolCallId: call.id, approvalId: event.tool.approvalId, approved: true }), { code: 'TOOL_APPROVAL_MISMATCH' });
  call.arguments.path = 'changed.txt'; call.arguments.content = 'changed'; event.tool.arguments.content = 'event-mutated';
  f.service.approve({ conversationId: ctx.conversationId.toUpperCase(), requestId: ctx.requestId.toUpperCase(),
    toolCallId: call.id, approvalId: event.tool.approvalId.toUpperCase(), approved: true });
  parsed(await result);
  assert.equal(await readFile(join(f.workspace, 'safe.txt'), 'utf8'), 'original');
  await assert.rejects(stat(join(f.workspace, 'changed.txt')), { code: 'ENOENT' });
  assert.throws(() => approve(f.service, ctx, event.tool), { code: 'TOOL_APPROVAL_NOT_FOUND' });
  const denied = await pendingApproval(f.service, ctx, f.call('filesystem.mkdir', { path: 'denied' }));
  approve(f.service, ctx, denied.event.tool, false);
  assert.equal((await denied.result).code, 'TOOL_DENIED');
  await assert.rejects(stat(join(f.workspace, 'denied')), { code: 'ENOENT' });
  assert.equal((await f.service.execute({ ...ctx }, f.call('filesystem.read', { path: 'safe.txt' }))).code, 'INVALID_TOOL_CONTEXT');
  const released = await pendingApproval(f.service, ctx, f.call('filesystem.mkdir', { path: 'released' }));
  await f.service.releaseContext(ctx); assert.equal((await released.result).code, 'TOOL_CANCELLED');
  assert.throws(() => approve(f.service, ctx, released.event.tool), { code: 'TOOL_APPROVAL_NOT_FOUND' });
});

test('Smart deletion needs approval, is never recursive, and Full outside access needs a reason', async t => {
  const f = await toolFixture(t), smart = await f.context('smart'), full = await f.context('full');
  parsed(await f.run(smart, 'filesystem.mkdir', { path: 'folder' }));
  const created = parsed(await f.run(smart, 'filesystem.write', { path: 'folder/file.txt', content: 'keep', expectedHash: null }));
  assert.equal((await f.run(smart, 'filesystem.delete', { path: 'folder/file.txt', expectedHash: created.sha256 }, { interactive: false })).code, 'TOOL_APPROVAL_REQUIRED');
  assert.equal((await f.run(full, 'filesystem.delete', { path: 'folder', expectedHash: null })).isError, true);
  assert.equal(await readFile(join(f.workspace, 'folder/file.txt'), 'utf8'), 'keep');
  const deletion = await pendingApproval(f.service, smart, f.call('filesystem.delete', { path: 'folder/file.txt', expectedHash: created.sha256 }));
  approve(f.service, smart, deletion.event.tool); parsed(await deletion.result);
  parsed(await f.run(full, 'filesystem.delete', { path: 'folder', expectedHash: null }));
  assert.equal((await f.run(full, 'filesystem.delete', { path: '.', expectedHash: null })).code, 'UNSAFE_TOOL_PATH');
  const outside = join(f.root, 'outside.txt'); await writeFile(outside, 'outside');
  assert.equal((await f.run(full, 'filesystem.read', { path: outside })).code, 'OUTSIDE_WORKSPACE_REASON_REQUIRED');
  assert.equal(parsed(await f.run(full, 'filesystem.read', { path: outside, reason: 'Read the requested sibling note' })).content, 'outside');
  assert.equal((await f.run(smart, 'filesystem.read', { path: outside, reason: 'Requested sibling' }, { interactive: false })).code, 'TOOL_APPROVAL_REQUIRED');
  const standalone = await f.context('full', f.standaloneId);
  assert.equal((await f.run(standalone, 'filesystem.read', { path: 'invented.txt' })).isError, true);
  assert.ok(standalone.isolatedWorkspace && standalone.workspaceRoot);
});

test('authorized links bind scope, hardlinks stay readonly, and binary/oversized text stays bounded', async t => {
  const f = await toolFixture(t), ctx = await f.context('full'), outside = join(f.root, 'outside');
  await mkdir(outside); await writeFile(join(outside, 'private.txt'), 'private');
  await symlink(outside, join(f.workspace, 'linked'), process.platform === 'win32' ? 'junction' : 'dir');
  assert.equal((await f.run(ctx, 'filesystem.read', { path: 'linked/private.txt' })).code, 'OUTSIDE_WORKSPACE_REASON_REQUIRED');
  assert.equal((await f.run(ctx, 'filesystem.write', { path: 'linked/new.txt', content: 'bad', expectedHash: null })).code, 'OUTSIDE_WORKSPACE_REASON_REQUIRED');
  assert.equal(parsed(await f.run(ctx, 'filesystem.read', { path: 'linked/private.txt', reason: 'Read the explicitly selected linked sibling.' })).content, 'private');
  await link(join(outside, 'private.txt'), join(f.workspace, 'hardlinked.txt'));
  const hardlinked = parsed(await f.run(ctx, 'filesystem.read', { path: 'hardlinked.txt' }));
  assert.equal(hardlinked.content, 'private');
  assert.equal((await f.run(ctx, 'filesystem.write', { path: 'hardlinked.txt', content: 'bad', expectedHash: hardlinked.sha256 })).code, 'UNSAFE_TOOL_PATH');
  if (process.platform === 'win32') for (const path of ['D:ambiguous.txt', 'NUL.txt', 'note.txt:stream', 'folder.'])
    assert.equal((await f.run(ctx, 'filesystem.read', { path })).code, 'UNSAFE_TOOL_PATH');
  await writeFile(join(f.workspace, 'binary.dat'), Buffer.from([0xff, 0x00]));
  assert.equal((await f.run(ctx, 'filesystem.read', { path: 'binary.dat' })).code, 'NON_TEXT_FILE');
  await writeFile(join(f.workspace, 'huge.txt'), 'a'.repeat(1024 * 1024 + 1));
  assert.equal((await f.run(ctx, 'filesystem.read', { path: 'huge.txt' })).code, 'TOOL_FILE_TOO_LARGE');
  await writeFile(join(f.workspace, 'escaped.txt'), '\t'.repeat(64000));
  const bounded = await f.run(ctx, 'filesystem.read', { path: 'escaped.txt', maxChars: 64000 });
  assert.equal(bounded.isError, false); assert.ok(bounded.content.length <= 65536); assert.match(bounded.content, /truncated/);
  assert.equal((await f.run(ctx, 'filesystem.write', { path: 'ill-formed.txt', content: '\ud800', expectedHash: null })).isError, true);
  const search = parsed(await f.run(ctx, 'filesystem.search', { query: 'private' }));
  assert.deepEqual(search.matches.map(match => match.path), ['hardlinked.txt']); assert.ok(search.skipped >= 3);
});

test('search is bounded, skips nested formal Data and credential files, while Full can explicitly read other Data text', async t => {
  const f = await toolFixture(t, { nestedData: true }), full = await f.context('full'), ask = await f.context('ask');
  await writeFile(join(f.workspace, 'note.txt'), 'needle public');
  const dataText = join(f.conversations.root, 'notes.txt'); await writeFile(dataText, 'needle app-data');
  const key = join(f.dataHome, 'connections.json'), backup = join(f.dataHome, 'connections.json.backup');
  await mkdir(f.dataHome, { recursive: true }); await writeFile(key, 'needle synthetic-key'); await writeFile(backup, 'needle synthetic-backup-key');
  const backupRoot = join(f.conversations.root, 'Backups', 'Models'); await mkdir(backupRoot, { recursive: true });
  const oldKey = join(backupRoot, 'connections.json'); await writeFile(oldKey, 'needle synthetic-old-key');
  const scoped = parsed(await f.run(full, 'filesystem.search', { query: 'needle' }));
  assert.deepEqual(scoped.matches.map(value => value.path), ['note.txt']);
  assert.equal((await f.run(full, 'filesystem.read', { path: dataText })).code, 'OUTSIDE_WORKSPACE_REASON_REQUIRED');
  assert.equal(parsed(await f.run(full, 'filesystem.read', { path: dataText, reason: 'Inspect requested application note' })).content, 'needle app-data');
  assert.equal((await f.run(ask, 'filesystem.read', { path: dataText, reason: 'Inspect app note' }, { interactive: false })).code, 'TOOL_APPROVAL_REQUIRED');
  for (const path of [key, backup, oldKey]) for (const name of ['filesystem.read', 'filesystem.stat'])
    assert.equal((await f.run(full, name, { path, reason: 'Inspect' })).code, 'PROTECTED_MODEL_CREDENTIALS');
  const dataSearch = parsed(await f.run(full, 'filesystem.search', { path: f.conversations.root, query: 'needle', reason: 'Inspect requested application text' }));
  assert.deepEqual(dataSearch.matches.map(value => value.path), ['notes.txt']);
  for (const name of ['filesystem.write', 'filesystem.delete', 'filesystem.mkdir']) {
    const args = name === 'filesystem.write' ? { content: 'bad', expectedHash: null } : name === 'filesystem.delete' ? { expectedHash: null } : {};
    assert.equal((await f.run(full, name, { path: join(f.conversations.root, 'bad.txt'), reason: 'Requested', ...args })).code, 'PROTECTED_APP_DATA');
  }
  const list = parsed(await f.run(full, 'filesystem.list', { limit: 1 })); assert.equal(list.entries.length, 1); assert.equal(list.truncated, true);
});

test('pending tools stop on cancellation, expiration, workspace change and service shutdown', async t => {
  const f = await toolFixture(t, { approvalTimeoutMs: 30 }), ctx = await f.context('ask'), abort = new AbortController();
  const cancelled = await pendingApproval(f.service, ctx, f.call('filesystem.mkdir', { path: 'cancelled' }), { signal: abort.signal });
  abort.abort(); assert.equal((await cancelled.result).code, 'TOOL_CANCELLED');
  assert.throws(() => approve(f.service, ctx, cancelled.event.tool), { code: 'TOOL_APPROVAL_NOT_FOUND' });
  const expired = await pendingApproval(f.service, ctx, f.call('filesystem.mkdir', { path: 'expired' }));
  assert.equal((await expired.result).code, 'TOOL_APPROVAL_EXPIRED');
  f.service.approvals.timeoutMs = 5000;
  const changed = await pendingApproval(f.service, ctx, f.call('filesystem.mkdir', { path: 'changed' }));
  const catalog = await f.conversations.catalog(); catalog.Projects[0].IsFolderlessWorkspace = true; catalog.Projects[0].FolderPath = null;
  await f.conversations.saveCatalog(catalog); approve(f.service, ctx, changed.event.tool);
  assert.equal((await changed.result).code, 'WORKSPACE_CHANGED');
  const pending = await pendingApproval(f.service, ctx, f.call('filesystem.mkdir', { path: 'closed' }));
  await f.service.close(); assert.equal((await pending.result).code, 'TOOL_SERVICE_CLOSED');
  for (const name of ['cancelled', 'expired', 'changed', 'closed']) await assert.rejects(stat(join(f.workspace, name)), { code: 'ENOENT' });
});

test('custom legacy model homes keep connection writes owned by ModelStore and allow other explicit text reads', async t => {
  const f = await toolFixture(t, { legacyData: true }), full = await f.context('full');
  assert.equal(f.conversations.root, join(f.dataHome, 'Conversations'));
  const key = join(f.dataHome, 'connections.json'), note = join(f.dataHome, 'plain-note.txt');
  await writeFile(key, 'synthetic credential'); await writeFile(note, 'requested plain model-home note');
  assert.equal((await f.run(full, 'filesystem.read', { path: key, reason: 'Inspect model settings' })).code, 'PROTECTED_MODEL_CREDENTIALS');
  assert.equal((await f.run(full, 'filesystem.write', { path: key, content: 'overwrite', expectedHash: null, reason: 'Change settings' })).code, 'PROTECTED_APP_DATA');
  assert.equal((await f.run(full, 'filesystem.mkdir', { path: join(f.dataHome, 'new'), reason: 'Make folder' })).code, 'PROTECTED_APP_DATA');
  assert.equal(parsed(await f.run(full, 'filesystem.read', { path: note, reason: 'Inspect requested plain note' })).content, 'requested plain model-home note');
  assert.equal(await readFile(key, 'utf8'), 'synthetic credential');
});

test('default Desktop/Projects managed work uses normal scoped CRUD and passes trusted ownership to the sandbox', async t => {
  const terminal = [];
  const f = await toolFixture(t, { sandboxRunner: {
    capabilities: async () => ({ available: true, sandbox: 'appcontainer', failClosed: true, checksChildToken: true, commands: ['node', 'cmd'] }),
    run: async input => { terminal.push(input); return { sandbox: 'appcontainer', exitCode: 0, stdout: 'managed', workspaceCopy: true }; }
  } });
  const managed = join(f.conversations.root, 'Desktop', 'Projects', f.projectId.replaceAll('-', ''));
  await mkdir(managed, { recursive: true });
  const catalog = await f.conversations.catalog(); catalog.Projects[0].FolderPath = managed; await f.conversations.saveCatalog(catalog);
  const ctx = await f.context('smart'); assert.equal(ctx.managedWorkspace, true);
  parsed(await f.run(ctx, 'filesystem.mkdir', { path: 'files' }));
  const written = parsed(await f.run(ctx, 'filesystem.write', { path: 'files/note.txt', content: 'managed needle', expectedHash: null }));
  const read = await f.run(ctx, 'filesystem.read', { path: 'files/note.txt' }); assert.equal(read.outsideWorkspace, false); assert.equal(parsed(read).content, 'managed needle');
  const edited = parsed(await f.run(ctx, 'filesystem.edit', { path: 'files/note.txt', oldText: 'needle', newText: 'updated', expectedHash: written.sha256 }));
  assert.equal(parsed(await f.run(ctx, 'filesystem.search', { query: 'updated' })).matches.length, 1);
  assert.equal(parsed(await f.run(ctx, 'filesystem.list', { path: 'files' })).entries[0].name, 'note.txt');
  assert.equal((await f.run(ctx, 'terminal.run', { command: 'node', args: ['-e', 'console.log(1)'] }, { interactive: false })).isError, false);
  assert.equal(terminal[0].trustedManagedWorkspace, true); assert.equal(terminal[0].workspaceRoot, await realpath(managed));
  const rename = await f.conversations.catalog(); rename.Projects[0].Name = 'Renamed managed work'; await f.conversations.saveCatalog(rename);
  assert.equal(parsed(await f.run(ctx, 'filesystem.stat', { path: 'files/note.txt' })).sha256, edited.sha256);
  const deletion = await pendingApproval(f.service, ctx, f.call('filesystem.delete', { path: 'files/note.txt', expectedHash: edited.sha256 }));
  approve(f.service, ctx, deletion.event.tool); parsed(await deletion.result);
  await assert.rejects(stat(join(managed, 'files', 'note.txt')), { code: 'ENOENT' });
});

test('fake linked formal data, its ancestors and a different managed ID cannot gain file writes or terminal access', async t => {
  const f = await toolFixture(t, { sandboxRunner: {
    capabilities: async () => ({ available: true, sandbox: 'appcontainer', failClosed: true, checksChildToken: true, commands: ['node'] }),
    run: async () => assert.fail('formal data is never sent to the sandbox')
  } });
  const other = join(f.conversations.root, 'Desktop', 'Projects', randomUUID().replaceAll('-', ''));
  await mkdir(other, { recursive: true });
  for (const path of [f.conversations.root, join(f.conversations.root, 'Chats'), join(f.conversations.root, 'Agent'),
    join(f.conversations.root, 'Desktop', 'Projects'), other]) {
    const catalog = await f.conversations.catalog(); catalog.Projects[0].FolderPath = path; await f.conversations.saveCatalog(catalog);
    const ctx = await f.context('full'); assert.equal(ctx.managedWorkspace, false);
    assert.equal((await f.run(ctx, 'filesystem.write', { path: 'bad.txt', content: 'bad', expectedHash: null })).code, 'PROTECTED_APP_DATA');
    assert.equal((await f.run(ctx, 'terminal.run', { command: 'node', args: [] })).code, 'PROTECTED_APP_DATA');
    await assert.rejects(stat(join(path, 'bad.txt')), { code: 'ENOENT' });
  }
});

test('managed scope is rechecked after relinking or moving a chat, including an already pending approval', async t => {
  const f = await toolFixture(t), managed = join(f.conversations.root, 'Desktop', 'Projects', f.projectId);
  await mkdir(managed, { recursive: true });
  let catalog = await f.conversations.catalog(); catalog.Projects[0].FolderPath = managed; await f.conversations.saveCatalog(catalog);
  const old = await f.context('ask'); assert.equal(old.managedWorkspace, true, 'historical dashed managed folder matches the same ID');
  const pending = await pendingApproval(f.service, old, f.call('filesystem.write', { path: 'pending.txt', content: 'old scope', expectedHash: null }));
  const newId = randomUUID(); catalog = await f.conversations.catalog();
  const movedChat = catalog.Projects[0].Chats.pop(); catalog.Projects.push({ Id: newId, Name: 'Other synthetic work', FolderPath: managed, Chats: [movedChat] });
  await f.conversations.saveCatalog(catalog); approve(f.service, old, pending.event.tool);
  assert.equal((await pending.result).code, 'WORKSPACE_CHANGED'); await assert.rejects(stat(join(managed, 'pending.txt')), { code: 'ENOENT' });
  const moved = await f.context('smart'); assert.equal(moved.managedWorkspace, false);
  assert.equal((await f.run(moved, 'filesystem.write', { path: 'wrong-owner.txt', content: 'bad', expectedHash: null })).code, 'PROTECTED_APP_DATA');
  const correct = join(f.conversations.root, 'Desktop', 'Projects', newId.replaceAll('-', '')); await mkdir(correct);
  catalog = await f.conversations.catalog(); catalog.Projects.find(value => value.Id === newId).FolderPath = correct; await f.conversations.saveCatalog(catalog);
  assert.equal((await f.run(moved, 'filesystem.read', { path: join(managed, 'pending.txt'), reason: 'Old requested file' })).isError, true);
  const current = await f.context('smart'); assert.equal(current.managedWorkspace, true);
  parsed(await f.run(current, 'filesystem.write', { path: 'correct.txt', content: 'new scope', expectedHash: null }));
  assert.equal(await readFile(join(correct, 'correct.txt'), 'utf8'), 'new scope');
});

test('application skills expose metadata first, read on demand, isolate work scope and never discover development skills', async t => {
  const f = await toolFixture(t), ctx = await f.context('ask');
  const save = async (root, name, description, body) => { await mkdir(root, { recursive: true }); await writeFile(join(root, 'SKILL.md'), `---\nname: ${name}\ndescription: ${description}\n---\n${body}`); };
  await save(join(f.conversations.root, 'Skills', 'global'), 'global-app', '>\n  Global application\n  description', 'GLOBAL BODY');
  await save(join(f.workspace, '.kynxa', 'skills', 'work'), 'work-app', 'Work application description', 'WORK BODY');
  await save(join(f.workspace, '.agents', 'skills', 'development'), 'development', 'Must never load', 'DEVELOPMENT BODY');
  const global = await f.service.listSkills(); assert.deepEqual(global.map(value => value.name), ['global-app']);
  assert.equal(Object.hasOwn(global[0], 'content'), false);
  const scoped = await f.service.listSkills(ctx); assert.equal(scoped.length, 2);
  const work = scoped.find(value => value.name === 'work-app');
  assert.match((await f.service.readSkill(work.id, ctx)).content, /WORK BODY/);
  await assert.rejects(f.service.readSkill(work.id), { code: 'APP_SKILL_NOT_FOUND' });
  const prompt = await f.service.systemPrompt(ctx); assert.match(prompt, /work-app/); assert.doesNotMatch(prompt, /WORK BODY|GLOBAL BODY|DEVELOPMENT BODY/);
  assert.equal(parsed(await f.run(ctx, 'skill.read', { id: work.id })).name, 'work-app');
});

test('skill discovery isolates corrupt files, limits model headers and returns valid bounded pages', async t => {
  const f = await toolFixture(t), ctx = await f.context('smart'), skillRoot = join(f.conversations.root, 'Skills');
  for (let index = 0; index < 40; index++) {
    const folder = join(skillRoot, `good-${index}`); await mkdir(folder);
    await writeFile(join(folder, 'SKILL.md'), `---\nname: application-${index}\ndescription: ${'d'.repeat(2000)}\n---\nPRIVATE BODY ${index}`);
  }
  const corruptRoot = join(skillRoot, 'corrupt'), oversizedRoot = join(skillRoot, 'oversized');
  await mkdir(corruptRoot); await mkdir(oversizedRoot);
  const corrupt = join(corruptRoot, 'SKILL.md'), oversized = join(oversizedRoot, 'SKILL.md');
  await writeFile(corrupt, 'invalid metadata original'); await writeFile(oversized, 'x'.repeat(300000));
  const skills = await f.service.listSkills(ctx); assert.equal(skills.length, 42); assert.equal(skills.filter(value => value.status === 'unavailable').length, 2);
  const prompt = await f.service.systemPrompt(ctx);
  assert.equal((prompt.match(/Application skill [a-f0-9]{24}:/g) ?? []).length, 12);
  assert.ok(estimateTokens(prompt) < 2500, `Tool system prompt exceeds its budget: ${estimateTokens(prompt)}`);
  assert.doesNotMatch(prompt, /PRIVATE BODY/); assert.match(prompt, /Some application skills are unavailable/);
  const first = parsed(await f.run(ctx, 'skill.list', { limit: 128 })); assert.ok(first.skills.length < 42); assert.equal(first.hasMore, true);
  const next = parsed(await f.run(ctx, 'skill.list', { offset: first.nextOffset, limit: 128 })); assert.equal(next.hasMore, false);
  assert.equal(first.skills.length + next.skills.length, 42); assert.equal(first.discovery.unavailableCount, 2);
  assert.equal((await f.run(ctx, 'skill.read', { id: skills.find(value => value.status === 'unavailable').id })).isError, true);
  assert.equal(await readFile(corrupt, 'utf8'), 'invalid metadata original'); assert.equal((await stat(oversized)).size, 300000);
  parsed(await f.run(ctx, 'filesystem.write', { path: 'healthy.txt', content: 'still works', expectedHash: null }));
});

test('huge or broken configured skill directories cannot block normal tools; bundled skills are readonly', async t => {
  const f = await toolFixture(t), ctx = await f.context('smart'), directory = join(f.root, 'many-empty-skill-folders');
  await mkdir(directory);
  await Promise.all(Array.from({ length: 530 }, (_, index) => mkdir(join(directory, `candidate-${index}`))));
  await f.service.updateConfig({ version: 1, expectedRevision: 0, mcpServers: [], skillDirectories: [directory] });
  const skills = await f.service.listSkills(ctx), status = f.service.skills.discovery.get(skills);
  assert.equal(status.truncated, true); assert.equal(status.maxCandidatesPerDirectory, 512); assert.deepEqual(skills, []);
  await rename(directory, `${directory}-original`); await writeFile(directory, 'became a file after configuration');
  assert.match(await f.service.systemPrompt(ctx), /Some application skills are unavailable/);
  parsed(await f.run(ctx, 'filesystem.mkdir', { path: 'healthy' }));
  const bundle = join(f.root, 'synthetic-readonly-bundle'); await mkdir(bundle); const bundledFile = join(bundle, 'SKILL.md');
  await writeFile(bundledFile, '---\nname: bundled-app\ndescription: Bundled application guidance\n---\nBUNDLE BODY');
  const service = new ToolService({ conversationStore: f.conversations, dataHome: f.dataHome, bundledDirectory: bundle });
  try {
    const bundledContext = await service.createContext(f.conversationId, { requestId: randomUUID(), permissionMode: 'full' });
    const available = await service.listSkills(bundledContext); assert.ok(available.some(value => value.name === 'bundled-app'));
    assert.equal((await service.execute(bundledContext, f.call('filesystem.write', { path: bundledFile, content: 'bad', expectedHash: null, reason: 'Requested' }))).code, 'PROTECTED_APP_DATA');
    assert.match(await readFile(bundledFile, 'utf8'), /BUNDLE BODY/);
  } finally { await service.close(); }
});

test('Smart terminal uses verified AppContainer runner only, with no unsupported host fallback', async t => {
  const calls = [], cleanup = [], sandboxRunner = {
    capabilities: async () => ({ available: true, sandbox: 'appcontainer', failClosed: true, checksChildToken: true, commands: ['node', 'cmd'] }),
    run: async (input, signal) => { calls.push({ input, signal }); return { exitCode: 0, stdout: 'ok', stderr: '', sandbox: 'appcontainer', workspaceCopy: true, stagingDirectory: 'synthetic-owned-stage' }; },
    cleanup: async path => { cleanup.push(path); }
  };
  const f = await toolFixture(t, { sandboxRunner }), smart = await f.context('smart'), abort = new AbortController();
  const result = await f.run(smart, 'terminal.run', { command: 'node', args: ['-e', 'console.log(1)'] }, { signal: abort.signal, interactive: false });
  assert.equal(result.sandbox, 'appcontainer'); assert.equal(result.isError, false);
  assert.deepEqual(calls[0].input, { workspaceRoot: await realpath(f.workspace), command: 'node', args: ['-e', 'console.log(1)'], timeoutMs: 30000, trustedManagedWorkspace: false });
  assert.equal(calls[0].signal, abort.signal);
  assert.equal((await f.run(smart, 'terminal.run', { command: 'powershell', args: [] })).isError, true); assert.equal(calls.length, 1);
  assert.equal((await f.run(smart, 'terminal.run', { command: 'cmd', args: ['/c', 'echo unbounded'] })).isError, true); assert.equal(calls.length, 1);
  assert.equal((await f.run(smart, 'terminal.run', { command: 'cmd.exe', args: ['/d', '/c', 'echo bounded'] })).isError, false); assert.equal(calls.length, 2);
  const standalone = await f.context('full', f.standaloneId);
  assert.equal((await f.run(standalone, 'terminal.run', { command: 'node', args: [] })).isError, false);
  assert.equal(calls.at(-1).input.trustedManagedWorkspace, true);
  assert.equal(calls.at(-1).input.workspaceRoot, standalone.workspaceRoot);
  const unavailable = await toolFixture(t, { sandboxRunner: { capabilities: async () => ({ available: true, sandbox: 'host' }), run: async () => assert.fail('no host execution') } });
  assert.equal((await unavailable.run(await unavailable.context('full'), 'terminal.run', { command: 'node', args: [] })).code, 'SANDBOX_UNAVAILABLE');
  assert.match(await f.service.systemPrompt(smart), /--test-isolation=none/);
  await f.service.releaseContext(smart); assert.deepEqual(cleanup, ['synthetic-owned-stage']);
  assert.equal((await f.run(smart, 'terminal.run', { command: 'node', args: [] })).code, 'INVALID_TOOL_CONTEXT');
});

test('agent configuration has server revisions, restart persistence, concurrent conflicts and preserves unsupported data', async t => {
  const f = await toolFixture(t), initial = await f.service.getConfig();
  assert.deepEqual(initial, { version: 1, revision: 0, mcpServers: [], skillDirectories: [], disabledSkills: [] });
  await assert.rejects(stat(join(f.conversations.root, 'Agent', 'config.json')), { code: 'ENOENT' });
  const update = { version: 1, expectedRevision: 0, mcpServers: [{ id: 'test', name: 'Synthetic server', command: process.execPath, args: [], enabled: false }], skillDirectories: [] };
  const results = await Promise.allSettled([f.service.updateConfig(update), f.service.updateConfig(update)]);
  assert.equal(results.filter(value => value.status === 'fulfilled').length, 1);
  assert.equal(results.find(value => value.status === 'rejected').reason.code, 'AGENT_CONFIG_CONFLICT');
  assert.equal((await f.service.getConfig()).revision, 1);
  assert.equal((await new AgentConfigRepository(f.conversations.root).read()).revision, 1);
  const file = join(f.conversations.root, 'Agent', 'config.json');
  await writeFile(file, '{"version":99,"unknown":"preserve"}');
  await assert.rejects(f.service.getConfig(), { code: 'UNSUPPORTED_AGENT_CONFIG' });
  await assert.rejects(f.service.updateConfig({ ...update, expectedRevision: 1 }), { code: 'UNSUPPORTED_AGENT_CONFIG' });
  assert.equal(await readFile(file, 'utf8'), '{"version":99,"unknown":"preserve"}');
});

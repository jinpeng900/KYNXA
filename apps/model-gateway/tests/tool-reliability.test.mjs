import assert from 'node:assert/strict';
import { test } from 'node:test';
import { writeFile, readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { runToolLoop } from '../orchestration/tool-loop.mjs';
import { toolFixture } from './tool-fixture.mjs';

test('a completed real file write is committed in formal tool history before stop is honored', async t => {
  const f = await toolFixture(t), context = await f.context('full'), stop = new AbortController();
  const call = f.call('filesystem.write', { path: 'committed.txt', content: 'confirmed outcome', expectedHash: null });
  const execute = f.service.execute.bind(f.service);
  f.service.execute = async (...args) => { const result = await execute(...args); stop.abort(); return result; };
  const activities = [];
  let requests = 0;
  await assert.rejects(runToolLoop({ context, service: f.service, protocol: 'openai-completions', messages: [], system: '', declarations: [],
    inputBudgetTokens: 16000, signal: stop.signal, emit: () => {},
    requestTurn: async () => { requests++; return { content: '', reasoning: '', calls: [call] }; },
    saveActivity: async activity => {
      activities.push(activity);
      await f.conversations.upsertMessage(context.conversationId, { Id: context.requestId, Role: 'assistant', Content: '',
        Status: 'streaming', ToolActivities: [activity], CreatedAt: new Date().toISOString() });
    } }), { name: 'AbortError' });
  assert.equal(await readFile(join(f.workspace, 'committed.txt'), 'utf8'), 'confirmed outcome');
  assert.deepEqual(activities.map(item => item.status), ['running', 'completed']);
  assert.equal(requests, 1);
  const saved = (await f.conversations.readMessages(context.conversationId)).find(item => item.Id === context.requestId);
  assert.equal(saved.ToolActivities[0].status, 'completed');
  assert.ok(saved.ToolActivities[0].resultRef);
  assert.equal((await f.service.results.get(context, saved.ToolActivities[0].resultRef.id)).isError, false);
});

test('escaped long file results keep valid previews, restore complete content and protect raw result files', async t => {
  const f = await toolFixture(t), context = await f.context('full');
  const original = '\t'.repeat(64000); await writeFile(join(f.workspace, 'long.txt'), original);
  const result = await f.run(context, 'filesystem.read', { path: 'long.txt', maxChars: 64000 });
  assert.equal(result.isError, false); assert.ok(result.content.length <= 65536);
  const preview = JSON.parse(result.content); assert.equal(preview.truncated, true); assert.ok(result.resultRef);
  const full = await f.service.results.get(context, result.resultRef.id);
  assert.ok(full.structuredContent.content === original, '完整结果保留读取到的所有字符');
  let directory;
  await f.conversations.withConversationStorage(context.conversationId, value => { directory = value.sessionDirectory; });
  const raw = await f.run(context, 'filesystem.read', { path: join(directory, 'tool-results', result.resultRef.id + '.json'), reason: 'Inspect result' });
  assert.equal(raw.code, 'PROTECTED_TOOL_RESULT');
  const publicPage = await f.run(context, 'tool.result.read', { id: result.resultRef.id, offset: 0, limit: 16000 });
  assert.equal(JSON.parse(publicPage.content).text.length, 16000);
});

test('receipt storage failure does not turn a completed write into a cancelled execution', async t => {
  const f = await toolFixture(t), context = await f.context('full'), stop = new AbortController();
  f.service.results.save = async () => { stop.abort(); throw Object.assign(new Error('Private fixture storage error'), { code: 'EACCES' }); };
  const result = await f.run(context, 'filesystem.write', { path: 'saved.txt', content: 'already done', expectedHash: null }, { signal: stop.signal });
  assert.equal(await readFile(join(f.workspace, 'saved.txt'), 'utf8'), 'already done');
  assert.equal(result.isError, false); assert.equal(result.status, 'completed');
  assert.equal(result.code, 'TOOL_RESULT_SAVE_FAILED'); assert.equal(result.resultRef, undefined);
  const preview = JSON.parse(result.content); assert.equal(preview.storageError.saved, false);
  assert.ok(!result.content.includes('Private fixture'));
});

test('late terminal cancellation preserves the verified native outcome and rejects an unverified receipt', async t => {
  const native = { protocolVersion: 1, sandbox: 'appcontainer', tokenVerified: true, workspaceCopy: true,
    activeProcessesAfterExit: 0, cancelled: false, timedOut: false, exitCode: 0, stdout: 'finished', stderr: '' };
  let returned = native;
  const runner = { capabilities: async () => ({ available: true, sandbox: 'appcontainer', failClosed: true,
    checksChildToken: true, commands: ['node'] }), run: async () => {
    throw Object.assign(new Error('Native cancellation'), { name: 'AbortError', sandboxResult: returned });
  } };
  const f = await toolFixture(t, { sandboxRunner: runner }), context = await f.context('smart');
  const completed = await f.run(context, 'terminal.run', { command: 'node', args: ['-e', '0'] });
  assert.equal(completed.status, 'completed'); assert.equal(completed.isError, false);
  assert.ok(completed.resultRef);
  assert.equal((await f.service.results.get(context, completed.resultRef.id)).structuredContent.stdout, 'finished');
  returned = { ...native, cancelled: true, exitCode: -1, stdout: 'partial' };
  const cancelled = await f.run(context, 'terminal.run', { command: 'node', args: ['-e', '0'] });
  assert.equal(cancelled.status, 'cancelled'); assert.equal(cancelled.code, 'TOOL_CANCELLED');
  assert.equal((await f.service.results.get(context, cancelled.resultRef.id)).structuredContent.stdout, 'partial');
  returned = { ...native, activeProcessesAfterExit: 1 };
  const invalid = await f.run(context, 'terminal.run', { command: 'node', args: ['-e', '0'] });
  assert.equal(invalid.isError, true); assert.equal(invalid.resultRef, undefined);
  assert.equal(invalid.status, 'unknown');
  returned = { ...native, cancelled: undefined };
  const missing = await f.run(context, 'terminal.run', { command: 'node', args: ['-e', '0'] });
  assert.equal(missing.status, 'unknown'); assert.equal(missing.resultRef, undefined);
});

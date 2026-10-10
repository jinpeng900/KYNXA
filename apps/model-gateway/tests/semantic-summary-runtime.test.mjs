import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { createServer } from 'node:http';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { test } from 'node:test';
import { ConversationStore } from '../data/conversations.mjs';
import { MemoryRepository } from '../data/memory-repository.mjs';
import { ModelHistoryProjection } from '../models/model-history.mjs';
import { createSemanticSummaryPlan, selectSemanticSummary } from '../models/semantic-summary.mjs';
import { appendModelRound, modelOrigin } from '../platform/model-transcript.mjs';
import { runSemanticSummary } from '../orchestration/semantic-summary-runner.mjs';
import { ModelRuntime } from '../orchestration/runtime.mjs';
import { ModelStore } from '../models/store.mjs';
import { toolFixture } from './tool-fixture.mjs';

const protocol = 'openai-completions';
function savedMessages(count) {
  return Array.from({ length: count }, (_, index) => {
    const user = { Id: randomUUID(), Role: 'user', Status: 'completed', Content: `Goal ${index}. ` + 'User constraints and exact original paths. '.repeat(40) };
    return [user, { Id: randomUUID(), Role: 'assistant', ReplyTo: user.Id, Status: 'completed',
      Content: `Progress ${index}. ` + 'Actual completed source details; future work still needs verification. '.repeat(40),
      Reasoning: 'SYNTHETIC_PRIVATE_REASONING' }];
  }).flat();
}
function rawSummary(messageId, finishReason = 'stop') {
  return { choices: [{ finish_reason: finishReason, message: { role: 'assistant', content: JSON.stringify({ entries: [
    { kind: 'goal', text: 'Continue the source task within the stated scope. Read the original for exact paths and requirements.', sourceMessageIds: [messageId] }
  ] }) } }], usage: { prompt_tokens: 100, completion_tokens: 80, total_tokens: 180, rawSecret: 'UNSAFE_USAGE_VALUE' } };
}
async function fixture(t, { tools = false } = {}) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-semantic-summary-'));
  t.after(async () => {
    const suffix = relative(resolve(tmpdir()), resolve(root));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`), 'cleanup stays within this owned temporary fixture');
    await rm(root, { recursive: true, force: true });
  });
  const conversations = new ConversationStore({ dataHome: join(root, 'Models'), legacyDesktopDirectory: null }), conversationId = randomUUID();
  const messages = savedMessages(4);
  if (tools) {
    const assistant = messages[1], call = { id: 'saved-call', name: 'filesystem.read', arguments: { path: 'source.txt' } };
    assistant.ToolActivities = [{ round: 1, toolCallId: call.id, name: call.name, arguments: call.arguments,
      status: 'completed', result: 'Recorded public observation. '.repeat(50) }];
    assistant.ModelTranscript = appendModelRound(assistant, modelOrigin({ protocol, baseUrl: 'http://127.0.0.1:1', apiKey: 'SYNTHETIC' },
      { providerId: 'fixture', model: 'mock' }), { round: 1, text: 'Public stage exists only in the formal model transcript.', calls: [call] });
  }
  await conversations.ensureConversation(conversationId, { title: 'Synthetic source' });
  for (const message of messages) await conversations.upsertMessage(conversationId, message);
  const history = await conversations.readModelMessages(conversationId);
  const projectionOptions = { history, protocol, inputBudgetTokens: 16000, availableTools: [] };
  const sourceTurns = new ModelHistoryProjection(projectionOptions).summarySources();
  assert.equal(sourceTurns.length, 4, `saved model turn count from ${history.length} messages`);
  const planned = createSemanticSummaryPlan({ conversationId, sourceTurns, budgetTokens: 1024, inputBudgetTokens: 16000 });
  const { plan } = planned;
  assert.ok(plan, planned.reason);
  const directory = await conversations.resolveSessionDirectory(conversationId);
  return { root, conversationId, conversations, history, projectionOptions, plan, sourceTurns,
    repository: new MemoryRepository({ conversationStore: conversations }), cooldowns: new Map(),
    connection: { protocol }, journal: join(directory, 'events.jsonl'), summaryFile: join(directory, 'context.json') };
}
const run = (f, generate, extra = {}) => runSemanticSummary({ ...f, generate, ...extra });

test('one runner call saves a restart-safe schema3 checkpoint under the real source lock and leaves raw model/tool history byte-for-byte intact', async t => {
  const f = await fixture(t, { tools: true }), original = await readFile(f.journal);
  const result = await run(f, async (request, dispatched) => {
    dispatched(); assert.equal(request.maxOutputTokens, 1024);
    assert.match(request.messages[0].content, /Public stage exists only in the formal model transcript/);
    assert.doesNotMatch(request.messages[0].content, /SYNTHETIC_PRIVATE_REASONING|connectionFingerprint/);
    return rawSummary(f.plan.sourceMessageIds[0]);
  });
  assert.equal(result.audit.state, 'saved', JSON.stringify(result.audit));
  assert.equal(result.audit.modelCalls, 1);
  assert.equal(result.value.programState.toolReceipts[0].status, 'completed');
  assert.doesNotMatch(JSON.stringify(result.audit), /UNSAFE_USAGE_VALUE/);
  assert.deepEqual(await readFile(f.journal), original);
  const restarted = new MemoryRepository({ conversationStore: f.conversations }), saved = await restarted.readSummary(f.conversationId);
  assert.equal(saved.contentHash, result.value.contentHash);
  assert.ok(selectSemanticSummary({ summary: saved, conversationId: f.conversationId, sourceTurns: f.sourceTurns }).value);
  await assert.rejects(restarted.writeSummary(f.conversationId, saved), { code: 'SUMMARY_VALIDATION_REQUIRED' });
});

test('an edit during model generation is rejected by the locked fresh-source read and preserves the previous checkpoint', async t => {
  const f = await fixture(t);
  const first = await run(f, async (_, dispatched) => { dispatched(); return rawSummary(f.plan.sourceMessageIds[0]); });
  assert.equal(first.audit.state, 'saved');
  const originalSummary = await readFile(f.summaryFile);
  const result = await run(f, async (_, dispatched) => {
    dispatched();
    await f.conversations.upsertMessage(f.conversationId, { ...f.history[0], Content: f.history[0].Content + ' A new explicit correction.' });
    return rawSummary(f.plan.sourceMessageIds[0]);
  });
  assert.equal(result.audit.state, 'fallback');
  assert.equal(result.audit.reason, 'SUMMARY_SOURCE_CHANGED');
  assert.deepEqual(await readFile(f.summaryFile), originalSummary);
  assert.match((await f.conversations.readMessages(f.conversationId))[0].Content, /new explicit correction/);
});

test('truncated generation never saves a partial checkpoint; failure cooldown makes the next request perform no summary call', async t => {
  const f = await fixture(t), originalJournal = await readFile(f.journal);
  let calls = 0;
  const generate = async (_, dispatched) => { calls++; dispatched(); return { ...rawSummary(f.plan.sourceMessageIds[0], 'length'),
    usage: { prompt_tokens: 3000, completion_tokens: 500 } }; };
  const first = await run(f, generate, { now: 1000 });
  assert.equal(first.audit.state, 'fallback'); assert.equal(first.audit.reason, 'incomplete-generation');
  assert.equal(first.audit.modelCalls, 1); assert.equal(calls, 1);
  assert.equal(first.audit.usageKnown, true);
  assert.deepEqual(first.audit.usage, { prompt_tokens: 3000, completion_tokens: 500 });
  assert.equal(await f.repository.readSummary(f.conversationId), null);
  const second = await run(f, generate, { now: 1001 });
  assert.equal(second.audit.state, 'deferred'); assert.equal(second.audit.modelCalls, 0); assert.equal(calls, 1);
  assert.deepEqual(await readFile(f.journal), originalJournal);
});

test('cancellation after a complete provider response prevents a new checkpoint from being written', async t => {
  const f = await fixture(t), controller = new AbortController();
  await assert.rejects(run(f, async (_, dispatched) => {
    dispatched(); controller.abort(new Error('Synthetic cancelled summary'));
    return rawSummary(f.plan.sourceMessageIds[0]);
  }, { signal: controller.signal }), /Synthetic cancelled summary/);
  assert.equal(await f.repository.readSummary(f.conversationId), null);
});

test('cancellation after locked source validation prevents the checkpoint commit and preserves original history', async t => {
  const f = await fixture(t), controller = new AbortController(), originalJournal = await readFile(f.journal);
  const writeSummary = f.repository.writeSummary.bind(f.repository), inspectPath = f.repository._safe.bind(f.repository);
  let shouldCancelBeforeWrite = false, cancelledAfterValidation = false;
  f.repository.writeSummary = (conversationId, summary, options) => writeSummary(conversationId, summary, {
    ...options, validateCurrent: async history => {
      const current = await options.validateCurrent(history);
      shouldCancelBeforeWrite = current;
      return current;
    }
  });
  // Abort at the first filesystem operation after source validation, before the atomic rename commits.
  // 在来源核验后的首次文件操作处取消，触发原子 rename 正式提交之前的取消窗口。
  f.repository._safe = async (...args) => {
    if (shouldCancelBeforeWrite) {
      shouldCancelBeforeWrite = false; cancelledAfterValidation = true;
      controller.abort(new Error('Synthetic cancellation after source validation'));
    }
    return inspectPath(...args);
  };
  await assert.rejects(run(f, async (_, dispatched) => {
    dispatched(); return rawSummary(f.plan.sourceMessageIds[0]);
  }, { signal: controller.signal }), /Synthetic cancellation after source validation/);
  assert.equal(cancelledAfterValidation, true, 'the cancellation window must actually be exercised');
  assert.equal(await f.repository.readSummary(f.conversationId), null);
  assert.deepEqual(await readFile(f.journal), originalJournal);
});

test('actual runtime uses one normal call for a short chat and one optional mock-model call under pressure, then reuses the cached prefix', async t => {
  const f = await toolFixture(t), requests = [], summaryRequests = [];
  const upstream = createServer(async (request, response) => {
    let body = ''; for await (const part of request) body += part;
    const input = JSON.parse(body); requests.push(input);
    const isSummary = input.messages.some(message => message.role === 'system' && message.content.includes('Create compact source-cited navigation'));
    response.setHeader('Content-Type', 'application/json');
    if (isSummary) {
      summaryRequests.push(input);
      const source = JSON.parse(input.messages.find(message => message.role === 'user').content);
      response.end(JSON.stringify(rawSummary(source.allowedSourceMessageIds[0])));
    } else response.end(JSON.stringify({ choices: [{ finish_reason: 'stop', message: { role: 'assistant', content: 'A short source answer.' } }] }));
  });
  await new Promise(resolveListen => upstream.listen(0, '127.0.0.1', resolveListen));
  t.after(() => new Promise(resolveClose => { upstream.closeAllConnections(); upstream.close(resolveClose); }));
  const store = new ModelStore({ dataHome: f.dataHome });
  await store.save({ providerId: 'fixture', displayName: 'Fixture', baseUrl: `http://127.0.0.1:${upstream.address().port}/v1`,
    models: ['mock-model'], contextWindowTokens: 8192, maxOutputTokens: 1024 });
  const runtime = new ModelRuntime({ modelStore: store, conversationStore: f.conversations, toolService: f.service,
    dataHome: f.dataHome, resourceService: {} });
  runtime.retrieval.onMemoryChanged = async () => {};
  await runtime.retrieval.settings.patchGlobal({ expectedRevision: 0, patch: { local: { semantic: 'off' } } });
  t.after(() => runtime.close());
  const input = { provider: 'fixture', model: 'mock-model', conversationId: f.standaloneId, message: '你好' };
  assert.equal(await runtime.reply(input), 'A short source answer.');
  assert.equal(requests.length, 1); assert.equal(summaryRequests.length, 0);
  for (const message of savedMessages(12)) await f.conversations.upsertMessage(f.conversationId, message);
  const requestId = randomUUID();
  assert.equal(await runtime.reply({ ...input, conversationId: f.conversationId, requestId, message: 'Continue the source task.' }), 'A short source answer.');
  assert.equal(requests.length, 3); assert.equal(summaryRequests.length, 1);
  const saved = await runtime.memory.repository.readSummary(f.conversationId);
  assert.equal(saved.algorithm, 'model-semantic-v1');
  const assistant = (await f.conversations.readMessages(f.conversationId)).find(message => message.Id === requestId);
  assert.equal(assistant.ContextAssembly.semanticSummary.state, 'saved');
  assert.equal(assistant.ContextAssembly.semanticSummary.modelCalls, 1);
  assert.equal(summaryRequests[0].tools, undefined);
  assert.equal(await runtime.reply({ ...input, conversationId: f.conversationId, message: 'Continue.' }), 'A short source answer.');
  assert.equal(requests.length, 4); assert.equal(summaryRequests.length, 1);
});

test('cancelling sendStream while the real summary HTTP request is pending aborts its fetch, saves no checkpoint and dispatches no answer', { timeout: 10000 }, async t => {
  const f = await toolFixture(t), requests = [], events = [];
  let notifySummaryStarted, notifySummaryClosed;
  const summaryStarted = new Promise(resolveStarted => { notifySummaryStarted = resolveStarted; });
  const summaryClosed = new Promise(resolveClosed => { notifySummaryClosed = resolveClosed; });
  const upstream = createServer(async (request, response) => {
    let body = ''; for await (const part of request) body += part;
    const input = JSON.parse(body); requests.push(input);
    const isSummary = input.messages.some(message => message.role === 'system' && message.content.includes('Create compact source-cited navigation'));
    if (!isSummary) {
      response.setHeader('Content-Type', 'application/json');
      response.end(JSON.stringify({ choices: [{ finish_reason: 'stop', message: { content: 'An unexpected final-answer dispatch.' } }] }));
      return;
    }
    response.on('close', () => notifySummaryClosed({ completed: response.writableEnded }));
    notifySummaryStarted();
    // Keep this mock response open until the client's fetch is cancelled; no timer completes the summary.
    // 模拟摘要响应保持挂起，直到客户端 fetch 取消，不用计时器完成摘要。
  });
  await new Promise(resolveListen => upstream.listen(0, '127.0.0.1', resolveListen));
  t.after(() => new Promise(resolveClose => { upstream.closeAllConnections(); upstream.close(resolveClose); }));
  const store = new ModelStore({ dataHome: f.dataHome });
  await store.save({ providerId: 'fixture', displayName: 'Fixture', baseUrl: `http://127.0.0.1:${upstream.address().port}/v1`,
    models: ['mock-model'], contextWindowTokens: 8192, maxOutputTokens: 1024 });
  const runtime = new ModelRuntime({ modelStore: store, conversationStore: f.conversations, toolService: f.service,
    dataHome: f.dataHome, resourceService: {}, timeoutMs: 30000 });
  runtime.retrieval.onMemoryChanged = async () => {};
  await runtime.retrieval.settings.patchGlobal({ expectedRevision: 0, patch: { local: { semantic: 'off' } } });
  t.after(() => runtime.close());
  for (const message of savedMessages(12)) await f.conversations.upsertMessage(f.conversationId, message);
  const controller = new AbortController(), requestId = randomUUID();
  const input = { provider: 'fixture', model: 'mock-model', conversationId: f.conversationId, requestId,
    userMessageId: randomUUID(), message: 'Continue the source task.' };
  const pending = runtime.sendStream(input, f.conversationId, event => events.push(event), controller.signal);
  const rejected = assert.rejects(pending, error => error.type === 'interrupted');
  await summaryStarted;
  assert.equal(requests.length, 1);
  controller.abort(new Error('Synthetic client cancelled preparation'));
  await rejected;
  const closed = await summaryClosed;
  assert.equal(closed.completed, false, 'the mock server observed an aborted connection instead of a completed response');
  assert.equal(requests.length, 1, 'no retry or normal-answer request followed cancellation');
  assert.equal(await runtime.memory.repository.readSummary(f.conversationId), null);
  const saved = (await f.conversations.readMessages(f.conversationId)).find(message => message.Id === requestId);
  assert.equal(saved.Status, 'interrupted');
  assert.equal(saved.ContextAssembly.semanticSummary.state, 'cancelled');
  assert.equal(saved.ContextAssembly.semanticSummary.modelCalls, 1);
  assert.equal(saved.Content, '');
  assert.equal(events.some(event => event.type === 'text_delta'), false);
});

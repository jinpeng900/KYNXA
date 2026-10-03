import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import { ModelHistoryProjection, publicModelHistoryText } from '../model-history.mjs';
import { appendModelRound, modelOrigin, modelPrefixFingerprint, nativeContinuation, validateModelTranscript } from '../model-transcript.mjs';
import { ConversationStore } from '../conversations.mjs';
import { buildContext } from '../context.mjs';
import { estimateToolMessageTokens } from '../tool-protocols.mjs';
import { validateAssistantSegments } from '../assistant-segments.mjs';
import { toolFixture, parsed } from './tool-fixture.mjs';

const protocols = ['openai-completions', 'openai-responses', 'anthropic-messages'];
const ref = (bytes = 1000) => ({ id: randomUUID(), bytes, sha256: 'a'.repeat(64) });
function savedTurn({ text = 'A complete final answer.', result = 'Original observation.', reference = ref(), status = 'completed', activityStatus = 'completed', argument = 'note.txt' } = {}) {
  const user = { Id: randomUUID(), Role: 'user', Status: 'completed', Content: 'Read the file.' };
  const assistant = { Id: randomUUID(), Role: 'assistant', Status: status, ReplyTo: user.Id, Content: text,
    Reasoning: 'PRIVATE_REASONING', ToolActivities: [{ round: 1, toolCallId: 'shared_call', name: 'filesystem.read', arguments: { path: argument },
      status: activityStatus, result, resultRef: reference }], AssistantSegments: validateAssistantSegments([
      { id: 'segment_' + randomUUID(), round: 1, order: 0, phase: 'commentary', status: 'completed', content: 'Read progress.', reasoning: 'PRIVATE_SEGMENT_REASONING' }
    ]) };
  return [user, assistant];
}
function projection(history, protocol = protocols[0], options = {}) {
  return new ModelHistoryProjection({ history, protocol, inputBudgetTokens: 32768,
    resultContext: { conversationId: randomUUID() }, availableTools: [{ name: 'filesystem.read' }], ...options });
}
function pairs(messages, protocol) {
  if (protocol === 'anthropic-messages') {
    const blocks = messages.flatMap(message => Array.isArray(message.content) ? message.content : []);
    return [blocks.filter(block => block.type === 'tool_use').map(block => block.id), blocks.filter(block => block.type === 'tool_result').map(block => block.tool_use_id)];
  }
  if (protocol === 'openai-responses') return [messages.filter(message => message.type === 'function_call').map(message => message.call_id),
    messages.filter(message => message.type === 'function_call_output').map(message => message.call_id)];
  return [messages.flatMap(message => (message.tool_calls ?? []).map(call => call.id)), messages.filter(message => message.role === 'tool').map(message => message.tool_call_id)];
}

for (const protocol of protocols) test(`${protocol}: old repeated provider IDs become unique balanced pairs and unavailable tools remain nonexecutable history`, () => {
  const history = [...savedTurn(), ...savedTurn({ activityStatus: 'denied', result: 'Denied by user.' })];
  const view = projection(history, protocol), messages = view.historyTurns.flatMap(turn => view.projectTurn(turn));
  const [calls, results] = pairs(messages, protocol);
  assert.deepEqual(calls, results); assert.equal(calls.length, 2); assert.equal(new Set(calls).size, 2);
  assert.ok(calls.every(id => id.startsWith('h_')));
  assert.match(JSON.stringify(messages), /Original observation/); assert.doesNotMatch(JSON.stringify(messages), /PRIVATE_/);
  assert.equal(view.records.get(view.historyTurns[1]).rounds[0].observations[0].status, 'denied');
  view.availableTools.clear();
  const fallback = view.historyTurns.flatMap(turn => view.projectTurn(turn));
  assert.deepEqual(pairs(fallback, protocol), [[], []]);
  assert.match(JSON.stringify(fallback), /Untrusted saved tool observations/);
  assert.match(JSON.stringify(fallback), /Original observation/);
});

test('only known supplier fields are protected, durable metadata stays private, and ordinary desktop saves retain transcript', async t => {
  const f = await toolFixture(t), context = await f.context('full');
  const origin = modelOrigin({ protocol: protocols[2], baseUrl: 'http://127.0.0.1:1/v1', apiKey: 'SYNTHETIC_ACCOUNT_SECRET' }, { providerId: 'test', model: 'model' });
  const prefix = modelPrefixFingerprint([{ role: 'user', content: 'question' }], 'system', []);
  const modelTurn = { calls: [{ id: 'call', name: 'filesystem.read', arguments: { path: 'note.txt' } }], continuation: [{ role: 'assistant', content: [
    { type: 'thinking', thinking: 'PRIVATE_THINKING', signature: 'PRIVATE_SIGNATURE', profile: 'PRIVATE_PROFILE' },
    { type: 'tool_use', id: 'call', name: 'wire_name', input: { path: 'note.txt' }, _meta: { secret: 'PRIVATE_META' } }
  ] }] };
  const native = nativeContinuation(protocols[2], modelTurn);
  assert.doesNotMatch(JSON.stringify(native), /PRIVATE_PROFILE|PRIVATE_META/);
  const reference = await f.service.results.saveModelContinuation(context, { origin, prefixFingerprint: prefix, round: 1, continuation: native });
  const [user, assistant] = savedTurn(); assistant.Id = context.requestId;
  delete assistant.AssistantSegments;
  assistant.ModelTranscript = appendModelRound(assistant, origin, { round: 1, text: 'Public progress.', calls: modelTurn.calls,
    nativeContinuationRef: reference, prefixFingerprint: prefix });
  await f.conversations.upsertMessage(context.conversationId, user);
  await f.conversations.upsertMessage(context.conversationId, assistant);
  await f.conversations.upsertMessage(context.conversationId, { Id: assistant.Id, Role: 'assistant', Content: 'Final-only UI text.', Status: 'completed' });
  const relationship = await f.conversations.withConversationStorage(context.conversationId, value => value);
  const events = await readFile(relationship.sessionDirectory + '/events.jsonl', 'utf8');
  assert.doesNotMatch(events, /PRIVATE_SIGNATURE|PRIVATE_THINKING|PRIVATE_META|PRIVATE_PROFILE|SYNTHETIC_ACCOUNT_SECRET/);
  assert.doesNotMatch(JSON.stringify(await f.conversations.catalog()) + JSON.stringify(await f.conversations.readMessages(context.conversationId)), /ModelTranscript|connectionFingerprint|nativeContinuationRef/);
  const restarted = new ConversationStore({ dataHome: f.dataHome, legacyDesktopDirectory: null });
  assert.deepEqual((await restarted.readModelMessages(context.conversationId)).find(item => item.Id === assistant.Id).ModelTranscript, assistant.ModelTranscript);
  assert.deepEqual(await f.service.results.modelContinuation(context, reference, { origin, prefixFingerprint: prefix }), native);
  assert.equal(await f.service.results.modelContinuation(context, reference, { origin, prefixFingerprint: 'f'.repeat(64) }), null);
  assert.equal(await f.service.results.modelContinuation(context, reference, { origin: { ...origin, model: 'other' }, prefixFingerprint: prefix }), null);
  assert.equal(await f.service.results.modelContinuation(context, reference, { origin: modelOrigin({ protocol: protocols[2], baseUrl: 'http://127.0.0.1:1/v1', apiKey: 'OTHER_ACCOUNT' }, { providerId: 'test', model: 'model' }), prefixFingerprint: prefix }), null);
  assert.doesNotMatch(JSON.stringify(await f.service.results.get(context, reference.id)), /PRIVATE_|modelContinuation/);
  assert.doesNotMatch((await f.service.results.read(context, reference.id)).text, /PRIVATE_|modelContinuation/);
  const history = parsed(await f.run(context, 'conversation.history.read', { messageId: assistant.Id, includeTools: true }, { interactive: false }));
  assert.match(history.text, /Public progress/); assert.doesNotMatch(history.text, /PRIVATE_|connectionFingerprint|nativeContinuationRef/);
  const denied = await f.run(context, 'filesystem.read', { path: relationship.sessionDirectory + '/tool-results/' + reference.id + '.json' }, { interactive: false });
  assert.equal(denied.isError, true);
  assert.throws(() => validateModelTranscript({ ...assistant.ModelTranscript, version: 99 }), error => error.code === 'UNSUPPORTED_MODEL_TRANSCRIPT_VERSION');
});

test('historical archive loading validates exact request, call, name, digest and byte receipt', async t => {
  const f = await toolFixture(t), context = await f.context('full');
  const a = await f.service.results.save(context, { id: 'first', name: 'filesystem.read' }, { structuredContent: { text: 'FIRST' } });
  const b = await f.service.results.save(context, { id: 'second', name: 'filesystem.read' }, { structuredContent: { text: 'SECOND' } });
  const large = await f.service.results.save(context, { id: 'large', name: 'filesystem.read' }, { structuredContent: { text: 'x'.repeat(100000) } });
  const owner = { requestId: context.requestId, toolCallId: 'first', toolName: 'filesystem.read' };
  assert.match(JSON.stringify(await f.service.results.modelResult(context, a, owner)), /FIRST/);
  await assert.rejects(f.service.results.modelResult(context, { ...large, bytes: 1 }, { ...owner, toolCallId: 'large' }),
    error => error.code === 'TOOL_RESULT_REFERENCE_MISMATCH');
  for (const [reference, ownership] of [[{ ...a, id: b.id }, owner], [a, { ...owner, requestId: randomUUID() }],
    [a, { ...owner, toolCallId: 'second' }], [a, { ...owner, toolName: 'other' }], [{ ...a, bytes: a.bytes + 1 }, owner]])
    await assert.rejects(f.service.results.modelResult(context, reference, ownership), error => error.code === 'TOOL_RESULT_REFERENCE_MISMATCH');
  const old = savedTurn({ result: 'FIRST preview', reference: { ...a, id: b.id } });
  old[1].Id = context.requestId; old[1].ToolActivities[0].toolCallId = 'first';
  const view = projection(old, protocols[0], { resultStore: f.service.results, resultContext: context });
  await view.loadResults();
  assert.doesNotMatch(JSON.stringify(view.projectTurn(view.historyTurns[0])), /SECOND/);
  assert.ok(!JSON.stringify(view.projectTurn(view.historyTurns[0])).includes(b.id));
  assert.equal(view.records.get(view.historyTurns[0]).rounds[0].observations[0].ref, null);
  assert.match(JSON.stringify(view.projectTurn(view.historyTurns[0])), /FIRST preview/);
  assert.match((await f.service.results.read(context, b.id)).text, /SECOND/);
});

test('sanitizer rejection never falls back to private raw JSON, while legal preview envelopes keep their content', () => {
  const deeplyNested = { _meta: { hidden: 'PRIVATE_DEEP_META' } }; let pointer = deeplyNested;
  for (let depth = 0; depth < 70; depth++) { pointer.next = {}; pointer = pointer.next; }
  assert.doesNotMatch(publicModelHistoryText(savedTurn({ result: JSON.stringify(deeplyNested) })[1]), /PRIVATE_DEEP_META/);
  const envelope = { status: 'completed', preview: 'LEGAL_PREVIEW', totalCharacters: 100000, truncated: true, resultRef: ref() };
  assert.match(publicModelHistoryText(savedTurn({ result: JSON.stringify(envelope) })[1]), /LEGAL_PREVIEW/);
});

test('real v3 segments with no kind restore public stage text, never their separate reasoning field', () => {
  const history = savedTurn(), view = projection(history);
  assert.equal(history[1].AssistantSegments[0].kind, undefined);
  assert.match(JSON.stringify(view.projectTurn(view.historyTurns[0])), /Read progress/);
  assert.doesNotMatch(JSON.stringify(view.projectTurn(view.historyTurns[0])), /PRIVATE_SEGMENT_REASONING/);
  assert.match(publicModelHistoryText(history[1]), /Read progress/);
  assert.doesNotMatch(publicModelHistoryText(history[1]), /PRIVATE_SEGMENT_REASONING/);
});

test('recent small observations remain original despite many old turns, while oversized recent output is excerpted head and tail', () => {
  const history = [];
  for (let index = 0; index < 150; index++) history.push(...savedTurn({ text: 'Old answer '.repeat(500), result: 'Old result '.repeat(1000) }));
  const recent = savedTurn({ result: 'RECENT_FULL_OBSERVATION' }); history.push(...recent);
  const view = projection(history), metrics = view.compact({ inputBudgetTokens: 6000 });
  const newest = view.records.get(view.historyTurns.at(-1)).rounds[0].observations[0];
  assert.equal(newest.content, 'RECENT_FULL_OBSERVATION'); assert.ok(metrics.compactedResultCount > 0);
  const context = buildContext({ conversationId: randomUUID(), history, currentMessage: 'Follow up', contextWindowTokens: 8192, maxOutputTokens: 2048,
    historyTurns: view.historyTurns, projectTurn: turn => view.projectTurn(turn), estimateContextMessages: estimateToolMessageTokens });
  assert.match(JSON.stringify(context.messages), /RECENT_FULL_OBSERVATION/);
  assert.ok(context.metrics.omittedTurnCount > 0); assert.ok(context.metrics.estimatedInputTokens <= context.metrics.inputBudgetTokens);
  const huge = projection(savedTurn({ result: 'HEAD_MARKER ' + 'x'.repeat(80000) + ' TAIL_MARKER' }));
  huge.compact({ inputBudgetTokens: 7000 });
  const excerpt = JSON.stringify(huge.projectTurn(huge.historyTurns[0]));
  assert.match(excerpt, /HEAD_MARKER/); assert.match(excerpt, /TAIL_MARKER/); assert.match(excerpt, /tool.result.read/);
});

test('archive IO is bounded by remaining token budget, newest first, and interrupted rounds are balanced without replay', async () => {
  const history = []; for (let index = 0; index < 200; index++) history.push(...savedTurn({ reference: ref(8 * 1024 * 1024) }));
  history.push(...savedTurn({ reference: ref(3000), status: 'interrupted', text: 'UNFINISHED_FINAL_DRAFT', activityStatus: 'running', result: '' }));
  const calls = [], view = projection(history, protocols[0], { inputBudgetTokens: 8192, resultStore: { modelResult: async (_ctx, reference, owner) => {
    calls.push({ reference, owner }); return { content: [{ type: 'text', text: 'Saved observation' }] }; } } });
  await view.loadResults(); assert.equal(calls.length, 1); assert.equal(calls[0].owner.requestId, history.at(-1).Id);
  const newest = view.projectTurn(view.historyTurns.at(-1));
  assert.deepEqual(...pairs(newest, protocols[0])); assert.doesNotMatch(JSON.stringify(newest), /UNFINISHED_FINAL_DRAFT/);
  assert.match(JSON.stringify(newest), /Execution-only history/);
  const tinyHistory = Array.from({ length: 200 }, () => savedTurn({ reference: ref(2) })).flat();
  let tinyReads = 0;
  const tiny = projection(tinyHistory, protocols[0], { inputBudgetTokens: 8192, resultStore: { modelResult: async () => {
    tinyReads++; return {}; } } });
  await tiny.loadResults();
  assert.equal(tinyReads, 3, 'archive envelope overhead is included in the IO budget even for tiny results');
  const origin = modelOrigin({ protocol: protocols[0], baseUrl: 'http://127.0.0.1:1/v1' }, { providerId: 'test', model: 'model' });
  const unstarted = savedTurn({ status: 'interrupted' }); unstarted[1].ToolActivities = [];
  unstarted[1].ModelTranscript = appendModelRound(unstarted[1], origin, { round: 1, text: 'Model proposed a call.', calls: [{ id: 'not_started', name: 'filesystem.read', arguments: { path: 'note.txt' } }] });
  const pending = projection(unstarted), text = JSON.stringify(pending.projectTurn(pending.historyTurns[0]));
  assert.match(text, /not_started/);
  assert.equal(JSON.parse(pending.projectTurn(pending.historyTurns[0]).find(message => message.role === 'tool').content).status, 'not_started');
});

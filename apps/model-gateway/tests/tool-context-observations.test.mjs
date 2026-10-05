import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from 'node:test';
import { ModelHistoryProjection } from '../models/model-history.mjs';
import { ToolContextProjection } from '../models/tool-context.mjs';
import { appendToolResults, estimateToolMessageTokens } from '../models/tool-protocols.mjs';
import { describeToolObservation, planObservationCompaction } from '../models/tool-observation-compaction.mjs';
import { evidenceSourceRef } from '../data/retrieval/evidence-references.mjs';
import { toolFixture } from './tool-fixture.mjs';

const protocols = ['openai-completions', 'openai-responses', 'anthropic-messages'];
const BUDGET = 200000;

function nativeTurn(protocol, call, round) {
  const continuation = protocol === 'anthropic-messages' ? [{ role: 'assistant', content: [
    { type: 'thinking', thinking: `Private thought ${round}`, signature: `opaque-signature-${round}` },
    { type: 'tool_use', id: call.id, name: 'read_wire', input: call.arguments }] }]
    : protocol === 'openai-responses' ? [{ type: 'reasoning', encrypted_content: `opaque-state-${round}`, summary: [] },
      { type: 'function_call', call_id: call.id, name: 'read_wire', arguments: JSON.stringify(call.arguments) }]
      : [{ role: 'assistant', content: `Read ${round}`, reasoning_content: `opaque-reasoning-${round}`,
        tool_calls: [{ id: call.id, type: 'function', function: { name: 'read_wire', arguments: JSON.stringify(call.arguments) } }] }];
  return { calls: [call], continuation };
}

function outputs(messages) {
  return messages.flatMap(message => message.role === 'tool' ? [{ id: message.tool_call_id, text: message.content }]
    : message.type === 'function_call_output' ? [{ id: message.call_id, text: message.output }]
      : Array.isArray(message.content) ? message.content.filter(block => block.type === 'tool_result')
        .map(block => ({ id: block.tool_use_id, text: block.content })) : []);
}

function assertPairs(messages) {
  const calls = messages.flatMap(message => [...(message.tool_calls ?? []).map(call => call.id),
    ...(message.type === 'function_call' ? [message.call_id] : []),
    ...(Array.isArray(message.content) ? message.content.filter(block => block.type === 'tool_use').map(block => block.id) : [])]);
  assert.deepEqual(calls.toSorted(), outputs(messages).map(item => item.id).toSorted());
  assert.equal(new Set(calls).size, calls.length);
}

async function observedReads(t, protocol, { change = false, text } = {}) {
  const f = await toolFixture(t), context = await f.context('full'), path = join(f.workspace, 'record.txt');
  const firstText = text ?? 'Original public record text.\n'.repeat(400), laterText = 'Changed public record text.\n'.repeat(400);
  await writeFile(path, firstText, 'utf8');
  const initial = [{ role: 'user', content: 'Use the actual observed file state.' }];
  const projection = new ToolContextProjection({ protocol, messages: initial, conversationId: context.conversationId,
    resultStore: f.service.results, resultContext: context });
  let messages = initial;
  const observed = [];
  for (const name of change ? ['filesystem.read', 'filesystem.write', 'filesystem.read'] : ['filesystem.read', 'filesystem.read']) {
    const previous = observed.at(-1), call = { id: `observed-${observed.length + 1}`, name,
      arguments: name === 'filesystem.write' ? { path: 'record.txt', content: laterText, expectedHash: JSON.parse(previous.result.content).sha256 }
        : { path: 'record.txt', maxChars: 64000 } };
    const result = await f.service.execute(context, call, { interactive: false });
    assert.equal(result.status, 'completed');
    observed.push({ call, result });
    messages = appendToolResults(protocol, messages, nativeTurn(protocol, call, observed.length), [{ call, result }],
      { onResult: (message, pair) => projection.observeResult(message, pair, observed.length) });
  }
  return { ...f, context, projection, messages, observed, firstText, laterText };
}

for (const protocol of protocols) {
  test(`${protocol}: verified repeated reads shrink without pressure, preserve native state and keep the newest output complete`, async t => {
    const f = await observedReads(t, protocol), original = structuredClone(f.messages);
    const archiveBefore = await f.service.results.read(f.context, f.observed[0].result.resultRef.id, { limit: 16000 });
    const prepare = await f.projection.prepareObservations(f.messages, { inputBudgetTokens: BUDGET });
    assert.equal(prepare.verifiedArchiveCount, 2);
    const result = f.projection.compact(f.messages, { inputBudgetTokens: BUDGET });
    assertPairs(result.messages); assert.deepEqual(f.messages, original);
    assert.ok(estimateToolMessageTokens(result.messages) < estimateToolMessageTokens(f.messages));
    const results = outputs(result.messages), envelope = JSON.parse(results[0].text);
    assert.equal(envelope.reason, 'duplicate-observation'); assert.equal(envelope.status, 'completed');
    assert.equal(envelope.resultRef.id, f.observed[0].result.resultRef.id);
    assert.equal(envelope.replacedBy.resultRef.id, f.observed[1].result.resultRef.id);
    assert.equal(results.at(-1).text, f.observed.at(-1).result.content);
    assert.deepEqual(result.messages.filter(message => !['tool', 'function_call_output'].includes(message.role ?? message.type)
      && !(Array.isArray(message.content) && message.content.some(block => block.type === 'tool_result'))),
    original.filter(message => !['tool', 'function_call_output'].includes(message.role ?? message.type)
      && !(Array.isArray(message.content) && message.content.some(block => block.type === 'tool_result'))));
    assert.equal(result.metrics.observationCompaction.duplicateCount, 1);
    const again = await f.projection.prepareObservations(result.messages, { inputBudgetTokens: BUDGET });
    assert.equal(again.verifiedArchiveCount, 2, 'already checked receipts do not cause another archive read');
    assert.equal((await f.service.results.read(f.context, envelope.resultRef.id, { limit: 16000 })).text, archiveBefore.text);
  });

  test(`${protocol}: actual file version changes shrink only the superseded read, with write receipt and call arguments intact`, async t => {
    const f = await observedReads(t, protocol, { change: true }), original = structuredClone(f.messages);
    await f.projection.prepareObservations(f.messages, { inputBudgetTokens: BUDGET });
    const result = f.projection.compact(f.messages, { inputBudgetTokens: BUDGET });
    assertPairs(result.messages); assert.deepEqual(f.messages, original);
    const observed = outputs(result.messages), envelope = JSON.parse(observed[0].text);
    assert.equal(envelope.reason, 'superseded-version');
    assert.equal(envelope.observedVersion.sha256, JSON.parse(f.observed[0].result.content).sha256);
    assert.equal(envelope.replacedBy.observedVersion.sha256, JSON.parse(f.observed[2].result.content).sha256);
    assert.notEqual(envelope.observedVersion.sha256, envelope.replacedBy.observedVersion.sha256);
    assert.equal(observed[1].text, f.observed[1].result.content, 'the completed write receipt is retained verbatim');
    assert.equal(observed[2].text, f.observed[2].result.content);
    assert.equal(result.metrics.observationCompaction.supersededCount, 1);
    assert.equal(result.metrics.observationCompaction.duplicateCount, 0);
  });

  test(`${protocol}: formal archived history reuses verified read identities without mutating journals or replaying calls`, async t => {
    const f = await toolFixture(t), path = join(f.workspace, 'history.txt'), text = 'Recorded historical public content.\n'.repeat(300);
    await writeFile(path, text, 'utf8');
    const history = [];
    for (let index = 0; index < 2; index++) {
      const context = await f.context('full'), call = { id: `historical-${index}`, name: 'filesystem.read', arguments: { path: 'history.txt', maxChars: 64000 } };
      const result = await f.service.execute(context, call, { interactive: false }), userId = randomUUID();
      history.push({ Id: userId, Role: 'user', Status: 'completed', Content: 'Read the recorded document.' },
        { Id: context.requestId, Role: 'assistant', ReplyTo: userId, Status: 'completed', Content: 'Recorded read complete.',
          ToolActivities: [{ toolCallId: call.id, name: call.name, arguments: call.arguments, round: 1, status: result.status,
            result: result.content, resultRef: result.resultRef }] });
    }
    const original = structuredClone(history), view = new ModelHistoryProjection({ history, protocol, resultStore: f.service.results,
      resultContext: { conversationId: f.conversationId, projectId: f.projectId }, inputBudgetTokens: BUDGET,
      availableTools: [{ name: 'filesystem.read' }] });
    await view.loadResults();
    const metrics = view.compact({ inputBudgetTokens: BUDGET }), messages = view.historyTurns.flatMap(turn => view.projectTurn(turn));
    assertPairs(messages); assert.deepEqual(history, original);
    assert.equal(metrics.observationCompaction.duplicateCount, 1); assert.equal(metrics.observationCompaction.verifiedArchiveCount, 2);
    assert.equal(JSON.parse(outputs(messages)[0].text).reason, 'duplicate-observation');
    assert.equal(JSON.parse(outputs(messages)[1].text).structuredContent.content, text);
  });

  test(`${protocol}: different legal evidence handles and follow-up gaps still compact the same verified read fact`, async t => {
    const f = await toolFixture(t), context = await f.context('full'), sourceId = randomUUID(), text = 'Verified source body.\n'.repeat(400);
    const initial = [{ role: 'user', content: 'Compare the verified source facts.' }], sourceRefs = [evidenceSourceRef(randomUUID(), 1), evidenceSourceRef(randomUUID(), 2)];
    const projection = new ToolContextProjection({ protocol, messages: initial, conversationId: context.conversationId,
      resultStore: f.service.results, resultContext: context });
    let messages = initial;
    const saved = [];
    for (let index = 0; index < 2; index++) {
      const call = { id: `source-read-${index}`, name: 'knowledge.read', arguments: { sourceRef: sourceRefs[index], gap: `Follow-up gap ${index}` } };
      const canonical = { structuredContent: { sourceId, scopeKey: `project:${f.projectId}`, contentHash: 'd'.repeat(64),
        sourceRevision: 'current-fixture-version', sourceRef: sourceRefs[index], text, offset: 0, nextOffset: text.length,
        totalCharacters: text.length, hasMore: false, mode: 'page', evidenceDecision: { sufficiency: 'not-evaluated', missingInformation: call.arguments.gap } } };
      const resultRef = await f.service.results.save(context, call, canonical), result = { status: 'completed', content: JSON.stringify(canonical), resultRef };
      saved.push({ call, canonical, result });
      messages = appendToolResults(protocol, messages, nativeTurn(protocol, call, index + 1), [{ call, result }],
        { onResult: (message, pair) => projection.observeResult(message, pair, index + 1) });
    }
    const original = structuredClone(messages);
    assert.notEqual(sourceRefs[0], sourceRefs[1]);
    await projection.prepareObservations(messages, { inputBudgetTokens: BUDGET });
    const compacted = projection.compact(messages, { inputBudgetTokens: BUDGET });
    assertPairs(compacted.messages); assert.deepEqual(messages, original);
    assert.equal(JSON.parse(outputs(compacted.messages)[0].text).reason, 'duplicate-observation');
    assert.equal(outputs(compacted.messages).at(-1).text, saved.at(-1).result.content);
    assert.equal(compacted.metrics.observationCompaction.verifiedArchiveCount, 2);
    for (const observation of saved) assert.deepEqual(await f.service.results.modelResult(context, observation.result.resultRef,
      { requestId: context.requestId, toolCallId: observation.call.id, toolName: observation.call.name }), observation.canonical);
  });
}

test('age, different scopes/targets/pages, unverified content, and failed or side-effect observations never prove supersession', () => {
  const path = join(process.cwd(), 'owned-fixture.txt'), payload = { path, sha256: 'a'.repeat(64), content: 'same text', offset: 0, nextOffset: 9, hasMore: false };
  const describe = options => describeToolObservation({ name: 'filesystem.read', status: 'completed', payload,
    scopeKey: 'chat-one', archiveVerified: true, ...options });
  const source = identity => ({ callId: randomUUID(), name: 'filesystem.read', status: 'completed', resultRef: { id: randomUUID() }, identity });
  const first = source(describe());
  for (const identity of [describe({ scopeKey: 'chat-two' }), describe({ archiveVerified: false }),
    describe({ payload: { ...payload, path: join(process.cwd(), 'different.txt') } }),
    describe({ payload: { ...payload, offset: 10, nextOffset: 19 } }),
    describe({ payload: { ...payload, extraMetadata: 'changed metadata without a changed version' } })])
    assert.deepEqual(planObservationCompaction([first, source(identity)]), []);
  for (const status of ['error', 'unknown', 'running', 'denied', 'cancelled']) assert.equal(describe({ status }), null);
  for (const name of ['filesystem.write', 'filesystem.delete', 'terminal.run', 'mcp.synthetic.read']) assert.equal(describe({ name }), null);
  assert.deepEqual(planObservationCompaction([first]), []);
});

test('a completed first section stays distinct from a full document, and short source handles retain canonical read identity', () => {
  const sourceId = randomUUID(), scopeKey = 'project:synthetic', contentHash = 'b'.repeat(64);
  const section = { sourceId, scopeKey, contentHash, sourceRevision: 'verified-revision',
    sourceRef: 'ev1:synthetic-short-source', text: 'First section text.', offset: 0, nextOffset: 19,
    totalCharacters: 64, hasMore: false, mode: 'section', section: { startOffset: 0, endOffset: 19 } };
  const describe = payload => describeToolObservation({ name: 'knowledge.read', status: 'completed', payload: { structuredContent: payload },
    scopeKey: 'synthetic-chat', archiveVerified: true });
  const source = identity => ({ callId: randomUUID(), name: 'knowledge.read', status: 'completed', resultRef: { id: randomUUID() }, identity });
  const sectionIdentity = describe(section), wholeIdentity = describe({ ...section, text: 'x'.repeat(64), nextOffset: 64, mode: 'page' });
  assert.deepEqual(sectionIdentity.target, { sourceId, scopeKey });
  assert.deepEqual(sectionIdentity.version, { contentHash, sourceRevision: 'verified-revision' });
  assert.equal(sectionIdentity.page, '[0,19]'); assert.equal(wholeIdentity.page, 'whole');
  assert.deepEqual(planObservationCompaction([source(sectionIdentity), source(wholeIdentity)]), []);
  assert.equal(describe({ ...section, totalCharacters: undefined }).page, '[0,19]');
});

test('source body, section/window boundaries and unknown metadata are retained in read equality checks', () => {
  const text = 'Identical source body.', payload = { sourceId: randomUUID(), scopeKey: 'global', sourceRef: evidenceSourceRef(randomUUID(), 1),
    contentHash: 'e'.repeat(64), sourceRevision: 4, text, offset: 0, nextOffset: text.length, totalCharacters: text.length, hasMore: false,
    mode: 'section', section: { startOffset: 0, endOffset: text.length } };
  const describe = value => describeToolObservation({ name: 'knowledge.read', status: 'completed', payload: value,
    scopeKey: 'synthetic-chat', archiveVerified: true });
  const source = identity => ({ callId: randomUUID(), name: 'knowledge.read', status: 'completed', resultRef: { id: randomUUID() }, identity });
  const first = source(describe(payload));
  for (const changed of [{ ...payload, text: 'x'.repeat(text.length) }, { ...payload, mode: 'window' },
    { ...payload, section: { startOffset: 0, endOffset: text.length + 10 } }, { ...payload, window: { anchorOffset: 3 } },
    { ...payload, clip: { before: true } }, { ...payload, unknownMetadata: 'different' }])
    assert.deepEqual(planObservationCompaction([first, source(describe(changed))]), []);
});

test('missing archive metadata disables new compaction, and IO is counted once within the real input budget', async t => {
  const f = await observedReads(t, 'openai-completions'), original = structuredClone(f.messages);
  const modelResult = f.service.results.modelResult.bind(f.service.results); let reads = 0;
  f.projection.resultStore = { modelResult: async (...args) => {
    reads++; if (args[1].id === f.observed[0].result.resultRef.id) throw Object.assign(new Error('Synthetic missing archive'), { code: 'TOOL_RESULT_NOT_FOUND' });
    return modelResult(...args);
  } };
  const insufficient = await f.projection.prepareObservations(f.messages, { inputBudgetTokens: 1024 });
  assert.equal(reads, 0); assert.equal(insufficient.archiveReadBytes, 0);
  const prepared = await f.projection.prepareObservations(f.messages, { inputBudgetTokens: BUDGET });
  assert.equal(reads, 2); assert.equal(prepared.unavailableArchiveCount, 1);
  assert.ok(prepared.archiveReadBytes <= BUDGET * 4);
  const unchanged = f.projection.compact(f.messages, { inputBudgetTokens: BUDGET });
  assert.deepEqual(unchanged.messages, original);
  assert.equal(unchanged.metrics.observationCompaction.unavailableArchiveCount, 1);
  await f.projection.prepareObservations(f.messages, { inputBudgetTokens: BUDGET }); assert.equal(reads, 2);
});

test('small repeated results avoid both archive IO and a larger replacement envelope', async t => {
  const f = await observedReads(t, 'openai-completions', { text: 'Small complete public value.' });
  let reads = 0; f.projection.resultStore = { modelResult: async () => { reads++; throw new Error('Tiny outputs need no compaction IO'); } };
  const prepared = await f.projection.prepareObservations(f.messages, { inputBudgetTokens: BUDGET });
  assert.equal(reads, 0); assert.equal(prepared.archiveReadBytes, 0);
  assert.equal(f.projection.compact(f.messages, { inputBudgetTokens: BUDGET }).messages, f.messages);
});

test('cancellation waits for every admitted archive check to settle before releasing preparation', async t => {
  const f = await observedReads(t, 'openai-completions'), controller = new AbortController(), gates = [];
  let enter;
  const ready = new Promise(resolve => { enter = resolve; });
  const modelResult = f.service.results.modelResult.bind(f.service.results);
  f.projection.resultStore = { modelResult: async (...args) => {
    await new Promise(resolve => { gates.push(resolve); if (gates.length === 2) enter(); }); return modelResult(...args);
  } };
  let settled = false;
  const pending = f.projection.prepareObservations(f.messages, { inputBudgetTokens: BUDGET, signal: controller.signal });
  pending.then(() => { settled = true; }, () => { settled = true; });
  await ready; controller.abort(); gates[0](); await new Promise(resolve => setImmediate(resolve));
  assert.equal(settled, false); gates[1](); await assert.rejects(pending, { name: 'AbortError' });
});

import assert from 'node:assert/strict';
import { test } from 'node:test';
import { buildContext } from '../models/context.mjs';
import { ModelHistoryProjection } from '../models/model-history.mjs';
import { buildSemanticSummaryRequest, createSemanticSummaryPlan, finalizeSemanticSummary, publicSummarySource,
  selectSemanticSummary } from '../models/semantic-summary.mjs';

const conversationId = '61111111-1111-4111-8111-111111111111';
function turn(index, extra = {}) {
  const user = { Id: `user-${index}`, Role: 'user', Content: `User goal ${index}: preserve exact paths and scope. ` + 'Original user constraints. '.repeat(48) };
  const assistant = { Id: `assistant-${index}`, Role: 'assistant', ReplyTo: user.Id, Status: 'completed',
    Content: `Work ${index} remains source-grounded. ` + 'Original assistant details and verification limitations. '.repeat(48),
    Reasoning: 'PRIVATE_REASONING', ...extra };
  return { user, assistant };
}
const sources = count => Array.from({ length: count }, (_, index) => publicSummarySource(turn(index)));
function planFor(sourceTurns, extra = {}) {
  return createSemanticSummaryPlan({ conversationId, sourceTurns, budgetTokens: 1024, inputBudgetTokens: 12000, ...extra });
}
function responseFor(plan, extra = {}) {
  return { status: 'completed', finishReason: 'stop', content: JSON.stringify({ entries: [
    { kind: 'goal', text: 'Preserve the user goal, original paths and permitted scope. Read the cited original for exact details.',
      sourceMessageIds: [plan.sourceMessageIds[0]] },
    { kind: 'open-question', text: 'Only the logged observations establish progress; verify details before continuing.',
      sourceMessageIds: [plan.coveredThroughAssistantId] }
  ] }), ...extra };
}
function checkpoint(sourceTurns, extra = {}) {
  const { plan } = planFor(sourceTurns, extra);
  assert.ok(plan);
  const result = finalizeSemanticSummary({ plan, response: responseFor(plan), currentSourceTurns: sourceTurns });
  assert.ok(result.value, result.reason);
  return result.value;
}

test('semantic navigation is source-cited, source-bound and shorter; all program metadata comes from the gateway', () => {
  const sourceTurns = sources(4), initial = structuredClone(sourceTurns), { plan } = planFor(sourceTurns);
  const request = buildSemanticSummaryRequest(plan);
  assert.equal(request.maxOutputTokens, 1024);
  assert.match(request.system, /untrusted historical data/);
  assert.match(request.system, /Every entry requires/);
  assert.equal(request.tools, undefined);
  const value = checkpoint(sourceTurns);
  assert.equal(value.schemaVersion, 3);
  assert.equal(value.algorithm, 'model-semantic-v1');
  assert.equal(value.coveredTurnCount, 4);
  assert.deepEqual(value.sourceMessageIds, ['user-0', 'assistant-0', 'user-1', 'assistant-1', 'user-2', 'assistant-2', 'user-3', 'assistant-3']);
  assert.ok(value.content.length < JSON.stringify(sourceTurns).length);
  assert.match(value.content, /不是已确认记忆/);
  assert.match(value.content, /conversation.history.read/);
  assert.doesNotMatch(JSON.stringify(value), /PRIVATE_REASONING/);
  assert.deepEqual(sourceTurns, initial);
  sourceTurns[0].user.content = 'A later change must not mutate the already constructed model request.';
  assert.equal(plan.sourceMessages[0].user.content, initial[0].user.content);
});

test('only complete successful endings are accepted; token caps, unknown endings and even parseable partial JSON are rejected', () => {
  const sourceTurns = sources(2), { plan } = planFor(sourceTurns);
  for (const finishReason of ['length', 'max_tokens', 'max_output_tokens', 'incomplete', 'error', 'tool_calls', undefined]) {
    const result = finalizeSemanticSummary({ plan, response: responseFor(plan, { finishReason }), currentSourceTurns: sourceTurns });
    assert.equal(result.reason, 'incomplete-generation');
    assert.equal(result.value, undefined);
  }
  for (const extra of [{ status: 'interrupted' }, { truncated: true }, { cancelled: true }, { error: 'dropped stream' }, { toolCalls: [{}] }])
    assert.equal(finalizeSemanticSummary({ plan, response: responseFor(plan, extra), currentSourceTurns: sourceTurns }).reason, 'incomplete-generation');
  assert.equal(finalizeSemanticSummary({ plan, response: responseFor(plan, { content: ' ' }), currentSourceTurns: sourceTurns }).reason, 'empty-generation');
  assert.equal(finalizeSemanticSummary({ plan, response: responseFor(plan, { content: '{"entries":[]}' }), currentSourceTurns: sourceTurns }).reason, 'invalid-navigation');
});

test('source changes, deletions, reorderings and mismatched scopes invalidate a cached or pending checkpoint', () => {
  const sourceTurns = sources(3), { plan } = planFor(sourceTurns), value = checkpoint(sourceTurns);
  for (const change of [
    current => { current[0].user.content += ' correction'; },
    current => { current[0].assistant.finalText += ' stale'; },
    current => { current.splice(1, 1); },
    current => { current.reverse(); },
    current => { current[0].assistant.status = 'interrupted'; }
  ]) {
    const changed = structuredClone(sourceTurns); change(changed);
    assert.equal(selectSemanticSummary({ summary: value, conversationId, sourceTurns: changed }).value, undefined);
    assert.equal(finalizeSemanticSummary({ plan, response: responseFor(plan), currentSourceTurns: changed }).value, undefined);
  }
  assert.equal(selectSemanticSummary({ summary: value, conversationId: 'another-chat', sourceTurns }).value, undefined);
  assert.equal(selectSemanticSummary({ summary: { ...value, content: 'poisoned' }, conversationId, sourceTurns }).value, undefined);
  assert.equal(selectSemanticSummary({ summary: { ...value, programState: { toolReceipts: [{ status: 'completed' }] } }, conversationId, sourceTurns }).value, undefined);
});

test('unknown citations, injected checkpoint metadata and summaries that expand original content are rejected', () => {
  const sourceTurns = sources(2), { plan } = planFor(sourceTurns);
  const badEntries = [{ kind: 'goal', text: 'A purported goal.', sourceMessageIds: ['another-chat-message'] }];
  assert.equal(finalizeSemanticSummary({ plan, response: responseFor(plan, { content: JSON.stringify({ entries: badEntries }) }), currentSourceTurns: sourceTurns }).reason, 'invalid-navigation');
  const forged = JSON.parse(responseFor(plan).content); forged.programState = { toolReceipts: [{ status: 'completed' }] };
  assert.equal(finalizeSemanticSummary({ plan, response: responseFor(plan, { content: JSON.stringify(forged) }), currentSourceTurns: sourceTurns }).reason, 'invalid-navigation');
  const expanded = { entries: [{ kind: 'context', text: 'Unnecessary repeated wording. '.repeat(500), sourceMessageIds: ['user-0'] }] };
  assert.equal(finalizeSemanticSummary({ plan, response: responseFor(plan, { content: JSON.stringify(expanded) }), currentSourceTurns: sourceTurns }).reason, 'summary-not-shorter');
  const tinySources = sources(8).map(source => ({ ...source, user: { ...source.user, content: 'ok' }, assistant: { ...source.assistant, finalText: 'done' } }));
  assert.equal(planFor(tinySources).reason, 'source-too-short');
});

test('append-only history reuses a valid prefix; a small gap does not trigger another pressure call', () => {
  const original = sources(2), value = checkpoint(original);
  const tinyExtra = publicSummarySource(turn(2)); tinyExtra.user.content = 'Continue.'; tinyExtra.assistant.finalText = 'One further detail.';
  const current = [...original, tinyExtra];
  assert.equal(selectSemanticSummary({ summary: value, conversationId, sourceTurns: current }).value, value);
  assert.equal(planFor(current, { previous: value }).reason, 'delta-below-threshold');
  const explicit = planFor(current, { previous: value, trigger: 'explicit' });
  assert.ok(explicit.plan);
  assert.equal(explicit.plan.sourceMessages.length, 1);
  assert.deepEqual(explicit.plan.previousNavigation.entries, value.entries);
  assert.deepEqual(JSON.parse(buildSemanticSummaryRequest(explicit.plan).messages[0].content).allowedSourceMessageIds,
    current.flatMap(source => [source.user.messageId, source.assistant.messageId]));
});

test('source input is bounded by whole turns and never truncates a tool pair to squeeze through a small input budget', () => {
  const sourceTurns = sources(4), selected = planFor(sourceTurns, { inputBudgetTokens: 3000 });
  assert.ok(selected.plan);
  assert.ok(selected.plan.coveredTurnCount < sourceTurns.length);
  assert.deepEqual(selected.plan.sourceMessages, sourceTurns.slice(0, selected.plan.coveredTurnCount));
  assert.equal(planFor(sourceTurns, { inputBudgetTokens: 128 }).reason, 'source-unit-does-not-fit');
  const half = structuredClone(sourceTurns); half[0].checkpointComplete = false;
  assert.equal(planFor(half).reason, 'incomplete-checkpoint');
  assert.equal(planFor(sourceTurns, { trigger: 'none' }).reason, 'not-requested');
});

test('model history supplies all public stages and balanced receipts before pruning while protecting private reasoning and native state', () => {
  const saved = turn(0, { Status: 'interrupted', Content: 'This final draft did not complete.',
    ModelTranscript: { rounds: [{ round: 1, text: 'Public stage before tool.', calls: [{ id: 'call-1', name: 'filesystem.write', arguments: { path: 'file.txt' } }],
      continuation: { secret: 'PRIVATE_NATIVE' } }] },
    ToolActivities: [{ round: 1, toolCallId: 'call-1', name: 'filesystem.write', arguments: { path: 'file.txt' },
      status: 'completed', result: 'Original formal tool observation. '.repeat(80) }],
    AssistantSegments: [{ round: 1, order: 0, phase: 'commentary', status: 'completed', content: 'A different public stage.', reasoning: 'PRIVATE_SEGMENT' }] });
  const history = [saved.user, saved.assistant], initial = structuredClone(history);
  const view = new ModelHistoryProjection({ history, protocol: 'openai-completions', inputBudgetTokens: 4096, availableTools: [] });
  const sourceTurns = view.summarySources();
  assert.equal(sourceTurns[0].checkpointComplete, true);
  assert.equal(sourceTurns[0].assistant.finalText, '');
  assert.equal(sourceTurns[0].rounds[0].tools[0].status, 'completed');
  assert.match(JSON.stringify(sourceTurns), /A different public stage/);
  assert.match(JSON.stringify(sourceTurns), /Public stage before tool/);
  assert.doesNotMatch(JSON.stringify(sourceTurns), /PRIVATE_|final draft/);
  const value = checkpoint(sourceTurns);
  assert.equal(value.programState.requestStates[0].hasCompletedFinalAnswer, false);
  assert.equal(value.programState.toolReceipts[0].status, 'completed');
  view.compact({ inputBudgetTokens: 256 });
  assert.deepEqual(view.summarySources(), sourceTurns, 'request projection pruning cannot change a summary source');
  assert.deepEqual(history, initial);
  history[1].ToolActivities[0].status = 'running';
  assert.equal(planFor(view.summarySources()).reason, 'incomplete-checkpoint');
  history[1].ToolActivities = [];
  assert.equal(planFor(view.summarySources()).reason, 'incomplete-checkpoint');
});

test('context exposes a pressure plan and keeps the extractive fallback; cached semantic navigation does not become confirmed memory', () => {
  const turns = Array.from({ length: 12 }, (_, index) => turn(index)), history = turns.flatMap(item => [item.user, item.assistant]);
  const args = { conversationId, history, currentMessage: 'Continue preserving the paths.', contextWindowTokens: 8192 };
  const first = buildContext(args);
  assert.ok(first.semanticSummaryPlan);
  assert.equal(first.summaryUpdate.algorithm, 'extractive-v2');
  const sourceTurns = turns.map(publicSummarySource), generated = finalizeSemanticSummary({ plan: first.semanticSummaryPlan,
    response: responseFor(first.semanticSummaryPlan), currentSourceTurns: sourceTurns });
  assert.ok(generated.value, generated.reason);
  const reused = buildContext({ ...args, summary: generated.value });
  assert.equal(reused.metrics.summaryAlgorithm, 'model-semantic-v1');
  assert.equal(reused.summaryUpdate, undefined);
  assert.match(reused.system, /模型语义导航/);
  assert.equal(reused.memoryProjection.length, 0);
  assert.equal(reused.semanticSummaryPlan, undefined);
  const disabled = buildContext({ ...args, semanticSummaryTrigger: 'none' });
  assert.equal(disabled.semanticSummaryPlan, undefined);
});

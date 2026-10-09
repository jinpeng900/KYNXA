import assert from 'node:assert/strict';
import { test } from 'node:test';
import { evaluateRun, summarizeRuns, evaluatePlan, RETRIEVAL_ABLATIONS } from '../../../tools/development/evaluate-unified-retrieval.mjs';

test('retrieval hits or model-declared success cannot substitute for independent task acceptance', () => {
  const task = { id: 'one', split: 'heldout', expectedSourceIds: ['a'], acceptanceCheckIds: ['test'] };
  const result = evaluateRun(task, { sourceIds: ['a'], acceptanceReceipts: [{ id: 'test', origin: 'model', passed: true, exitCode: 0 }] }, 10);
  assert.equal(result.recallAt20, 1); assert.equal(result.taskPassed, null);
  assert.equal(result.inputTokens, null); assert.equal(result.observedScopeViolations, null);
  const verified = evaluateRun(task, { acceptanceReceipts: [{ id: 'test', origin: 'independent-validator', passed: true, exitCode: 0 }],
    attempts: [{ modelCalls: 1, toolCalls: 2, inputTokens: 100, outputTokens: 10 },
      { modelCalls: 2, toolCalls: 3, inputTokens: 200, outputTokens: 20 }] }, 30);
  assert.equal(verified.taskPassed, true); assert.equal(verified.inputTokens, 300);
  assert.equal(summarizeRuns([result, verified]).tasksWithUnknownTokenCost, 1);
  assert.equal(summarizeRuns([result, verified]).measuredRetries, 1);
});

test('smoke evaluation keeps original queries, rotates variants and never reports a full quality benchmark', async () => {
  const seen = [], query = 'long query '.repeat(400);
  const manifest = { version: 1, model: 'fixed-fixture', datasetRevision: 'fixture-one', seed: 1,
    tasks: [{ id: 'one', split: 'development', query, expectedSourceIds: [], acceptanceCheckIds: [] }] };
  const adapter = { fixedModel: manifest.model, supportedAblations: RETRIEVAL_ABLATIONS.map(item => item.id),
    run: async input => { seen.push(input); return {}; }, dispose: async () => {} };
  const report = await evaluatePlan(manifest, adapter, { smoke: true });
  assert.equal(seen.length, 5); assert.ok(seen.every(input => input.task.query === query));
  assert.equal(report.qualityBenchmarkCompleted, false);
  await assert.rejects(evaluatePlan(manifest, adapter), /held-out/u);
  await assert.rejects(evaluatePlan({ ...manifest, model: 'different' }, adapter, { smoke: true }), /same model/u);
});

import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtemp, readFile, rm, appendFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { freezeLegacyLocalizationSuite, createVersionedEvaluationAdapter,
  normalizeVersionedEvaluationResult, createEvaluationRunJournal } from '../evaluation-version-adapter.mjs';

const bytes = value => Buffer.from(JSON.stringify(value));
const sha = value => createHash('sha256').update(value).digest('hex');
function inputs() {
  const publicCases = { cases: [{ instance_id: 'sample__sample-1', repo: 'sample/sample',
    base_commit: 'a'.repeat(40), problem_statement: 'Find the addition implementation.\n```js\nadd(1, 2)\n```' }] };
  const publicCasesBytes = bytes(publicCases);
  return { publicCasesBytes,
    selectionBytes: bytes({ selection_only: true, case_count: 1, seed: 'frozen-legacy-seed',
      model_public_file_sha256: sha(publicCasesBytes), cases: [{ instance_id: 'sample__sample-1', repo: 'sample/sample',
        base_commit: 'a'.repeat(40), problem_sha256: sha(publicCases.cases[0].problem_statement),
        reason: 'PRIVATE_SELECTION_LABEL', localization_proxy: { patch_target_files: ['PRIVATE_GOLD_PATH'] } }] }),
    scoringDefinitionBytes: Buffer.from('immutable post-run patch target coverage definition'),
    configurationBytes: bytes({ model: { tag: 'qwen3:8b', think: false, manifestSha256: 'b'.repeat(64), modelBlobSha256: 'c'.repeat(64),
      modelRoot: 'PRIVATE_MACHINE_PATH', runtimeExecutable: 'PRIVATE_EXECUTABLE_PATH',
      inputTokenSoftCap: 3072, options: { num_ctx: 4096, num_predict: 256, temperature: 0, seed: 42 } },
      arms: { autonomous_max10_model_calls: { maxModelCalls: 10 } } }), datasetRevision: 'legacy-fixture-v1' };
}

test('legacy freeze preserves exact public issues/budgets and excludes selection labels, paths and unsupported translations', async () => {
  const source = inputs(), suite = freezeLegacyLocalizationSuite(source);
  assert.equal(suite.tasks[0].query, JSON.parse(source.publicCasesBytes).cases[0].problem_statement);
  assert.equal(suite.runtime.options.num_ctx, 4096); assert.equal(suite.runtime.options.num_predict, 256);
  assert.equal(suite.runtime.inputTokenSoftCap, 3072); assert.equal(suite.runtime.maximumAutonomousCalls, 10);
  assert.deepEqual(suite.runtime.languages, ['en']); assert.equal(suite.compatibility.unifiedExecutionAdapter, false);
  assert.doesNotMatch(JSON.stringify(suite), /PRIVATE_/u);
  await assert.rejects(createVersionedEvaluationAdapter(suite), { code: 'EVALUATION_LEGACY_RUNTIME_UNSUPPORTED' });
  const changed = JSON.parse(source.publicCasesBytes); changed.cases[0].problem_statement += ' changed';
  assert.throws(() => freezeLegacyLocalizationSuite({ ...source, publicCasesBytes: bytes(changed) }), { code: 'EVALUATION_LEGACY_CONTRACT_INVALID' });
  const injected = JSON.parse(source.publicCasesBytes); injected.cases[0].patch = 'PRIVATE_GOLD';
  const injectedBytes = bytes(injected), selected = JSON.parse(source.selectionBytes); selected.model_public_file_sha256 = sha(injectedBytes);
  assert.throws(() => freezeLegacyLocalizationSuite({ ...source, publicCasesBytes: injectedBytes, selectionBytes: bytes(selected) }),
    { code: 'EVALUATION_LEGACY_ISSUE_CHANGED' });
});

test('version dispatch reuses the new adapter and requires pinned legacy runner plus independent indexes', async () => {
  const newManifest = { version: 1, model: 'fixed', tasks: [] };
  let calls = 0;
  const adapter = await createVersionedEvaluationAdapter(newManifest, { createUnifiedAdapter: async value => {
    calls++; assert.equal(value, newManifest); return { fixedModel: value.model }; } });
  assert.equal(adapter.fixedModel, 'fixed'); assert.equal(calls, 1);
  const source = inputs(), suite = freezeLegacyLocalizationSuite(source);
  const translationBytes = bytes({ source_public_file_sha256: sha(source.publicCasesBytes), case_count: 1,
    cases: [{ instance_id: suite.tasks[0].id, original_problem_sha256: suite.tasks[0].querySha256, problem_statement_zh: '查找加法实现。' }] });
  const paired = freezeLegacyLocalizationSuite({ ...source, translationBytes });
  assert.equal(paired.tasks[0].translations.zh.query, '查找加法实现。');
  const runner = await createVersionedEvaluationAdapter(paired, { legacyRunner: { fixedModel: paired.model,
    protocolId: paired.runtime.wireProtocol, configurationHash: paired.sourceHashes.configuration,
    scoringDefinitionHash: paired.sourceHashes.scoringDefinition, run: async request => request, dispose: async () => {} } });
  const request = { task: paired.tasks[0], legacyArm: { language: 'zh', policy: 'B', mode: 'autonomous' },
    isolation: { separateDataRoot: true, noSharedLegacyIndex: true } };
  assert.equal((await runner.run(request)).task.translations.zh.query, '查找加法实现。');
  await assert.rejects(runner.run({ ...request, isolation: { separateDataRoot: true } }), { code: 'EVALUATION_LEGACY_RUN_INVALID' });
  await runner.dispose();
});

test('legacy localization coverage never becomes code repair success and unknown token costs stay unknown', () => {
  const suite = freezeLegacyLocalizationSuite(inputs()), task = suite.tasks[0];
  const result = { instance_id: task.id, metric: suite.scoring.metric, config_sha256: suite.sourceHashes.configuration,
    source_version_valid: true, owned_processes_stopped: true, arms: { autonomous_max10_model_calls: {
      status: 'completed', patch_target_coverage: 1, top5_patch_target_hit: true, best_patch_target_rank: 1,
      model_calls: 3, output_tokens_total: 10, wall_seconds: 1.5 } } };
  const legacyRun = { instance_id: task.id, base_commit: task.baseCommit, model: suite.model,
    manifest_sha256: suite.modelIdentity.manifestSha256, config_sha256: suite.sourceHashes.configuration };
  const report = normalizeVersionedEvaluationResult({ contract: suite, task, result, legacyRun });
  const arm = report.measurement.arms.autonomous_max10_model_calls;
  assert.equal(arm.patchTargetCoverage, 1); assert.equal(arm.taskPassed, null); assert.equal(arm.inputTokens, null);
  assert.equal(report.measurement.codeRepairSuccess, null); assert.equal(arm.durationMs, 1500);
  const invalid = normalizeVersionedEvaluationResult({ contract: suite, task, legacyRun, result: { ...result, source_version_valid: false } });
  assert.equal(invalid.measurement.arms.autonomous_max10_model_calls.patchTargetCoverage, null);
  assert.throws(() => normalizeVersionedEvaluationResult({ contract: suite, task, result, legacyRun: { ...legacyRun, model: 'other' } }),
    { code: 'EVALUATION_LEGACY_RECEIPT_MISMATCH' });
});

test('separate journals preserve stopped attempts, reject unresolved replay and pin resume identity', async () => {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-evaluation-version-'));
  const contract = { version: 1, model: 'fixed', datasetRevision: 'fixture-v1', tasks: [{ id: 'case-1' }], seed: 17 };
  try {
    const journal = await createEvaluationRunJournal({ outputRoot: root, contract, runId: 'new-run' });
    const attempts = await Promise.allSettled([journal.startAttempt({ taskId: 'case-1', arm: 'integrated', attemptId: 'one' }),
      journal.startAttempt({ taskId: 'case-1', arm: 'integrated', attemptId: 'two' })]);
    assert.equal(attempts.filter(value => value.status === 'fulfilled').length, 1);
    await journal.requestStop(); assert.equal(journal.signal.aborted, true);
    await assert.rejects(createEvaluationRunJournal({ outputRoot: root, contract, resumeRunId: 'new-run' }),
      { code: 'EVALUATION_RESUME_UNCONFIRMED' });
    await assert.rejects(journal.finishAttempt({ attemptId: 'one', status: 'stopped', ownedExecutorsSettled: false }),
      { code: 'EVALUATION_ATTEMPT_UNCONFIRMED' });
    await journal.finishAttempt({ attemptId: 'one', status: 'stopped', ownedExecutorsSettled: true }); await journal.close();
    await assert.rejects(createEvaluationRunJournal({ outputRoot: root, contract: { ...contract, model: 'changed' }, resumeRunId: 'new-run' }),
      { code: 'EVALUATION_RESUME_CONTRACT_CHANGED' });
    const resumed = await createEvaluationRunJournal({ outputRoot: root, contract, resumeRunId: 'new-run' });
    assert.equal(resumed.status().stopped, false); assert.equal(resumed.status().attempts[0].status, 'stopped');
    const next = await resumed.startAttempt({ taskId: 'case-1', arm: 'integrated', attemptId: 'next' });
    await resumed.finishAttempt({ attemptId: next, status: 'completed', ownedExecutorsSettled: true });
    await assert.rejects(resumed.startAttempt({ taskId: 'case-1', arm: 'integrated' }), { code: 'EVALUATION_ATTEMPT_INVALID' });
    await resumed.close();
    const log = await readFile(join(root, 'new-run', 'events.jsonl'), 'utf8');
    assert.equal(log.split('\n').filter(Boolean).length, 6); assert.doesNotMatch(log, /PRIVATE_/u);
    await appendFile(join(root, 'new-run', 'events.jsonl'), '{incomplete');
    await assert.rejects(createEvaluationRunJournal({ outputRoot: root, contract, resumeRunId: 'new-run' }), { code: 'EVALUATION_JOURNAL_INVALID' });
    await assert.rejects(createEvaluationRunJournal({ outputRoot: join(root, 'historical'), protectedRoots: [root], contract }),
      { code: 'EVALUATION_OUTPUT_NOT_ISOLATED' });
  } finally { await rm(root, { recursive: true, force: true }); }
});

test('interrupted attempt recovery needs independent host settlement and never turns interruption into success', async () => {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-evaluation-reconcile-'));
  const contract = { version: 1, model: 'fixed', datasetRevision: 'fixture-v1', tasks: [{ id: 'case-1' }], seed: 17 };
  try {
    const journal = await createEvaluationRunJournal({ outputRoot: root, contract, runId: 'interrupted' });
    await journal.startAttempt({ taskId: 'case-1', arm: 'integrated', attemptId: 'old' }); await journal.close();
    const receipt = { attemptId: 'old', origin: 'independent-host-executor-reconciliation',
      ownedExecutorsSettled: true, receiptHash: 'd'.repeat(64), status: 'stopped' };
    await assert.rejects(createEvaluationRunJournal({ outputRoot: root, contract, resumeRunId: 'interrupted',
      recoveryReceipts: [{ ...receipt, origin: 'model' }] }), { code: 'EVALUATION_RESUME_UNCONFIRMED' });
    await assert.rejects(createEvaluationRunJournal({ outputRoot: root, contract, resumeRunId: 'interrupted',
      recoveryReceipts: [{ ...receipt, status: 'completed' }] }), { code: 'EVALUATION_RESUME_UNCONFIRMED' });
    const recovered = await createEvaluationRunJournal({ outputRoot: root, contract, resumeRunId: 'interrupted', recoveryReceipts: [receipt] });
    assert.equal(recovered.status().attempts[0].status, 'stopped');
    await recovered.startAttempt({ taskId: 'case-1', arm: 'integrated', attemptId: 'new' });
    assert.equal(recovered.status().attempts.length, 2); await recovered.close();
  } finally { await rm(root, { recursive: true, force: true }); }
});

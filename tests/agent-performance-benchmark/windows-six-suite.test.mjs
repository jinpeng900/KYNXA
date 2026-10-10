import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';
import { TASKS } from './evaluation.mjs';
import { WINDOWS_SIX_TASKS, WINDOWS_SIX_LIMITS, CHANGED_PENDING_WORK, PENDING_WORK_TEST,
  prepareSixWorkspace, verifySixTask, windowsSixPlan } from './windows-six-suite.mjs';
import { assertPreparedRetryAdmission, assertPreparedManifestMatches, normalizePreparedLinkTarget,
  observeSixResourceAcquisitions, observeSixDocumentEmbeddings, sentAutomaticEvidenceRecords,
  sixActionReadiness, loadSixProduct, readPreparedFixture } from './run-windows-six.mjs';

const repositoryRoot = fileURLToPath(new URL('../../', import.meta.url));
const artifactRoot = join(repositoryRoot, 'artifacts', 'verification', 'windows-six-runner-contract');

test('six frozen tasks are separate from the existing five-task scores and default CLI never infers', () => {
  assert.equal(TASKS.length, 5);
  assert.equal(WINDOWS_SIX_TASKS.length, 6);
  for (const state of ['ready', 'partial', 'changed']) assert.equal(WINDOWS_SIX_TASKS.filter(task => task.indexState === state).length, 2);
  assert.equal(WINDOWS_SIX_LIMITS.maxDurationMs, 1800000);
  const result = spawnSync(process.execPath, ['tests/agent-performance-benchmark/run-windows-six.mjs'],
    { cwd: repositoryRoot, encoding: 'utf8', timeout: 10000 });
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(JSON.parse(result.stdout), windowsSixPlan());
  assert.equal(JSON.parse(result.stdout).realModelCalls, 0);
});

test('the independent Node repair starts passing, is genuinely broken after change, and cannot pass from claimed tests', async t => {
  await mkdir(artifactRoot, { recursive: true });
  const root = await mkdtemp(join(artifactRoot, 'fixture-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const task = WINDOWS_SIX_TASKS.find(item => item.type === 'small-repair'), workspace = join(root, 'Work');
  await prepareSixWorkspace(task, workspace, repositoryRoot);
  const execute = () => spawnSync(process.execPath, ['--test', '--test-isolation=none', 'pending-work.test.mjs'],
    { cwd: workspace, encoding: 'utf8', timeout: 10000 });
  assert.equal(execute().status, 0);
  await writeFile(join(workspace, 'lib', 'pending-work.mjs'), CHANGED_PENDING_WORK);
  assert.equal(execute().status, 1);
  assert.equal(await readFile(join(workspace, 'pending-work.test.mjs'), 'utf8'), PENDING_WORK_TEST);
  const asserted = verifySixTask(task, { answers: [{ content: 'Fixed; tests passed.', status: 'completed' }],
    observations: task.requiredFiles.map(path => ({ name: 'filesystem.read', status: 'completed', arguments: { path } })),
    finalNodeTest: { exitCode: 0, testBytesPreserved: true } });
  assert.equal(asserted.checks.modelInvokedVerifiedSandbox, false);
  assert.equal(asserted.success, false);
});

test('source-grounded scores require observations and preserve missing-information boundaries', () => {
  const supported = WINDOWS_SIX_TASKS.find(item => item.type === 'document-question');
  const answer = 'scifact-31715818.md: magnetic nanoparticles and quantum dots for stem cell labeling and in vivo tracking';
  const sourceText = 'The review describes magnetic nanoparticles and quantum dots for stem cell labeling and in vivo tracking.';
  assert.equal(verifySixTask(supported, { answers: [{ content: answer, status: 'completed' }] }).success, false);
  const observations = [{ name: 'filesystem.read', status: 'completed', arguments: { path: 'scifact-31715818.md' },
    result: JSON.stringify({ path: 'scifact-31715818.md', content: sourceText,
      offset: 0, totalCharacters: sourceText.length, sha256: 'a'.repeat(64) }) }];
  assert.equal(verifySixTask(supported, { answers: [{ content: answer, status: 'completed' }], observations }).success, true);
  assert.equal(verifySixTask(supported, { answers: [{ content: answer, status: 'interrupted' }], observations }).success, false);
  const missing = WINDOWS_SIX_TASKS.find(item => item.type === 'insufficient-evidence');
  assert.equal(verifySixTask(missing, { answers: [{ content: 'scifact-31715818.md 未提供数值，无法确定。', status: 'completed' }], observations }).success, true);
  assert.equal(verifySixTask(missing, { answers: [{ content: 'scifact-31715818.md 未提供数值，可能是 15%。', status: 'completed' }], observations }).success, false);
  const explanation = WINDOWS_SIX_TASKS.find(item => item.type === 'cross-file-explanation');
  const assessed = verifySixTask(explanation, { answers: [{ content: 'Media merge', status: 'completed' }], observations:
    explanation.requiredFiles.map(path => ({ name: 'filesystem.read', status: 'completed', arguments: { path },
      result: JSON.stringify({ path, content: 'Actual source text.' }) })) });
  assert.equal(assessed.mechanicalPassed, true);
  assert.equal(assessed.success, null);
  assert.equal(assessed.semanticReview, 'pending');
  assert.equal(verifySixTask(supported, { answers: [{ content: answer, status: 'completed' }], observations: [{
    name: 'filesystem.read', status: 'completed', arguments: { path: 'scifact-31715818.md' },
    result: JSON.stringify({ path: 'unrelated.md', content: 'Different text.' }) }] }).success, false);
});

test('action readiness permits source reads and confirmed memory without requiring a completed vector index', () => {
  const localization = WINDOWS_SIX_TASKS.find(item => item.type === 'code-localization');
  const code = sixActionReadiness(localization, { filesReadable: true });
  assert.equal(code.available, true); assert.equal(code.required, 'workspace-read');
  assert.equal(code.semanticCompletionRequired, false);
  const document = WINDOWS_SIX_TASKS.find(item => item.type === 'document-question');
  const partial = sixActionReadiness(document, { filesReadable: true, lexicalPaths: [] });
  assert.equal(partial.available, true); assert.equal(partial.preferredReady, false);
  assert.equal(partial.effective, 'workspace-read');
  assert.equal(sixActionReadiness(document, { filesReadable: true, lexicalPaths: document.requiredFiles }).preferredReady, true);
  assert.equal(sixActionReadiness(document, { filesReadable: false }).available, false);
  assert.equal(sixActionReadiness(WINDOWS_SIX_TASKS.find(item => item.type === 'memory-and-scope')).required, 'confirmed-memory');
});

test('prepared execution admits real bounded embedding calls and preserves cancellation and original receipts', async () => {
  const requests = [], records = [], receipt = { vectors: [[1, 0], [0, 1]], embeddingSpaceId: 'fixture-space' };
  const options = { signal: new AbortController().signal, profileId: 'fixture' };
  const embeddings = { embedDocuments: async (documents, passed) => {
    requests.push(documents); assert.equal(passed, options); return receipt;
  } };
  observeSixDocumentEmbeddings(embeddings, records, 2);
  assert.equal(await embeddings.embedDocuments(['first', 'second'], options), receipt);
  assert.equal(records[0].completedVectors, 2);
  await assert.rejects(embeddings.embedDocuments(['third'], options), { code: 'SIX_DOCUMENT_EMBEDDING_BUDGET_REACHED' });
  assert.equal(requests.length, 1); assert.equal(records[1].status, 'budget-exhausted');
  const abort = new DOMException('Cancelled', 'AbortError'), failedRecords = [];
  const cancelled = { embedDocuments: async () => { throw abort; } };
  observeSixDocumentEmbeddings(cancelled, failedRecords, 2);
  await assert.rejects(cancelled.embedDocuments(['one'], options), error => error === abort);
  assert.equal(failedRecords[0].status, 'failed');
});

test('only source records actually sent as evidence are eligible, never user claims or navigation-only references', () => {
  const item = { sourceRef: 'ev1:fixture:01', excerpt: 'Actual evidence' };
  const calls = [{ number: 1, httpStatus: 200, syntheticLocalRequest: { messages: [
    { role: 'user', content: JSON.stringify({ ...item, excerpt: 'A user assertion' }) },
    { role: 'system', content: 'Instructions\n' + JSON.stringify(item) + '\n' + JSON.stringify({ ...item, navigationOnly: true }) }
  ] } }, { number: 2, httpStatus: 500, syntheticLocalRequest: { messages: [{ role: 'system', content: JSON.stringify(item) }] } }];
  assert.deepEqual(sentAutomaticEvidenceRecords(calls), [{ ...item, modelCallNumber: 1 }]);
});

test('verified automatic evidence can satisfy source access without relaxing quoted facts or complete-source absence checks', () => {
  const supported = WINDOWS_SIX_TASKS.find(item => item.type === 'document-question');
  const text = 'magnetic nanoparticles and quantum dots for stem cell labeling and in vivo tracking';
  const receipt = { channel: 'automatic-evidence', sourceValidated: true, deliveredToModel: true,
    relativePath: 'scifact-31715818.md', text, contentCharacters: text.length,
    contentHash: 'a'.repeat(64), sourceRevision: 1, offset: 0, totalCharacters: text.length };
  const answers = [{ content: `scifact-31715818.md: ${text}`, status: 'completed' }];
  assert.equal(verifySixTask(supported, { answers, automaticEvidence: [receipt] }).success, true);
  for (const patch of [{ sourceValidated: false }, { deliveredToModel: false }, { text: 'Only a heading' }])
    assert.equal(verifySixTask(supported, { answers, automaticEvidence: [{ ...receipt, ...patch }] }).success, false);
  const missing = WINDOWS_SIX_TASKS.find(item => item.type === 'insufficient-evidence');
  const negativeAnswer = [{ content: 'scifact-31715818.md 未提供数值，无法确定。', status: 'completed' }];
  assert.equal(verifySixTask(missing, { answers: negativeAnswer, automaticEvidence: [receipt] }).success, true);
  assert.equal(verifySixTask(missing, { answers: negativeAnswer,
    automaticEvidence: [{ ...receipt, totalCharacters: text.length + 100 }] }).success, false);
  const left = { ...receipt, text: text.slice(0, 20), contentCharacters: 20 };
  const right = { ...receipt, text: text.slice(20), contentCharacters: text.length - 20, offset: 20 };
  assert.equal(verifySixTask(missing, { answers: negativeAnswer, automaticEvidence: [left, right] }).success, true);
  assert.equal(verifySixTask(missing, { answers: negativeAnswer,
    automaticEvidence: [left, { ...right, sourceRevision: 2 }] }).success, false);
});

test('the evaluator loads production module boundaries without starting an index, terminal or model', async () => {
  const product = await loadSixProduct(repositoryRoot);
  assert.deepEqual(Object.keys(product), ['ModelRuntime', 'ConversationStore', 'MemoryService', 'ToolService', 'SandboxRunner', 'EmbeddingService']);
});

test('prepared retries accept failed model-only attempts but never replay tools, unknown effects or nonempty answers', () => {
  const prior = { taskId: 'node-pending-work-repair', executionStatus: 'error', modelCalls: [{ status: 'completed', nativeToolCalls: 0 }],
    observations: [], formalToolActivities: [], answers: [{ status: 'error', content: '' }],
    callAccounting: { modelRequests: 1, formalToolAttempts: 0 } };
  const messages = [{ Role: 'user', Content: 'Initial state.' }, { Role: 'user', Content: 'Repair task.' },
    { Role: 'assistant', Status: 'error', Content: '', ToolActivities: [] }];
  assert.equal(assertPreparedRetryAdmission(prior.taskId, prior, messages), true);
  for (const patch of [{ observations: [{ dispatched: true, status: 'unknown' }] },
    { formalToolActivities: [{ name: 'filesystem.write', status: 'completed' }] },
    { answers: [{ status: 'error', content: 'some actual answer' }] },
    { modelCalls: [{ status: 'completed', nativeToolCalls: 1 }] },
    { modelCalls: [{ status: 'unknown', nativeToolCalls: 0 }] }, { closeErrorCode: 'UNKNOWN_CLOSE_STATE' }])
    assert.throws(() => assertPreparedRetryAdmission(prior.taskId, { ...prior, ...patch }, messages), { code: 'SIX_PREPARED_RETRY_NOT_SAFE' });
  const toolMessage = structuredClone(messages); toolMessage[2].ModelTranscript = { rounds: [{ calls: [{ name: 'filesystem.write' }] }] };
  assert.throws(() => assertPreparedRetryAdmission(prior.taskId, prior, toolMessage), { code: 'SIX_PREPARED_RETRY_NOT_SAFE' });
});

test('prepared reuse verifies real work/test bytes and preserves the original failure receipt', async t => {
  await mkdir(artifactRoot, { recursive: true });
  const root = await mkdtemp(join(artifactRoot, 'prepared-')); t.after(() => rm(root, { recursive: true, force: true }));
  const task = WINDOWS_SIX_TASKS.find(item => item.type === 'small-repair'), workspace = join(root, 'Work');
  const fixture = await prepareSixWorkspace(task, workspace, repositoryRoot);
  await writeFile(join(workspace, 'lib/pending-work.mjs'), CHANGED_PENDING_WORK);
  const priorBytes = JSON.stringify({ taskId: task.id, fixture: fixture.provenance });
  await writeFile(join(root, 'result.json'), priorBytes);
  const options = { preparedRoot: root, cacheRoot: repositoryRoot };
  assert.equal((await readPreparedFixture(task, options)).workFileCount, 2);
  await writeFile(join(workspace, 'pending-work.test.mjs'), PENDING_WORK_TEST + '\n');
  await assert.rejects(readPreparedFixture(task, options), { code: 'SIX_PREPARED_WORK_CHANGED' });
  assert.equal(await readFile(join(root, 'result.json'), 'utf8'), priorBytes);
});

test('Windows prepared links accept separator changes only for the same contained relative target', () => {
  const linkPath = 'docs/_theme/djangodocs-epub/static/docicons-note.png';
  const expected = [{ path: linkPath, link: '../../djangodocs/static/docicons-note.png' }];
  const actual = [{ path: linkPath,
    link: normalizePreparedLinkTarget(repositoryRoot, linkPath, '..\\..\\djangodocs\\static\\docicons-note.png', 'win32') }];
  assert.doesNotThrow(() => assertPreparedManifestMatches(actual, expected));
});

test('Windows prepared links still reject changed targets, repository escapes and changed ordinary file bytes', () => {
  const linkPath = 'docs/_theme/djangodocs-epub/static/docicons-note.png';
  const expected = [{ path: linkPath, link: '../../djangodocs/static/docicons-note.png' }];
  const changed = [{ path: linkPath,
    link: normalizePreparedLinkTarget(repositoryRoot, linkPath, '../../djangodocs/static/docicons-warning.png', 'win32') }];
  assert.throws(() => assertPreparedManifestMatches(changed, expected), { code: 'SIX_PREPARED_WORK_CHANGED' });
  for (const target of ['../../../../../outside.png', join(repositoryRoot, 'inside.png')])
    assert.throws(() => normalizePreparedLinkTarget(repositoryRoot, linkPath, target, 'win32'),
      { code: 'SIX_PREPARED_WORK_LINK_OUTSIDE_REPOSITORY' });
  assert.throws(() => assertPreparedManifestMatches([{ path: 'source.py', bytes: 1, sha256: 'changed' }],
    [{ path: 'source.py', bytes: 1, sha256: 'original' }]), { code: 'SIX_PREPARED_WORK_CHANGED' });
});

test('repeated empty failures remain resumable only within the same fixture task and no-effect contract', () => {
  const task = WINDOWS_SIX_TASKS.find(item => item.type === 'insufficient-evidence');
  const prior = { taskId: task.id, executionStatus: 'error', modelCalls: [], observations: [],
    formalToolActivities: [], answers: [{ status: 'error', content: '' }],
    callAccounting: { modelRequests: 0, formalToolAttempts: 0 } };
  const emptyFailure = { Role: 'assistant', Status: 'error', Content: '', ToolActivities: [] };
  const messages = [{ Role: 'user', Content: 'Windows isolated acceptance fixture.' },
    { Role: 'user', Content: task.instruction }, emptyFailure,
    { Role: 'user', Content: task.instruction }, structuredClone(emptyFailure)];
  assert.equal(assertPreparedRetryAdmission(task.id, prior, messages), true);
  for (const patch of [{ Content: 'Already answered.' }, { Status: 'unknown' },
    { ToolActivities: [{ name: 'filesystem.write', status: 'completed' }] }]) {
    const changed = structuredClone(messages); Object.assign(changed[4], patch);
    assert.throws(() => assertPreparedRetryAdmission(task.id, prior, changed), { code: 'SIX_PREPARED_RETRY_NOT_SAFE' });
  }
  const changedTask = structuredClone(messages); changedTask[3].Content = 'Different user task.';
  assert.throws(() => assertPreparedRetryAdmission(task.id, prior, changedTask), { code: 'SIX_PREPARED_RETRY_NOT_SAFE' });
});

test('native resource observations preserve exact request/options and original lease identity', async () => {
  const request = { cpuThreads: 1, memoryBytes: 16, gpuMemoryBytes: 0 }, options = { signal: new AbortController().signal };
  const denied = { status: 'denied', reason: 'RESOURCE_WAIT_TIMEOUT', mode: 'rust' };
  const granted = { status: 'granted', leaseId: 'real-lease' }, records = [];
  let snapshots = 0, nextLease = denied;
  const resources = { status: () => ({ mode: 'rust' }), snapshot: async () => { snapshots++; return { budget: { memoryBytes: 8 } }; },
    acquire: async (actualRequest, actualOptions) => { assert.equal(actualRequest, request); assert.equal(actualOptions, options); return nextLease; } };
  observeSixResourceAcquisitions(resources, records);
  assert.equal(await resources.acquire(request, options), denied);
  assert.equal(records[0].observation.snapshot.budget.memoryBytes, 8);
  nextLease = granted;
  assert.equal(await resources.acquire(request, options), granted);
  assert.equal(snapshots, 1);
  assert.equal(records[1].observation, undefined);
});

test('resource observation failures never replace the original thrown object or trigger fallback sampling', async () => {
  const original = Object.assign(new Error('Original acquisition failure.'), { code: 'RESOURCE_ORIGINAL_FAILURE' });
  const records = [];
  const resources = { status: () => ({ mode: 'rust' }),
    snapshot: async () => { throw Object.assign(new Error('Sampling failed.'), { code: 'RESOURCE_SAMPLING_FAILED' }); },
    acquire: async () => { throw original; } };
  observeSixResourceAcquisitions(resources, records);
  await assert.rejects(resources.acquire({ cpuThreads: 1 }), error => error === original);
  assert.equal(records[0].errorCode, 'RESOURCE_ORIGINAL_FAILURE');
  assert.equal(records[0].observation.errorCode, 'RESOURCE_SAMPLING_FAILED');
  resources.status = () => ({ mode: 'fallback' });
  resources.snapshot = async () => { assert.fail('Fallback observation must not trigger service recovery.'); };
  await assert.rejects(resources.acquire({ cpuThreads: 1 }), error => error === original);
  assert.equal(records[1].observation.status, 'not-sampled-mode');
});

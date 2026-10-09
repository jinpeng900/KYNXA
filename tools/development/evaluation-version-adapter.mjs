import { createHash, randomUUID } from 'node:crypto';
import { mkdir, readFile, writeFile, appendFile, lstat } from 'node:fs/promises';
import { resolve, join, relative, isAbsolute, sep } from 'node:path';
import { createEvaluationAdapter as createUnifiedEvaluationAdapter } from './unified-agent-evaluation-adapter.mjs';
import { evaluateRun } from './evaluate-unified-retrieval.mjs';
import { inspectLocalPath } from '../../apps/model-gateway/platform/tool-paths.mjs';

const LEGACY_KIND = 'legacy-localization-v1';
const LEGACY_METRIC = 'patch-target coverage; reference patch paths are not exhaustive relevance gold';
const MAX_DOCUMENT_BYTES = 16 * 1024 * 1024;
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');
const failure = (code, message) => Object.assign(new Error(message), { code });
const isHash = value => typeof value === 'string' && /^[a-f0-9]{64}$/u.test(value);
const inside = (root, path) => { const child = relative(root, path); return !child || !isAbsolute(child) && child !== '..' && !child.startsWith(`..${sep}`); };

function frozenJson(bytes) {
  if (!Buffer.isBuffer(bytes) || bytes.length > MAX_DOCUMENT_BYTES)
    throw failure('EVALUATION_FREEZE_INVALID', 'Bounded original document bytes are required. / 必须提供有界原始文档字节。');
  try { return JSON.parse(bytes.toString('utf8').replace(/^\uFEFF/u, '')); }
  catch { throw failure('EVALUATION_FREEZE_INVALID', 'Frozen JSON cannot be parsed. / 冻结 JSON 无法解析。'); }
}

function canonical(value) {
  if (Array.isArray(value)) return value.map(canonical);
  if (value && typeof value === 'object') return Object.fromEntries(Object.keys(value).sort().map(key => [key, canonical(value[key])]));
  return value;
}
const contractHash = value => sha256(JSON.stringify(canonical(value)));
function validateFrozenLegacy(document) {
  const { suiteHash, ...suite } = document;
  if (!isHash(suiteHash) || contractHash(suite) !== suiteHash)
    throw failure('EVALUATION_LEGACY_CONTRACT_CHANGED', 'Frozen legacy contract changed. / 冻结旧评测合同已变化。');
}

/** Preserve complete public issues and the original scoring/configuration hashes; selection-only labels never reach tasks.
 * 保留完整公开问题及原评分、配置哈希；选样专用标签不进入任务输入。 */
export function freezeLegacyLocalizationSuite({ publicCasesBytes, selectionBytes, scoringDefinitionBytes,
  configurationBytes, translationBytes, datasetRevision }) {
  const publicDocument = frozenJson(publicCasesBytes), selection = frozenJson(selectionBytes), configuration = frozenJson(configurationBytes);
  if (!Buffer.isBuffer(scoringDefinitionBytes) || !scoringDefinitionBytes.length || scoringDefinitionBytes.length > MAX_DOCUMENT_BYTES ||
      typeof datasetRevision !== 'string' || !datasetRevision || !Array.isArray(publicDocument.cases) ||
      !publicDocument.cases.length || publicDocument.cases.length > 10000 || selection.selection_only !== true ||
      !Array.isArray(selection.cases) || selection.case_count !== publicDocument.cases.length ||
      selection.model_public_file_sha256 !== sha256(publicCasesBytes) || typeof selection.seed !== 'string' || !selection.seed)
    throw failure('EVALUATION_LEGACY_CONTRACT_INVALID', 'Public cases, selection receipt and scoring definition must be frozen together. / 公开问题、选样回执与评分定义必须一起冻结。');
  const selected = new Map(selection.cases.map(value => [value.instance_id, value]));
  if (selected.size !== publicDocument.cases.length) throw failure('EVALUATION_LEGACY_CONTRACT_INVALID', 'Selection IDs are duplicated or incomplete.');
  const ids = new Set();
  const tasks = publicDocument.cases.map(value => {
    const receipt = selected.get(value.instance_id);
    if (Object.keys(value).sort().join(',') !== 'base_commit,instance_id,problem_statement,repo' ||
        typeof value.instance_id !== 'string' || !value.instance_id || value.instance_id.length > 512 || ids.has(value.instance_id) ||
        typeof value.repo !== 'string' || !/^[a-f0-9]{40}$/u.test(value.base_commit) ||
        typeof value.problem_statement !== 'string' || !value.problem_statement ||
        receipt?.repo !== value.repo || receipt?.base_commit !== value.base_commit ||
        receipt?.problem_sha256 !== sha256(value.problem_statement))
      throw failure('EVALUATION_LEGACY_ISSUE_CHANGED', 'Public issue identity/content differs from its frozen receipt. / 公开问题身份或内容与冻结回执不一致。');
    ids.add(value.instance_id);
    return { id: value.instance_id, repo: value.repo, baseCommit: value.base_commit,
      query: value.problem_statement, querySha256: sha256(value.problem_statement) };
  });
  if (translationBytes) {
    const translated = frozenJson(translationBytes);
    if (translated.source_public_file_sha256 !== sha256(publicCasesBytes) || !Array.isArray(translated.cases) ||
        translated.case_count !== tasks.length || translated.cases.length !== tasks.length)
      throw failure('EVALUATION_LEGACY_TRANSLATION_UNFROZEN', 'Translated issues must reference the same frozen public cases.');
    const pairs = new Map(translated.cases.map(value => [value.instance_id, value]));
    if (pairs.size !== tasks.length) throw failure('EVALUATION_LEGACY_TRANSLATION_UNFROZEN', 'Translated issue IDs are duplicated.');
    for (const task of tasks) {
      const pair = pairs.get(task.id);
      if (pair?.original_problem_sha256 !== task.querySha256 || typeof pair.problem_statement_zh !== 'string' || !pair.problem_statement_zh)
        throw failure('EVALUATION_LEGACY_TRANSLATION_UNFROZEN', 'Translated issue does not match its original.');
      task.translations = { zh: { query: pair.problem_statement_zh, querySha256: sha256(pair.problem_statement_zh) } };
    }
  }
  const model = configuration.model;
  if (typeof model?.tag !== 'string' || !model.tag || !isHash(model.manifestSha256) ||
      !isHash(model.modelBlobSha256) || typeof model.think !== 'boolean' ||
      !Number.isSafeInteger(model.inputTokenSoftCap) || model.inputTokenSoftCap < 1 ||
      !Number.isSafeInteger(model.options?.num_ctx) || !Number.isSafeInteger(model.options?.num_predict))
    throw failure('EVALUATION_LEGACY_MODEL_INVALID', 'A fixed legacy model identity and original budgets are required. / 必须固定旧模型身份与原预算。');
  const options = Object.fromEntries(['num_ctx', 'num_predict', 'num_batch', 'num_thread', 'temperature', 'seed']
    .filter(key => model.options[key] !== undefined).map(key => [key, model.options[key]]));
  if (Object.values(options).some(value => typeof value !== 'number' || !Number.isFinite(value) || value < 0))
    throw failure('EVALUATION_LEGACY_MODEL_INVALID', 'Legacy model options are invalid.');
  const maximumAutonomousCalls = configuration.arms?.autonomous_max10_model_calls?.maxModelCalls;
  if (!Number.isSafeInteger(maximumAutonomousCalls) || maximumAutonomousCalls < 1 || maximumAutonomousCalls > 1000)
    throw failure('EVALUATION_LEGACY_MODEL_INVALID', 'Legacy autonomous call budget is missing.');
  const suite = { version: 1, kind: LEGACY_KIND, datasetRevision, model: model.tag, seed: selection.seed, tasks,
    modelIdentity: { manifestSha256: model.manifestSha256, blobSha256: model.modelBlobSha256,
      quantization: model.quantization ?? null },
    runtime: { wireProtocol: 'ollama-json-action', options, think: model.think, inputTokenSoftCap: model.inputTokenSoftCap,
      singleModelCalls: 1, maximumAutonomousCalls, languages: translationBytes ? ['en', 'zh'] : ['en'], policies: ['A', 'B'] },
    sourceHashes: { publicCases: sha256(publicCasesBytes), selection: sha256(selectionBytes),
      scoringDefinition: sha256(scoringDefinitionBytes), configuration: sha256(configurationBytes),
      translation: translationBytes ? sha256(translationBytes) : null },
    scoring: { definitionId: 'legacy-patch-target-coverage-v1', metric: LEGACY_METRIC,
      codeRepairSuccess: null, exhaustiveRelevanceGold: false },
    compatibility: { unifiedExecutionAdapter: false,
      reason: 'Legacy localization arms, JSON action wire, translated issues and A/B indexing policies require an explicit matching runner.' } };
  return Object.freeze({ ...suite, suiteHash: contractHash(suite) });
}

/** Dispatch only declared compatible versions; a migrated label must not silently change a benchmark.
 * 仅分派明确兼容的版本；更换清单标签不能悄悄改变原评测。 */
export async function createVersionedEvaluationAdapter(document, { legacyRunner, createUnifiedAdapter = createUnifiedEvaluationAdapter,
  unifiedOptions } = {}) {
  if (document?.kind !== LEGACY_KIND) {
    if (document?.version !== 1 || !Array.isArray(document.tasks))
      throw failure('EVALUATION_VERSION_UNSUPPORTED', 'Unsupported evaluation version. / 不支持此评测版本。');
    return createUnifiedAdapter(document, unifiedOptions);
  }
  const { suiteHash, ...suite } = document;
  if (contractHash(suite) !== suiteHash || legacyRunner?.fixedModel !== document.model ||
      legacyRunner?.protocolId !== document.runtime.wireProtocol ||
      legacyRunner?.configurationHash !== document.sourceHashes.configuration ||
      legacyRunner?.scoringDefinitionHash !== document.sourceHashes.scoringDefinition ||
      typeof legacyRunner.run !== 'function' || typeof legacyRunner.dispose !== 'function')
    throw failure('EVALUATION_LEGACY_RUNTIME_UNSUPPORTED', 'Legacy matrix needs an explicitly pinned compatible runner; new Agent scores cannot replace it. / 旧矩阵需要显式固定的兼容运行器，不能用新版 Agent 得分替代。');
  document = structuredClone(document);
  return { fixedModel: document.model, kind: LEGACY_KIND,
    async run({ task, legacyArm, isolation, ...control }) {
      const original = document.tasks.find(value => value.id === task?.id);
      if (!original || contractHash(task) !== contractHash(original) || !document.runtime.languages.includes(legacyArm?.language) ||
          !['A', 'B'].includes(legacyArm.policy) || !['single', 'autonomous'].includes(legacyArm.mode) ||
          isolation?.separateDataRoot !== true || isolation?.noSharedLegacyIndex !== true)
        throw failure('EVALUATION_LEGACY_RUN_INVALID', 'Original task, explicit arm and independent data/index roots are required.');
      return legacyRunner.run({ task: structuredClone(original), legacyArm: { ...legacyArm }, isolation: { ...isolation }, ...control });
    },
    dispose: () => legacyRunner.dispose() };
}

// Keep the existing CLI adapter factory contract while using explicit version dispatch.
// 保留现有 CLI 的适配器工厂接口，同时使用显式版本分派。
export function createEvaluationAdapter(document, options) {
  return createVersionedEvaluationAdapter(document, options);
}

/** Keep localization and independently verified execution scores separate, including unknown costs.
 * 文件定位与独立执行验收分别归一化，未知成本仍保持未知。 */
export function normalizeVersionedEvaluationResult({ contract, task, result, legacyRun, durationMs }) {
  if (contract?.kind !== LEGACY_KIND) return { version: 1, benchmarkKind: 'agent-execution', model: contract.model,
    datasetRevision: contract.datasetRevision, measurement: evaluateRun(task, result, durationMs) };
  validateFrozenLegacy(contract);
  const frozenTask = contract.tasks.find(value => value.id === task?.id);
  if (!frozenTask || contractHash(frozenTask) !== contractHash(task) || result?.instance_id !== task.id || result.metric !== LEGACY_METRIC ||
      result.config_sha256 !== contract.sourceHashes.configuration || legacyRun?.model !== contract.model ||
      legacyRun.manifest_sha256 !== contract.modelIdentity.manifestSha256 ||
      legacyRun.config_sha256 !== contract.sourceHashes.configuration || legacyRun.instance_id !== task.id ||
      legacyRun.base_commit !== task.baseCommit)
    throw failure('EVALUATION_LEGACY_RECEIPT_MISMATCH', 'Legacy score/model/configuration receipt does not match the frozen task. / 旧评分、模型或配置回执不匹配冻结任务。');
  const valid = result.source_version_valid === true && result.owned_processes_stopped === true;
  const finite = value => typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : null;
  const arms = Object.fromEntries(Object.entries(result.arms ?? {}).map(([id, arm]) => [id, {
    status: ['completed', 'failed', 'blocked', 'interrupted', 'preflight_failed'].includes(arm.status) ? arm.status : 'unknown',
    stopReason: ['model_final', 'call_budget_final', 'explicit_error_or_budget_stop', 'input_budget_unexecutable_complete_issue_retained'].includes(arm.stop_reason)
      ? arm.stop_reason : null,
    patchTargetCoverage: valid && finite(arm.patch_target_coverage) !== null && arm.patch_target_coverage <= 1 ? arm.patch_target_coverage : null,
    top5PatchTargetHit: valid && typeof arm.top5_patch_target_hit === 'boolean' ? arm.top5_patch_target_hit : null,
    bestPatchTargetRank: valid ? finite(arm.best_patch_target_rank) : null,
    inputTokens: finite(arm.prompt_tokens_total), outputTokens: finite(arm.output_tokens_total),
    modelCalls: finite(arm.model_calls), priorFailedModelCalls: finite(arm.prior_failed_harness_model_calls),
    durationMs: finite(arm.wall_seconds) === null ? null : arm.wall_seconds * 1000,
    taskPassed: null,
  }]));
  return { version: 1, benchmarkKind: LEGACY_KIND, model: contract.model, datasetRevision: contract.datasetRevision,
    taskId: task.id, suiteHash: contract.suiteHash, scoringDefinitionHash: contract.sourceHashes.scoringDefinition,
    measurement: { sourceVersionValid: result.source_version_valid === true, ownedExecutorsSettled: result.owned_processes_stopped === true,
      metric: LEGACY_METRIC, arms, taskPassed: null, codeRepairSuccess: null } };
}

/** A separate append-only run journal records acknowledged stops; unfinished attempts are never silently replayed.
 * 独立追加日志保存停止确认；未完成尝试不能静默重放，不复用旧索引或历史得分。
 */
export async function createEvaluationRunJournal({ outputRoot, contract, runId, resumeRunId,
  recoveryReceipts = [], protectedRoots = [resolve('test/agent-benchmark-20261008')] }) {
  outputRoot = resolve(outputRoot);
  contract = structuredClone(contract);
  if (contract.kind === LEGACY_KIND) validateFrozenLegacy(contract);
  if (protectedRoots.some(root => inside(resolve(root), outputRoot)))
    throw failure('EVALUATION_OUTPUT_NOT_ISOLATED', 'New runs cannot write into the legacy benchmark directory. / 新运行不能写入旧评测目录。');
  const selectedRunId = resumeRunId ?? runId ?? randomUUID();
  if (typeof selectedRunId !== 'string' || !/^[A-Za-z0-9_-]{1,96}$/u.test(selectedRunId))
    throw failure('EVALUATION_RUN_ID_INVALID', 'Invalid evaluation run ID.');
  const directory = join(outputRoot, selectedRunId), contractPath = join(directory, 'contract.json'), eventsPath = join(directory, 'events.jsonl');
  const identity = { version: 1, contractHash: contractHash(contract), model: contract.model,
    datasetRevision: contract.datasetRevision, benchmarkKind: contract.kind ?? 'agent-execution',
    scoringDefinitionHash: contract.sourceHashes?.scoringDefinition ?? null };
  await inspectLocalPath(outputRoot, { allowMissing: true });
  await mkdir(outputRoot, { recursive: true });
  await inspectLocalPath(outputRoot);
  let stopped = false, sequence = 0, closed = false, journalBytes = 0, queue = Promise.resolve();
  const attempts = new Map(), controller = new AbortController();
  let pendingRecovery = [];
  function apply(event) {
    if (event.type === 'attempt-started') attempts.set(event.attemptId, { ...event, status: 'running' });
    else if (event.type === 'attempt-finished') attempts.set(event.attemptId, { ...attempts.get(event.attemptId), ...event });
    else if (event.type === 'stop-requested') stopped = true;
    else if (event.type === 'resumed') stopped = false;
  }
  if (resumeRunId) {
    await inspectLocalPath(directory); await inspectLocalPath(contractPath); await inspectLocalPath(eventsPath);
    const info = await lstat(eventsPath);
    if (!info.isFile() || info.isSymbolicLink() || info.size > MAX_DOCUMENT_BYTES)
      throw failure('EVALUATION_JOURNAL_INVALID', 'Evaluation journal is unavailable or exceeds its bound.');
    journalBytes = info.size;
    const previous = frozenJson(await readFile(contractPath));
    if (contractHash(previous) !== contractHash(identity))
      throw failure('EVALUATION_RESUME_CONTRACT_CHANGED', 'Model, tasks, scoring or configuration changed; begin a separate run. / 模型、题目、评分或配置已变化，必须另建运行。');
    const log = await readFile(eventsPath, 'utf8');
    if (log && !log.endsWith('\n')) throw failure('EVALUATION_JOURNAL_INVALID', 'Incomplete event remains preserved; no implicit repair or replay.');
    for (const line of log.split('\n').filter(Boolean)) {
      let event; try { event = JSON.parse(line); } catch { throw failure('EVALUATION_JOURNAL_INVALID', 'Malformed event remains preserved.'); }
      if (event.sequence !== ++sequence || !['attempt-started', 'attempt-finished', 'stop-requested', 'resumed'].includes(event.type))
        throw failure('EVALUATION_JOURNAL_INVALID', 'Invalid evaluation event sequence.');
      if (event.type === 'attempt-started' && (!contract.tasks?.some(task => task.id === event.taskId) || attempts.has(event.attemptId) ||
          typeof event.arm !== 'string' || typeof event.attemptId !== 'string') ||
          event.type === 'attempt-finished' && (attempts.get(event.attemptId)?.status !== 'running' || event.ownedExecutorsSettled !== true ||
            !['completed', 'failed', 'blocked', 'stopped'].includes(event.status)))
        throw failure('EVALUATION_JOURNAL_INVALID', 'Invalid attempt transition in preserved evaluation events.');
      apply(event);
    }
    const unsettled = [...attempts.values()].filter(attempt => attempt.status === 'running' || !attempt.ownedExecutorsSettled);
    if (unsettled.length) {
      const receipts = new Map(recoveryReceipts.map(receipt => [receipt.attemptId, receipt]));
      if (receipts.size !== unsettled.length || recoveryReceipts.length !== unsettled.length || unsettled.some(attempt => {
        const receipt = receipts.get(attempt.attemptId);
        return receipt?.origin !== 'independent-host-executor-reconciliation' || receipt.ownedExecutorsSettled !== true ||
          !isHash(receipt.receiptHash) || !['failed', 'blocked', 'stopped'].includes(receipt.status);
      })) throw failure('EVALUATION_RESUME_UNCONFIRMED', 'An interrupted attempt has no confirmed executor settlement; it cannot be replayed. / 中断尝试没有执行器结束确认，不能重放。');
      pendingRecovery = unsettled.map(attempt => ({ type: 'attempt-finished', attemptId: attempt.attemptId,
        status: receipts.get(attempt.attemptId).status, ownedExecutorsSettled: true,
        reconciliationReceiptHash: receipts.get(attempt.attemptId).receiptHash }));
    } else if (recoveryReceipts.length) throw failure('EVALUATION_RESUME_UNCONFIRMED', 'Recovery receipts do not identify pending attempts.');
  } else {
    await mkdir(directory);
    await writeFile(contractPath, `${JSON.stringify(identity, null, 2)}\n`, { flag: 'wx' });
    await writeFile(eventsPath, '', { flag: 'wx' });
  }
  function append(payloadFactory) {
    const operation = queue.then(async () => {
      if (closed) throw failure('EVALUATION_JOURNAL_CLOSED', 'Evaluation journal is closed.');
      const payload = typeof payloadFactory === 'function' ? payloadFactory() : payloadFactory;
      const event = { ...payload, sequence: sequence + 1, utc: new Date().toISOString() };
      const frame = `${JSON.stringify(event)}\n`;
      if (journalBytes + Buffer.byteLength(frame) > MAX_DOCUMENT_BYTES)
        throw failure('EVALUATION_JOURNAL_LIMIT', 'Evaluation journal reached its explicit byte limit.');
      try { await appendFile(eventsPath, frame); }
      catch (error) { closed = true; throw error; }
      sequence++; journalBytes += Buffer.byteLength(frame);
      apply(event); return event;
    });
    queue = operation.catch(() => {}); return operation;
  }
  // Only the harness owner may supply independent host reconciliation; a model result never confirms process exit.
  // 仅运行器拥有者可提供独立宿主核对回执；模型返回文字不能证明进程已经结束。
  for (const receipt of pendingRecovery) await append(receipt);
  if (resumeRunId) await append({ type: 'resumed' });
  return { directory, signal: controller.signal,
    status: () => ({ stopped, activeAttempts: [...attempts.values()].filter(attempt => attempt.status === 'running').length,
      attempts: [...attempts.values()].map(attempt => ({ ...attempt })) }),
    async startAttempt({ taskId, arm, attemptId = randomUUID() }) {
      await append(() => {
        if (stopped || !contract.tasks?.some(task => task.id === taskId) || typeof arm !== 'string' || !/^[A-Za-z0-9:_-]{1,96}$/u.test(arm) ||
            typeof attemptId !== 'string' || !/^[A-Za-z0-9_-]{1,96}$/u.test(attemptId) || attempts.has(attemptId) ||
            [...attempts.values()].some(attempt => attempt.taskId === taskId && attempt.arm === arm && ['running', 'completed'].includes(attempt.status)))
          throw failure('EVALUATION_ATTEMPT_INVALID', 'Stopped, completed or active tasks cannot be silently repeated.');
        return { type: 'attempt-started', taskId, arm, attemptId };
      }); return attemptId;
    },
    async finishAttempt({ attemptId, status, ownedExecutorsSettled, resultHash = null }) {
      await append(() => {
        if (attempts.get(attemptId)?.status !== 'running' || !['completed', 'failed', 'blocked', 'stopped'].includes(status) ||
            ownedExecutorsSettled !== true || resultHash !== null && !isHash(resultHash))
          throw failure('EVALUATION_ATTEMPT_UNCONFIRMED', 'A terminal receipt requires confirmed executor settlement.');
        return { type: 'attempt-finished', attemptId, status, ownedExecutorsSettled, resultHash };
      });
    },
    async requestStop(reason = 'user-request') {
      if (!['user-request', 'resource-pressure', 'shutdown', 'timeout'].includes(reason))
        throw failure('EVALUATION_STOP_INVALID', 'Unknown stop reason.');
      try { await append({ type: 'stop-requested', reason }); }
      finally { controller.abort(); }
    },
    async close() { await queue; closed = true; },
  };
}

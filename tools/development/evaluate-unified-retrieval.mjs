import { createHash } from 'node:crypto';
import { readFile, mkdir, writeFile } from 'node:fs/promises';
import { cpus, totalmem, platform, arch } from 'node:os';
import { resolve, dirname } from 'node:path';
import { pathToFileURL } from 'node:url';
import { execFileSync } from 'node:child_process';
import { parseArgs } from 'node:util';

export const RETRIEVAL_ABLATIONS = Object.freeze([
  { id: 'baseline', gaps: false, adaptiveResources: false, relations: false, experience: false },
  { id: 'gap-only', gaps: true, adaptiveResources: false, relations: false, experience: false },
  { id: 'resource-only', gaps: false, adaptiveResources: true, relations: false, experience: false },
  { id: 'gap-resource', gaps: true, adaptiveResources: true, relations: false, experience: false },
  { id: 'integrated', gaps: true, adaptiveResources: true, relations: true, experience: true }
]);

const hash = value => createHash('sha256').update(typeof value === 'string' ? value : JSON.stringify(value)).digest('hex');
const finite = value => typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : null;
const mean = values => values.length ? values.reduce((sum, value) => sum + value, 0) / values.length : null;
const percentile = (values, fraction) => values.length ? [...values].sort((a, b) => a - b)[Math.min(values.length - 1, Math.ceil(values.length * fraction) - 1)] : null;

function validateManifest(manifest) {
  if (manifest?.version !== 1 || typeof manifest.model !== 'string' || !manifest.model ||
      typeof manifest.datasetRevision !== 'string' || !manifest.datasetRevision ||
      !Number.isSafeInteger(manifest.seed) || !Array.isArray(manifest.tasks) || !manifest.tasks.length || manifest.tasks.length > 10000)
    throw new Error('A fixed model, dataset revision, seed and task manifest are required. / 必须固定模型、数据版本、随机种子和任务清单。');
  const seen = new Set();
  for (const task of manifest.tasks) {
    if (typeof task.id !== 'string' || !task.id || seen.has(task.id) || !['development', 'heldout'].includes(task.split) ||
        typeof task.query !== 'string' || !task.query || task.query.length > 262144 ||
        !Array.isArray(task.expectedSourceIds) || !Array.isArray(task.acceptanceCheckIds))
      throw new Error('Invalid or duplicated task. / 任务无效或重复；超长问题必须明确拒绝，不能静默截断。');
    seen.add(task.id);
  }
}

/** Score retrieval and execution separately; unknown costs/checks are never manufactured as zero/pass.
 * 检索与执行分别计分；未知成本或验收不能伪造为零开销或通过。 */
export function evaluateRun(task, result = {}, durationMs) {
  const hits = new Set((result.sourceIds ?? []).slice(0, 20));
  const expected = new Set(task.expectedSourceIds);
  const recallAt20 = expected.size ? [...expected].filter(id => hits.has(id)).length / expected.size : null;
  const receipts = new Map((result.acceptanceReceipts ?? []).filter(receipt => receipt.origin === 'independent-validator')
    .map(receipt => [receipt.id, receipt]));
  const checks = task.acceptanceCheckIds.map(id => ({ id, state: !receipts.has(id) ? 'unknown' :
    receipts.get(id).passed === true && receipts.get(id).exitCode === 0 ? 'passed' : 'failed' }));
  const taskPassed = checks.length && checks.every(check => check.state !== 'unknown') ? checks.every(check => check.state === 'passed') : null;
  // Every retry is counted. Provider billing and estimates stay distinct.
  // 重试全部计入；供应商实际计费 token 与估算值保持分开。
  const attempts = result.attempts ?? [];
  const total = field => attempts.length && attempts.every(attempt => finite(attempt[field]) !== null)
    ? attempts.reduce((sum, attempt) => sum + attempt[field], 0) : null;
  return { id: task.id, split: task.split, recallAt20, taskPassed, checks, durationMs,
    executionPolicy: Object.fromEntries(['flags', 'fixedBudget', 'resourceMode', 'cpuThreadsLimit', 'batchBudget', 'retrievalBudgets']
      .filter(key => result.executionPolicy?.[key] !== undefined).map(key => [key, result.executionPolicy[key]])),
    sourceBindings: (result.sourceBindings ?? []).slice(0, 1000).map(item => Object.fromEntries(
      ['stableSourceId', 'sourceId', 'contentHash', 'relativePath'].filter(key => item[key] !== undefined).map(key => [key, item[key]]))),
    firstUsefulWorkMs: finite(result.firstUsefulWorkMs), peakRssBytes: finite(result.peakRssBytes),
    peakGpuBytes: finite(result.peakGpuBytes), inputTokens: total('inputTokens'), outputTokens: total('outputTokens'),
    estimatedGeneratedTokens: finite(result.estimatedGeneratedTokens), modelCalls: total('modelCalls'),
    toolCalls: total('toolCalls'), attemptCount: attempts.length, errorCode: result.errorCode ?? null,
    coldStartMs: finite(result.coldStartMs), indexPreparationMs: finite(result.indexPreparationMs),
    retrievalMs: finite(result.retrievalMs), modelMs: finite(result.modelMs),
    observedScopeViolations: finite(result.observedScopeViolations), verifiedSources: result.verifiedSources === true };
}

export function summarizeRuns(runs) {
  const measured = field => runs.map(run => run[field]).filter(value => value !== null && value !== undefined);
  return { tasks: runs.length, recallAt20: mean(measured('recallAt20')),
    verifiedTaskSuccessRate: mean(measured('taskPassed').map(Boolean).map(Number)),
    tasksWithIndependentChecks: measured('taskPassed').length, tasksWithoutIndependentChecks: runs.filter(run => run.taskPassed === null).length,
    durationP50Ms: percentile(measured('durationMs'), 0.5), durationP95Ms: percentile(measured('durationMs'), 0.95),
    firstUsefulWorkP95Ms: percentile(measured('firstUsefulWorkMs'), 0.95),
    measuredInputTokens: measured('inputTokens').reduce((sum, value) => sum + value, 0),
    measuredOutputTokens: measured('outputTokens').reduce((sum, value) => sum + value, 0),
    tasksWithUnknownTokenCost: runs.filter(run => run.inputTokens === null || run.outputTokens === null).length,
    measuredRetries: runs.reduce((sum, run) => sum + Math.max(0, run.attemptCount - 1), 0),
    observedScopeViolations: measured('observedScopeViolations').reduce((sum, value) => sum + value, 0),
    tasksWithoutScopeMeasurement: runs.filter(run => run.observedScopeViolations === null).length };
}

/** Adapters must explicitly support isolated ablations; a label alone is not an experiment.
 * 适配器必须明确支持隔离消融；只改标签并不构成实验。 */
export async function evaluatePlan(manifest, adapter, { smoke = false } = {}) {
  validateManifest(manifest);
  if (typeof adapter?.run !== 'function' || typeof adapter?.dispose !== 'function' || adapter.fixedModel !== manifest.model ||
      RETRIEVAL_ABLATIONS.some(variant => !adapter.supportedAblations?.includes(variant.id)))
    throw new Error('Adapter must pin the same model and support all isolated variants. / 适配器必须固定同一模型并支持各组隔离。');
  const tasks = smoke ? manifest.tasks.filter(task => task.split === 'development').slice(0, 2) : manifest.tasks;
  if (!tasks.length || !smoke && !tasks.some(task => task.split === 'heldout'))
    throw new Error('Full evaluation requires held-out tasks. / 完整评测必须有独立留出任务。');
  const variants = new Map(RETRIEVAL_ABLATIONS.map(variant => [variant.id, []]));
  try {
    for (const [taskIndex, task] of tasks.entries()) {
      // Rotate order to reduce systematic cold-start/load ordering bias.
      // 轮换执行顺序，减少冷启动或后台负载总落在同一组的偏差。
      for (let offset = 0; offset < RETRIEVAL_ABLATIONS.length; offset++) {
        const variant = RETRIEVAL_ABLATIONS[(offset + taskIndex + Math.abs(manifest.seed)) % RETRIEVAL_ABLATIONS.length];
        const started = performance.now();
        let result;
        try { result = await adapter.run({ task: structuredClone(task), variant, seed: manifest.seed,
          isolation: { separateDataRoot: true, waitForPreviousExecutors: true, noHeavyPolling: true } }); }
        catch (error) { result = { errorCode: typeof error.code === 'string' ? error.code : 'EVALUATION_RUN_FAILED' }; }
        variants.get(variant.id).push(evaluateRun(task, result, performance.now() - started));
      }
    }
  } finally { await adapter.dispose(); }
  const complete = !smoke && [...variants.values()].every(runs => runs.every(run => !run.errorCode &&
    (!run.checks.length || run.taskPassed !== null)));
  return { version: 1, mode: smoke ? 'smoke' : 'full', qualityBenchmarkCompleted: complete,
    model: manifest.model, datasetRevision: manifest.datasetRevision, seed: manifest.seed,
    manifestHash: hash(manifest), generatedAt: new Date().toISOString(),
    hardware: { platform: platform(), architecture: arch(), logicalCpu: cpus().length, memoryBytes: totalmem() },
    variants: Object.fromEntries([...variants].map(([id, runs]) => [id, { summary: summarizeRuns(runs), runs }])),
    note: 'Ablation does not certify originality or reproduce a paper. Retrieval scores do not certify task completion. / 消融不证明原创性或论文复现，检索得分不证明任务完成。' };
}

async function main() {
  const { values } = parseArgs({ options: { manifest: { type: 'string' }, adapter: { type: 'string' },
    output: { type: 'string' }, smoke: { type: 'boolean', default: false } } });
  if (!values.manifest || !values.adapter || !values.output) throw new Error('Use --manifest --adapter --output [--smoke].');
  const manifest = JSON.parse(await readFile(resolve(values.manifest), 'utf8'));
  const module = await import(pathToFileURL(resolve(values.adapter)).href);
  const report = await evaluatePlan(manifest, await module.createEvaluationAdapter(manifest), { smoke: values.smoke });
  try { report.commit = execFileSync('git', ['rev-parse', 'HEAD'], { encoding: 'utf8', windowsHide: true }).trim(); }
  catch { report.commit = null; }
  report.adapterHash = hash(await readFile(resolve(values.adapter), 'utf8'));
  const destination = resolve(values.output); await mkdir(dirname(destination), { recursive: true });
  await writeFile(destination, `${JSON.stringify(report, null, 2)}\n`, 'utf8');
  process.stdout.write(`${JSON.stringify({ mode: report.mode, tasks: Object.values(report.variants)[0].runs.length,
    variants: report.variants ? Object.keys(report.variants).length : 0, report: destination })}\n`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href)
  main().catch(error => { process.stderr.write(`${error.message}\n`); process.exitCode = 1; });

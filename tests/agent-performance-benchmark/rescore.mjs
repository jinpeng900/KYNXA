import { createHash } from 'node:crypto';
import { readFile, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { VERIFIER_VERSION, answerLanguageDiagnostic, assessReviewerRelations, summarizeRuns } from './evaluation.mjs';

/**
 * Only reviewer relations are rechecked; stored citation/disk/date checks and observations stay untouched.
 * 仅复核人物关系，沿用原运行的引用/磁盘/日期检查和全部观察，不重建已清理的临时环境，也不调用模型。
 */
export function rescoreResults(original) {
  if (original?.schemaVersion !== 1 || !Array.isArray(original.runs)) throw new Error('INVALID_RESCORE_INPUT');
  const runs = original.runs.map(run => {
    const rescored = structuredClone(run);
    if (run.taskId === 'followup-entity') {
      const assessment = assessReviewerRelations(run.answers.at(-1)?.content);
      rescored.checks.entityRetained = assessment.entityRetained;
      rescored.success = run.executionStatus === 'completed' && !run.errorCode && !run.closeErrorCode && !run.cleanupErrorCode
        && Object.values(rescored.checks).every(Boolean);
      rescored.reviewerRelationDiagnostic = assessment;
    }
    if (run.taskId === 'english-source-chinese-answer') {
      const assessment = assessReviewerRelations(run.answers.at(-1)?.content);
      rescored.checks.goldFacts = run.checks.goldFacts && assessment.entityRetained;
      rescored.success = run.executionStatus === 'completed' && !run.errorCode && !run.closeErrorCode && !run.cleanupErrorCode
        && Object.values(rescored.checks).every(Boolean);
      rescored.reviewerRelationDiagnostic = assessment;
    }
    if (run.taskId === 'dependent-file-cycle') rescored.languageDiagnostic = answerLanguageDiagnostic(run.answers.at(-1)?.content);
    return rescored;
  });
  return { ...structuredClone(original), verifierVersion: VERIFIER_VERSION, runs, summary: summarizeRuns(runs),
    configurations: Object.fromEntries([...new Set(runs.map(run => run.configuration))].map(configuration =>
      [configuration, summarizeRuns(runs.filter(run => run.configuration === configuration))])),
    rescoring: { version: 1, verifierVersion: VERIFIER_VERSION, sameModelRun: true, newModelCalls: 0,
      changedChecks: ['followup-entity.entityRetained', 'english-source-chinese-answer.goldFacts (relation tightening only)'],
      reason: 'Require affirmed ORION=Mara Chen; reject denied or contradictory gold and affirmed ORION=Beatrice Hall; allow correctly attributed VEGA contrast.',
      preservedChecks: 'All other checks retained; QA goldFacts can only be tightened, never relaxed. Date, required ORION naming, language and citations remain unchanged.',
      languageDiagnosticAffectsSuccess: false, originalSummary: structuredClone(original.summary),
      originalVerifierVersion: original.verifierVersion ?? 'not-recorded',
      originalSourceFingerprintStatus: original.sourceFingerprint ? 'recorded' : 'not-recorded',
      originalConfigurations: structuredClone(original.configurations), changedRuns: runs.filter((run, index) => run.success !== original.runs[index].success)
        .map(run => ({ taskId: run.taskId, configuration: run.configuration, ...(run.repetition ? { repetition: run.repetition } : {}), rescoredSuccess: run.success })) },
    ...(original.repetitions ? { repetitions: Object.fromEntries(Object.keys(original.repetitions).map(repetition => {
      const subset = runs.filter(run => run.repetition === Number(repetition));
      return [repetition, { summary: summarizeRuns(subset), configurations: Object.fromEntries([...new Set(runs.map(run => run.configuration))]
        .map(configuration => [configuration, summarizeRuns(subset.filter(run => run.configuration === configuration))])) }];
    })) } : {}) };
}

export async function writeRescore(path) {
  const originalBytes = await readFile(path), original = JSON.parse(originalBytes.toString('utf8').replace(/^\uFEFF/, ''));
  const result = rescoreResults(original);
  result.rescoring.originalResultSha256 = createHash('sha256').update(originalBytes).digest('hex');
  result.rescoring.verifierSourceSha256 = createHash('sha256').update(await readFile(new URL('./evaluation.mjs', import.meta.url))).digest('hex');
  result.rescoring.rescoredAt = new Date().toISOString();
  const output = join(dirname(resolve(path)), `rescored.${VERIFIER_VERSION}.json`);
  // Exclusive creation keeps the original result and any prior reassessment intact.
  // 只允许新建，保留原始结果与先前重评分，不覆盖既有产物。
  await writeFile(output, JSON.stringify(result, null, 2) + '\n', { encoding: 'utf8', flag: 'wx' });
  return { resultsPath: resolve(path), rescoredPath: output, verifierVersion: VERIFIER_VERSION,
    newModelCalls: 0, originalScores: Object.fromEntries(Object.entries(original.configurations).map(([name, summary]) => [name, summary.successRate])),
    rescoredScores: Object.fromEntries(Object.entries(result.configurations).map(([name, summary]) => [name, summary.successRate])) };
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  if (process.argv.length !== 4 || process.argv[2] !== '--results') {
    process.stderr.write('Usage: node tests/agent-performance-benchmark/rescore.mjs --results PATH\n'); process.exitCode = 1;
  } else writeRescore(process.argv[3]).then(result => process.stdout.write(JSON.stringify(result, null, 2) + '\n'))
    .catch(() => { process.stderr.write('RESCORE_FAILED: original and existing rescored artifacts preserved.\n'); process.exitCode = 1; });
}

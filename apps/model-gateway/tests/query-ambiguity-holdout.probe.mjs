import { createHash } from 'node:crypto';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { tmpdir } from 'node:os';

// Freeze expectations before invoking production functions; never relabel from actual output.
// 先固定人工预期再调用生产函数，不能根据实际输出修改题目的预期标签。
const artifactRoot = resolve(process.argv[4] ?? join(tmpdir(), 'kynxa-query-ambiguity'));
await mkdir(artifactRoot, { recursive: true });
const repositoryRoot = resolve(process.argv[2] ?? fileURLToPath(new URL('../../..', import.meta.url)));
const runName = process.argv[3] ?? 'baseline';
if (!/^[a-z0-9-]+$/u.test(runName)) throw new Error('Use a simple run name.');
const fixturePath = fileURLToPath(new URL('./fixtures/query-ambiguity-holdout-20261009.json', import.meta.url));
const fixtureText = await readFile(fixturePath, 'utf8');
const fixture = JSON.parse(fixtureText);
const hash = value => createHash('sha256').update(value).digest('hex');
const productionPaths = ['apps/model-gateway/orchestration/retrieval/query-plan.mjs',
  'apps/model-gateway/platform/request-clause-signals.mjs',
  'apps/model-gateway/data/retrieval/retrieval-contracts.mjs'];
const sourceHashes = Object.fromEntries(await Promise.all(productionPaths.map(async path =>
  [path, hash(await readFile(join(repositoryRoot, path)))])));
const manifest = { startedAt: new Date().toISOString(), expectedCaseSha256: hash(fixtureText),
  caseCount: fixture.cases.length, expectationsFrozenBeforeInvokingCases: fixture.frozenBeforeFirstRun,
  repositoryRoot, sourceHashes, evaluationScope: fixture.purpose };
await writeFile(join(artifactRoot, `${runName}-manifest.json`), JSON.stringify(manifest, null, 2) + '\n');
const { retrievalPlan, interpretRetrievalQuery } = await import(pathToFileURL(join(repositoryRoot, productionPaths[0])));
const { analyzeRequestClauses } = await import(pathToFileURL(join(repositoryRoot, productionPaths[1])));

function inspectCase(item) {
  const clauses = analyzeRequestClauses(item.query);
  const options = fixture.contexts[item.context] ?? {};
  const interpretation = interpretRetrievalQuery(item.query, item.mode === 'intent' ? item.intentOptions :
    { taskContext: options.taskContext });
  const plan = item.mode === 'plan' ? retrievalPlan(item.query, options) : undefined;
  return { domain: plan?.domain ?? interpretation.intent.domain,
    preferredDomain: plan?.preferredDomain ?? interpretation.intent.preferredDomain ?? null,
    domainConstraint: plan?.domainConstraint ?? interpretation.domainConstraint,
    path: plan?.path ?? interpretation.intent.path ?? null,
    symbol: plan?.symbol ?? interpretation.intent.symbol ?? null,
    ...(plan ? { shouldRetrieve: plan.shouldRetrieve,
      inheritedHistory: plan.queryDerivation.additions.length > 0,
      originalPreserved: plan.originalQuery === item.query,
      taskRelation: plan.taskRelation, taskType: plan.taskType, derivedQuery: plan.query,
      additions: plan.queryDerivation.additions, replacements: plan.queryDerivation.replacements } : {}),
    activeText: clauses.activeText, boundary: clauses.boundary,
    excludedClauses: clauses.excludedClauses, clues: interpretation.clues };
}

function compareExpected(expected, actual) {
  const differences = [];
  for (const [field, value] of Object.entries(expected)) {
    if (field === 'derivedIncludes' || field === 'derivedExcludes') {
      for (const fragment of value) {
        const contains = actual.derivedQuery?.includes(fragment) ?? false;
        if (contains !== (field === 'derivedIncludes')) differences.push({ field, fragment,
          expected: field === 'derivedIncludes' ? 'present' : 'absent', actual: contains ? 'present' : 'absent' });
      }
    } else if (actual[field] !== value) differences.push({ field, expected: value, actual: actual[field] ?? null });
  }
  return differences;
}

const rows = fixture.cases.map(item => {
  try {
    const actual = inspectCase(item), differences = compareExpected(item.expected, actual);
    const effects = [...new Set(differences.map(difference =>
      difference.field === 'domain' ? 'hard-domain-contract' :
      difference.field === 'shouldRetrieve' ? 'automatic-retrieval' :
      difference.field === 'preferredDomain' ? 'soft-ranking' :
      ['path', 'symbol'].includes(difference.field) ? 'locator-candidate' :
      ['derivedIncludes', 'derivedExcludes', 'inheritedHistory'].includes(difference.field) ? 'derived-query-content' : 'metadata'))];
    return { id: item.id, category: item.category, query: item.query, rationale: item.rationale,
      knownBeforeEvaluation: item.knownBeforeEvaluation ?? false,
      expected: item.expected, actual, passed: !differences.length, differences, effects,
      authorizationEvidence: 'Not evaluated: these pure functions do not execute tools or inspect authorization snapshots.' };
  } catch (error) {
    return { id: item.id, category: item.category, query: item.query, expected: item.expected, passed: false,
      differences: [{ field: 'exception', expected: 'no exception', actual: error.message }], effects: ['exception'] };
  }
});
const holdout = rows.filter(row => !row.knownBeforeEvaluation), reference = rows.filter(row => row.knownBeforeEvaluation);
const summary = { total: rows.length, passed: rows.filter(row => row.passed).length,
  failed: rows.filter(row => !row.passed).length,
  heldout: { total: holdout.length, passed: holdout.filter(row => row.passed).length, failed: holdout.filter(row => !row.passed).length },
  knownRegression: { total: reference.length, passed: reference.filter(row => row.passed).length },
  byCategory: Object.fromEntries([...new Set(rows.map(row => row.category))].map(category => {
    const values = rows.filter(row => row.category === category);
    return [category, { total: values.length, passed: values.filter(row => row.passed).length,
      failed: values.filter(row => !row.passed).length }];
  })) };
const report = { manifest, finishedAt: new Date().toISOString(), summary,
  limitations: ['This diagnoses deterministic query/clause metadata only; it is not a real-model semantic benchmark.',
    'Automatic-retrieval means the plan flag; these cases do not run the coordinator, search index, tools or permissions.',
    'The original user message can remain intact while a derived query loses content; these are distinct observations.'], rows };
await writeFile(join(artifactRoot, `${runName}-results.json`), JSON.stringify(report, null, 2) + '\n');
const markdown = [`# Frozen query/clause probe (${runName})`, '',
  `Held-out expressions: ${summary.heldout.passed}/${summary.heldout.total} passed; known navigation regression: ${summary.knownRegression.passed}/${summary.knownRegression.total}.`, '',
  'Pure deterministic metadata only. No real API model, tool execution or authorization was evaluated.', '',
  '| ID | Category | Result | Differences |', '| --- | --- | --- | --- |',
  ...rows.map(row => `| ${row.id} | ${row.category} | ${row.passed ? 'PASS' : 'FAIL'} | ${row.differences.map(difference =>
    `${difference.field}${difference.fragment ? `: ${difference.fragment}` : ''}`).join('; ').replace(/\|/gu, '\\|')} |`), '',
  'Expectations are recorded in the frozen query fixture; full actual fields are recorded in the run results JSON.'];
await writeFile(join(artifactRoot, `${runName}-results.md`), markdown.join('\n') + '\n');
console.log(JSON.stringify(summary, null, 2));
console.log(`Results: ${join(artifactRoot, `${runName}-results.json`)}`);

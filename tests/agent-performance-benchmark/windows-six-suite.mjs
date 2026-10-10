import { createHash } from 'node:crypto';
import { cp, mkdir, readFile, readdir, writeFile } from 'node:fs/promises';
import { join } from 'node:path';

export const WINDOWS_SIX_VERSION = 'windows-six-agent-v1';
export const WINDOWS_SIX_ADAPTER_VERSION = 'windows-six-action-readiness-v2';
export const WINDOWS_SIX_SCORING_VERSION = 'windows-six-received-evidence-v2';
export const WINDOWS_SIX_EMBEDDING_DOCUMENT_LIMIT = 1024;
export const WINDOWS_SIX_LIMITS = Object.freeze({ maxRounds: 24, maxToolCalls: 64,
  maxGeneratedTokens: 196608, maxDurationMs: 1800000 });
export const WINDOWS_SIX_MODEL = Object.freeze({ model: 'qwen3:8b', contextWindowTokens: 32768,
  maxOutputTokens: 8192, temperature: 0, seed: 42, think: false });

const hash = value => createHash('sha256').update(value).digest('hex');
const fail = code => Object.assign(new Error(code), { code });
const PUBLIC_CASE_FILE = 'test/agent-benchmark-20261008/stratified-subset30-v2/model-public-cases.json';
const PUBLIC_CASE_HASH = '4d5857d75c277f997c5c67907eeb4f0e4df07d9f2142a9bd5543737f687482f6';
const CORPUS_FILE = 'artifacts/verification/rag-benchmark/datasets/scifact/corpus.jsonl';
const CORPUS_HASH = 'dec31c8182f3d744c7d2c09423756fd1d17cbef75808db13ba01cc0aab4d1ac6';

// Freeze task selection and scoring before inference; public issue bodies are loaded without patches or hints.
// 推理前固定题号与验收要求；公开问题只读取原文，不读取标准补丁或提示答案。
export const WINDOWS_SIX_TASKS = Object.freeze([
  { id: 'swe-astropy-12907-localize', type: 'code-localization', indexState: 'ready',
    source: 'SWE-bench Lite selected30; localization adaptation, not repair score',
    instanceId: 'astropy__astropy-12907', repo: 'astropy/astropy',
    commit: 'd16bfe05a744909de4b27f5875fe0d4ed41ce607',
    instruction: '请依据这个完整问题，在工作仓库中找到负责实现，读取原文并用中文简短说明根因位置。给出实际文件路径、相关函数和依据。不要修改文件。',
    requiredFiles: ['astropy/modeling/separable.py'], requiredSymbols: ['_cstack'], semanticReviewRequired: true },
  { id: 'swe-django-11019-explain', type: 'cross-file-explanation', indexState: 'partial',
    source: 'SWE-bench Lite selected30; explanation adaptation, not repair score',
    instanceId: 'django__django-11019', repo: 'django/django',
    commit: '93e892bb645b16ebaf287beb5fe7f3ffe8d10408',
    instruction: '请依据这个完整问题，结合实现、表单调用处和现有测试解释为什么三组 Media 合并会产生多余顺序约束。读取相关原文，列出文件和函数依据。不修改源码，不运行 Python。',
    requiredFiles: ['django/forms/widgets.py', 'django/forms/forms.py', 'tests/forms_tests/tests/test_media.py'],
    requiredSymbols: ['Media', 'merge'], semanticReviewRequired: true },
  { id: 'node-pending-work-repair', type: 'small-repair', indexState: 'changed',
    source: 'KYNXA custom independent Node fixture; not SWE-bench repair',
    instruction: '工作副本最近修改后，待处理文件操作的合并逻辑有回归：同一路径先 upsert 后 delete，最后仍留下 upsert。请读取实现和测试，只修改 lib/pending-work.mjs，保留按路径最后一次操作的语义。使用可用终端运行 node --test --test-isolation=none pending-work.test.mjs，依据真实回执验证。不能修改测试。',
    requiredFiles: ['lib/pending-work.mjs', 'pending-work.test.mjs'], semanticReviewRequired: false },
  { id: 'scifact-31715818-supported', type: 'document-question', indexState: 'ready',
    source: 'BEIR SciFact public document 31715818; custom source-grounded question, not official claim score',
    instruction: '根据工作资料 scifact-31715818.md，文献具体列出了哪两类用于干细胞标记与体内追踪的纳米技术？用中文简短回答，保留英文名称，引用你实际读取的原句和资料名。不要自行加入疗效结论。',
    requiredFiles: ['scifact-31715818.md'], semanticReviewRequired: false },
  { id: 'scifact-31715818-insufficient', type: 'insufficient-evidence', indexState: 'partial',
    source: 'Same public SciFact document; custom insufficient-evidence engineering task',
    instruction: '仅依据工作资料 scifact-31715818.md，告诉我这两类追踪纳米技术统一适用的细胞毒性百分比阈值。先查原文，资料没给出数值就明确无法确定，不能编造或联网。说明已有原文说了什么、缺少什么，并引用资料名。',
    requiredFiles: ['scifact-31715818.md'], semanticReviewRequired: false },
  { id: 'project-memory-update-scope', type: 'memory-and-scope', indexState: 'changed',
    source: 'KYNXA custom confirmed-memory lifecycle and scope engineering task',
    instruction: '请根据本工作的当前确认记忆告诉我交付代号。只用当前允许范围，回答必须体现当前有效值，不使用过期值或其他工作资料；本次不创建新记忆。',
    requiredFiles: [], semanticReviewRequired: false }
]);

export const ORIGINAL_PENDING_WORK = 'export function coalescePending(items) {\n'
  + '  const latest = new Map();\n'
  + '  for (const item of items) latest.set(item.path, { ...item });\n'
  + '  return [...latest.values()];\n}\n';
export const CHANGED_PENDING_WORK = ORIGINAL_PENDING_WORK.replace('for (const item of items) latest.set',
  "for (const item of items) if (item.kind !== 'delete') latest.set");
export const PENDING_WORK_TEST = "import assert from 'node:assert/strict';\nimport { test } from 'node:test';\n"
  + "import { coalescePending } from './lib/pending-work.mjs';\n"
  + "test('last operation is retained for every path, including deletion', () => {\n"
  + "  const first = { path: 'a.js', kind: 'upsert', revision: 1 };\n"
  + "  const deleted = { path: 'a.js', kind: 'delete', revision: 2 };\n"
  + "  const independent = { path: 'b.js', kind: 'upsert', revision: 3 };\n"
  + '  assert.deepEqual(coalescePending([first, independent, deleted]), [deleted, independent]);\n'
  + '  assert.deepEqual(coalescePending([deleted, first]), [first]);\n'
  + '  assert.deepEqual(coalescePending([deleted]), [deleted]);\n'
  + '  assert.deepEqual(coalescePending([]), []);\n'
  + '  assert.equal(first.kind, \'upsert\');\n});\n';

export function windowsSixPlan() {
  return { schemaVersion: 2, fixtureVersion: WINDOWS_SIX_VERSION, adapterVersion: WINDOWS_SIX_ADAPTER_VERSION,
    scoringVersion: WINDOWS_SIX_SCORING_VERSION, measurementKind: 'plan-only', realModelCalls: 0,
    tasks: WINDOWS_SIX_TASKS, model: WINDOWS_SIX_MODEL, limits: WINDOWS_SIX_LIMITS,
    indexingPolicy: { readiness: 'required-action-capability', cacheReuse: true,
      documentEmbeddingLimit: WINDOWS_SIX_EMBEDDING_DOCUMENT_LIMIT, fullRepositoryEmbeddingRequired: false },
    fixtureHash: hash(JSON.stringify([WINDOWS_SIX_TASKS, ORIGINAL_PENDING_WORK, CHANGED_PENDING_WORK, PENDING_WORK_TEST])),
    stateCounts: { ready: 2, partial: 2, changed: 2 },
    distinctions: ['Two selected30 tasks preserve complete public issues; no official repair score claimed.',
      'Four custom tasks exercise source-grounded answers, execution and memory; scores are not public benchmark rankings.',
      'Mechanical checks do not certify the semantic correctness of cross-file explanations.',
      'Adapter and scoring versions must match for paired comparisons; older scores are not silently reclassified.'] };
}

export async function prepareSixWorkspace(task, workspace, cacheRoot) {
  await mkdir(workspace, { recursive: true });
  if (task.instanceId) {
    const raw = await readFile(join(cacheRoot, PUBLIC_CASE_FILE));
    if (hash(raw) !== PUBLIC_CASE_HASH) throw fail('SIX_PUBLIC_CASE_FILE_CHANGED');
    const cases = JSON.parse(raw.toString('utf8')).cases;
    const instance = cases.find(item => item.instance_id === task.instanceId);
    if (!instance || instance.base_commit !== task.commit || instance.repo !== task.repo ||
        Object.keys(instance).some(key => !['instance_id', 'repo', 'base_commit', 'problem_statement'].includes(key)))
      throw fail('SIX_PUBLIC_CASE_IDENTITY_INVALID');
    const root = join(cacheRoot, 'test/formal-source-cache/fixtures', `${task.repo.replace('/', '__')}--${task.commit}`);
    const children = await readdir(root, { withFileTypes: true });
    const folder = children.find(entry => entry.isDirectory() && entry.name.endsWith(task.commit));
    if (!folder) throw fail('SIX_SOURCE_CACHE_MISSING');
    if ((await readdir(workspace)).length) throw fail('SIX_SOURCE_WORKSPACE_NOT_EMPTY');
    await cp(join(root, folder.name), workspace, { recursive: true, force: false, errorOnExist: false,
      dereference: false, verbatimSymlinks: true });
    return { message: `${task.instruction}\n\n公开问题原文：\n${instance.problem_statement}`,
      provenance: { instanceId: task.instanceId, repo: task.repo, commit: task.commit,
        publicCaseFileHash: PUBLIC_CASE_HASH, issueHash: hash(instance.problem_statement), fullRepositoryCopy: true } };
  }
  if (task.type === 'small-repair') {
    await mkdir(join(workspace, 'lib'));
    await writeFile(join(workspace, 'lib/pending-work.mjs'), ORIGINAL_PENDING_WORK);
    await writeFile(join(workspace, 'pending-work.test.mjs'), PENDING_WORK_TEST);
    return { message: task.instruction, provenance: { originalHash: hash(ORIGINAL_PENDING_WORK),
      changedHash: hash(CHANGED_PENDING_WORK), testHash: hash(PENDING_WORK_TEST), customFixture: true } };
  }
  if (task.type === 'document-question' || task.type === 'insufficient-evidence') {
    const raw = await readFile(join(cacheRoot, CORPUS_FILE));
    if (hash(raw) !== CORPUS_HASH) throw fail('SIX_SCIFACT_CORPUS_CHANGED');
    const document = raw.toString('utf8').split(/\r?\n/).filter(Boolean).map(line => JSON.parse(line))
      .find(item => item._id === '31715818');
    if (!document) throw fail('SIX_SCIFACT_DOCUMENT_MISSING');
    const content = `# ${document.title}\n\n${document.text}\n\nPublic corpus: BEIR SciFact, document 31715818.\n`;
    await writeFile(join(workspace, 'scifact-31715818.md'), content);
    return { message: task.instruction, provenance: { corpusHash: CORPUS_HASH, documentId: document._id,
      sourceHash: hash(content), customQuestion: true } };
  }
  await writeFile(join(workspace, 'delivery.md'), 'This work uses its current confirmed project memory as the delivery identifier.\n');
  return { message: task.instruction, provenance: { customFixture: true, explicitConfirmationThroughExistingMemoryApi: true } };
}

const normalizePath = value => String(value ?? '').replaceAll('\\', '/').replace(/\/+/g, '/');
const matchesPath = (actual, expected) => normalizePath(actual) === expected || normalizePath(actual).endsWith(`/${expected}`);
const SUPPORTED_QUOTE = 'magnetic nanoparticles and quantum dots for stem cell labeling and in vivo tracking';

function receivedSourceEvidence(path, observations, automaticEvidence) {
  const receipts = [];
  for (const item of observations) {
    if (item.status !== 'completed' || !['filesystem.read', 'knowledge.read'].includes(item.name)) continue;
    if (item.readReceipt && matchesPath(item.readReceipt.relativePath, path)) {
      receipts.push(item.readReceipt); continue;
    }
    let result;
    try { result = JSON.parse(item.result); } catch { continue; }
    const value = result?.structuredContent ?? result, text = value?.text ?? value?.content;
    if (typeof text === 'string' && matchesPath(value.path ?? value.locator?.relativePath, path))
      receipts.push({ text, contentCharacters: text.trim().length, offset: value.offset,
        totalCharacters: value.totalCharacters, contentHash: value.contentHash ?? value.sha256, sourceRevision: value.sourceRevision });
  }
  for (const item of automaticEvidence) if (item.sourceValidated === true && item.deliveredToModel === true &&
      item.channel === 'automatic-evidence' && matchesPath(item.relativePath, path)) receipts.push(item);
  return receipts.filter(item => item.contentCharacters > 0);
}

function receivedCompleteSource(receipts) {
  const groups = new Map();
  for (const receipt of receipts) {
    if (typeof receipt.text !== 'string' || !Number.isSafeInteger(receipt.offset) || receipt.offset < 0 ||
        !Number.isSafeInteger(receipt.totalCharacters) || receipt.totalCharacters < 1 ||
        receipt.offset + receipt.text.length > receipt.totalCharacters || !/^[a-f0-9]{64}$/u.test(receipt.contentHash ?? '')) continue;
    const key = JSON.stringify([receipt.contentHash, receipt.sourceRevision ?? null, receipt.totalCharacters]);
    let start = receipt.offset, end = receipt.offset + receipt.text.length;
    // Verified separators contain no omitted facts; arbitrary gaps or other source revisions never count as coverage.
    // 已核验的空白分隔符不含遗漏事实；任意正文缺口或其他来源版本仍不能计入完整覆盖。
    if (receipt.channel === 'automatic-evidence' && receipt.sourceValidated === true &&
        Number.isSafeInteger(receipt.coverageStartOffset) && receipt.coverageStartOffset >= 0 && receipt.coverageStartOffset <= start &&
        Number.isSafeInteger(receipt.coverageEndOffset) && receipt.coverageEndOffset >= end && receipt.coverageEndOffset <= receipt.totalCharacters) {
      start = receipt.coverageStartOffset; end = receipt.coverageEndOffset;
    }
    const ranges = groups.get(key) ?? []; ranges.push([start, end]); groups.set(key, ranges);
  }
  return [...groups].some(([key, ranges]) => {
    let end = 0;
    for (const range of ranges.sort((left, right) => left[0] - right[0])) {
      if (range[0] > end) return false;
      end = Math.max(end, range[1]);
    }
    return end === JSON.parse(key)[2];
  });
}

export function verifySixTask(task, { answers = [], observations = [], automaticEvidence = [], finalNodeTest, memoryScopeCheck } = {}) {
  const answer = answers.at(-1)?.content ?? '';
  const evidence = new Map(task.requiredFiles.map(path => [path, receivedSourceEvidence(path, observations, automaticEvidence)]));
  const checks = { finalAnswerPresent: answer.trim().length > 0,
    finalAnswerCompleted: answers.at(-1)?.status === 'completed' };
  for (const path of task.requiredFiles) checks[`read:${path}`] = evidence.get(path).length > 0;
  for (const symbol of task.requiredSymbols ?? []) checks[`mentions:${symbol}`] = answer.includes(symbol);
  if (task.type === 'small-repair') {
    checks.actualNodeTestsPassed = finalNodeTest?.exitCode === 0;
    checks.modelInvokedVerifiedSandbox = observations.some(item => item.name === 'terminal.run' &&
      item.status === 'completed' && item.sandbox === 'appcontainer');
    checks.testBytesPreserved = finalNodeTest?.testBytesPreserved === true;
  }
  if (task.type === 'document-question') {
    checks.citesSource = answer.includes('scifact-31715818');
    checks.namesTechniques = /magnetic nanoparticles/i.test(answer) && /quantum dots/i.test(answer);
    checks.actualQuote = answer.includes(SUPPORTED_QUOTE);
    checks.quoteWasReceived = evidence.get('scifact-31715818.md')?.some(item => item.text?.includes(SUPPORTED_QUOTE)) === true;
  }
  if (task.type === 'insufficient-evidence') {
    checks.citesSource = answer.includes('scifact-31715818');
    checks.explicitMissingValue = /无法确定|未(?:提供|给出|说明)|没有(?:给出|提供)|不足以|不能确定/.test(answer);
    checks.noInventedPercentage = !/\d\s*[%％]|百分之\s*[\d一二三四五六七八九十百]/.test(answer);
    checks.completeSourceObserved = receivedCompleteSource(evidence.get('scifact-31715818.md') ?? []);
  }
  if (task.type === 'memory-and-scope') {
    checks.currentValue = answer.includes('BETA_8426');
    checks.oldValueAbsent = !answer.includes('ALPHA_7091');
    checks.otherWorkAbsent = !JSON.stringify([answer, observations]).includes('PRIVATE_OTHER_6631');
    checks.scopePreserved = memoryScopeCheck === true;
  }
  const mechanicalPassed = Object.values(checks).every(value => value === true);
  return { scoringVersion: WINDOWS_SIX_SCORING_VERSION, checks, mechanicalPassed,
    evidenceAcceptance: 'successful-read-or-verified-model-visible-automatic-evidence',
    semanticReview: task.semanticReviewRequired ? 'pending' : 'not-required',
    success: task.semanticReviewRequired ? null : mechanicalPassed };
}

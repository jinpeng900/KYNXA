import assert from 'node:assert/strict';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { test } from 'node:test';
import { ConversationStore } from '../data/conversations.mjs';
import { RetrievalCoordinator } from '../orchestration/retrieval/coordinator.mjs';
import { selectCandidates } from '../orchestration/retrieval/candidate-selection.mjs';
import { evidencePrompt, retrievalPlan } from '../orchestration/retrieval/source-projection.mjs';
import { estimateTokens } from '../models/context-tokens.mjs';
import { toolFixture } from './tool-fixture.mjs';

function candidate(id, sourceId, excerpt, startOffset = 0) {
  return { chunkId: id, sourceId, scopeKey: 'user', sourceRevision: 1, contentHash: `${sourceId}-version`,
    title: sourceId, sourceType: 'knowledge', sourceRef: `fixture-ref:${id}`, excerpt,
    locator: { startOffset, endOffset: startOffset + excerpt.length }, score: 1 / 61 };
}

async function auditCoordinator(t, extra = {}) {
  const fixture = await toolFixture(t);
  const embeddings = { status: () => ({ state: 'unavailable' }), close: async () => {} };
  const retrieval = new RetrievalCoordinator({ conversations: fixture.conversations,
    memory: { contextFor: async () => ({ entries: [] }) }, tools: {}, embeddings, ...extra });
  fixture.service.retrieval = retrieval;
  t.after(() => retrieval.close());
  await retrieval.settings.patchGlobal({ expectedRevision: 0, patch: { local: { semantic: 'off' } } });
  return { ...fixture, retrieval, context: { conversationId: fixture.conversationId, projectId: fixture.projectId } };
}

test('candidate selection removes repeats and overlaps without restricting a document to one evidence chunk', () => {
  const first = candidate('chunk-a', 'guide', '数据库恢复首先校验备份校验和以及恢复点。');
  const duplicate = { ...first };
  const overlap = candidate('chunk-overlap', 'guide', '数据库恢复首先校验备份校验和以及恢复点。后续步骤不同。', 1);
  const second = candidate('chunk-b', 'guide', '权限恢复需要重新核实访问策略以及审计事件。', 200);
  const copy = candidate('chunk-copy', 'other-guide', first.excerpt);
  const alternate = candidate('chunk-c', 'checklist', '恢复完毕后对数据库执行完整性检查。');
  const result = selectCandidates([first, duplicate, overlap, copy, second, alternate],
    { query: '数据库权限恢复', maximumTokens: 3000 });
  assert.equal(result.selection.duplicateCount, 3);
  assert.equal(result.items.filter(item => item.sourceId === 'guide').length, 2);
  assert.equal(result.items.length, 3);
  assert.equal(result.evidenceAssessment.state, 'usable');
  assert.equal(result.items[0].sourceRef, first.sourceRef);
  assert.equal(result.items[0].score, first.score);
});

test('evidence budgets include source metadata and retain complete records instead of partial excerpts', () => {
  const tooLarge = candidate('huge', 'long-document', '恢复步骤'.repeat(1000));
  const concise = candidate('small', 'guide', '恢复步骤先校验完整备份。');
  const result = selectCandidates([tooLarge, concise], { query: '恢复步骤', maximumTokens: 140 });
  assert.deepEqual(result.items.map(item => item.chunkId), ['small']);
  assert.equal(result.selection.omittedForBudget, 1);
  const prompt = evidencePrompt(result.items, 10000, { maximumTokens: 600, assessment: result.evidenceAssessment });
  assert.ok(prompt);
  assert.ok(estimateTokens(prompt) <= 600);
  assert.equal(JSON.parse(prompt.split('\n').at(-1)).excerpt, concise.excerpt);
  assert.equal(selectCandidates([concise], { maximumTokens: 0 }).items.length, 0);
});

test('exact symbol and file targets survive reranking, duplicate copies and diversity selection', () => {
  const target = { ...candidate('target', 'target.cs', 'bool CancelJob() { return true; }'), exactTargetMatch: true };
  const duplicate = { ...candidate('copy', 'other.cs', target.excerpt), rerankScore: 100 };
  const distractors = Array.from({ length: 12 }, (_, index) => ({ ...candidate(`note-${index}`, `note-${index}.md`,
    `CancelJob cancellation note ${index} has no target implementation.`), rerankScore: 99 - index }));
  const selected = selectCandidates([duplicate, ...distractors, target], { query: 'CancelJob target.cs', limit: 1, maximumTokens: 2000 });
  assert.equal(selected.items[0].sourceRef, target.sourceRef);
  assert.equal(selected.items[0].exactTargetMatch, true);
  assert.equal(selected.selection.duplicateCount, 1);
});

test('recent context suppresses repeated evidence while weak lexical fragments remain labelled as weak', () => {
  const present = candidate('present', 'guide', '数据库恢复需要先核实当前备份以及目标版本，确认权限后执行。');
  const result = selectCandidates([present], { query: '数据库恢复', existingContext: [{ role: 'assistant', content: present.excerpt }] });
  assert.equal(result.items.length, 0);
  assert.equal(result.selection.alreadyPresentCount, 1);
  assert.equal(result.evidenceAssessment.reason, 'already-in-context');
  const weak = selectCandidates([candidate('unrelated', 'guide', '数据库目录只有位置说明。')],
    { query: '数据库恢复权限冲突和备份校验失败如何处理' });
  assert.equal(weak.evidenceAssessment.state, 'weak');
  assert.equal(weak.evidenceAssessment.requiresSourceRead, true);
  assert.match(evidencePrompt(weak.items, 10000, { assessment: weak.evidenceAssessment }), /Evidence support is unverified/);
  const semanticOnly = selectCandidates([{ ...candidate('en', 'English manual', 'Reset your password in Account Settings.'), vectorRank: 1, distance: 0.05 }],
    { query: '账户密码如何重置' });
  assert.equal(semanticOnly.evidenceAssessment.reason, 'semantic-only-unverified');
  assert.equal(semanticOnly.items.length, 1);
});

test('request routing skips social chat and expands only a continuation of a prior local request', () => {
  for (const message of ['你好！', '谢谢你', '讲个笑话', '今天心情怎么样', '计算 2 + 2'])
    assert.equal(retrievalPlan(message).shouldRetrieve, false);
  assert.equal(retrievalPlan('读取 coordinator.mjs 的取消逻辑').taskType, 'file');
  const history = [{ Role: 'user', Status: 'completed', Content: '根据项目文档比较 SQLite 与 PostgreSQL 的方案' }];
  const followup = retrievalPlan('继续，SQLite 的恢复步骤是什么？', { history });
  assert.equal(followup.shouldRetrieve, true);
  assert.equal(followup.rewritten, true);
  assert.ok(followup.query.startsWith('继续，SQLite 的恢复步骤是什么？'));
  assert.match(followup.query, /PostgreSQL/);
  assert.equal(retrievalPlan('继续讲讲', { history: [{ Role: 'user', Content: '讲个笑话' }] }).shouldRetrieve, false);
  const longQuestion = `继续说明 ${'x'.repeat(3900)} SQLiteUniqueEntity`;
  const rewritten = retrievalPlan(longQuestion, { history });
  assert.ok(rewritten.query.startsWith(longQuestion));
  assert.ok(rewritten.query.length <= 4000);
});

test('direct file and terminal execution bypasses automatic evidence without suppressing source questions or analysis', () => {
  for (const message of [
    '读取 input.txt，把内容写入 output.txt，然后读回核实。',
    'Read instructions.txt, write the exact text to completed.txt, then read it back to verify.',
    'Read logic.py, write its exact content to document.txt, then read document.txt to verify.',
    '读取分析.md 全文，原样写入历史.txt，然后读回核实。',
    '先读取input.txt。',
    '请你读取分析.md。',
    '在项目目录运行 npm test，把输出保存到 result.txt。',
    'Run the code in runner.py and save its output to run.json.',
    '列出项目文件，然后创建一个空的 notes.md。',
    '继续执行，然后读回刚才写入的文件核实。',
  ]) {
    const plan = retrievalPlan(message, { history: [{ Role: 'user', Content: '根据项目文档比较数据库恢复方案' }] });
    assert.equal(plan.shouldRetrieve, false, message); assert.equal(plan.reason, 'direct-execution', message);
    assert.equal(plan.evidenceTokens, 0); assert.equal(plan.rewritten, false);
  }
  for (const message of [
    '读取 coordinator.mjs 的取消逻辑',
    'Read core.py and explain the implementation.',
    '分析项目文件中重试失败的原因。',
    'Review storage.ts for bugs and race conditions.',
    '根据项目文档运行迁移，需要哪些权限？',
    'Read the documentation and summarize the recovery requirements.',
    'Read the project docs and summarize the recovery requirements.',
    '查看之前讨论的工作约束。',
    '回忆聊天历史里确认过的选择。',
    'Remember the project decision we made last time.',
    '根据记忆，创建项目时用户有哪些偏好？',
  ]) assert.equal(retrievalPlan(message).shouldRetrieve, true, message);
  assert.equal(retrievalPlan('继续，它是什么意思？', { history: [{ Role: 'user', Content: 'Read input.txt and write output.txt.' }] }).shouldRetrieve, false);
  assert.equal(retrievalPlan('继续，它为什么这样实现？', { history: [{ Role: 'user', Content: '解释 core.py 的取消逻辑' }] }).shouldRetrieve, true);
  assert.equal(retrievalPlan('Continue with its constraints.', { history: [{ Role: 'user', Content: 'Summarize the docs.' }] }).shouldRetrieve, true);
  assert.equal(retrievalPlan('继续，它的条件是什么？', { history: [{ Role: 'user', Content: '回忆聊天历史里确认过的选择。' }] }).shouldRetrieve, true);
});

test('deferred evidence archives only final injected excerpts without another search or exposed draft items', async t => {
  const f = await auditCoordinator(t), retrieval = f.retrieval;
  const backup = '恢复备份需要先校验数据库检查点。', permission = '恢复权限需要检查访问策略并完成审计。';
  await retrieval.library.add([
    { path: join(f.workspace, 'backup.md'), title: '恢复备份', text: backup },
    { path: join(f.workspace, 'permission.md'), title: '恢复权限', text: permission },
  ], { scope: 'user' });
  const context = { ...f.context, requestId: f.conversationId }, archives = [];
  retrieval.tools.results = { save: async (_context, _call, result) => { archives.push(result); return 'final-evidence-result'; } };
  const search = retrieval.search.bind(retrieval); let searches = 0;
  retrieval.search = (...args) => { searches++; return search(...args); };
  const draft = await retrieval.evidence(context, '根据资料，恢复备份和权限有哪些步骤？', { maximumTokens: 4096, deferArchive: true });
  assert.equal(draft.references.length, 2); assert.equal(archives.length, 0);
  assert.ok(draft.prepared); assert.deepEqual(Object.keys(draft.prepared), []);
  assert.equal(draft.items, undefined); assert.equal(draft.result, undefined);
  assert.match(draft.prompt, /校验数据库检查点/); assert.match(draft.prompt, /访问策略/);
  await assert.rejects(retrieval.finalizeEvidence({ ...context, conversationId: f.standaloneId }, draft),
    { code: 'RETRIEVAL_PREPARATION_INVALID' });
  await assert.rejects(retrieval.finalizeEvidence(context, { prepared: {} }), { code: 'RETRIEVAL_PREPARATION_INVALID' });
  const final = await retrieval.finalizeEvidence(context, draft, { existingContext: [{ role: 'assistant', content: backup }] });
  assert.equal(searches, 1); assert.equal(archives.length, 1); assert.equal(final.references.length, 1);
  assert.equal(final.resultRef, 'final-evidence-result'); assert.equal(final.prepared, undefined);
  assert.doesNotMatch(final.prompt, /校验数据库检查点/); assert.match(final.prompt, /访问策略/);
  assert.equal(archives[0].structuredContent.items.length, 1);
  assert.equal(draft.budgetAudit.projections[0].phase, 'prepare');
  assert.equal(draft.budgetAudit.actual.projectedCount, 2);
  assert.equal(final.budgetAudit.projections[1].phase, 'finalize');
  assert.equal(final.budgetAudit.actual.projectedCount, 1);
  assert.equal(final.budgetAudit.earlyCutReasons.find(item => item.phase === 'finalize' && item.reason === 'already-in-model-context').count, 1);
  assert.equal(archives[0].structuredContent.selection.projectionSupport.semanticSupportVerified, false);
  assert.equal(archives[0].structuredContent.items[0].sourceRef, final.references[0].sourceRef);
  assert.ok(estimateTokens(final.prompt) <= estimateTokens(draft.prompt));
  await assert.rejects(retrieval.finalizeEvidence(context, draft), { code: 'RETRIEVAL_PREPARATION_INVALID' });
});

test('final evidence can shrink to empty without saving unused references or expanding its reserved prompt', async t => {
  const f = await auditCoordinator(t), retrieval = f.retrieval, text = '备份恢复必须先检查恢复点和版本。';
  await retrieval.library.add([{ path: join(f.workspace, 'restore.md'), title: '备份恢复', text }], { scope: 'user' });
  const context = { ...f.context, requestId: f.conversationId }; let archives = 0;
  retrieval.tools.results = { save: async () => { archives++; return 'unused'; } };
  const draft = await retrieval.evidence(context, '根据资料如何恢复备份？', { maximumTokens: 4096, deferArchive: true });
  assert.equal(draft.references.length, 1);
  const final = await retrieval.finalizeEvidence(context, draft, { existingContext: [text], maximumTokens: 4096 });
  assert.equal(final.prompt, ''); assert.deepEqual(final.references, []); assert.equal(final.resultRef, undefined);
  assert.equal(final.evidenceAssessment.reason, 'already-in-context'); assert.equal(archives, 0);
  assert.equal(final.outcome.state, 'evidence-already-in-context');
  assert.equal(final.outcome.next, 'check-existing-context-support');
  const smaller = await retrieval.evidence(context, '根据资料如何恢复备份？', { maximumTokens: 4096, deferArchive: true });
  const empty = await retrieval.finalizeEvidence(context, smaller, { maximumTokens: 0 });
  assert.equal(empty.prompt, ''); assert.deepEqual(empty.references, []); assert.equal(archives, 0);
  assert.equal(empty.outcome.state, 'evidence-budget-exhausted');
  assert.equal(empty.budgetAudit.actual.projectedCount, 0);
  assert.equal(empty.budgetAudit.projections.at(-1).requested.remainingContextTokens, 0);
  assert.ok(empty.budgetAudit.earlyCutReasons.some(item => item.reason === 'model-context-exhausted'));
  assert.equal(empty.outcome.next, 'use-current-evidence-or-state-context-limit');
});

test('deferred evidence rechecks revocation, source bytes, enabled settings and project scope before publication', async t => {
  for (const change of ['revoked', 'changed', 'disabled', 'moved']) await t.test(change, async child => {
    const f = await auditCoordinator(child), retrieval = f.retrieval;
    const imported = await retrieval.library.add([{ path: join(f.workspace, 'scope.md'), title: 'Scope recovery',
      text: '恢复配置必须校验原始检查点。' }], { scope: 'user' });
    const context = { ...f.context, requestId: f.conversationId }; let archived = 0;
    retrieval.tools.results = { save: async () => { archived++; return 'never'; } };
    const draft = await retrieval.evidence(context, '根据资料恢复配置如何校验？', { maximumTokens: 4096, deferArchive: true });
    assert.equal(draft.references.length, 1);
    const search = retrieval.search; retrieval.search = () => { throw new Error('Finalization must never search again'); };
    if (change === 'revoked') await retrieval.removeSource(imported.sources[0].id, { expectedRevision: imported.sources[0].revision });
    else if (change === 'changed') await writeFile(join(retrieval.library.folder, imported.sources[0].id, 'source', 'document.txt'), '新文件内容。', 'utf8');
    else if (change === 'disabled') await retrieval.settings.patchGlobal({ expectedRevision: 1, patch: { local: { enabled: false } } });
    else {
      const catalog = await f.conversations.catalog(), chat = catalog.Projects[0].Chats.find(item => item.Id === f.conversationId);
      catalog.Projects[0].Chats = catalog.Projects[0].Chats.filter(item => item.Id !== f.conversationId);
      catalog.Chats.push(chat); await f.conversations.saveCatalog(catalog);
    }
    if (change === 'disabled' || change === 'moved') await assert.rejects(retrieval.finalizeEvidence(context, draft),
      { code: change === 'disabled' ? 'RETRIEVAL_DISABLED' : 'RETRIEVAL_SCOPE_CHANGED' });
    else {
      const final = await retrieval.finalizeEvidence(context, draft);
      assert.equal(final.prompt, ''); assert.deepEqual(final.references, []); assert.equal(final.resultRef, undefined);
      assert.equal(final.outcome.state, 'source-changed');
    }
    assert.equal(archived, 0); retrieval.search = search;
  });
});

test('concurrent finalizers cannot archive the same prepared request twice while its source read is pending', async t => {
  const f = await auditCoordinator(t), retrieval = f.retrieval;
  await retrieval.library.add([{ path: join(f.workspace, 'one.md'), title: 'Recovery', text: '恢复权限需要检查审计日志。' }], { scope: 'user' });
  const context = { ...f.context, requestId: f.conversationId };
  const draft = await retrieval.evidence(context, '根据资料恢复权限如何检查？', { maximumTokens: 4096, deferArchive: true });
  const readSource = retrieval.library.readSource.bind(retrieval.library); let release, entered;
  const ready = new Promise(resolveReady => { entered = resolveReady; });
  retrieval.library.readSource = async (...args) => {
    entered(); await new Promise(resolveRead => { release = resolveRead; }); return readSource(...args);
  };
  let archived = 0; retrieval.tools.results = { save: async () => { archived++; return 'final'; } };
  const finalizing = retrieval.finalizeEvidence(context, draft); await ready;
  await assert.rejects(retrieval.finalizeEvidence(context, draft), { code: 'RETRIEVAL_PREPARATION_INVALID' });
  release(); assert.equal((await finalizing).references.length, 1); assert.equal(archived, 1);
});

test('revision caches avoid whole-source rescans but re-read hits and reject changed snapshots immediately', async t => {
  const f = await auditCoordinator(t), retrieval = f.retrieval;
  const imported = await retrieval.library.add([{ path: join(f.workspace, 'recovery.md'), title: 'Recovery',
    text: '星际调度恢复需要核对检查点和恢复版本。' }], { scope: 'user' });
  const originalReadAll = retrieval.library.readAll.bind(retrieval.library);
  const originalReadSource = retrieval.library.readSource.bind(retrieval.library);
  let scans = 0, exactReads = 0;
  retrieval.library.readAll = async (...args) => { scans++; return originalReadAll(...args); };
  retrieval.library.readSource = async (...args) => { exactReads++; return originalReadSource(...args); };
  const first = await retrieval.search(f.context, { query: '星际调度恢复' });
  assert.equal(first.items.length, 1);
  const firstExactReads = exactReads;
  assert.equal((await retrieval.search(f.context, { query: '星际调度恢复' })).items.length, 1);
  assert.equal(scans, 0, 'lazy corpus snapshots never reload the whole authorized library');
  assert.equal(exactReads, firstExactReads + 1, 'a cached corpus still re-reads its selected evidence');
  assert.equal(first.selection.candidateCount, 1);
  const reference = first.items[0].sourceRef;
  await retrieval.read(f.context, { sourceRef: reference });
  assert.equal(scans, 0);
  await writeFile(join(retrieval.library.folder, imported.sources[0].id, 'source', 'document.txt'), '意外修改的快照。', 'utf8');
  assert.equal((await retrieval.search(f.context, { query: '星际调度恢复' })).items.length, 0);
  await assert.rejects(retrieval.read(f.context, { sourceRef: reference }), { code: 'STALE_RETRIEVAL_SOURCE' });
  await retrieval.library.add([{ path: join(f.workspace, 'new.md'), title: 'New recovery',
    text: '新版本星际调度恢复使用独立检查点。' }], { scope: 'user' });
  assert.ok((await retrieval.search(f.context, { query: '星际调度恢复' })).items.length);
  assert.equal(scans, 0);
});

test('conditional reranking preserves source identity and falls back without a configured profile', async t => {
  let calls = 0;
  const reranker = { status: () => ({ state: 'ready', profileId: 'builtin-multilingual-reranker' }), close: async () => {},
    rerank: async ({ candidates, limit }) => {
      calls++;
      assert.equal(limit, 40);
      return { profileId: 'builtin-multilingual-reranker', modelVersion: 'fixture-reranker',
        items: candidates.toReversed().map(item => ({ ...item, excerpt: 'FORGED', rerankScore: 0.8 })) };
    } };
  // Identity/ranking coverage has an adequate explicit lease; resource clipping is covered separately below.
  // 身份与排序验证使用明确充足授额；资源裁限在下面独立覆盖，不能依赖测试机瞬时压力。
  const resources = { acquire: async () => ({ leaseId: 'identity-fixture-lease', suggestions: { rerankCandidateLimit: 60 } }),
    renew: async () => {}, release: async () => {}, report: async () => {} };
  const f = await auditCoordinator(t, { reranker, resources }), retrieval = f.retrieval;
  await retrieval.library.add([{ path: join(f.workspace, 'a.md'), title: 'Database', text: '恢复数据库需要验证完整备份。' },
    { path: join(f.workspace, 'b.md'), title: 'Permissions', text: '恢复访问权限需要验证审计记录。' }], { scope: 'user' });
  await retrieval.search(f.context, { query: '恢复', taskType: 'research' });
  assert.equal(calls, 0);
  await retrieval.settings.patchGlobal({ expectedRevision: 1, patch: { local: { rerankProfileId: 'builtin-multilingual-reranker', rerankCandidates: 40 } } });
  await retrieval.search(f.context, { query: '恢复', taskType: 'lookup' });
  assert.equal(calls, 0);
  const result = await retrieval.search(f.context, { query: '恢复', gap: 'Compare backup recovery with permission recovery', taskType: 'research' });
  assert.equal(calls, 1);
  assert.equal(result.rerank.profileId, 'builtin-multilingual-reranker');
  assert.equal(result.items.length, 2);
  assert.equal(result.budget.audit.configured.localRerankCandidates, 40);
  assert.equal(result.budget.audit.approved.rerankCandidates, 40);
  assert.equal(result.budget.audit.actual.rerankScoredCandidates, 2);
  assert.equal(result.budget.audit.rerankBudgetSource, 'local-setting');
  assert.ok(result.items.every(item => item.sourceRef && item.excerpt !== 'FORGED' && item.score > 0));
  reranker.rerank = async () => { throw Object.assign(new Error('Fixture failed'), { code: 'RERANK_FIXTURE_FAILED' }); };
  const fallback = await retrieval.search(f.context, { query: '恢复', gap: 'Compare backup recovery with permission recovery', taskType: 'research' });
  assert.equal(fallback.items.length, 2);
  assert.equal(fallback.rerankDiagnostic, 'RERANK_FIXTURE_FAILED');
});

test('adaptive rerank tiers use resource suggestions only when local configuration explicitly allows them', async t => {
  let resourceRerankLimit = 60;
  const observed = [], resources = { acquire: async () => ({ leaseId: 'fixture-lease',
    suggestions: { rerankCandidates: 60, rerankCandidateLimit: resourceRerankLimit } }), renew: async () => {}, release: async () => {},
  report: async () => {}, snapshot: async () => ({ memory: { availableBytes: 16 * 1024 ** 3 } }) };
  const reranker = { status: () => ({ state: 'ready', profileId: 'builtin-multilingual-reranker' }), close: async () => {},
    rerank: async ({ candidates, limit }) => { observed.push(limit);
      return { items: candidates.map(item => ({ ...item, rerankScore: 0.8 })) }; } };
  const f = await auditCoordinator(t, { resources, reranker });
  await f.retrieval.library.add([{ path: join(f.workspace, 'a.md'), title: 'Alpha', text: 'alpha original evidence' },
    { path: join(f.workspace, 'b.md'), title: 'Other alpha', text: 'alpha independent explanation' }], { scope: 'user' });
  let revision = 1;
  for (const configured of [20, 40, 60, null]) {
    await f.retrieval.settings.patchGlobal({ expectedRevision: revision++, patch: { local: {
      rerankProfileId: 'builtin-multilingual-reranker', rerankCandidates: configured } } });
    const result = await f.retrieval.search(f.context, { query: 'alpha', gap: 'Compare the two accounts', taskType: 'research' });
    assert.equal(observed.at(-1), configured ?? 60);
    assert.equal(result.budget.audit.rerankBudgetSource, configured === null ? 'resource-suggestion' : 'local-setting');
    assert.equal(result.budget.audit.approved.rerankCandidates, configured ?? 60);
  }
  resourceRerankLimit = 20;
  await f.retrieval.settings.patchGlobal({ expectedRevision: revision++, patch: { local: { rerankCandidates: 40 } } });
  const reduced = await f.retrieval.search(f.context, { query: 'alpha', gap: 'Compare the accounts under pressure', taskType: 'research' });
  assert.equal(observed.at(-1), 20);
  assert.equal(reduced.budget.audit.configured.localRerankCandidates, 40);
  assert.equal(reduced.budget.audit.rerankRequestedCandidates, 40);
  assert.equal(reduced.budget.audit.approved.rerankCandidates, 20);
  assert.ok(reduced.budget.audit.earlyCutReasons.some(item => item.reason === 'resource-grant' && item.field === 'rerankCandidates'));
});

test('a stale preferred duplicate falls back to a current source without publishing the stale reference', async t => {
  const f = await auditCoordinator(t), retrieval = f.retrieval;
  const text = '恢复审计检查点时，需要先核对记录的版本和当前权限。';
  await retrieval.library.add([{ path: join(f.workspace, 'copy-a.md'), title: 'Copy A', text },
    { path: join(f.workspace, 'copy-b.md'), title: 'Copy B', text }], { scope: 'user' });
  const initial = await retrieval.search(f.context, { query: '恢复审计检查点' });
  assert.equal(initial.items.length, 1);
  const previous = initial.items[0];
  await writeFile(join(retrieval.library.folder, previous.sourceId, 'source', 'document.txt'), 'Changed fixture snapshot', 'utf8');
  const current = await retrieval.search(f.context, { query: '恢复审计检查点' });
  assert.equal(current.items.length, 1);
  assert.notEqual(current.items[0].sourceId, previous.sourceId);
  assert.equal(current.items[0].excerpt, text);
  assert.equal(current.selection.staleSourceCount, 1);
});

test('cancellation during source freshness waits for all admitted reads before releasing the queue', async t => {
  const f = await auditCoordinator(t), retrieval = f.retrieval;
  await retrieval.library.add([{ path: join(f.workspace, 'a.md'), title: 'A', text: '取消时的审计恢复检查点。' },
    { path: join(f.workspace, 'b.md'), title: 'B', text: '取消时的审计恢复权限验证。' }], { scope: 'user' });
  // Prepare lexical derivations before gating concurrent evidence reads, rather than blocking sequential lazy hydration.
  // 先完成词法派生，再阻止并发证据回读，避免把顺序的按需正文加载误当成并发回读。
  await retrieval.search(f.context, { query: '审计恢复' });
  const originalReadSource = retrieval.library.readSource.bind(retrieval.library);
  let enterReads, releaseReads;
  const entered = new Promise(resolve => { enterReads = resolve; });
  const gate = new Promise(resolve => { releaseReads = resolve; });
  let admitted = 0;
  retrieval.library.readSource = async (...args) => {
    if (++admitted === 2) enterReads();
    await gate;
    return originalReadSource(...args);
  };
  const controller = new AbortController();
  const search = retrieval.search(f.context, { query: '审计恢复' }, { signal: controller.signal });
  await entered;
  controller.abort();
  let queueReleased = false;
  const next = retrieval._serialize(() => { queueReleased = true; });
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(queueReleased, false);
  releaseReads();
  await assert.rejects(search, { name: 'AbortError' });
  await next;
  assert.equal(queueReleased, true);
});

test('a settings change during worker search invalidates the response and cancelled work leaves formal history intact', async t => {
  const f = await auditCoordinator(t), retrieval = f.retrieval;
  await retrieval.library.add([{ path: join(f.workspace, 'scope.md'), title: 'Scope', text: '范围检索版本校验。' }], { scope: 'user' });
  const original = retrieval.index.search.bind(retrieval.index);
  retrieval.index.search = async input => {
    const result = await original(input);
    await retrieval.settings.patchGlobal({ expectedRevision: 1, patch: { local: { enabled: false } } });
    return result;
  };
  await assert.rejects(retrieval.search(f.context, { query: '范围检索' }), { code: 'RETRIEVAL_SCOPE_CHANGED' });
  const messages = await f.conversations.readMessages(f.conversationId);
  const controller = new AbortController(); controller.abort();
  await assert.rejects(retrieval.search(f.context, { query: '范围检索' }, { signal: controller.signal }), { name: 'AbortError' });
  assert.deepEqual(await f.conversations.readMessages(f.conversationId), messages);
});

test('a 200-message conversation searches while unrelated background embedding remains pending', async t => {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-rag-coordinator-audit-'));
  const conversations = new ConversationStore({ root, dataHome: join(root, 'Models'), legacyDesktopDirectory: null });
  const catalog = await conversations.catalog();
  catalog.Projects.push({ Id: 'work-a', Name: 'Synthetic work', FolderPath: null, Chats: [{ Id: 'chat-a',
    Title: '200 synthetic messages', Messages: Array.from({ length: 200 }, (_, index) => ({ Id: `msg-${index}`,
      Role: 'user', Content: `保存的历史记忆${index}。`, Status: 'completed' })) }] });
  await conversations.saveCatalog(catalog);
  let markDocumentStarted, releaseDocument, timeout;
  const documentStarted = new Promise(ready => { markDocumentStarted = ready; });
  const documentGate = new Promise(ready => { releaseDocument = ready; });
  const embeddings = {
    status: () => ({ state: 'ready', modelVersion: 'fixture-v1' }),
    embedQuery: async () => ({ vector: [1, 0], profileId: 'builtin-multilingual', modelVersion: 'fixture-v1' }),
    embedDocuments: async texts => {
      markDocumentStarted();
      await documentGate;
      return { vectors: texts.map(() => [1, 0]), modelVersion: 'fixture-v1' };
    },
    close: async () => { releaseDocument(); }
  };
  const retrieval = new RetrievalCoordinator({ conversations, memory: { contextFor: async () => ({ entries: [] }) },
    tools: {}, embeddings });
  t.after(async () => {
    clearTimeout(timeout);
    releaseDocument();
    await retrieval.close();
    const suffix = relative(resolve(tmpdir()), resolve(root));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(root, { recursive: true, force: true });
  });
  await retrieval.library.add([{ path: join(root, 'synthetic-document.md'), title: 'Synthetic document',
    text: '项目资料中的历史记忆。' }], { scope: 'user' });
  const job = await retrieval.rebuild();
  await documentStarted;
  const result = await Promise.race([
    retrieval.search({ conversationId: 'chat-a', projectId: 'work-a' }, { query: '历史记忆0' }),
    new Promise((_, reject) => { timeout = setTimeout(() => reject(new Error('Foreground blocked behind background embedding')), 5000); })
  ]);
  assert.ok(result.items.length);
  assert.equal((await retrieval.jobs.get(job.jobId)).status, 'running');
  assert.equal((await conversations.readMessages('chat-a')).length, 200);
  // Background completion is deliberately gated; this proves isolation rather than a timing guess.
  // 后台完成被夹具主动阻止，以此证明前后台隔离，而非只比较易抖动的耗时。
});

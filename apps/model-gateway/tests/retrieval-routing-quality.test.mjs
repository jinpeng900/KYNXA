import assert from 'node:assert/strict';
import { test } from 'node:test';
import { buildRetrievalIntent, retrievalPlan } from '../orchestration/retrieval/query-plan.mjs';
import { selectCandidates } from '../orchestration/retrieval/candidate-selection.mjs';
import { lexicalTerms, lexicalText, matchExpression, retrievalQueryTerms } from '../data/retrieval/retrieval-text.mjs';

function candidate(id, path, excerpt, structure) {
  return { sourceId: path, chunkId: id, sourceRef: `test-ref:${id}`, scopeKey: 'project:fixture',
    sourceType: 'work-file', contentHash: `${path}-fixture`, title: path, excerpt,
    locator: { relativePath: path, startOffset: Number(id.replace(/\D/gu, '')) * 1000,
      endOffset: Number(id.replace(/\D/gu, '')) * 1000 + excerpt.length }, ...(structure ? { structure } : {}), score: 0.02 };
}

test('task context resolves ambiguous technical intent without stealing new document entities', () => {
  const taskContext = { domain: 'code', message: 'Inspect the mounted source repository.' };
  for (const query of ['取消后什么时候写入检查点？', 'Where is the selected language persisted?']) {
    assert.equal(buildRetrievalIntent(query, { taskContext }).domain, 'code');
    assert.equal(retrievalPlan(query, { taskContext }).shouldRetrieve, true);
  }
  assert.equal(buildRetrievalIntent('论文的方法及对照实验', { taskContext }).domain, 'knowledge');
  assert.equal(buildRetrievalIntent('Which method is preferable?').domain, 'mixed');
  assert.equal(buildRetrievalIntent('这种测试方法好吗？').domain, 'mixed');
  assert.equal(buildRetrievalIntent('分析这个实现方法').domain, 'mixed');
  assert.equal(buildRetrievalIntent('分析文档中的恢复步骤').domain, 'knowledge');
  assert.deepEqual(buildRetrievalIntent('Where is the persisted state?', { taskContext: 'Inspect SecretStore.cs and WriteToken.' }),
    { domain: 'code' }, 'task context supplies domain, never an unrelated prior file or symbol');
  assert.deepEqual(buildRetrievalIntent('Explain the documentation for OpenDoor'), { domain: 'knowledge' });
});

test('code task context never forces retrieval for greetings, generic news or direct execution', () => {
  const taskContext = { domain: 'code', message: 'Inspect the repository.' };
  for (const query of ['你好', '谢谢你', 'Tell me a joke', 'What is the latest football result?',
    '运行 node 检查脚本', 'Read input.txt and write output.txt.'])
    assert.equal(retrievalPlan(query, { taskContext }).shouldRetrieve, false, query);
  assert.equal(buildRetrievalIntent('Explain the documentation for OpenDoor', { taskContext }).domain, 'knowledge');
  assert.equal(buildRetrievalIntent('Explain the paper methods', { taskContext }).domain, 'knowledge');
  assert.equal(buildRetrievalIntent('How does the website at example.com handle login?', { taskContext }).domain, 'mixed');
  assert.equal(retrievalPlan('How does the website at example.com handle login?', { taskContext }).shouldRetrieve, false);
  assert.equal(buildRetrievalIntent('浏览器里的上下文怎样切换？', { taskContext }).domain, 'mixed');
  assert.equal(retrievalPlan('浏览器里的上下文怎样切换？', { taskContext }).shouldRetrieve, false);
  assert.equal(buildRetrievalIntent('浏览器模块的源码如何保存上下文？', { taskContext }).domain, 'code');
  assert.equal(retrievalPlan('给我讲讲论文的方法').shouldRetrieve, true);
});

test('quoted natural-language terms remain documents while precise code targets retain identifiers', () => {
  assert.deepEqual(buildRetrievalIntent('解释文档中 `persisted` 这个词'), { domain: 'knowledge' });
  assert.deepEqual(buildRetrievalIntent('Find `PersistReceipt` in ReceiptStore.ts'),
    { domain: 'code', symbol: 'PersistReceipt', path: 'receiptstore.ts' });
  assert.deepEqual(buildRetrievalIntent('解释文档和代码中 PersistReceipt 的差异'),
    { domain: 'mixed', symbol: 'PersistReceipt' });
});

test('technical query bridges are bounded and never change source term frequency or quoted evidence', () => {
  const source = 'persistReceipt cancelTask queueWork';
  const original = lexicalText(source);
  const expanded = retrievalQueryTerms('取消后持久化队列状态', { domain: 'code' });
  assert.ok(expanded.includes('cancel') && expanded.includes('persist') && expanded.includes('queue'));
  assert.ok(retrievalQueryTerms('cancellation after persistence', { domain: 'code' }).includes('persist'));
  assert.equal(retrievalQueryTerms('取消后持久化队列状态', { domain: 'knowledge' }).includes('cancel'), false);
  assert.equal(retrievalQueryTerms('取消索引缓存保存', { maximumTerms: 6 }).length, 6);
  assert.equal(lexicalText(source), original);
  assert.equal(lexicalTerms('cancelTask')[0], 'canceltask');
  assert.match(matchExpression('取消队列', { domain: 'code' }), /"cancel"/u);
  assert.doesNotMatch(matchExpression('权限 OR * "', { domain: 'code' }), / OR \*/u);
});

test('selection promotes actual declaration metadata and bilingual operation relevance without rewriting items', () => {
  const irrelevant = candidate('c1', 'src/samples.mjs', 'const unrelated = "Example words and unrelated output.";');
  const relevant = candidate('c2', 'src/work-runner.mjs', 'if (signal.aborted) saveReceipt();',
    { domain: 'code', kind: 'method', symbolName: 'cancelAndPersist', qualifiedName: 'WorkRunner.cancelAndPersist' });
  const result = selectCandidates([irrelevant, relevant], { query: '取消后如何持久化？', limit: 1, maximumTokens: 1000,
    retrievalIntent: { domain: 'code' } });
  assert.equal(result.items[0], relevant);
  assert.equal(result.items[0].excerpt, 'if (signal.aborted) saveReceipt();');
  const exact = selectCandidates([irrelevant, relevant], { query: 'cancelAndPersist', limit: 1, maximumTokens: 1000,
    retrievalIntent: { domain: 'code', symbol: 'cancelAndPersist' } });
  assert.equal(exact.items[0], relevant);
});

test('soft file diversity keeps cross-file roles and permits essential same-file evidence beyond two chunks', () => {
  const sameFile = [
    candidate('c1', 'src/jobs.mjs', 'export function scheduleTask() { queue.push(task); }'),
    candidate('c2', 'src/jobs.mjs', 'export function startTask() { task.running = true; }'),
    candidate('c3', 'src/jobs.mjs', 'export function finishTask() { task.completed = true; }'),
    candidate('c4', 'src/jobs.mjs', 'export function retryTask() { task.attempts++; }'),
  ];
  const tests = candidate('c5', 'tests/jobs.test.mjs', 'test("queue and retries", () => verifyTaskLifecycle());');
  const settings = candidate('c6', 'config/jobs-settings.json', '{"queueLimit": 8, "retryLimit": 3}');
  const diverse = selectCandidates([...sameFile, tests, settings], { query: '解释任务流程、测试和配置', limit: 4,
    maximumTokens: 4000, requiresSourceRead: true, retrievalIntent: { domain: 'code' } });
  assert.ok(diverse.items.some(item => item.sourceId === tests.sourceId));
  assert.ok(diverse.items.some(item => item.sourceId === settings.sourceId));
  const precise = selectCandidates([...sameFile, tests], { query: 'Explain src/jobs.mjs', limit: 4,
    maximumTokens: 4000, retrievalIntent: { domain: 'code', path: 'src/jobs.mjs' } });
  assert.equal(precise.items.filter(item => item.sourceId === 'src/jobs.mjs').length, 4);
});

test('bounded diversity keeps supplementary roles available behind many distinct same-file fragments', () => {
  const fragments = Array.from({ length: 12 }, (_, index) => candidate(`c${index + 1}`, 'src/tasks.mjs',
    `export function stage${index}() { task.part${index} = ${index}; }`, { domain: 'code', symbolName: `stage${index}` }));
  const tests = candidate('c14', 'tests/tasks.test.mjs', 'test("task lifecycle", () => verifyTask());');
  const settings = candidate('c15', 'settings/task-config.json', '{"taskQueue":4,"retryCount":2}');
  const result = selectCandidates([...fragments, tests, settings], { query: '解释任务的实现流程、测试和配置',
    requiresSourceRead: true, limit: 6, maximumTokens: 4096, retrievalIntent: { domain: 'code' } });
  assert.ok(result.items.some(item => item.sourceId === tests.sourceId));
  assert.ok(result.items.some(item => item.sourceId === settings.sourceId));
  assert.ok(result.items.filter(item => item.sourceId === 'src/tasks.mjs').length > 2);
  const copy = candidate('c16', 'src/copied.mjs', fragments[0].excerpt);
  const precise = selectCandidates([copy, fragments[0]], { query: 'src/tasks.mjs', limit: 1, maximumTokens: 1000,
    retrievalIntent: { domain: 'code', path: 'src/tasks.mjs' } });
  assert.equal(precise.items[0], fragments[0]);
});

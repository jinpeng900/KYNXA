import assert from 'node:assert/strict';
import { test } from 'node:test';
import { buildRetrievalIntent, classifyTaskRelation, interpretRetrievalQuery, planEvidenceAcquisition, retrievalPlan } from '../orchestration/retrieval/query-plan.mjs';
import { selectCandidates } from '../orchestration/retrieval/candidate-selection.mjs';
import { lexicalTerms, lexicalText, matchExpression, retrievalQueryTerms } from '../data/retrieval/retrieval-text.mjs';

function candidate(id, path, excerpt, structure) {
  return { sourceId: path, chunkId: id, sourceRef: `test-ref:${id}`, scopeKey: 'project:fixture',
    sourceType: 'work-file', contentHash: `${path}-fixture`, title: path, excerpt,
    locator: { relativePath: path, startOffset: Number(id.replace(/\D/gu, '')) * 1000,
      endOffset: Number(id.replace(/\D/gu, '')) * 1000 + excerpt.length }, ...(structure ? { structure } : {}), score: 0.02 };
}

test('task context prefers a domain without excluding new document entities or opposite-domain evidence', () => {
  const taskContext = { domain: 'code', message: 'Inspect the mounted source repository.' };
  for (const query of ['取消后什么时候写入检查点？', 'Where is the selected language persisted?']) {
    assert.deepEqual(buildRetrievalIntent(query, { taskContext }), { domain: 'mixed', preferredDomain: 'code' });
    assert.equal(retrievalPlan(query, { taskContext }).shouldRetrieve, false,
      'a historical domain plus technical vocabulary is a clue, not an automatic local search');
  }
  assert.deepEqual(buildRetrievalIntent('论文的方法及对照实验', { taskContext }), { domain: 'mixed', preferredDomain: 'knowledge' });
  assert.equal(buildRetrievalIntent('Which method is preferable?').domain, 'mixed');
  assert.equal(buildRetrievalIntent('这种测试方法好吗？').domain, 'mixed');
  assert.equal(buildRetrievalIntent('分析这个实现方法').domain, 'mixed');
  assert.deepEqual(buildRetrievalIntent('分析文档中的恢复步骤'), { domain: 'mixed', preferredDomain: 'knowledge' });
  assert.deepEqual(buildRetrievalIntent('Where is the persisted state?', { taskContext: 'Inspect SecretStore.cs and WriteToken.' }),
    { domain: 'mixed', preferredDomain: 'code' }, 'task context supplies a ranking clue, never an unrelated prior target');
  assert.deepEqual(buildRetrievalIntent('Explain the documentation for OpenDoor'), { domain: 'mixed', preferredDomain: 'knowledge' });
});

test('code task context never forces retrieval for greetings, generic news or direct execution', () => {
  const taskContext = { domain: 'code', message: 'Inspect the repository.' };
  for (const query of ['你好', '谢谢你', 'Tell me a joke', 'What is the latest football result?',
    '运行 node 检查脚本', 'Read input.txt and write output.txt.'])
    assert.equal(retrievalPlan(query, { taskContext }).shouldRetrieve, false, query);
  assert.deepEqual(buildRetrievalIntent('Explain the documentation for OpenDoor', { taskContext }), { domain: 'mixed', preferredDomain: 'knowledge' });
  assert.deepEqual(buildRetrievalIntent('Explain the paper methods', { taskContext }), { domain: 'mixed', preferredDomain: 'knowledge' });
  assert.equal(buildRetrievalIntent('How does the website at example.com handle login?', { taskContext }).domain, 'mixed');
  assert.equal(retrievalPlan('How does the website at example.com handle login?', { taskContext }).shouldRetrieve, false);
  assert.equal(buildRetrievalIntent('浏览器里的上下文怎样切换？', { taskContext }).domain, 'mixed');
  assert.equal(retrievalPlan('浏览器里的上下文怎样切换？', { taskContext }).shouldRetrieve, false);
  assert.deepEqual(buildRetrievalIntent('浏览器模块的源码如何保存上下文？', { taskContext }), { domain: 'mixed', preferredDomain: 'code' });
  assert.equal(retrievalPlan('给我讲讲论文的方法').shouldRetrieve, true);
});

test('quoted natural-language terms remain documents while precise code targets retain identifiers', () => {
  assert.deepEqual(buildRetrievalIntent('解释文档中 `persisted` 这个词'), { domain: 'mixed', preferredDomain: 'knowledge' });
  assert.deepEqual(buildRetrievalIntent('Find `PersistReceipt` in ReceiptStore.ts'),
    { domain: 'mixed', symbol: 'PersistReceipt', path: 'receiptstore.ts', preferredDomain: 'code' });
  const navigation = retrievalPlan('Find the definition of `normalizeAttention` and check zero totals.');
  assert.deepEqual(navigation.retrievalIntent, { domain: 'mixed', symbol: 'normalizeAttention', preferredDomain: 'code' });
  assert.equal(navigation.shouldRetrieve, true);
  assert.equal(navigation.domainConstraint, 'none');
  assert.deepEqual(buildRetrievalIntent('解释文档和代码中 PersistReceipt 的差异'),
    { domain: 'mixed' }, 'capitalization alone cannot make a comparison an exact declaration lookup');
});

test('weak words, brands and technology topics stay open until the request names a concrete source or declaration', () => {
  const broadQueries = [
    'Which method is preferable?', 'What is the function of this policy?', 'Explain this symbol in the poem.',
    '怎样发展人际 network？', '设备 network 的概念是什么？', 'How should people react to criticism?',
    'React 框架与普通响应有什么区别？', '解释 OpenAI 品牌', '解释 `OpenAI` 的品牌名',
    'Explain the "OpenAI" company name.', 'Find a method for OpenAI research.', '查找 OpenAI 的方法',
    '查找OpenAI方法的优势', "Find the definition of OpenAI's training method.",
  ];
  for (const query of broadQueries) {
    const interpreted = interpretRetrievalQuery(query);
    assert.deepEqual(interpreted.intent, { domain: 'mixed' }, query);
    assert.equal(interpreted.domainConstraint, 'none', query);
    assert.equal(retrievalPlan(query).shouldRetrieve, false, query);
  }
  assert.deepEqual(buildRetrievalIntent('解释论文中这个 method 的对照实验'), { domain: 'mixed', preferredDomain: 'knowledge' });
  assert.deepEqual(buildRetrievalIntent('解释仓库中 network 连接模块的源码'), { domain: 'mixed', preferredDomain: 'code' });
  assert.deepEqual(buildRetrievalIntent('分析 React 项目的源码'), { domain: 'mixed', preferredDomain: 'code' });
  assert.deepEqual(buildRetrievalIntent('src/client.ts 中 OpenAI 请求支持吗？'),
    { domain: 'mixed', path: 'src/client.ts', preferredDomain: 'code' }, 'a source path does not turn every mixed-case name into a symbol');
  assert.deepEqual(buildRetrievalIntent('查找 `OpenAI` 函数的定义'), { domain: 'mixed', symbol: 'OpenAI', preferredDomain: 'code' });
  assert.ok(interpretRetrievalQuery('解释 OpenAI 品牌').clues.some(clue =>
    clue.kind === 'symbol-candidate' && clue.value === 'OpenAI' && clue.strength === 'weak'));
});

test('explicit caller filters keep their semantics while unrelated historical domains never become hard restrictions', () => {
  const interpreted = interpretRetrievalQuery('解释这个 method', { domain: 'knowledge',
    symbol: 'OpenAI', path: 'notes/Methods.md', taskContext: { domain: 'code' } });
  assert.deepEqual(interpreted.intent, { domain: 'knowledge', symbol: 'OpenAI', path: 'notes/methods.md' });
  assert.equal(interpreted.domainConstraint, 'explicit');
  assert.ok(interpreted.clues.filter(clue => clue.origin === 'caller').every(clue => clue.strength === 'strong'));
  assert.deepEqual(buildRetrievalIntent('为什么这个蛋糕很干？', { taskContext: { domain: 'code' } }), { domain: 'mixed' });
  assert.deepEqual(buildRetrievalIntent('新话题：来源 https://example.com/src/main.ts 是什么？'), { domain: 'mixed' });
  assert.equal(buildRetrievalIntent('比较 docs/Guide.md 与 src/main.ts').path, undefined,
    'a multi-file comparison cannot be constrained to the first mentioned file');
});

test('affirmative source names and file extensions prefer candidates while caller domains alone constrain them', () => {
  for (const [query, preferredDomain] of [
    ['分析仓库代码的取消流程', 'code'], ['解释文档中模型训练方法', 'knowledge'],
    ['Explain src/runner.ts cancellation.', 'code'], ['Review docs/research.pdf experiments.', 'knowledge'],
  ]) {
    const interpreted = interpretRetrievalQuery(query);
    assert.equal(interpreted.intent.domain, 'mixed', query);
    assert.equal(interpreted.intent.preferredDomain, preferredDomain, query);
    assert.equal(interpreted.domainConstraint, 'none', query);
    assert.ok(interpreted.clues.filter(clue => clue.kind === 'domain-candidate').every(clue => clue.strength === 'weak'));
  }
  const explicit = interpretRetrievalQuery('分析 src/runner.ts 中的实现', { domain: 'knowledge' });
  assert.equal(explicit.intent.domain, 'knowledge');
  assert.equal(explicit.domainConstraint, 'explicit');
  const codePlan = retrievalPlan('优化仓库源码中的取消逻辑');
  assert.deepEqual(codePlan.evidenceRoles, ['source', 'conditions']);
  assert.ok(codePlan.suggestedEvidenceRoles.includes('implementation'));
  assert.deepEqual(planEvidenceAcquisition({ query: '优化论文中的解释',
    intent: buildRetrievalIntent('优化论文中的解释') }).requiredChecks,
  ['re-read-current-source', 'check-deliverable-against-requirements']);
  assert.deepEqual(planEvidenceAcquisition({ query: '修复 src/worker.ts',
    intent: buildRetrievalIntent('修复 src/worker.ts', { domain: 'code' }) }).requiredChecks,
  ['re-read-current-source', 'run-relevant-validation']);
});

test('affirmative clause projection removes rejected sources and natural task exits before local retrieval planning', () => {
  const prior = { Role: 'user', Content: '解释 src/worker.ts 中任务取消的实现' };
  const options = { history: [prior], taskContext: { domain: 'code' } };
  for (const original of [
    '先不说代码了，聊聊这个蛋糕为什么会干',
    '现在聊另一件事：这个方法在心理学实验中有什么用途',
    '不是让你查看 src/worker.ts，我是想了解人际网络的方法',
    '暂且搁置仓库，OpenAI这个品牌是怎么命名的',
    '算了，聊点别的：这个方法如何帮助建立人际信任',
    'Never mind the previous task. How can this method improve a social network?',
  ]) {
    const plan = retrievalPlan(original, options);
    assert.equal(plan.originalQuery, original);
    assert.equal(plan.domain, 'mixed');
    assert.equal(plan.path, undefined);
    assert.equal(plan.symbol, undefined);
    assert.equal(plan.preferredDomain, undefined);
    assert.equal(plan.shouldRetrieve, false, original);
    assert.equal(plan.taskRelation.allowsInheritance, false);
    assert.deepEqual(plan.queryDerivation.additions, []);
  }
  const corrected = retrievalPlan('不要查代码，我想看这篇论文的结果', options);
  assert.equal(corrected.preferredDomain, 'knowledge');
  assert.equal(corrected.shouldRetrieve, true);
  assert.doesNotMatch(corrected.query, /代码|worker\.ts/u);
  assert.deepEqual(corrected.queryDerivation.additions, []);
  for (const original of ['为什么这个蛋糕没有保存水分？', '取消订单后，退款为何超时？']) {
    const plan = retrievalPlan(original, options);
    assert.equal(plan.shouldRetrieve, false, original);
    assert.equal(plan.taskRelation.allowsInheritance, false);
    assert.equal(plan.domain, 'mixed');
  }
});

test('derived continuation queries preserve original text and record a bounded addition without inheriting hard targets', () => {
  const original = '  继续，这个方法为什么需要取消检查？\n';
  const prior = { Id: 'user-prior', Role: 'user', Status: 'completed',
    Content: '解释 src/runner.ts 中 CancelWork 方法的定义与生命周期' };
  const plan = retrievalPlan(original, { history: [prior] });
  assert.equal(plan.originalQuery, original);
  assert.ok(plan.query.startsWith(`${original}\n`));
  assert.equal(plan.taskRelation.type, 'continue');
  assert.equal(plan.shouldRetrieve, true);
  assert.deepEqual(plan.retrievalIntent, { domain: 'mixed', preferredDomain: 'code' });
  assert.equal(plan.path, undefined);
  assert.equal(plan.symbol, undefined);
  assert.deepEqual(plan.queryDerivation, { originalQuery: original,
    additions: [{ text: prior.Content, origin: 'history', historyId: 'user-prior', basis: 'explicit-continuation' }],
    replacements: [], preservedOriginal: true });
  const long = retrievalPlan(`继续，${'x'.repeat(3970)}`, { history: [{ ...prior, Content: `${'😀'.repeat(100)} 文档` }] });
  assert.ok(long.query.length <= 4000);
  assert.doesNotMatch(long.query, /[\uD800-\uDBFF]$/u);
});

test('the latest actual source takes precedence over an older task domain during a genuine continuation', () => {
  const query = '还有，这个方法的实验条件是什么？';
  const latest = { Id: 'latest-paper', Role: 'user', Status: 'completed',
    Content: '总结 docs/psychology.md 的对照实验' };
  const plan = retrievalPlan(query, { taskContext: { domain: 'code', message: '检查早先的代码任务' }, history: [
    { Role: 'user', Status: 'completed', Content: '解释 src/worker.ts 的取消逻辑' },
    { Role: 'user', Status: 'completed', Content: '换个话题，谈谈心理咨询中的方法' }, latest,
  ] });
  assert.equal(plan.shouldRetrieve, true);
  assert.deepEqual(plan.retrievalIntent, { domain: 'mixed', preferredDomain: 'knowledge' });
  assert.equal(plan.originalQuery, query);
  assert.equal(plan.path, undefined);
  assert.equal(plan.symbol, undefined);
  assert.deepEqual(plan.queryDerivation.additions, [{ text: latest.Content, origin: 'history',
    basis: 'explicit-continuation', historyId: latest.Id }]);
  assert.doesNotMatch(plan.query, /src\/worker\.ts/u);
  assert.deepEqual(buildRetrievalIntent(query, { domain: 'code', taskContext: latest.Content }), { domain: 'code' },
    'a caller domain remains explicit even when conversational evidence has a different preference');
});

test('topic switches, source conflicts and intervening unrelated turns stop inherited query pollution', () => {
  const history = [{ Role: 'user', Content: '解释 src/runner.ts 的取消逻辑' }];
  const taskContext = { domain: 'code', message: history[0].Content };
  for (const query of ['换个话题，蛋糕为什么干？', 'New topic: how should people react to criticism?']) {
    const plan = retrievalPlan(query, { history, taskContext });
    assert.equal(plan.taskRelation.type, 'topic-switch');
    assert.equal(plan.shouldRetrieve, false);
    assert.deepEqual(plan.retrievalIntent, { domain: 'mixed' });
    assert.equal(plan.originalQuery, query);
    assert.deepEqual(plan.queryDerivation.additions, []);
  }
  const paper = retrievalPlan('另外，这篇论文的方法和对照实验是什么？', { history, taskContext });
  assert.equal(paper.domain, 'mixed');
  assert.equal(paper.preferredDomain, 'knowledge');
  assert.equal(paper.shouldRetrieve, true);
  assert.equal(paper.taskRelation.allowsInheritance, false);
  assert.equal(paper.taskRelation.reason, 'current-target-conflicts-with-prior-context');
  assert.deepEqual(paper.queryDerivation.additions, []);
  const otherFile = retrievalPlan('继续解释 src/client.ts 的取消逻辑', { history, taskContext });
  assert.equal(otherFile.path, 'src/client.ts');
  assert.deepEqual(otherFile.queryDerivation.additions, []);
  const unrelated = retrievalPlan('继续，它为什么这么干？', { history: [...history,
    { Role: 'user', Content: '换个话题，蛋糕太干了' }] });
  assert.equal(unrelated.shouldRetrieve, false);
  assert.deepEqual(unrelated.queryDerivation.additions, []);
});

test('explicit corrections replace rejected retrieval conditions and leave original wording available for audit', () => {
  const history = [{ Role: 'user', Content: '解释 src/old.ts 的恢复流程，使用 Windows 11 和 2025 年版本' }];
  const original = '不是 src/old.ts，而是 docs/new.md，分析 Linux 版本的恢复条件';
  const plan = retrievalPlan(original, { history, taskContext: { domain: 'code' } });
  assert.equal(plan.originalQuery, original);
  assert.equal(plan.taskRelation.type, 'correction');
  assert.equal(plan.domain, 'mixed');
  assert.equal(plan.preferredDomain, 'knowledge');
  assert.equal(plan.path, 'docs/new.md');
  assert.match(plan.query, /docs\/new\.md，分析 Linux 版本的恢复条件/u);
  assert.doesNotMatch(plan.query, /old\.ts|Windows|2025/u);
  assert.deepEqual(plan.queryDerivation.additions, []);
  assert.equal(plan.queryDerivation.replacements.length, 1);
  assert.match(plan.queryDerivation.replacements[0].rejected, /src\/old\.ts/u);
  assert.equal(plan.queryDerivation.replacements[0].accepted, plan.query);
  assert.equal(plan.queryDerivation.replacements[0].basis, 'explicit-user-replacement');
  const correctedTopic = retrievalPlan('不是代码，是人际网络的联系', { history });
  assert.equal(correctedTopic.shouldRetrieve, false);
  assert.equal(correctedTopic.domain, 'mixed');
  const changedYear = retrievalPlan('更正：论文年份改为 2024，不是 2025', { history });
  assert.equal(changedYear.taskRelation.type, 'correction');
  assert.deepEqual(changedYear.queryDerivation.additions, []);
  assert.equal(changedYear.originalQuery, '更正：论文年份改为 2024，不是 2025');
  assert.match(changedYear.query, /2024/u);
  assert.doesNotMatch(changedYear.query, /2025/u);
  assert.equal(classifyTaskRelation('另外，补充条件').type, 'supplement');
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

test('Windows source paths retain their drive and filename discussions do not schedule source reads', () => {
  const message = '请看 C:\\Projects\\KYNXA\\src\\worker.ts 的取消逻辑。';
  const windows = retrievalPlan(message);
  assert.equal(windows.path, 'c:/projects/kynxa/src/worker.ts');
  assert.equal(windows.originalQuery, message);
  assert.equal(windows.shouldRetrieve, true);
  assert.equal(windows.domain, 'mixed');
  const example = '讨论一个示例文件名“src/ghost.ts”的命名风格，不要读取它。';
  const nameOnly = retrievalPlan(example);
  assert.equal(nameOnly.shouldRetrieve, false);
  assert.equal(nameOnly.originalQuery, example);
  assert.deepEqual(nameOnly.queryDerivation.additions, []);
  assert.equal(retrievalPlan('解释文件名“src/worker.ts”的命名风格，并检查源码实现').shouldRetrieve, true);
  assert.equal(retrievalPlan('根据项目文档讨论示例文件名“src/ghost.ts”的命名风格').shouldRetrieve, true);
  assert.equal(retrievalPlan('解释“src/worker.ts”的取消逻辑').shouldRetrieve, true);
});

test('successive explicit continuations retain a source anchor until an unrelated turn or boundary', () => {
  const source = { Id: 'source-user', Role: 'user', Status: 'completed', Content: '解释 docs/design.md 的恢复步骤' };
  const followup = { Role: 'user', Status: 'completed', Content: '继续分析它的失败原因' };
  const message = '继续比较它们的恢复方案';
  const plan = retrievalPlan(message, { history: [source, followup], taskContext: followup.Content });
  assert.equal(plan.shouldRetrieve, true);
  assert.equal(plan.originalQuery, message);
  assert.match(plan.query, /docs\/design\.md/u);
  assert.equal(plan.queryDerivation.additions[0].historyId, source.Id);
  assert.equal(plan.preferredDomain, 'knowledge');
  for (const boundary of ['换个话题，蛋糕为什么干？', '不是文档，是品牌发音', '取消全部操作', '蛋糕为什么干？']) {
    const unrelated = retrievalPlan(message, { history: [source, { Role: 'user', Content: boundary }, followup] });
    assert.equal(unrelated.shouldRetrieve, false, boundary);
    assert.deepEqual(unrelated.queryDerivation.additions, [], boundary);
  }
  const replacement = retrievalPlan(message, { history: [source,
    { Role: 'user', Content: '换个话题，解释 docs/new.md 的恢复方案' }, followup] });
  assert.equal(replacement.shouldRetrieve, true);
  assert.match(replacement.query, /docs\/new\.md/u);
  assert.doesNotMatch(replacement.query, /docs\/design\.md/u);
});

import { MAX_QUERY_CHARACTERS, validateRetrievalIntent } from '../../data/retrieval/retrieval-contracts.mjs';
import { analyzeRequestClauses, requestInstructionText } from '../../platform/request-clause-signals.mjs';

export function isSimpleGreeting(message) {
  return /^(?:你好|您好|嗨|哈喽|早上好|晚上好|早安|晚安|hello|hi|hey|good morning|good evening)[\s!！。.~～]*$/iu.test(message.trim());
}

const LOCAL_CONTEXT_PATTERN = /之前|记得|记忆|资料|文档|知识|项目|工作|代码|文件|我们讨论|上次|history|remember|document|knowledge|project|code|file|previous/iu;
const FILE_REFERENCE_PATTERN = /(?<![\w\p{Script=Han}./\\-])(?:[\w\p{Script=Han}./\\-]+\.(?:md|txt|pdf|docx|cs|mjs|js|jsx|ts|tsx|json|yaml|yml|py|rs|go|sql|xaml))\b/iu;
const FOLLOWUP_PATTERN = /(?:它|他们|她们|它们)|(?:这个|那个)(?:方法|函数|问题|文件|模块|模型|方案|结果|条件|符号)|这份|那份|这篇|那篇|继续|刚才|上面|\b(?:it|its|continue|what about|(?:this|that)\s+(?:method|function|question|file|module|model|result|constraint))\b/iu;
const SMALL_TALK_PATTERN = /^(?:谢谢(?:你)?|多谢|好的|好吧|没事|嗯|聊聊天|陪我聊聊|讲个笑话|最近怎么样|今天心情怎么样|thanks?(?: you)?|okay|ok|how are you|tell me a joke)[\s!！?？。.~～]*$/iu;
const KNOWLEDGE_DEPENDENCY_PATTERN = /资料|文档|知识|记忆|记得|回忆|历史|我们讨论|上次|(?:之前|以前).{0,16}(?:说|讲|讨论|决定|确认|记)|\b(?:history|remember|recall|knowledge|documentation|docs?|previous\s+(?:discussion|decision|conversation)|saved\s+(?:memory|notes?))\b/iu;
const ANALYSIS_REQUEST_PATTERN = /分析|解释|讲解|总结|概括|归纳|比较|对比|权衡|架构|方案|研究|调查|综述|审查|评审|诊断|为何|为什么|如何|怎么|怎样|哪里|在哪|何处|何时|什么时候|是否|是什么|有哪些|逻辑|原理|机制|报错原因|缺陷|漏洞|\b(?:explain|analy[sz]e|summari[sz]e|compare|review|inspect|diagnose|investigate|research|architecture|logic|implementation|semantics|why|where|when|which|what\s+(?:is|are|does)|how\s+(?:to|is|are|does|do|can|should)|find\s+(?:bugs?|errors?))\b/iu;
const EXECUTION_ACTION_PATTERN = /读取|读回|回读|读出|打开|查看|列出|写入|写到|写出|保存|创建|新建|追加|复制|移动|重命名|删除|执行|运行|启动|安装|构建|编译|终端|命令行|\b(?:read|open|list|write|save|create|append|copy|move|rename|delete|execute|run|launch|install|build|compile|terminal|shell)\b/iu;
const FILE_ACTION_PREFIX_PATTERN = /^(?:(?:请|你|先|然后|再|依次|直接|帮我)*)(读取|读回|回读|读出|打开|查看|列出|写入|写到|写出|保存|创建|新建|追加|复制|移动|重命名|删除)/u;
const EXTERNAL_CONTEXT_PATTERN = /https?:\/\/|网页|网站|网址|浏览器|联网|上网|天气|新闻|\b(?:web|websites?|browser|online|internet|weather|news)\b/iu;
const TASK_DETAIL_PATTERN = /取消|中断|恢复|重试|超时|队列|并发|绑定|缓存|索引|权限|审批|持久化|保存|存储|写入|序列化|分块|分词|嵌入|重排|上下文|引用|来源|配置|设置|生命周期|\b(?:cancel\w*|abort\w*|retry|retries|timeout|queue\w*|concurren\w*|binding|cache\w*|index\w*|permission\w*|approval|persist\w*|stor\w*|serializ\w*|chunk\w*|tokeniz\w*|embedding\w*|rerank\w*|context|reference\w*|configuration|settings|lifecycle)\b/iu;
const CODE_SOURCE_PATTERN = /代码|源码|源文件|仓库|代码库|调用方|调用链|类型定义|单元测试|\b(?:source\s+code|source\s+files?|repository|repo|codebase|unit\s+tests?)\b|\b(?:explain|analy[sz]e|review|inspect|find|search|fix|modify|refactor)\b.{0,48}\bcode\b/iu;
const DOCUMENT_SOURCE_PATTERN = /论文|文档|知识库|章节|笔记|\b(?:documentation|documents?|docs?|manuals?|chapters?)\b|\b(?:papers?)\s+(?:methods?|results?|experiments?|findings?)\b|\b(?:explain|analy[sz]e|review|read|summari[sz]e|compare)\b.{0,48}\b(?:papers?|notes?)\b/iu;
const DECLARATION_TARGET_PATTERN = /(?:函数|方法|符号|类型).{0,12}(?:定义|实现|调用方|引用)|(?:查找|找到|定位).{0,40}定义|\b(?:find|locate|go\s+to)\b.{0,48}\b(?:definition|declaration)\b|\b(?:function|method|symbol|type)\b.{0,24}\b(?:definition|implementation|references|callers)\b/iu;
const IDENTIFIER_PATTERN = /[A-Za-z_$][\w$]*(?:\.[A-Za-z_$][\w$]*)*/gu;
const TARGET_PATH_PATTERN = /(?<![\w\p{Script=Han}./\\-])(?:[A-Za-z]:[\\/])?(?:[\w\p{Script=Han}.-]+[\\/])*[\w\p{Script=Han}.-]+\.(?:cs|[cm]?js|jsx|[cm]?ts|tsx|py|rs|go|java|cpp|c|h|ps1|sql|json|yaml|yml|xaml|xml|toml|html|css|md|markdown|txt|pdf|docx)\b/giu;
const CODE_PATH_PATTERN = /\.(?:cs|[cm]?js|jsx|[cm]?ts|tsx|py|rs|go|java|cpp|c|h|ps1|sql|xaml|html|css)$/iu;
const DOCUMENT_PATH_PATTERN = /\.(?:md|markdown|txt|pdf|docx)$/iu;
const SUPPLEMENT_PATTERN = /^(?:补充|还有|另外[，,]|此外|\b(?:also|additionally|one\s+more\s+thing)\b)/iu;

function retrievalDomain(query) {
  const paths = queryPaths(query);
  const code = CODE_SOURCE_PATTERN.test(query) || paths.some(value => CODE_PATH_PATTERN.test(value));
  const document = DOCUMENT_SOURCE_PATTERN.test(query) || paths.some(value => DOCUMENT_PATH_PATTERN.test(value));
  return code && document ? 'mixed' : code ? 'code' : document ? 'knowledge' : 'mixed';
}

function taskContextDomain(context) {
  if (context && typeof context === 'object' && ['code', 'knowledge', 'mixed'].includes(context.domain)) return context.domain;
  const text = typeof context === 'string' ? context : context?.message;
  return retrievalDomain(typeof text === 'string' ? analyzeRequestClauses(text.slice(0, MAX_QUERY_CHARACTERS)).activeText : '');
}

/** Relation signals allow context reuse; they never authorize an operation or rewrite a correction.
 * 任务关系线索只允许复用上下文，不授予操作权限，也不把纠正前的条件追加回来。 */
export function classifyTaskRelation(message) {
  const clauses = analyzeRequestClauses(message), text = clauses.activeText.trim();
  if (clauses.boundary !== 'none') return { type: clauses.boundary, allowsInheritance: false,
    reason: clauses.boundaryReason ?? 'explicit-task-boundary' };
  if (clauses.excludedClauses.length) return { type: 'uncertain', allowsInheritance: false, reason: 'explicit-clause-exclusion' };
  if (SUPPLEMENT_PATTERN.test(text)) return { type: 'supplement', allowsInheritance: true, reason: 'explicit-supplement' };
  if (FOLLOWUP_PATTERN.test(text)) return { type: 'continue', allowsInheritance: true, reason: 'continuation-reference' };
  return { type: 'new', allowsInheritance: false, reason: 'no-continuation-reference' };
}

function queryPaths(text) {
  if (!text.includes('.')) return [];
  const urls = [...text.matchAll(/https?:\/\/[^\s`"'<>]+/giu)];
  return [...text.matchAll(TARGET_PATH_PATTERN)].filter(match => !urls.some(url =>
    match.index >= url.index && match.index < url.index + url[0].length))
    .map(match => match[0].replace(/^(?:(?:请|你|先|然后|再|依次|直接|帮我)*)(?:读取|读回|回读|读出|打开|查看|列出|写入|写到|写出|保存|创建|新建|追加|复制|移动|重命名|删除)/u, ''));
}

/** Caller domains constrain retrieval; natural-language source and target clues only rank candidates.
 * 只有调用方指定的领域才约束检索，自然语言来源与目标线索只用于候选排序，用户原话保持不变。 */
export function interpretRetrievalQuery(query, { domain, symbol, path, preferredDomain, taskContext,
  taskRelation = classifyTaskRelation(query) } = {}) {
  const originalText = String(query ?? ''), clues = [], clauses = analyzeRequestClauses(originalText);
  const text = clauses.activeText;
  for (const clause of clauses.excludedClauses) clues.push({ kind: 'rejected-condition', value: clause.text,
    strength: 'weak', origin: 'user', basis: clause.basis, startOffset: clause.startOffset, endOffset: clause.endOffset });
  const codeSource = CODE_SOURCE_PATTERN.test(text), documentSource = DOCUMENT_SOURCE_PATTERN.test(text);
  const paths = [...new Set(queryPaths(text))];
  const explicitDomain = domain !== undefined, explicitPath = path !== undefined, explicitSymbol = symbol !== undefined;
  if (explicitDomain) clues.push({ kind: 'domain', value: domain, strength: 'strong', origin: 'caller' });
  if (explicitPath) clues.push({ kind: 'path', value: path, strength: 'strong', origin: 'caller' });
  if (explicitSymbol) clues.push({ kind: 'symbol', value: symbol, strength: 'strong', origin: 'caller' });
  for (const value of paths) clues.push({ kind: 'path', value, strength: 'strong', origin: 'user' });
  if (!explicitPath && paths.length === 1) path = paths[0];
  if (codeSource) clues.push({ kind: 'domain-candidate', value: 'code', strength: 'weak', origin: 'user' });
  if (documentSource) clues.push({ kind: 'domain-candidate', value: 'knowledge', strength: 'weak', origin: 'user' });
  const quotedIdentifiers = [...text.matchAll(/([`"'“])([A-Za-z_$][\w$]*(?:\.[A-Za-z_$][\w$]*)*)(\(\))?[`"'”]/gu)];
  const identifiers = text.match(IDENTIFIER_PATTERN) ?? [];
  const isPathPart = value => paths.some(candidate => candidate.toLowerCase().includes(value.toLowerCase())) ||
    typeof path === 'string' && path.toLowerCase().includes(value.toLowerCase());
  const isSymbolShape = value => /[a-z\d][A-Z]|[A-Z][a-z]+[A-Z]|\w_\w|\./u.test(value);
  const codePath = typeof path === 'string' && CODE_PATH_PATTERN.test(path);
  const namedSymbols = /函数|方法|符号|类型|\b(?:function|method|symbol|type)\b/iu.test(text) ? [
    ...text.matchAll(/(?:函数|方法|符号|类型|\b(?:function|method|symbol|type)\b)\s*[`"'“]?([A-Za-z_$][\w$]*(?:\.[A-Za-z_$][\w$]*)*)/giu),
    ...text.matchAll(/[`"'“]?([A-Za-z_$][\w$]*(?:\.[A-Za-z_$][\w$]*)*)[`"'”]?\s*(?:函数|方法|符号|类型|\s+\b(?:function|method|symbol|type)\b)/giu),
  ].map(match => match[1]).filter(value => !isPathPart(value) && (isSymbolShape(value) ||
    quotedIdentifiers.some(match => match[2] === value))) : [];
  const namedDeclaration = namedSymbols.length > 0 && (codeSource || codePath);
  const declarationRequest = (DECLARATION_TARGET_PATTERN.test(text) || namedDeclaration) &&
    (!documentSource || codeSource || codePath);
  const declarationTarget = declarationRequest && (namedSymbols.length > 0 ||
    quotedIdentifiers.length > 0 && /函数|符号|类型|\b(?:function|symbol|type)\b/iu.test(text));
  const namedCodeTarget = declarationTarget && (codeSource || codePath ||
    /函数|符号|类型|\b(?:function|symbol|type)\b/iu.test(text));
  const quotedTarget = quotedIdentifiers.find(match => !isPathPart(match[2]) &&
    (namedCodeTarget || declarationRequest && match[1] === '`' && /定义|\b(?:definition|declaration)\b/iu.test(text) ||
      codePath && match[1] === '`' && /查找|找到|定位|\b(?:find|locate)\b/iu.test(text) ||
      !documentSource && match[1] === '`' && (match[3] || match[2].includes('.'))));
  if (!explicitSymbol) {
    symbol = quotedTarget?.[2] ?? (namedCodeTarget ? namedSymbols[0] : undefined);
  }
  if (symbol && !explicitSymbol) clues.push({ kind: 'symbol', value: symbol, strength: 'strong', origin: 'user' });
  const identifierCandidates = new Set([...quotedIdentifiers.map(match => match[2]), ...identifiers.filter(isSymbolShape)]);
  for (const value of identifierCandidates) if (!isPathPart(value) && value !== symbol)
    clues.push({ kind: 'symbol-candidate', value, strength: 'weak', origin: 'user' });
  const ambiguousTerms = [...text.matchAll(/方法|函数|符号|\b(?:method|function|symbol)\b/giu)];
  for (const match of ambiguousTerms) clues.push({ kind: 'ambiguous-term', value: match[0], strength: 'weak', origin: 'user' });
  const domainConstraint = explicitDomain ? 'explicit' : 'none';
  if (!explicitDomain) {
    const codeTarget = codeSource || declarationTarget || Boolean(symbol) || paths.some(value => CODE_PATH_PATTERN.test(value)) ||
      typeof path === 'string' && CODE_PATH_PATTERN.test(path);
    const documentTarget = documentSource || paths.some(value => DOCUMENT_PATH_PATTERN.test(value)) ||
      typeof path === 'string' && DOCUMENT_PATH_PATTERN.test(path);
    domain = 'mixed';
    if (codeTarget !== documentTarget) preferredDomain ??= codeTarget ? 'code' : 'knowledge';
    const contextRelated = taskRelation.allowsInheritance || ANALYSIS_REQUEST_PATTERN.test(text) && TASK_DETAIL_PATTERN.test(text);
    if (domain === 'mixed' && !codeTarget && !documentTarget && contextRelated && !['topic-switch', 'correction'].includes(taskRelation.type) &&
        !EXTERNAL_CONTEXT_PATTERN.test(text)) {
      const contextDomain = taskContextDomain(taskContext);
      if (contextDomain !== 'mixed') {
        preferredDomain ??= contextDomain;
        clues.push({ kind: 'domain-preference', value: contextDomain, strength: 'weak', origin: 'task-context' });
      }
    }
  }
  const intent = validateRetrievalIntent({ domain, ...(symbol !== undefined ? { symbol } : {}),
    ...(path !== undefined ? { path } : {}), ...(preferredDomain !== undefined ? { preferredDomain } : {}) });
  return { intent, domainConstraint, clues, taskRelation, activeText: text,
    excludedClauses: clauses.excludedClauses, queryCorrections: clauses.provenance,
    ...(clauses.provenance.length ? { queryCorrection: clauses.provenance[0] } : {}) };
}

function requestedOperation(query) {
  if (/所有.{0,12}(?:引用|调用方)|穷举.{0,12}引用|\b(?:all\s+(?:references|callers)|enumerate\s+references)\b/iu.test(query)) return 'references';
  if (/(?:查找|找到|定位).{0,12}定义|\b(?:find|go\s+to|locate)\s+(?:the\s+)?definition\b/iu.test(query)) return 'definition';
  return 'search';
}

function isDirectExecutionTask(message) {
  // An explicit source question takes precedence over file/terminal verbs; execution keeps its tools.
  // 明确的资料问答优先于文件或终端动作；纯执行只省略自动检索，仍保留工具调用。
  const intent = message.replace(new RegExp(FILE_REFERENCE_PATTERN.source, 'giu'),
    reference => FILE_ACTION_PREFIX_PATTERN.exec(reference)?.[1] ?? ' ');
  return EXECUTION_ACTION_PATTERN.test(intent) && !KNOWLEDGE_DEPENDENCY_PATTERN.test(intent) &&
    !ANALYSIS_REQUEST_PATTERN.test(intent);
}

/** Route local evidence only when the request or its explicit continuation needs it.
 * 仅在请求或其明确续问需要本地资料时安排检索，指代补充不替换当前问题中的实体或额外调用模型。 */
export function retrievalPlan(message, { history = [], maximumTokens, taskContext } = {}) {
  const originalQuery = String(message ?? ''), currentText = originalQuery.trim();
  let taskRelation = classifyTaskRelation(originalQuery);
  const currentInterpretation = interpretRetrievalQuery(originalQuery);
  const planningText = currentInterpretation.activeText;
  const isGreeting = isSimpleGreeting(planningText);
  const isSmallTalk = SMALL_TALK_PATTERN.test(planningText.trim());
  const directExecution = isDirectExecutionTask(planningText);
  const explicitFile = queryPaths(planningText).length > 0;
  const instructionText = requestInstructionText(planningText);
  // Discussing a quoted filename's naming style does not request the file's contents.
  // 讨论引号内文件名的命名风格不表示请求文件内容，尤其不能让否定读取变成检索触发。
  const quotedFileNameDiscussion = /文件(?:名|名称)|\b(?:file\s*names?|filenames?)\b/iu.test(instructionText) &&
    /命名|名字|名称|风格|\b(?:naming|names?|style|convention)\b/iu.test(instructionText) &&
    !queryPaths(instructionText).length && !KNOWLEDGE_DEPENDENCY_PATTERN.test(instructionText) &&
    !CODE_SOURCE_PATTERN.test(instructionText) &&
    !/读取|查看|内容|实现|定义|源码|源文件|\b(?:read|contents?|implementation|definition|source)\b/iu.test(instructionText);
  const localRequest = !quotedFileNameDiscussion && (LOCAL_CONTEXT_PATTERN.test(planningText) || KNOWLEDGE_DEPENDENCY_PATTERN.test(planningText) ||
    CODE_SOURCE_PATTERN.test(planningText) || DOCUMENT_SOURCE_PATTERN.test(planningText) ||
    Boolean(currentInterpretation.intent.symbol) || explicitFile);
  const taskDomain = taskContextDomain(taskContext);
  const taskReference = !['topic-switch', 'correction'].includes(taskRelation.type) && taskDomain !== 'mixed' &&
    !EXTERNAL_CONTEXT_PATTERN.test(planningText) && ANALYSIS_REQUEST_PATTERN.test(planningText) &&
    (TASK_DETAIL_PATTERN.test(planningText) || FOLLOWUP_PATTERN.test(planningText));
  if (taskReference && !localRequest && taskRelation.type === 'new')
    taskRelation = { type: 'uncertain', allowsInheritance: false, reason: 'technical-detail-may-relate-to-current-task' };
  const followup = !isGreeting && !isSmallTalk && !directExecution && !quotedFileNameDiscussion && taskRelation.allowsInheritance;
  // Follow a chain of explicit continuations, stopping at the first unrelated turn or task boundary.
  // 沿明确续问链寻找来源锚点，遇到无关消息或任务边界即停止，不能越过换题复活旧来源。
  const completedUsers = followup ? history.filter(item => (item.Role ?? item.role) === 'user' &&
    (!(item.Status ?? item.status) || (item.Status ?? item.status) === 'completed') &&
    String(item.Content ?? item.content ?? '').trim() !== currentText) : [];
  let previous = null, priorText = '';
  for (let index = completedUsers.length - 1; index >= 0; index--) {
    const candidate = completedUsers[index];
    const candidateText = String(candidate.Content ?? candidate.content ?? '');
    const candidateProjection = analyzeRequestClauses(candidateText);
    const candidateQuery = candidateProjection.activeText;
    if (isDirectExecutionTask(candidateQuery)) break;
    if (LOCAL_CONTEXT_PATTERN.test(candidateQuery) || KNOWLEDGE_DEPENDENCY_PATTERN.test(candidateQuery) ||
        CODE_SOURCE_PATTERN.test(candidateQuery) || DOCUMENT_SOURCE_PATTERN.test(candidateQuery) || queryPaths(candidateQuery).length > 0) {
      previous = candidate; priorText = candidateQuery; break;
    }
    if (!classifyTaskRelation(candidateText).allowsInheritance) break;
  }
  if (previous) {
    const currentIntent = currentInterpretation.intent, priorIntent = buildRetrievalIntent(priorText);
    const sourceConflict = currentIntent.preferredDomain && priorIntent.preferredDomain &&
      currentIntent.preferredDomain !== priorIntent.preferredDomain;
    const targetConflict = currentIntent.path && priorIntent.path && currentIntent.path !== priorIntent.path ||
      currentIntent.symbol && priorIntent.symbol && currentIntent.symbol !== priorIntent.symbol;
    if (sourceConflict || targetConflict) {
      previous = null;
      taskRelation = { type: 'uncertain', allowsInheritance: false, reason: 'current-target-conflicts-with-prior-context' };
    }
  }
  const shouldRetrieve = !isGreeting && !isSmallTalk && !directExecution && (localRequest || Boolean(previous));
  let taskType = explicitFile ? 'file' : /记忆|回忆|历史|之前|上次|讨论|history|remember|recall|previous/iu.test(planningText) ? 'recall' : 'lookup';
  if (/比较|对比|权衡|架构|方案|全面|综述|调查|研究|compare|research|trade.?off/iu.test(planningText)) taskType = 'research';
  if (directExecution) taskType = 'execution';
  const defaultTokens = { recall: 4096, lookup: 8192, file: 12288, research: 16384, execution: 0 }[taskType];
  const evidenceTokens = maximumTokens === undefined ? defaultTokens : Math.max(0, Math.min(defaultTokens, Math.floor(maximumTokens)));
  const additionBudget = Math.max(0, Math.min(256, MAX_QUERY_CHARACTERS - planningText.length - 1));
  let priorQuery = previous ? priorText.trim().slice(0, additionBudget) : '';
  if (/[\uD800-\uDBFF]$/u.test(priorQuery)) priorQuery = priorQuery.slice(0, -1);
  const currentQuery = planningText || originalQuery;
  const query = shouldRetrieve && priorQuery ? `${currentQuery}\n${priorQuery}` : currentQuery;
  // A genuine continuation follows the latest admitted subject ahead of a stale task preference.
  // 确认的续问优先沿用最近接纳的主题，旧任务偏好不能压过后来明确的来源。
  const interpretation = interpretRetrievalQuery(originalQuery, { taskContext: previous ? priorText : taskContext, taskRelation });
  const { intent, domainConstraint, clues } = interpretation, { domain } = intent;
  const operation = requestedOperation(planningText);
  const additions = shouldRetrieve && priorQuery ? [{ text: priorQuery, origin: 'history', basis: 'explicit-continuation',
    ...(previous?.Id ?? previous?.id ? { historyId: previous.Id ?? previous.id } : {}) }] : [];
  const replacements = currentInterpretation.queryCorrections.map(correction => ({ ...correction, origin: 'user' }));
  // Domain and operation are separate: Top-K evidence cannot certify a complete reference enumeration.
  // 检索领域与操作分开标记：Top-K 证据不能证明引用已经穷举。
  return { shouldRetrieve, taskType, query, originalQuery, evidenceTokens, requiresSourceRead: taskType === 'research' || taskType === 'file',
    ...intent, retrievalIntent: intent, domainConstraint, clues, taskRelation,
    queryDerivation: { originalQuery, additions, replacements, preservedOriginal: true },
    operation, supportedOperations: ['search', 'read', 'indexed-definition'], operationSupported: operation !== 'references',
    evidenceRoles: domain === 'code' ? ['implementation', 'callers', 'configuration', 'tests'] : ['source', 'conditions'],
    suggestedEvidenceRoles: intent.preferredDomain === 'code' ? ['implementation', 'callers', 'configuration', 'tests'] : ['source', 'conditions'],
    reason: isGreeting ? 'greeting' : isSmallTalk ? 'small-talk' : directExecution ? 'direct-execution' : previous ? 'prior-local-request' : localRequest ? 'local-reference' : taskReference ? 'task-context-reference' : 'no-local-reference',
    rewritten: query !== originalQuery };
}

export function shouldRetrieve(message, options) { return retrievalPlan(message, options).shouldRetrieve; }

export function hasExactRetrievalTarget(item, intent) {
  if (!intent.path && !intent.symbol) return false;
  const itemPath = String(item.locator?.relativePath ?? item.title ?? '').replace(/\\/gu, '/').toLowerCase();
  const pathMatches = !intent.path || itemPath === intent.path.toLowerCase() || itemPath.endsWith(`/${intent.path.toLowerCase()}`);
  const symbolMatches = !intent.symbol || [item.structure?.symbolName, item.structure?.qualifiedName]
    .some(symbol => typeof symbol === 'string' && (symbol === intent.symbol || symbol.endsWith(`.${intent.symbol}`)));
  return item.exactTargetMatch === true || pathMatches && symbolMatches;
}

/** Plan the next evidence operation from observable gaps, not an inferred answer probability.
 * 根据可观察的证据缺口规划下一步，不把排序分数或关键词覆盖当作答案正确概率。 */
export function planEvidenceAcquisition({ query = '', gap, intent = buildRetrievalIntent(query), items = [],
  assessment, taskType = 'lookup', embeddingStatus, rerankerStatus, semanticEnabled = true,
  indexingPending = false, remainingSearches = 1, newEvidenceCount, reused = false } = {}) {
  // Admitted declaration metadata can corroborate a named candidate after search, never before it.
  // 检索后的已接纳声明元信息可以验证命名候选，检索前不会凭词形把候选升级为确定目标。
  const declarationNavigation = /定义|\b(?:definition|declaration)\b/iu.test(query);
  const namedCandidates = declarationNavigation ? interpretRetrievalQuery(query).clues.filter(clue =>
    clue.kind === 'symbol-candidate' && query.includes(`\`${clue.value}\``)) : [];
  const corroboratedSymbol = !intent.symbol ? namedCandidates.find(clue => items.some(item =>
    hasExactRetrievalTarget({ ...item, exactTargetMatch: false }, { ...intent, symbol: clue.value })))?.value : undefined;
  const observedIntent = corroboratedSymbol ? { ...intent, symbol: corroboratedSymbol } : intent;
  const exactTarget = items.some(item => item.exactTargetMatch === true || hasExactRetrievalTarget(item, observedIntent));
  const targeted = Boolean(observedIntent.path || observedIntent.symbol || exactTarget);
  const complex = ['complex', 'research'].includes(taskType);
  const embeddingReady = embeddingStatus?.state === 'ready' && embeddingStatus.loaded !== false;
  const rerankerReady = rerankerStatus?.state === 'ready';
  const missingEvidence = assessment?.missingEvidence ?? [];
  const channels = targeted ? ['lexical', 'structure'] : ['lexical'];
  if (semanticEnabled && embeddingReady && !exactTarget) channels.push('vector');
  const shouldRerank = complex && items.length > 1 && rerankerReady &&
    !(exactTarget && !missingEvidence.length) &&
    (assessment?.state !== 'usable' || assessment?.requiresSourceRead || Boolean(gap) || items.length > 6 || missingEvidence.length > 0);
  const needsRead = items.length > 0 && (assessment?.requiresSourceRead || missingEvidence.length > 0);
  const noProgress = reused || newEvidenceCount === 0;
  const budgetExhausted = remainingSearches <= 0;
  const next = needsRead ? 'read-source' : items.length ? 'evaluate-support-and-answer' :
    budgetExhausted ? 'state-unresolved-gap' : indexingPending ? 'direct-search-or-read' : 'search-specific-gap';
  const requiresVerification = /修改|修复|重构|优化|实现|生成|\b(?:fix|modify|refactor|implement|optimi[sz]e|generate)\b/iu.test(query);
  return { channels, shouldEmbed: semanticEnabled && embeddingReady && !exactTarget,
    shouldRerank, rerankReason: shouldRerank ? 'ambiguous-or-incomplete-evidence' :
      !complex ? 'simple-task' : !rerankerReady ? 'optional-model-not-ready' : 'targeted-or-small-evidence-set',
    missingInformation: gap ?? null, missingEvidence, next,
    shouldContinueSearch: !budgetExhausted && !noProgress && !needsRead && !items.length,
    requiresVerification, requiredChecks: requiresVerification ? ['re-read-current-source',
      intent.domain === 'code' ? 'run-relevant-validation' : 'check-deliverable-against-requirements'] : [],
    coverage: indexingPending ? 'partial-index' : 'bounded-candidates',
    sufficiency: 'not-evaluated' };
}

/** Reorder only admitted source metadata; priority cannot add files or change authorization.
 * 只调整已接纳来源元信息的顺序，任务优先级不能新增文件或改变授权范围。 */
export function prioritizeEvidenceSources(sources, priorityPaths = new Set()) {
  const rank = source => {
    const path = String(source.locator?.relativePath ?? source.title ?? '').replace(/\\/gu, '/').toLowerCase();
    const targeted = [...priorityPaths].some(target => path === target.toLowerCase() || path.startsWith(`${target.toLowerCase()}/`) ||
      path.endsWith(`/${target.toLowerCase()}`));
    if (targeted) return 0;
    if (/(?:^|\/)(?:readme|agents)(?:\.|$)|(?:^|\/)(?:tests?|src)(?:\/|$)|\.(?:cs|[cm]?js|[cm]?ts|py|rs|go|json|ya?ml|toml)$/iu.test(path)) return 1;
    return 2;
  };
  return sources.map((source, index) => ({ source, index, rank: rank(source) }))
    .sort((left, right) => left.rank - right.rank || left.index - right.index).map(item => item.source);
}

/** Exact identifiers and paths supplement semantic lookup; neither can expand authorized scopes.
 * 精确标识符与路径补充语义检索，两者均不能扩大授权范围。 */
export function buildRetrievalIntent(query, options = {}) {
  return interpretRetrievalQuery(query, options).intent;
}

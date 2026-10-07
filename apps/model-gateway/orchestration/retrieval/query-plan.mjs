import { MAX_QUERY_CHARACTERS, validateRetrievalIntent } from '../../data/retrieval/retrieval-contracts.mjs';

export function isSimpleGreeting(message) {
  return /^(?:你好|您好|嗨|哈喽|早上好|晚上好|早安|晚安|hello|hi|hey|good morning|good evening)[\s!！。.~～]*$/iu.test(message.trim());
}

const LOCAL_CONTEXT_PATTERN = /之前|记得|记忆|资料|文档|知识|项目|工作|代码|文件|我们讨论|上次|history|remember|document|knowledge|project|code|file|previous/iu;
const FILE_REFERENCE_PATTERN = /(?:[\w\p{Script=Han}./\\-]+\.(?:md|txt|pdf|docx|cs|mjs|js|jsx|ts|tsx|json|yaml|yml|py|rs|go|sql|xaml))\b/iu;
const FOLLOWUP_PATTERN = /它|这个|那个|这份|那份|继续|刚才|然后|还有|为什么|上面|\b(?:it|this|that|continue|what about|why)\b/iu;
const SMALL_TALK_PATTERN = /^(?:谢谢(?:你)?|多谢|好的|好吧|没事|嗯|聊聊天|陪我聊聊|讲个笑话|最近怎么样|今天心情怎么样|thanks?(?: you)?|okay|ok|how are you|tell me a joke)[\s!！?？。.~～]*$/iu;
const KNOWLEDGE_DEPENDENCY_PATTERN = /资料|文档|知识|记忆|记得|回忆|历史|我们讨论|上次|(?:之前|以前).{0,16}(?:说|讲|讨论|决定|确认|记)|\b(?:history|remember|recall|knowledge|documentation|docs?|previous\s+(?:discussion|decision|conversation)|saved\s+(?:memory|notes?))\b/iu;
const ANALYSIS_REQUEST_PATTERN = /分析|解释|讲解|总结|概括|归纳|比较|对比|权衡|架构|方案|研究|调查|综述|审查|评审|诊断|为何|为什么|如何|怎么|怎样|哪里|在哪|何处|何时|什么时候|是否|是什么|有哪些|逻辑|原理|机制|报错原因|缺陷|漏洞|\b(?:explain|analy[sz]e|summari[sz]e|compare|review|inspect|diagnose|investigate|research|architecture|logic|implementation|semantics|why|where|when|which|what\s+(?:is|are|does)|how\s+(?:to|is|are|does|do|can|should)|find\s+(?:bugs?|errors?))\b/iu;
const EXECUTION_ACTION_PATTERN = /读取|读回|回读|读出|打开|查看|列出|写入|写到|写出|保存|创建|新建|追加|复制|移动|重命名|删除|执行|运行|启动|安装|构建|编译|终端|命令行|\b(?:read|open|list|write|save|create|append|copy|move|rename|delete|execute|run|launch|install|build|compile|terminal|shell)\b/iu;
const FILE_ACTION_PREFIX_PATTERN = /^(?:(?:请|你|先|然后|再|依次|直接|帮我)*)(读取|读回|回读|读出|打开|查看|列出|写入|写到|写出|保存|创建|新建|追加|复制|移动|重命名|删除)/u;
const CODE_CONTEXT_PATTERN = /代码|源码|仓库|函数|符号|调用方|调用链|类型定义|源文件|单元测试|\b(?:code|repository|repo|function|symbol|caller|source\s+code|unit\s+tests?)\b|\.(?:cs|[cm]?js|jsx|[cm]?ts|tsx|py|rs|go|xaml)\b/iu;
const DOCUMENT_CONTEXT_PATTERN = /论文|资料|文档|知识库|章节|笔记|\b(?:papers?|documents?|documentation|docs?|manuals?|knowledge|chapters?|notes?)\b|\.(?:md|txt|pdf|docx)\b/iu;
const EXTERNAL_CONTEXT_PATTERN = /https?:\/\/|网页|网站|网址|浏览器|联网|上网|天气|新闻|\b(?:web|websites?|browser|online|internet|weather|news)\b/iu;
const TASK_DETAIL_PATTERN = /取消|中断|恢复|重试|超时|队列|并发|绑定|缓存|索引|权限|审批|持久化|保存|存储|写入|序列化|分块|分词|嵌入|重排|上下文|引用|来源|配置|设置|生命周期|\b(?:cancel\w*|abort\w*|retry|retries|timeout|queue\w*|concurren\w*|binding|cache\w*|index\w*|permission\w*|approval|persist\w*|stor\w*|serializ\w*|chunk\w*|tokeniz\w*|embedding\w*|rerank\w*|context|reference\w*|configuration|settings|lifecycle)\b/iu;

function retrievalDomain(query) {
  const code = CODE_CONTEXT_PATTERN.test(query), document = DOCUMENT_CONTEXT_PATTERN.test(query);
  return code && document ? 'mixed' : code ? 'code' : document ? 'knowledge' : 'mixed';
}

function taskContextDomain(context) {
  if (context && typeof context === 'object' && ['code', 'knowledge', 'mixed'].includes(context.domain)) return context.domain;
  const text = typeof context === 'string' ? context : context?.message;
  return retrievalDomain(typeof text === 'string' ? text.slice(0, MAX_QUERY_CHARACTERS) : '');
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
  const originalQuery = String(message ?? '').trim();
  const isGreeting = isSimpleGreeting(originalQuery);
  const isSmallTalk = SMALL_TALK_PATTERN.test(originalQuery);
  const directExecution = isDirectExecutionTask(originalQuery);
  const explicitFile = FILE_REFERENCE_PATTERN.test(originalQuery);
  const explicitSymbol = /`[A-Za-z_$][\w$]*(?:\.[A-Za-z_$][\w$]*)*(?:\(\))?`/u.test(originalQuery);
  const localRequest = LOCAL_CONTEXT_PATTERN.test(originalQuery) || KNOWLEDGE_DEPENDENCY_PATTERN.test(originalQuery) ||
    CODE_CONTEXT_PATTERN.test(originalQuery) || DOCUMENT_CONTEXT_PATTERN.test(originalQuery) || explicitSymbol || explicitFile;
  const taskDomain = taskContextDomain(taskContext);
  const taskReference = taskDomain !== 'mixed' && !EXTERNAL_CONTEXT_PATTERN.test(originalQuery) && ANALYSIS_REQUEST_PATTERN.test(originalQuery) &&
    (TASK_DETAIL_PATTERN.test(originalQuery) || FOLLOWUP_PATTERN.test(originalQuery));
  const followup = !isGreeting && !isSmallTalk && !directExecution && FOLLOWUP_PATTERN.test(originalQuery);
  const previous = followup ? history.filter(item => (item.Role ?? item.role) === 'user' &&
    (!item.Status || item.Status === 'completed') && String(item.Content ?? item.content ?? '').trim() !== originalQuery)
    .slice(-3).reverse().find(item => {
      const priorMessage = item.Content ?? item.content ?? '';
      return !isDirectExecutionTask(priorMessage) && (LOCAL_CONTEXT_PATTERN.test(priorMessage) ||
        KNOWLEDGE_DEPENDENCY_PATTERN.test(priorMessage) || CODE_CONTEXT_PATTERN.test(priorMessage) || FILE_REFERENCE_PATTERN.test(priorMessage));
    }) : null;
  const shouldRetrieve = !isGreeting && !isSmallTalk && !directExecution && (localRequest || Boolean(previous) || taskReference);
  let taskType = explicitFile ? 'file' : /记忆|回忆|历史|之前|上次|讨论|history|remember|recall|previous/iu.test(originalQuery) ? 'recall' : 'lookup';
  if (/比较|对比|权衡|架构|方案|全面|综述|调查|研究|compare|research|trade.?off/iu.test(originalQuery)) taskType = 'research';
  if (directExecution) taskType = 'execution';
  const defaultTokens = { recall: 2048, lookup: 4096, file: 6144, research: 8192, execution: 0 }[taskType];
  const evidenceTokens = maximumTokens === undefined ? defaultTokens : Math.max(0, Math.min(defaultTokens, Math.floor(maximumTokens)));
  const priorCharacters = Array.from(String(previous?.Content ?? previous?.content ?? '').trim());
  const priorQuery = priorCharacters.slice(0, Math.max(0, Math.min(256, MAX_QUERY_CHARACTERS - originalQuery.length - 1))).join('');
  const query = shouldRetrieve && priorQuery ? `${originalQuery}\n${priorQuery}` : originalQuery;
  const domain = buildRetrievalIntent(query, { taskContext }).domain, operation = requestedOperation(originalQuery);
  // Domain and operation are separate: Top-K evidence cannot certify a complete reference enumeration.
  // 检索领域与操作分开标记：Top-K 证据不能证明引用已经穷举。
  return { shouldRetrieve, taskType, query, originalQuery, evidenceTokens, requiresSourceRead: taskType === 'research' || taskType === 'file',
    domain, operation, supportedOperations: ['search', 'read', 'indexed-definition'], operationSupported: operation !== 'references',
    evidenceRoles: domain === 'knowledge' ? ['source', 'conditions'] : ['implementation', 'callers', 'configuration', 'tests'],
    reason: isGreeting ? 'greeting' : isSmallTalk ? 'small-talk' : directExecution ? 'direct-execution' : previous ? 'prior-local-request' : localRequest ? 'local-reference' : taskReference ? 'task-context-reference' : 'no-local-reference',
    rewritten: query !== originalQuery };
}

export function shouldRetrieve(message, options) { return retrievalPlan(message, options).shouldRetrieve; }

/** Exact identifiers and paths supplement semantic lookup; neither can expand authorized scopes.
 * 精确标识符与路径补充语义检索，两者均不能扩大授权范围。 */
export function buildRetrievalIntent(query, { domain, symbol, path, taskContext } = {}) {
  const text = String(query ?? '');
  if (path === undefined) {
    const match = /(?:[\w\p{Script=Han}.-]+[\\/])*[\w\p{Script=Han}.-]+\.(?:cs|[cm]?js|jsx|[cm]?ts|tsx|py|rs|go|java|cpp|c|h|ps1|sql|json|yaml|yml|xaml|xml|toml|html|css|md|markdown|txt)\b/iu.exec(text);
    path = match?.[0];
  }
  if (symbol === undefined) {
    const quoted = /[`"'“]([A-Za-z_$][\w$]*(?:\.[A-Za-z_$][\w$]*)*)(?:\(\))?[`"'”]/u.exec(text);
    const identifiers = text.match(/[A-Za-z_$][\w$]*(?:\.[A-Za-z_$][\w$]*)*/gu) ?? [];
    const hasCodeContext = CODE_CONTEXT_PATTERN.test(text), hasDocumentContext = DOCUMENT_CONTEXT_PATTERN.test(text);
    const externalOnly = EXTERNAL_CONTEXT_PATTERN.test(text) && !hasCodeContext;
    const symbolShape = value => /[a-z\d][A-Z]|[A-Z][a-z]+[A-Z]|\w_\w|\./u.test(value);
    const quotedSymbol = !externalOnly && quoted && !path?.toLowerCase().includes(quoted[1].toLowerCase()) &&
      (hasCodeContext || !hasDocumentContext && (symbolShape(quoted[1]) || /\(\)/u.test(quoted[0]))) ? quoted[1] : undefined;
    symbol = quotedSymbol ?? (externalOnly ? undefined : identifiers.find(value => !path?.toLowerCase().includes(value.toLowerCase()) &&
      symbolShape(value) && (hasCodeContext || !hasDocumentContext)));
  }
  if (domain === undefined) {
    const inferred = retrievalDomain(text);
    if (CODE_CONTEXT_PATTERN.test(text) && DOCUMENT_CONTEXT_PATTERN.test(text) || symbol && DOCUMENT_CONTEXT_PATTERN.test(text)) domain = 'mixed';
    else if (symbol || inferred === 'code' || path && /\.(?:cs|[cm]?js|jsx|[cm]?ts|tsx|py|rs|go)$/iu.test(path)) domain = 'code';
    else domain = DOCUMENT_CONTEXT_PATTERN.test(text) ? 'knowledge' : EXTERNAL_CONTEXT_PATTERN.test(text) ? 'mixed' : taskContextDomain(taskContext);
  }
  return validateRetrievalIntent({ domain, ...(symbol !== undefined ? { symbol } : {}), ...(path !== undefined ? { path } : {}) });
}

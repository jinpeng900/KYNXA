const CLAUSE_SEPARATOR = /[，。；：,;!?！？\n]+|\.(?=\s|$)|:(?=\s)/gu;
const CONTRAST_BOUNDARY = /(?=不是|并非|而是|改(?:为|成)|\bbut\b(?=\s+\S)|\binstead\b(?!\s+of\b)(?=\s+\S))/giu;
const REQUEST_LEAD = /^(?:(?:请|先|暂时|暂且|现在|目前|我们|我|那|就|并|也|好(?:的)?|这次)\s*|(?:please|for now|now|currently|we|let['’]s)\s+)*/iu;
const TASK_EXIT = /^(?:不(?:再)?|别(?:再)?)(?:聊|谈|说|讨论)|^(?:停止(?:讨论|谈论)|搁置|跳过|放下|算了)|^(?:stop|quit)\s+(?:talking|discussing)|^(?:leave|put|set)\s+.+\s+aside\b|^drop\s+(?:this|that|the)\s+(?:topic|subject)\b|^never\s+mind\b/iu;
const TOPIC_SWITCH = /^(?:新任务|新话题|换(?:个|一个)?(?:话题|问题)|(?:聊|谈|说|讨论)(?:另(?:外)?一件事|其他|点别的)|另外一件|另一个问题)|^(?:new\s+(?:task|topic)|different\s+topic|(?:switch|change)\s+(?:the\s+)?(?:topic|subject)|unrelated\s+question)\b/iu;
const CORRECTION = /^(?:不是|并非|更正|纠正|我说的是|而是|改(?:为|成))|^(?:rather\s+than\b|i\s+mean(?:t)?\b|correction\b|instead\b)/iu;
const NEGATED_CLAUSE = /^(?:不是|并非|不要|不必|不用|不使用|无需|不能|不许|别|勿|禁止|避免)|^(?:do\s+not|don['’]t|never|not|no\s+need\s+to|without|rather\s+than)\b/iu;
const NEGATED_ACTION = /^(?:不是|并非|不要|不必|不用|无需|不能|不许|别|勿|禁止|避免)\s*(?:(?:让|要|叫|请)(?:你)?\s*)?(?:再\s*)?(?:查|检查|读|看|检索|搜索|分析|讨论|解释|打开|启动|在|用|使用|通过|操作|关闭|刷新|删除|执行|运行)|^不使用|^(?:do\s+not|don['’]t|never|not|no\s+need\s+to|without|rather\s+than)\s+(?:open(?:ing)?|launch(?:ing)?|start(?:ing)?|use|using|read(?:ing)?|search(?:ing)?|analy[sz](?:e|ing)|discuss(?:ing)?|operate|close|refresh|delete|execute|run)\b/iu;
const NEGATED_SOURCE = /^(?:不是|并非|not|rather\s+than)\s*(?:(?:a|the|this|that)\s+)?(?:代码|文档|文件|源码|仓库|知识库|code(?:\s+repository)?|documents?|docs?|files?|repository|[\w./\\-]+\.[A-Za-z]{1,8})(?:\s|$)/iu;
const RETAINED_NEGATION = /^(?:不是|并非)\s*(?:不|所有|全部|每|任何|一定|总是)|^(?:不要|不能|不许|别|勿|禁止|避免)\s*(?:忽略|遗漏|省略|丢弃|改动|修改|改变|更改)|^(?:do\s+not|don['’]t|never|not)\s+(?:avoid|ignore|overlook|omit|drop|modify|change|all|every|always|necessarily|before|after)\b/iu;
const CANCELLED_TASK = /^(?:取消|停止|结束)(?:全部|所有|当前|之前)?(?:的)?(?:任务|操作|浏览器操作)(?:了)?$|^(?:(?:浏览器|当前|之前|本次|全部|所有)\s*)*(?:任务|操作)(?:全部|都)?(?:已)?(?:取消|停止|结束)(?:了)?$|^(?:cancel|stop|end)\s+(?:(?:all|the|current|previous)\s+)*(?:browser\s+)?(?:tasks?|operations?)$/iu;
const CODE_FILE_EXTENSIONS = new Set(['cs', 'js', 'mjs', 'cjs', 'jsx', 'ts', 'mts', 'cts', 'tsx', 'py', 'pyi', 'rs', 'go',
  'java', 'c', 'cpp', 'cxx', 'cc', 'h', 'hpp', 'hxx', 'ps1', 'psm1', 'sh', 'bat', 'cmd', 'sql', 'xaml', 'html', 'css']);
const DOCUMENT_FILE_EXTENSIONS = new Set(['md', 'markdown', 'txt', 'pdf', 'docx']);
const REQUEST_FILE_EXTENSIONS = [...CODE_FILE_EXTENSIONS, ...DOCUMENT_FILE_EXTENSIONS, 'json', 'yaml', 'yml', 'xml', 'toml', 'ini', 'csv'];
const FILE_CLUE_PATTERN = new RegExp(`(?<![\\w\\p{Script=Han}./\\\\-])(?:[A-Za-z]:[\\\\/])?(?:[\\w\\p{Script=Han}.-]+[\\\\/])*[\\w\\p{Script=Han}.-]+\\.(?:${REQUEST_FILE_EXTENSIONS.join('|')})\\b`, 'giu');
const FILE_ACTION_LEAD = /^(?:(?:请|你|先|然后|再|依次|直接|帮我)*)(?:读取|读回|回读|读出|打开|查看|列出|写入|写到|写出|保存|创建|新建|追加|复制|移动|重命名|删除)/u;
const CONTINUATION_REFERENCE = /(?:它|他们|她们|它们)|(?:这个|那个)(?:方法|函数|问题|文件|模块|模型|方案|结果|条件|符号)|这份|那份|这篇|那篇|继续|刚才|上面|\b(?:it|its|continue|what about|(?:this|that)\s+(?:method|function|question|file|module|model|result|constraint))\b/iu;
const EXPLICIT_RETRY = /^(?:(?:请|帮我)\s*|please\s+)?(?:再来|重试|再次|再试|尝试|打不开|还是|再(?:查|看)|刷新|现在呢|again\b|retry\b|try again\b|refresh\b)|^what about now[?.!\s]*$/iu;
const SUPPLEMENT = /^(?:补充|还有|另外[，,]|此外|\b(?:also|additionally|one\s+more\s+thing)\b)/iu;

/** A filename is a source clue, not proof that a file exists or can be accessed.
 * 文件名只是来源线索，不证明文件存在或可读取；URL 中的路径不冒充本机文件。
 */
export function requestFileReferences(message) {
  const text = String(message ?? ''), urls = [...text.matchAll(/https?:\/\/[^\s`"'<>]+/giu)];
  return [...text.matchAll(FILE_CLUE_PATTERN)].filter(match => !urls.some(url =>
    match.index >= url.index && match.index < url.index + url[0].length)).map(match => {
    const value = match[0].replace(FILE_ACTION_LEAD, '');
    const startOffset = match.index + match[0].length - value.length;
    return { value, startOffset, endOffset: startOffset + value.length, domain: requestPathDomain(value) };
  });
}

/** File extensions describe candidate source kinds; only explicit caller filters constrain a domain.
 * 扩展名说明候选来源类型，只有调用方明确过滤才约束领域。
 */
export function requestPathDomain(path) {
  const extension = /\.([a-z0-9]+)$/iu.exec(String(path ?? ''))?.[1].toLowerCase();
  return CODE_FILE_EXTENSIONS.has(extension) ? 'code' : DOCUMENT_FILE_EXTENSIONS.has(extension) ? 'knowledge' : 'mixed';
}

/** Shared relation hints govern context eligibility, never referent identity or inherited permission.
 * 共享任务关系只决定上下文是否可参考，不确认指代对象，也不继承权限。
 */
export function classifyRequestTaskRelation(message) {
  const clauses = analyzeRequestClauses(message), text = clauses.activeText.trim();
  if (clauses.boundary !== 'none') return { type: clauses.boundary, allowsInheritance: false,
    reason: clauses.boundaryReason ?? 'explicit-task-boundary' };
  if (clauses.excludedClauses.length) return { type: 'uncertain', allowsInheritance: false, reason: 'explicit-clause-exclusion' };
  if (SUPPLEMENT.test(text)) return { type: 'supplement', allowsInheritance: true, reason: 'explicit-supplement' };
  if (CONTINUATION_REFERENCE.test(text) || text.length <= 240 && EXPLICIT_RETRY.test(text))
    return { type: 'continue', allowsInheritance: true, reason: 'continuation-reference' };
  return { type: 'new', allowsInheritance: false, reason: 'no-continuation-reference' };
}

function quotedSpans(text) {
  const pairs = new Map([['“', '”'], ['‘', '’'], ['「', '」'], ['『', '』'], ['"', '"'], ["'", "'"]]);
  const spans = [];
  for (let index = 0; index < text.length; index++) {
    const opener = text[index];
    if (opener !== '`' && !pairs.has(opener)) continue;
    if (opener === "'" && /[\p{L}\p{N}]/u.test(text[index - 1] ?? '') && /[\p{L}\p{N}]/u.test(text[index + 1] ?? '')) continue;
    const delimiter = opener === '`' ? (text.slice(index).match(/^`+/u)?.[0] ?? '`') : opener;
    const closer = pairs.get(opener) ?? delimiter;
    let end = index + delimiter.length;
    while (end < text.length) {
      if (text[end] === '\\') { end += 2; continue; }
      if (text.startsWith(closer, end) && !(closer === "'" && /[\p{L}\p{N}]/u.test(text[end - 1] ?? '') && /[\p{L}\p{N}]/u.test(text[end + 1] ?? ''))) break;
      end++;
    }
    const closed = end < text.length;
    spans.push({ start: index, end: closed ? end + closer.length : text.length,
      content: text.slice(index + delimiter.length, end), closed });
    index = spans.at(-1).end - 1;
  }
  return spans;
}

/** Quoted source text can supply evidence, but its command words do not become an instruction.
 * 引文可提供资料线索，但其中的命令词不自动变成用户操作；掩码保留位置与原文长度。
 */
export function requestInstructionText(message, { preserveQuotedLiteral } = {}) {
  const text = String(message ?? ''), spans = quotedSpans(text);
  let projected = '', cursor = 0;
  for (const span of spans) {
    projected += text.slice(cursor, span.start);
    projected += span.closed && preserveQuotedLiteral?.(span.content)
      ? text.slice(span.start, span.end) : text.slice(span.start, span.end).replace(/[^\r\n]/gu, ' ');
    cursor = span.end;
  }
  return projected + text.slice(cursor);
}

function requestClauses(text) {
  const clauses = [];
  const quoted = quotedSpans(text);
  const insideQuote = index => quoted.some(span => index >= span.start && index < span.end);
  let cursor = 0;
  const append = (startOffset, endOffset) => {
    const segment = text.slice(startOffset, endOffset);
    const boundaries = [...segment.matchAll(CONTRAST_BOUNDARY)].map(match => match.index)
      .filter(index => index > 0 && !insideQuote(startOffset + index));
    let previous = 0;
    for (const end of [...boundaries, segment.length]) {
      const raw = segment.slice(previous, end), value = raw.trim();
      if (value) {
        const start = startOffset + previous + raw.length - raw.trimStart().length;
        clauses.push({ text: value, startOffset: start, endOffset: start + value.length });
      }
      previous = end;
    }
  };
  for (const separator of text.matchAll(CLAUSE_SEPARATOR)) {
    if (insideQuote(separator.index)) continue;
    append(cursor, separator.index);
    cursor = separator.index + separator[0].length;
  }
  append(cursor, text.length);
  return clauses;
}

/**
 * Project explicit clause exclusions for consumers to interpret; this is neither intent proof nor permission.
 * 投影明确的子句排除供调用方解释；它不证明真实意图，也不授予权限，原始消息保持不变。
 */
export function analyzeRequestClauses(message) {
  const text = String(message ?? ''), active = [], excludedClauses = [];
  const protectedNegativeClauses = new Set();
  let replacementContext = false;
  let boundary = 'none', boundaryReason;
  const supersedeActive = () => {
    excludedClauses.push(...active.map(clause => ({ ...clause, basis: 'superseded-by-task-boundary' })));
    active.length = 0;
    protectedNegativeClauses.clear();
  };
  for (const clause of requestClauses(text)) {
    const instruction = requestInstructionText(clause.text).trim().replace(REQUEST_LEAD, '');
    const exitsTask = TASK_EXIT.test(instruction) || CANCELLED_TASK.test(instruction);
    // Only an identifiable denied action or an explicit replacement can exclude source text.
    // 只有明确被否定的动作或明确替换才排除来源文本；负事实、量词与输出约束保留。
    const negative = NEGATED_CLAUSE.test(instruction), retained = RETAINED_NEGATION.test(instruction);
    const excluded = exitsTask || !retained && (NEGATED_ACTION.test(instruction) || NEGATED_SOURCE.test(instruction) || negative && replacementContext);
    const protectedNegative = negative && !excluded;
    const correction = !protectedNegative && CORRECTION.test(instruction);
    if (exitsTask || TOPIC_SWITCH.test(instruction)) {
      boundary = 'topic-switch'; boundaryReason = exitsTask ? 'explicit-task-exit' : 'explicit-topic-switch';
      supersedeActive();
    } else if ((excluded || correction) && boundary !== 'topic-switch') {
      boundary = 'correction'; boundaryReason = 'explicit-user-replacement';
      if (!excluded && correction && !protectedNegativeClauses.size) supersedeActive();
    }
    replacementContext ||= excluded || correction;
    if (excluded) excludedClauses.push({ ...clause,
      basis: exitsTask ? 'explicit-task-exit' : CORRECTION.test(instruction) ? 'explicit-user-replacement' : 'explicit-negation' });
    else {
      active.push(clause);
      if (protectedNegative) protectedNegativeClauses.add(clause);
    }
  }
  const activeText = excludedClauses.length || boundary !== 'none' ? active.map(clause => clause.text).join('，') : text;
  return { activeText, excludedClauses, boundary, ...(boundaryReason ? { boundaryReason } : {}),
    provenance: excludedClauses.map(clause => ({ rejected: clause.text, accepted: activeText, basis: clause.basis })),
    originalPreserved: true, semanticVerified: false };
}

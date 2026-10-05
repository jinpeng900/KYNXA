import { basename } from 'node:path';

const integer = value => Number.isSafeInteger(value) && value >= 0 ? value : null;
const failure = code => Object.assign(new Error(code), { code });

export const FIXTURE_VERSION = 'agent-synthetic-v1';
export const VERIFIER_VERSION = 'agent-verifier-reviewer-relations-v3';
export const RUN_LIMITS = Object.freeze({ maxRounds: 12, maxToolCalls: 24,
  maxGeneratedTokens: 98_304, maxDurationMs: 120_000 });
export const SOURCE_TITLE = 'fixture-notes.md';
export const SOURCE_TEXT = '# Synthetic mission records\n\n'
  + 'ORION has an experimental launch window at 17:40 UTC on 14 November 2031. '
  + 'Its safety reviewer is Mara Chen. This is an experimental window, not a confirmed launch.\n\n'
  + 'VEGA has an experimental launch window at 09:15 UTC on 3 December 2032. '
  + 'Its safety reviewer is Beatrice Hall.\n\n'
  + 'No schedule or approval is recorded for NOVA.\n';
export const RECEIPT_TEXT = 'ticket=KYNXA-731926\n';
export const TOOL_NAMES = new Set(['filesystem.read', 'filesystem.write', 'filesystem.list', 'filesystem.search', 'filesystem.stat',
  'knowledge.search', 'knowledge.read', 'conversation.history.search', 'conversation.history.read',
  'tool.search', 'tool.load', 'tool.result.read']);
export const TASKS = Object.freeze([
  { id: 'greeting', messages: ['你好！'] },
  { id: 'english-source-chinese-answer', messages: [
    '根据已导入的英文资料 fixture-notes.md，用中文说明 ORION 的实验发射窗口日期、UTC 时间和安全审查人。保留英文人名，明确窗口不是已确认发射，并引用资料名；必要时可读取同名工作文件。'] },
  { id: 'followup-entity', messages: [
    '根据 fixture-notes.md，用中文比较 ORION 和 VEGA 的实验发射窗口时间，注明 UTC 并引用资料名；必要时可读取同名工作文件。',
    '继续，ORION 的安全审查人是谁？请保留英文人名并引用资料名。'] },
  { id: 'dependent-file-cycle', messages: [
    '依次读取工作文件 seed.txt 中 ticket 的值，新建 receipt.txt，内容恰好为 ticket=<实际值> 加一个换行；新建文件 expectedHash 应为 null。等待写入结果后，再读取 receipt.txt 核实内容，最后报告核实的 ticket 值。每步依赖上一步真实工具结果，不要省略读回。'] },
  { id: 'insufficient-evidence', messages: [
    '根据已导入资料 fixture-notes.md，NOVA 的正式发射日期和时间是什么？必要时可读取同名工作文件。资料没有确认时只说明无法确定，不列任何具体日期、时刻或其他项目的安排，不能猜测。'] }
]);
export function benchmarkPlan() {
  return { fixtureVersion: FIXTURE_VERSION, measurementKind: 'plan-only', tasks: TASKS.map(task => ({ id: task.id, turns: task.messages.length })),
    configurations: ['rag-off', 'rag-on'], limits: RUN_LIMITS, maxOutputTokens: 8192,
    contextWindowTokens: 32768, semantic: 'auto', rerank: null, realCalls: 0,
    instruction: 'Provide --connection-file PATH, --provider ID and --model ID to run. No implicit mock or model call.' };
}

/**
 * Cache-hit tokens are part of OpenAI/DeepSeek prompt tokens, but separate in Anthropic usage.
 * OpenAI/DeepSeek 的缓存命中属于 prompt tokens；Anthropic 缓存读取和创建需计入总输入，不重复相加。
 */
export function readUsage(protocol, payload) {
  const usage = payload?.usage ?? {};
  let inputTokens = integer(protocol === 'openai-completions' ? usage.prompt_tokens : usage.input_tokens);
  const outputTokens = integer(protocol === 'openai-completions' ? usage.completion_tokens : usage.output_tokens);
  const cacheReadTokens = integer(usage.prompt_cache_hit_tokens ?? usage.prompt_tokens_details?.cached_tokens
    ?? usage.input_tokens_details?.cached_tokens ?? usage.cache_read_input_tokens);
  const cacheWriteTokens = integer(usage.cache_creation_input_tokens);
  if (protocol === 'anthropic-messages' && inputTokens !== null)
    inputTokens += (cacheReadTokens ?? 0) + (cacheWriteTokens ?? 0);
  return { status: inputTokens !== null && outputTokens !== null ? 'reported' : 'unknown', inputTokens, outputTokens,
    totalTokens: inputTokens !== null && outputTokens !== null ? inputTokens + outputTokens : null,
    providerTotalTokens: integer(usage.total_tokens), cacheReadTokens, cacheWriteTokens,
    cacheMissTokens: integer(usage.prompt_cache_miss_tokens) };
}

function parseToolResult(activity) {
  try { return JSON.parse(activity.result); } catch { return {}; }
}

function hasObservedCitation(assistant, sourceId, activities, priorAssistants = []) {
  const cited = String(assistant.Content ?? '').toLowerCase().includes(SOURCE_TITLE);
  // The fixed source is unchanged and this small history stays in the input; duplicate evidence need not be fetched twice.
  // 固定资料未变且本组短历史完整保留在输入中，不要求为同一续问重复检索已经观察的证据。
  const retrieved = [assistant, ...priorAssistants].some(item => (item.EvidenceReferences ?? []).some(reference => reference.sourceId === sourceId));
  const read = activities.some(item => item.status === 'completed' && (
    item.name === 'filesystem.read' && basename(item.arguments?.path ?? '') === SOURCE_TITLE &&
      parseToolResult(item).content?.includes('Mara Chen') ||
    item.name === 'knowledge.read' && JSON.stringify(parseToolResult(item)).includes('Mara Chen')));
  return cited && (retrieved || read);
}

/**
 * A correctly attributed second reviewer is valid contrast, not entity loss. Parse explicit local pairs.
 * 正确归属另一项目的审查人属于有效对照，不是实体丢失；只识别本组资料中明确的局部配对，不充当通用语义裁判。
 */
export function assessReviewerRelations(answer) {
  const hasNegation = fragment => {
    const role = [...fragment.matchAll(/审查(?:人)?|评审(?:人)?|审核(?:人)?|review(?:er|ed|s)?/giu)].at(-1);
    const negative = /不是|并非|不为|非由|\bnot\b|(?:is|are|does)n['’]?t/iu;
    if (!role) return negative.test(fragment);
    // Negation of a launch/window elsewhere does not negate the later reviewer predicate.
    // 其他位置否定发射/窗口状态，不会否定后续审查人谓词；只检查该关系角色附近的否定。
    return negative.test(fragment.slice(role.index + role[0].length)) ||
      /(?:不是|并非|不为|非由)\s*(?:该项目的|其|该|安全)?\s*$/u.test(fragment.slice(0, role.index)) ||
      /(?:\bnot\b|(?:is|are|does)n['’]?t)\s+(?:(?:the|a|its|his|her|their|safety|been|being)\s+)*$/iu.test(fragment.slice(0, role.index));
  };
  const reviewerField = /审查|评审|审核|review(?:er|ed|s)?/iu;
  const entityHeading = /^\s*(?:#{1,6}\s*|[-]\s*|\d+[.)、]\s*)?(?:(?:任务|项目|关于|mission|project|about)\s*[:：]?\s*)?\b(?:ORION|VEGA)\b/iu;
  const tokenize = text => [...text.matchAll(/\bORION\b|\bVEGA\b|\bMara\s+Chen\b|\bBeatrice\s+Hall\b/giu)]
      .map(match => ({ value: match[0].replace(/\s+/gu, ' '), index: match.index, end: match.index + match[0].length,
        entity: /^(?:ORION|VEGA)$/iu.test(match[0]) }));
  const relations = [];
  let activeEntity, tableColumns;
  for (const line of String(answer ?? '').replace(/[*_`]/gu, '').split('\n')) {
    if (!line.trim()) { activeEntity = null; tableColumns = null; continue; }
    const lineTokens = tokenize(line), lineEntities = [...new Set(lineTokens.filter(token => token.entity).map(token => token.value.toUpperCase()))];
    const lineReviewers = lineTokens.filter(token => !token.entity);
    if (line.includes('|') && lineEntities.length === 2 && !lineReviewers.length) {
      tableColumns = line.split('|').flatMap((cell, column) => tokenize(cell).filter(token => token.entity)
        .map(token => ({ column, entity: token.value.toUpperCase() })));
      activeEntity = null; continue;
    }
    if (line.includes('|') && tableColumns && reviewerField.test(line) && !lineEntities.length) {
      const cells = line.split('|');
      for (const { column, entity } of tableColumns) for (const token of tokenize(cells[column] ?? '').filter(token => !token.entity))
        relations.push({ entity, reviewer: token.value, negated: hasNegation(cells[column] ?? '') || hasNegation(cells.slice(0, tableColumns[0].column).join('|')) });
      continue;
    }
    if (!line.includes('|')) tableColumns = null;
    // Only an explicit entity heading/field binds the following continuous reviewer bullets; blank lines end that scope.
    // 只有明确的实体标题/字段才绑定后续连续审查人条目；空行结束该范围，另一实体段会切换或清除范围。
    if (!lineReviewers.length && lineEntities.length === 1 &&
        entityHeading.test(line))
      activeEntity = lineEntities[0];
    else if (lineEntities.some(entity => entity !== activeEntity)) activeEntity = null;
    for (const clause of line.split(/[。！？!?；;]|\.(?=\s|$)|[,，]/u)) {
      const tokens = tokenize(clause), field = reviewerField.exec(clause), firstReviewer = tokens.find(token => !token.entity);
      const clauseEntities = [...new Set(tokens.filter(token => token.entity).map(token => token.value.toUpperCase()))];
      if (!firstReviewer && clauseEntities.length === 1 && entityHeading.test(clause)) activeEntity = clauseEntities[0];
      if (!tokens.some(token => token.entity) && activeEntity && firstReviewer && field && field.index < firstReviewer.index)
        tokens.unshift({ value: activeEntity, index: 0, end: 0, entity: true });
      if (!tokens.some(token => token.entity) || !tokens.some(token => !token.entity)) continue;
      const entities = tokens.filter(token => token.entity), reviewers = tokens.filter(token => !token.entity);
      if (entities.length === 2 && reviewers.length === 2 && /分别|respectively/iu.test(clause)) {
        // A shared negated predicate applies to the paired list too, in either entity-first or reviewer-first order.
        // 共用的否定谓词也作用于分别配对列表，实体在前或人名在前都不能默认认定为肯定。
        const targets = tokens[0].entity ? reviewers : entities, listNegated = hasNegation(clause.slice(0, targets[0].index));
        for (const [index, entity] of entities.entries()) relations.push({ entity: entity.value.toUpperCase(), reviewer: reviewers[index].value,
          negated: listNegated || index > 0 && hasNegation(clause.slice(targets[index - 1].end, targets[index].index)) });
        continue;
      }
      const startsWithEntity = tokens[0].entity;
      let anchor, previous;
      for (const token of tokens) {
        if (token.entity === startsWithEntity) { anchor = token; previous = token; continue; }
        if (!anchor) continue;
        const gap = clause.slice(previous?.end ?? anchor.end, token.index);
        const negated = hasNegation(gap);
        relations.push({ entity: (startsWithEntity ? anchor.value : token.value).toUpperCase(),
          reviewer: startsWithEntity ? token.value : anchor.value, negated });
        previous = token;
      }
    }
  }
  const targetRelations = relations.filter(relation => relation.entity === 'ORION');
  const affirmsGold = targetRelations.some(relation => !relation.negated && /^Mara Chen$/iu.test(relation.reviewer));
  const deniesGold = targetRelations.some(relation => relation.negated && /^Mara Chen$/iu.test(relation.reviewer));
  const affirmsWrong = targetRelations.some(relation => !relation.negated && /^Beatrice Hall$/iu.test(relation.reviewer));
  return { entityRetained: affirmsGold && !deniesGold && !affirmsWrong,
    contradictoryGold: affirmsGold && deniesGold, relations };
}

export function answerLanguageDiagnostic(answer) {
  const hasHanText = /\p{Script=Han}/u.test(answer ?? ''), hasLatinText = /[a-z]/iu.test(answer ?? '');
  return { promptLanguage: 'Chinese', hasHanText, hasLatinText,
    observation: !hasHanText && hasLatinText ? 'English-only response to Chinese prompt' : hasHanText ? 'Contains Chinese text' : 'No language-bearing text',
    affectsSuccess: false };
}

/**
 * Fixed gold facts and disk state are checked without an LLM judge. Citation checks are bounded fixture checks.
 * 固定标准事实与磁盘状态不用 LLM 裁判；引用检查仅覆盖本组固定资料，不冒充开放域事实核查。
 */
export function verifyTask(taskId, { assistants, sourceId, finalFile, expectedFile = RECEIPT_TEXT }) {
  const last = assistants.at(-1) ?? {}, answer = String(last.Content ?? ''),
    activities = assistants.flatMap(item => item.ToolActivities ?? []), checks = {};
  checks.completed = assistants.length > 0 && assistants.every(item => item.Status === 'completed');
  if (taskId === 'greeting') {
    checks.shortChineseGreeting = /[\p{Script=Han}]/u.test(answer) && answer.trim().length > 0 && answer.length <= 120;
    checks.noCapabilityList = new Set(answer.match(/搜索|检索|浏览器|代码|文件|工具|桌面|终端|记忆|任务规划/gu) ?? []).size < 2
      && !/能力(?:包括|有|如下)|功能(?:包括|有|如下)|我是(?:一(?:个|名|款))?(?:AI|人工智能|大语言模型)/iu.test(answer);
    checks.noTools = activities.length === 0;
    checks.noEvidence = !assistants.some(item => item.EvidenceReferences?.length);
  } else if (taskId === 'english-source-chinese-answer') {
    checks.chineseAnswer = /[\p{Script=Han}]/u.test(answer);
    checks.goldFacts = /ORION/iu.test(answer) && /17[:：]40/u.test(answer) && /UTC|协调世界时|世界协调时/iu.test(answer)
      && /Mara\s+Chen/iu.test(answer) && /2031/u.test(answer)
      && /11\s*月\s*14|11[-/]14|14\s+November/iu.test(answer) && assessReviewerRelations(answer).entityRetained;
    checks.experimentalOnly = /实验|试验/u.test(answer) && /未|不|尚|非/u.test(answer);
    checks.groundedCitation = hasObservedCitation(last, sourceId, activities, assistants.slice(0, -1));
  } else if (taskId === 'followup-entity') {
    const initial = String(assistants[0]?.Content ?? '');
    checks.twoTurns = assistants.length === 2;
    checks.initialFacts = /ORION/iu.test(initial) && /VEGA/iu.test(initial) && /17[:：]40/u.test(initial) && /09?[:：]15/u.test(initial);
    checks.entityRetained = assessReviewerRelations(answer).entityRetained;
    checks.groundedCitation = hasObservedCitation(last, sourceId, activities, assistants.slice(0, -1));
  } else if (taskId === 'dependent-file-cycle') {
    const readIndex = activities.findIndex(item => item.name === 'filesystem.read' && item.status === 'completed' &&
      basename(item.arguments?.path ?? '') === 'seed.txt');
    const writeIndex = activities.findIndex((item, index) => index > readIndex && item.name === 'filesystem.write' &&
      item.status === 'completed' && basename(item.arguments?.path ?? '') === 'receipt.txt');
    const rereadIndex = activities.findIndex((item, index) => index > writeIndex && item.name === 'filesystem.read' &&
      item.status === 'completed' && !item.reused && basename(item.arguments?.path ?? '') === 'receipt.txt');
    checks.dependentToolRounds = readIndex >= 0 && writeIndex > readIndex && rereadIndex > writeIndex &&
      activities[readIndex].round < activities[writeIndex].round && activities[writeIndex].round < activities[rereadIndex].round;
    checks.diskFinalState = finalFile === expectedFile;
    checks.readBackState = rereadIndex >= 0 && parseToolResult(activities[rereadIndex]).content === expectedFile;
    checks.finalReport = answer.includes(expectedFile.trim().split('=')[1]);
  } else if (taskId === 'insufficient-evidence') {
    checks.abstains = /无法|不能确定|未确认|未记录|未提供|未给出|未明确|不确定|没有.*(?:时间|日期|资料|证据)|不足/u.test(answer);
    checks.noInventedSchedule = !/\b(?:19|20)\d{2}\b|\d{1,2}[:：]\d{2}/u.test(answer);
  } else throw failure('INVALID_BENCHMARK_TASK');
  return { success: Object.values(checks).every(Boolean), checks };
}

function costFor(runs) {
  const calls = runs.flatMap(run => run.modelCalls), complete = calls.every(call => call.usage.status === 'reported');
  const known = field => calls.reduce((sum, call) => sum + (call.usage[field] ?? 0), 0);
  return { tasks: runs.length, modelCalls: calls.length, toolCalls: runs.reduce((sum, run) => sum + run.toolCalls, 0),
    durationMs: runs.reduce((sum, run) => sum + run.durationMs, 0), setupMs: runs.reduce((sum, run) => sum + run.setupMs, 0),
    usageStatus: complete ? 'reported' : 'unknown', inputTokens: complete ? known('inputTokens') : null,
    outputTokens: complete ? known('outputTokens') : null, totalTokens: complete ? known('totalTokens') : null,
    knownInputTokens: known('inputTokens'), knownOutputTokens: known('outputTokens'),
    knownCacheReadTokens: known('cacheReadTokens'), knownCacheWriteTokens: known('cacheWriteTokens'),
    modelTimeMs: calls.reduce((sum, call) => sum + call.durationMs, 0),
    callsMissingUsage: calls.filter(call => call.usage.status !== 'reported').length,
    monetaryCost: null, monetaryCostReason: 'No pricing table or billing claim; resource cost includes failed calls.' };
}

export function summarizeRuns(runs) {
  return { attempted: runs.length, successful: runs.filter(run => run.success).length,
    successRate: runs.length ? runs.filter(run => run.success).length / runs.length : null,
    allCost: costFor(runs), successfulCost: costFor(runs.filter(run => run.success)), failedCost: costFor(runs.filter(run => !run.success)) };
}

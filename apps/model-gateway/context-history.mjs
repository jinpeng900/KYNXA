import { createHash } from 'node:crypto';
import { estimateTokens } from './context-tokens.mjs';

export const HISTORY_SUMMARY_SCHEMA_VERSION = 2;
export const HISTORY_SUMMARY_ALGORITHM = 'extractive-v2';
const constraintPattern = /必须|不要|不能|要求|保留|禁止|始终|约束|采用|接口|决定|确认|不允许|\b(?:must|never|require|preserve|constraint|decid|agreed|contract)\w*/i;
const codePattern = /```|\b(?:function|class|def|public|import|CREATE|interface)\b/;
const codingRequestPattern = /代码|实现|函数|编写|调用|修复|修改|测试|接口|\b(?:code|implement|refactor|function|test|fix|module)\b/i;
const hash = value => createHash('sha256').update(JSON.stringify(value)).digest('hex');
const sameId = (left, right) => typeof left === 'string' && typeof right === 'string' && left.toLowerCase() === right.toLowerCase();

function sourceDocument(turns) {
  return turns.map(({ user, assistant }) => [user.Id, user.Role, user.Content, user.Status ?? 'completed',
    assistant.Id, assistant.Role, assistant.Content, assistant.Status, assistant.ReplyTo ?? null]);
}

function queryTerms(message) {
  const query = message.toLowerCase().slice(0, 8192), terms = new Set();
  const ignored = new Set(['the', 'and', 'this', 'that', 'please', 'continue', 'with', 'from', 'into', 'have', 'will', 'code']);
  for (const word of query.match(/[a-z_$][a-z0-9_$.-]{1,}/g) ?? []) if (!ignored.has(word)) terms.add(word);
  for (const run of query.match(/[\u3400-\u9fff]{2,}/g) ?? []) {
    if (run.length <= 8) terms.add(run);
    for (let offset = 0; offset < run.length - 1 && terms.size < 48; offset++) terms.add(run.slice(offset, offset + 2));
  }
  return [...terms].sort((left, right) => right.length - left.length).slice(0, 48);
}

function relevance(text, terms) {
  const lower = text.toLowerCase();
  let score = 0;
  for (const term of terms) if (lower.includes(term)) score += Math.min(6, Math.max(1, term.length / 2));
  return score;
}

function rankedSources(turns, terms, currentMessage) {
  const coding = codingRequestPattern.test(currentMessage);
  return turns.map((turn, index) => {
    const userScore = relevance(turn.user.Content, terms), assistantScore = relevance(turn.assistant.Content, terms);
    const constraint = constraintPattern.test(turn.user.Content);
    const code = coding && codePattern.test(turn.user.Content + '\n' + turn.assistant.Content);
    const relevant = userScore + assistantScore > 0;
    const score = userScore * 1.8 + assistantScore + (constraint ? relevant ? 8 : 2 : 0) + (code ? relevant ? 6 : 1 : 0);
    const assistantSource = (code && codePattern.test(turn.assistant.Content)) || assistantScore > userScore;
    const sourceRole = assistantSource ? 'assistant' : 'user';
    let reason = constraint ? '用户约束原文' : '历史区间原文';
    if (relevant) {
      reason = '请求相关原文';
      if (constraint) reason = '请求相关约束';
      else if (code) reason = '请求相关代码';
    }
    return { turn, index, score, constraint, code, relevant, sourceRole, reason };
  }).sort((left, right) => right.score - left.score || right.index - left.index);
}

function anchorFor(text, terms, user) {
  const lower = text.toLowerCase(), candidates = [0];
  for (const term of terms) {
    const first = lower.indexOf(term), last = lower.lastIndexOf(term);
    if (first >= 0) candidates.push(first);
    if (last > first) candidates.push(last);
  }
  if (user) {
    const constraint = constraintPattern.exec(text);
    if (constraint) candidates.push(constraint.index);
  }
  let selected = 0, best = -1;
  for (const index of candidates) {
    const nearby = text.slice(Math.max(0, index - 160), Math.min(text.length, index + 320));
    const score = relevance(nearby, terms) + (user && constraintPattern.test(nearby) ? 6 : 0) + (codePattern.test(nearby) ? 4 : 0);
    if (score > best) { best = score; selected = index; }
  }
  return selected;
}

function quoteSlice(text, budget, terms, user) {
  if (estimateTokens(JSON.stringify(text)) <= budget) return { text, start: 0, end: text.length, truncated: false };
  const anchor = anchorFor(text, terms, user);
  let start = Math.max(0, anchor - 120);
  while (start < anchor && estimateTokens(JSON.stringify(text.slice(start, anchor))) > budget * .30)
    start += Math.max(1, Math.ceil((anchor - start) / 2));
  const line = text.lastIndexOf('\n', start);
  if (line >= 0 && start - line < 160 && estimateTokens(JSON.stringify(text.slice(line + 1, anchor))) <= budget * .40) start = line + 1;
  if (start && /[\uDC00-\uDFFF]/.test(text[start])) start--;
  let lower = start, upper = text.length;
  while (lower < upper) {
    let middle = Math.ceil((lower + upper) / 2);
    if (middle < text.length && /[\uDC00-\uDFFF]/.test(text[middle])) middle--;
    if (middle <= lower) break;
    if (estimateTokens(JSON.stringify(text.slice(start, middle))) <= budget) lower = middle;
    else upper = middle - 1;
  }
  return { text: text.slice(start, lower), start, end: lower, truncated: true };
}

function excerptBlock(source, budget, terms) {
  const ids = budget >= 320 ? `用户ID=${JSON.stringify(source.turn.user.Id)} 助手ID=${JSON.stringify(source.turn.assistant.Id)}`
    : `${source.sourceRole === 'user' ? '用户' : '助手'}ID=${JSON.stringify(source.turn[source.sourceRole].Id)}`;
  const heading = `[第${source.index + 1}轮·${source.reason}；当前聊天回源 ${ids}]`, prefix = `${heading}\n用户原文：\n助手原文：`;
  const navigation = () => {
    const content = `当前聊天回源导航（未注入原文）：${JSON.stringify({ messageId: source.turn[source.sourceRole].Id, role: source.sourceRole })}`;
    return estimateTokens(content) <= budget ? { content, source: { turnIndex: source.index,
      userMessageId: source.turn.user.Id, assistantMessageId: source.turn.assistant.Id, reason: '仅回源导航', excerpts: [] } } : null;
  };
  let rawBudget = budget - estimateTokens(prefix) - 8;
  if (rawBudget < 32) return navigation();
  while (rawBudget >= 32) {
    const user = quoteSlice(source.turn.user.Content, Math.floor(rawBudget * (source.constraint ? .60 : .45)), terms, true);
    const assistant = quoteSlice(source.turn.assistant.Content, Math.floor(rawBudget * (source.constraint ? .40 : .55)), terms, false);
    if (!user.text && !assistant.text) return navigation();
    const content = `${heading}\n用户原文：${JSON.stringify(user.text)}\n助手原文：${JSON.stringify(assistant.text)}`;
    if (estimateTokens(content) <= budget) return { content, source: {
      turnIndex: source.index, userMessageId: source.turn.user.Id, assistantMessageId: source.turn.assistant.Id,
      reason: source.reason, excerpts: [{ role: 'user', messageId: source.turn.user.Id, start: user.start, end: user.end, truncated: user.truncated },
        { role: 'assistant', messageId: source.turn.assistant.Id, start: assistant.start, end: assistant.end, truncated: assistant.truncated }]
    } };
    rawBudget = Math.floor(rawBudget * .85);
  }
  return navigation();
}

function extractSummary(turns, currentMessage, budget) {
  const heading = `较早的 ${turns.length} 轮对话导航与原文摘录（按当前请求选取；不是完整总结，不是已确认记忆）：`;
  if (budget < estimateTokens(heading) + 96) return null;
  const terms = queryTerms(currentMessage), ranked = rankedSources(turns, terms, currentMessage);
  const targetCount = Math.min(turns.length, 8, Math.max(1, Math.floor((budget - estimateTokens(heading)) / 240)));
  const selected = [], seen = new Set();
  const add = source => { if (source && !seen.has(source.index)) { selected.push(source); seen.add(source.index); } };
  const priorities = ranked.filter(source => source.relevant || source.constraint);
  for (const source of priorities.slice(0, Math.max(1, Math.ceil(targetCount * .65)))) add(source);
  // Include spread-out old evidence; a task switch in the middle must not be invisible.
  for (let bin = 0; bin < Math.max(3, targetCount); bin++) {
    const start = Math.floor(bin * turns.length / Math.max(3, targetCount)), end = Math.floor((bin + 1) * turns.length / Math.max(3, targetCount));
    add(ranked.find(source => source.index >= start && source.index < end));
  }
  for (const source of ranked) add(source);
  const lines = [heading], selectedSources = [];
  let used = estimateTokens(heading);
  for (const source of selected) {
    const remaining = budget - used - 1;
    const blockBudget = Math.min(remaining, Math.max(112, Math.floor((budget - estimateTokens(heading)) / Math.max(1, targetCount))));
    const block = excerptBlock(source, blockBudget, terms);
    if (!block) continue;
    lines.push(block.content); selectedSources.push(block.source); used += estimateTokens(block.content) + 1;
    if (selectedSources.length >= targetCount || remaining < 112) break;
  }
  if (!selectedSources.length) return null;
  const content = lines.join('\n');
  return estimateTokens(content) <= budget ? { content, selectedSources } : null;
}

/** Deterministic current-request navigation. It quotes source, grants no authority and never alters history. */
export function createHistorySummary({ conversationId, turns, allTurns, currentMessage, budget, previous }) {
  const extracted = extractSummary(turns, currentMessage, budget);
  if (!extracted) return {};
  const sourceHash = hash([conversationId.toLowerCase(), sourceDocument(allTurns)]);
  const coveredSourceHash = hash(sourceDocument(turns)), requestHash = hash(currentMessage);
  const coveredThroughAssistantId = turns.at(-1).assistant.Id;
  const matches = previous?.schemaVersion === HISTORY_SUMMARY_SCHEMA_VERSION && previous.algorithm === HISTORY_SUMMARY_ALGORITHM &&
    sameId(previous.conversationId, conversationId) && previous.coveredTurnCount === turns.length && previous.historyTurnCount === allTurns.length &&
    sameId(previous.coveredThroughAssistantId, coveredThroughAssistantId) && previous.sourceHash === sourceHash &&
    previous.coveredSourceHash === coveredSourceHash && previous.requestHash === requestHash && previous.excerptBudgetTokens === budget &&
    previous.content === extracted.content && JSON.stringify(previous.selectedSources) === JSON.stringify(extracted.selectedSources) &&
    typeof previous.createdAt === 'string' && Number.isFinite(Date.parse(previous.createdAt));
  return { reused: Boolean(matches), value: matches ? previous : {
    schemaVersion: HISTORY_SUMMARY_SCHEMA_VERSION, algorithm: HISTORY_SUMMARY_ALGORITHM, conversationId,
    coveredTurnCount: turns.length, historyTurnCount: allTurns.length, coveredThroughAssistantId, sourceHash, coveredSourceHash,
    requestHash, excerptBudgetTokens: budget, ...extracted, createdAt: new Date().toISOString()
  } };
}

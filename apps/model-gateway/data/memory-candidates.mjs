import { MEMORY_CANDIDATE_ALGORITHM, MAX_MEMORY_CONTENT, DEFAULT_MEMORY_CANDIDATE_SETTINGS,
  memoryCandidateFingerprint, memoryFailure, memoryId } from './memory-contracts.mjs';
export { DEFAULT_MEMORY_CANDIDATE_SETTINGS, memoryCandidateSettings } from './memory-contracts.mjs';

function instruction(message) {
  const completed = /^\s*(?:(全局|用户|聊天|项目|工作)\s*)?(?:任务(?:已)?完成|已完成任务|这项任务完成)(?:\s*[:：，,]?\s*[\s\S]*)$/u.exec(message);
  if (completed) return { scope: ['全局', '用户'].includes(completed[1]) ? 'user' : ['项目', '工作'].includes(completed[1]) ? 'project' : 'chat', trigger: 'task-complete' };
  const match = /^\s*(?:(全局|用户|聊天|项目|工作)\s*)?(记住(?:这个)?|更正|纠正|修正|决定|决策|约定|我们决定|最终决定|任务完成|任务已完成|这项任务完成|已完成任务|记忆更正)\s*[:：，,]?\s*(\S[\s\S]*)$/u.exec(message);
  if (match) return { scope: ['全局', '用户'].includes(match[1]) ? 'user' : ['项目', '工作'].includes(match[1]) ? 'project' : 'chat',
    trigger: /记住/u.test(match[2]) ? 'remember' : /更正|纠正|修正/u.test(match[2]) ? 'correction'
      : /完成/u.test(match[2]) ? 'task-complete' : 'decision' };
  const english = /^\s*(remember|correction|correct this|decision|we decided|task completed)\s*[:：,]?\s*(\S[\s\S]*)$/iu.exec(message);
  if (english) return { scope: 'chat', trigger: /remember/iu.test(english[1]) ? 'remember' : /correct/iu.test(english[1])
    ? 'correction' : /completed/iu.test(english[1]) ? 'task-complete' : 'decision' };
  return null;
}

function eligibleUserText(message) {
  if (typeof message !== 'string' || !message.trim() || message.length > MAX_MEMORY_CONTENT ||
      /(?:```|~~~|^\s*>|\[KYNXA_|https?:\/\/|(?:网页|工具|模型|助手)(?:说|回复|输出|返回)|(?:quoted|tool output|web page)\s*[:：])/imu.test(message)) return false;
  return true;
}

function candidate(scope, trigger, quotes) {
  const content = quotes.map(quote => quote.text.trim()).join('\n\n');
  return { scope, content, kind: trigger === 'decision' ? 'decision' : /(?:偏好|喜欢|习惯|prefer|usually)/iu.test(content) ? 'preference' : 'fact',
    candidate: { algorithm: MEMORY_CANDIDATE_ALGORITHM, trigger, fingerprint: memoryCandidateFingerprint(scope, content), quotes } };
}

function completeRounds(messages) {
  const rounds = [];
  let pending;
  for (const message of messages) {
    if (message.Role === 'user') {
      if (pending?.complete) rounds.push(pending);
      pending = { user: message, complete: false, tokens: Math.ceil(Buffer.byteLength(message.Content ?? '', 'utf8') / 3) };
    } else if (pending && message.Role === 'assistant' && message.Status === 'completed' && message.Content?.trim()) {
      pending.complete = true;
      pending.tokens += Math.ceil(Buffer.byteLength(message.Content, 'utf8') / 3);
    }
  }
  if (pending?.complete) rounds.push(pending);
  return rounds;
}

/** Rules never interpret assistant/tool/web text as authority; complete assistant turns only pace cheap user-quote batches.
 * 规则不把助手、工具或网页内容当权威；完整助手轮次只用于调度廉价的用户原话批处理。 */
export function extractMemoryCandidates(messages, { phase = 'completed', userMessageId, hasProject = false,
  consumedMessageIds = new Set(), afterMessageId, taskCompleted = false, settings = DEFAULT_MEMORY_CANDIDATE_SETTINGS } = {}) {
  if (!['user', 'completed'].includes(phase)) throw memoryFailure('候选提取阶段无效。');
  if (typeof taskCompleted !== 'boolean') throw memoryFailure('任务阶段完成标记无效。');
  if (!settings.enabled) return { candidates: [], reason: 'disabled', completeTurns: 0, estimatedTokens: 0 };
  const users = messages.filter(message => message.Role === 'user');
  const current = userMessageId ? users.find(message => memoryId(message.Id) === memoryId(userMessageId)) : users.at(-1);
  if (userMessageId && !current) throw memoryFailure('候选来源须为已保存的用户消息。');
  const candidates = [];
  if (current && !consumedMessageIds.has(memoryId(current.Id)) && eligibleUserText(current.Content)) {
    const trigger = instruction(current.Content);
    if (trigger && (trigger.scope !== 'project' || hasProject)) candidates.push(candidate(trigger.scope, trigger.trigger,
      [{ messageId: memoryId(current.Id), text: current.Content }]));
  }
  if (phase === 'user') return { candidates, reason: candidates.length ? 'explicit-user-trigger' : 'no-trigger', completeTurns: 0, estimatedTokens: 0 };
  const complete = completeRounds(messages);
  // A trusted workflow completion may propose the user's goal, never an assistant's claimed outcome.
  // 受控流程完成时可提议用户目标原话，不能把助手宣称的执行结果提取成事实。
  if (taskCompleted && current && !consumedMessageIds.has(memoryId(current.Id)) && !candidates.length &&
      eligibleUserText(current.Content) && complete.some(round => memoryId(round.user.Id) === memoryId(current.Id)))
    candidates.push(candidate('chat', 'task-complete', [{ messageId: memoryId(current.Id), text: current.Content }]));
  const stagedSourceIds = new Set(candidates.flatMap(entry => entry.candidate.quotes.map(quote => quote.messageId)));
  const checkpoint = afterMessageId ? complete.findIndex(round => memoryId(round.user.Id) === afterMessageId) : -1;
  const rounds = complete.slice(checkpoint + 1).slice(-settings.maxTurns);
  const estimatedTokens = rounds.reduce((total, round) => total + round.tokens, 0);
  if (rounds.length < settings.minTurns || estimatedTokens < settings.minTokens && rounds.length < settings.maxTurns)
    return { candidates, reason: 'batch-not-due', completeTurns: rounds.length, estimatedTokens };
  let quotes = [], usedTokens = 0;
  const flush = () => {
    if (quotes.length && candidates.length < settings.maxCandidates) {
      const extracted = candidate('chat', 'ordinary-batch', quotes);
      extracted.candidate.batchThroughMessageId = memoryId(rounds.at(-1).user.Id);
      candidates.push(extracted);
    }
    quotes = [];
  };
  for (const { user } of rounds) {
    if (consumedMessageIds.has(memoryId(user.Id)) || stagedSourceIds.has(memoryId(user.Id)) || !eligibleUserText(user.Content) || instruction(user.Content) ||
        !/^\s*(?:我(?:更|通常|一直)?(?:喜欢|偏好|习惯|使用|希望|需要)|请(?:以后|始终|默认)|我们的?(?:约定|目标|要求|限制|计划)|(?:约定|目标|要求|限制|计划|下一步)\s*[:：]|I (?:prefer|usually|always|use|need)|We (?:use|plan|need)|My (?:preference|goal))/iu.test(user.Content)) continue;
    const tokenCount = Math.ceil(Buffer.byteLength(user.Content, 'utf8') / 3);
    if (usedTokens + tokenCount > settings.maxTokens) break;
    if (quotes.map(quote => quote.text.trim()).join('\n\n').length + user.Content.trim().length + 2 > MAX_MEMORY_CONTENT) flush();
    if (candidates.length >= settings.maxCandidates) break;
    quotes.push({ messageId: memoryId(user.Id), text: user.Content }); usedTokens += tokenCount;
  }
  flush();
  return { candidates, reason: 'complete-turn-batch', completeTurns: rounds.length, estimatedTokens };
}

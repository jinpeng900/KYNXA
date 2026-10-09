/** Allocate candidates separately from the evidence that enters a model request.
 * 候选召回池与模型证据预算分别计算；显式用户限制和上下文余量始终优先。 */
export function retrievalBudget({ taskType = 'lookup', suggestions = {}, limit, maximumTokens } = {}) {
  const complex = ['complex', 'research'].includes(taskType);
  const bounded = (value, fallback, minimum, maximum) => Number.isSafeInteger(value)
    ? Math.max(minimum, Math.min(maximum, value)) : fallback;
  const candidateLimit = bounded(suggestions.candidateLimit, complex ? 128 : 64, 1, complex ? 160 : 96);
  const configured = { channelCandidates: complex ? 128 : 64, fusedCandidates: complex ? 160 : 96,
    limit: complex ? 40 : taskType === 'file' ? 24 : 16,
    maximumTokens: complex ? 32768 : taskType === 'file' ? 16384 : 8192,
    rerankCandidates: complex ? 60 : taskType === 'file' ? 40 : 20 };
  const budget = { channelCandidates: candidateLimit,
    fusedCandidates: bounded(suggestions.fusedCandidateLimit, complex ? 160 : 96, 1, complex ? 192 : 128),
    limit: limit ?? configured.limit,
    maximumTokens: maximumTokens ?? bounded(suggestions.evidenceBudgetTokens,
      configured.maximumTokens, 0, 32768),
    rerankCandidates: Math.min([20, 40, 60].includes(suggestions.rerankCandidates) ? suggestions.rerankCandidates : configured.rerankCandidates,
      [20, 40, 60].includes(suggestions.rerankCandidateLimit) ? suggestions.rerankCandidateLimit : 60),
    source: suggestions.source ?? 'task-policy' };
  // A large recall pool is not a large final projection; record each contract without silently clamping one to the other.
  // 大召回池不等于大最终投影；分阶段记录契约，不能静默按最终引用数截断内部候选。
  budget.audit = { requested: { limit: limit ?? null, maximumTokens: maximumTokens ?? null }, configured,
    resourceSuggestions: Object.fromEntries(['candidateLimit', 'fusedCandidateLimit', 'evidenceBudgetTokens', 'rerankCandidates', 'rerankCandidateLimit']
      .filter(key => suggestions[key] !== undefined).map(key => [key, suggestions[key]])),
    approved: { channelCandidates: budget.channelCandidates, fusedCandidates: budget.fusedCandidates,
      limit: budget.limit, maximumTokens: budget.maximumTokens, rerankCandidates: budget.rerankCandidates },
    downstreamLimits: { channelCandidates: 160, fusedCandidates: 192, finalReferences: 60, maximumTokens: 32768 },
    actual: null, earlyCutReasons: [] };
  for (const [suggestion, approved, unit] of [['candidateLimit', 'channelCandidates', 'candidates'],
    ['fusedCandidateLimit', 'fusedCandidates', 'candidates'], ['evidenceBudgetTokens', 'maximumTokens', 'tokens']]) {
    if (Number.isSafeInteger(suggestions[suggestion]) && suggestions[suggestion] > budget[approved])
      budget.audit.earlyCutReasons.push({ reason: maximumTokens !== undefined && approved === 'maximumTokens' ?
        'requested-evidence-token-limit' : 'candidate-or-evidence-policy-limit',
      field: approved, proposed: suggestions[suggestion], approved: budget[approved], unit });
    if (Number.isSafeInteger(suggestions[suggestion]) && budget[approved] < configured[approved] &&
        !(maximumTokens !== undefined && approved === 'maximumTokens'))
      budget.audit.earlyCutReasons.push({ reason: 'resource-grant', field: approved,
        proposed: configured[approved], approved: budget[approved], unit });
  }
  return budget;
}

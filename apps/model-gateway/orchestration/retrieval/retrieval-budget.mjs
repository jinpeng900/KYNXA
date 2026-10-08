/** Allocate candidates separately from the evidence that enters a model request.
 * 候选召回池与模型证据预算分别计算；显式用户限制和上下文余量始终优先。 */
export function retrievalBudget({ taskType = 'lookup', suggestions = {}, limit, maximumTokens } = {}) {
  const complex = ['complex', 'research'].includes(taskType);
  const bounded = (value, fallback, minimum, maximum) => Number.isSafeInteger(value)
    ? Math.max(minimum, Math.min(maximum, value)) : fallback;
  const candidateLimit = bounded(suggestions.candidateLimit, complex ? 96 : 48, 24, 160);
  return { channelCandidates: complex ? Math.max(64, candidateLimit) : candidateLimit,
    fusedCandidates: complex ? Math.max(96, bounded(suggestions.fusedCandidateLimit, 128, 48, 192)) :
      bounded(suggestions.fusedCandidateLimit, 64, 24, 192),
    limit: limit ?? (complex ? 18 : taskType === 'file' ? 12 : 8),
    maximumTokens: maximumTokens ?? bounded(suggestions.evidenceBudgetTokens,
      complex ? 16384 : taskType === 'file' ? 12288 : 8192, complex ? 8192 : 4096, 32768),
    source: suggestions.source ?? 'task-policy' };
}

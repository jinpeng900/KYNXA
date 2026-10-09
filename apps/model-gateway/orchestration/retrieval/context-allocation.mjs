/** Allocate a task's evidence and schema ceilings without treating either reservation as actual use.
 * 按任务分配证据和工具声明上限，预留额度不能冒充实际用量，也不能超过真实剩余上下文。 */
export function contextAllocation(inputBudgetTokens, taskType = 'lookup') {
  const inputTokens = Math.max(0, Math.floor(inputBudgetTokens));
  const research = ['research', 'complex'].includes(taskType);
  const file = taskType === 'file';
  const evidenceFraction = research ? .24 : file ? .18 : .12;
  const schemaFraction = research ? .20 : file ? .30 : .40;
  return { version: 1, taskType, inputBudgetTokens: inputTokens, evidenceFraction, schemaFraction,
    requestedEvidenceTokens: Math.min(16384, Math.floor(inputTokens * evidenceFraction)),
    schemaCeilingTokens: Math.min(24000, Math.floor(inputTokens * schemaFraction)), safetyTokens: 512 };
}

export function contextAllocationAudit(allocation, { actualEvidenceTokens = 0, actualSchemaTokens = 0,
  finalEvidenceBudgetTokens = allocation.requestedEvidenceTokens, configuredEvidenceTokens = allocation.requestedEvidenceTokens,
  retrievalBudgetAudit } = {}) {
  const earlyCutReasons = [];
  const evidenceRequested = allocation.evidenceRequested !== false;
  if (!evidenceRequested) earlyCutReasons.push('retrieval-not-requested');
  if (evidenceRequested && allocation.requestedEvidenceTokens < configuredEvidenceTokens) earlyCutReasons.push('task-context-share');
  if (evidenceRequested && finalEvidenceBudgetTokens < Math.min(configuredEvidenceTokens, allocation.requestedEvidenceTokens)) earlyCutReasons.push('remaining-context');
  if (evidenceRequested && actualEvidenceTokens < finalEvidenceBudgetTokens) earlyCutReasons.push('available-selected-evidence');
  return { ...allocation, configuredEvidenceTokens,
    approvedEvidenceTokens: evidenceRequested ? Math.max(0, Math.min(configuredEvidenceTokens,
      allocation.requestedEvidenceTokens, finalEvidenceBudgetTokens)) : 0,
    actualEvidenceTokens, actualSchemaTokens, unusedSchemaTokens: Math.max(0, allocation.schemaCeilingTokens - actualSchemaTokens),
    earlyCutReasons, ...(retrievalBudgetAudit ? { retrieval: retrievalBudgetAudit } : {}) };
}

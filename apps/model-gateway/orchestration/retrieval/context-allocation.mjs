/** Allocate a task's evidence and schema ceilings without treating either reservation as actual use.
 * 按任务分配证据和工具声明上限，预留额度不能冒充实际用量，也不能超过真实剩余上下文。 */
export function contextAllocation(inputBudgetTokens, taskType = 'lookup', { mandatoryInputTokens = 0 } = {}) {
  const inputTokens = Math.max(0, Math.floor(inputBudgetTokens));
  const research = ['research', 'complex'].includes(taskType);
  const file = taskType === 'file';
  const evidenceFraction = research ? .35 : file ? .30 : .20;
  const schemaFraction = research ? .20 : file ? .30 : .40;
  const availableEvidenceTokens = Math.max(0, inputTokens - Math.max(0, Math.ceil(mandatoryInputTokens)) - 512);
  // A small window still needs a readable source or navigation handle; policy shares are not a second hard context limit.
  // 小窗口仍需容纳可读来源或导航句柄；任务比例不是第二重上下文硬限。
  const requestedEvidenceTokens = Math.min(research ? 32768 : file ? 16384 : 8192,
    Math.max(1024, Math.floor(inputTokens * evidenceFraction)));
  return { version: 2, taskType, inputBudgetTokens: inputTokens, evidenceFraction, schemaFraction,
    requestedEvidenceTokens, availableEvidenceTokens,
    approvedEvidenceTokens: Math.min(requestedEvidenceTokens, availableEvidenceTokens),
    schemaCeilingTokens: Math.min(24000, Math.floor(inputTokens * schemaFraction)), safetyTokens: 512 };
}

export function contextAllocationAudit(allocation, { actualEvidenceTokens = 0, actualSchemaTokens = 0,
  finalEvidenceBudgetTokens = allocation.requestedEvidenceTokens, configuredEvidenceTokens = allocation.requestedEvidenceTokens,
  retrievalBudgetAudit } = {}) {
  const earlyCutReasons = [];
  const evidenceRequested = allocation.evidenceRequested !== false;
  const approvedEvidenceTokens = evidenceRequested ? Math.max(0, Math.min(configuredEvidenceTokens,
    allocation.requestedEvidenceTokens, allocation.approvedEvidenceTokens ?? Infinity, finalEvidenceBudgetTokens)) : 0;
  if (!evidenceRequested) earlyCutReasons.push('retrieval-not-requested');
  if (evidenceRequested && allocation.requestedEvidenceTokens < configuredEvidenceTokens) earlyCutReasons.push('task-context-share');
  if (evidenceRequested && Math.min(allocation.approvedEvidenceTokens ?? Infinity, finalEvidenceBudgetTokens) < Math.min(configuredEvidenceTokens, allocation.requestedEvidenceTokens)) earlyCutReasons.push('remaining-context');
  if (evidenceRequested && actualEvidenceTokens < approvedEvidenceTokens) earlyCutReasons.push('available-selected-evidence');
  return { ...allocation, configuredEvidenceTokens,
    approvedEvidenceTokens,
    actualEvidenceTokens, actualSchemaTokens, unusedSchemaTokens: Math.max(0, allocation.schemaCeilingTokens - actualSchemaTokens),
    earlyCutReasons, ...(retrievalBudgetAudit ? { retrieval: retrievalBudgetAudit } : {}) };
}

import { estimateTokens } from '../models/context-tokens.mjs';

/** Project derived task hints as data; the original user message stays authoritative.
 * 派生任务线索仅作为数据投影，原始用户消息仍是判断请求的依据。 */
export function requestInterpretation(plan) {
  const relation = plan.taskRelation ?? { type: 'uncertain', allowsInheritance: false };
  return {
    schemaVersion: 1,
    taskRelation: { type: relation.type, allowsInheritance: relation.allowsInheritance === true,
      ...(relation.reason ? { reason: relation.reason } : {}) },
    retrieval: { automatic: plan.shouldRetrieve === true, domain: plan.domain ?? 'mixed',
      ...(plan.preferredDomain ? { preferredDomain: plan.preferredDomain } : {}),
      domainConstraint: plan.domainConstraint ?? 'none', operation: plan.operation ?? 'search' },
    clues: (plan.clues ?? []).slice(0, 12).map(({ kind, value, strength, origin, verified, basis, startOffset, endOffset }) =>
      ({ kind, value, strength, origin, ...(verified !== undefined ? { verified } : {}),
        ...(basis ? { basis } : {}), ...(Number.isInteger(startOffset) ? { startOffset, endOffset } : {}) })),
    queryDerivation: plan.queryDerivation ?? { originalQuery: plan.originalQuery,
      additions: [], preservedOriginal: true },
    grantsPermission: false,
    interpretationVerified: false
  };
}

/** Include bounded provenance only when it helps interpret a real request, not greetings.
 * 仅在真实任务中按预算提供有助于理解的来源记录，不挤占问候或当前用户原话。 */
export function requestInterpretationPrompt(interpretation, maximumTokens = 192) {
  const additions = interpretation.queryDerivation.additions ?? [];
  const replacements = interpretation.queryDerivation.replacements ?? [];
  const weakClues = interpretation.clues.filter(clue => clue.strength === 'weak');
  const relation = interpretation.taskRelation;
  if (relation.type === 'new' && !weakClues.length && !additions.length && !replacements.length && !interpretation.retrieval.preferredDomain) return '';
  // Raw clues remain in their user/history messages; system hints must not revive dismissed memory facts.
  // 原始线索保留在用户与历史消息中，系统提示不能借此复活已撤销的记忆事实。
  const value = { relation: relation.type, inherit: relation.allowsInheritance,
    ...(interpretation.retrieval.preferredDomain ? { preferredDomain: interpretation.retrieval.preferredDomain } : {}),
    ...(weakClues.length ? { candidateKinds: [...new Set(weakClues.map(clue => clue.kind))].slice(0, 3) } : {}),
    ...(additions.length ? { additions: additions.slice(0, 2).map(({ origin, basis, historyId }) =>
      ({ origin, basis, ...(historyId ? { historyId } : {}) })) } : {}),
    ...(replacements.length ? { explicitCorrectionCount: replacements.length } : {}) };
  const prefix = 'Request hints (unverified; check current request/evidence; no authorization): ';
  let prompt = prefix + JSON.stringify(value);
  if (estimateTokens(prompt) <= maximumTokens) return prompt;
  // Keep relation/eligibility facts when optional provenance cannot fit; never truncate a fact mid-string.
  // 可选来源记录超预算时保留关系与检索约束，不在字符串中途截断事实。
  prompt = prefix + JSON.stringify({ relation: value.relation, inherit: value.inherit,
    ...(value.preferredDomain ? { preferredDomain: value.preferredDomain } : {}), optionalCluesDeferred: true });
  return estimateTokens(prompt) <= maximumTokens ? prompt : '';
}

/** Recovery follows observed status, not an instruction to retry every failed query.
 * 恢复动作依据实际状态区分，不能把所有失败都处理成换词重搜。 */
export function retrievalOutcome(result, error) {
  if (error) {
    const code = typeof error.code === 'string' && /^[A-Z][A-Z0-9_]{0,79}$/u.test(error.code)
      ? error.code : 'RETRIEVAL_FAILED';
    if (/STALE|SOURCE_CHANGED|REFERENCE.*(?:STALE|INVALID)|INVALID.*REFERENCE/u.test(code))
      return { state: 'source-changed', code, next: 'resolve-or-read-current-source' };
    if (/INVALID|ARGUMENT|SCOPE_MISMATCH/u.test(code))
      return { state: 'invalid-arguments', code, next: 'correct-parameters-with-current-evidence' };
    if (/UNAVAILABLE|DISABLED|CLOSED|UNSUPPORTED/u.test(code))
      return { state: 'tool-unavailable', code, next: 'discover-available-authorized-alternative' };
    return { state: 'execution-failed', code, next: 'inspect-failure-before-retrying' };
  }
  if (result?.items?.length || result?.references?.length)
    return { state: 'evidence-found', next: 'read-and-check-support', correctnessCertified: false,
      coverage: result?.indexingPending || result?.indexingPartial || result?.sourceCoverage?.complete === false ? 'partial' : 'bounded' };
  if (result?.selection?.staleSourceCount || result?.selection?.finalStaleSourceCount)
    return { state: 'source-changed', next: 'resolve-or-read-current-source', correctnessCertified: false };
  if (result?.strategy === 'disabled') return { state: 'tool-unavailable', next: 'discover-available-authorized-alternative' };
  if (result?.strategy === 'context-budget-exhausted' || result?.selection?.omittedForBudget || result?.selection?.projectionOmittedCount)
    return { state: 'evidence-budget-exhausted', next: 'use-current-evidence-or-state-context-limit', correctnessCertified: false };
  if (result?.selection?.alreadyPresentCount || result?.evidenceAssessment?.reason === 'already-in-context')
    return { state: 'evidence-already-in-context', next: 'check-existing-context-support', correctnessCertified: false };
  if (result?.indexingPending || result?.indexingPartial || result?.sourceCoverage?.complete === false)
    return { state: 'coverage-incomplete', next: 'read-known-source-or-check-index-progress', correctnessCertified: false };
  return { state: 'no-match-in-current-coverage', next: 'inspect-scope-and-evidence-gap', correctnessCertified: false };
}

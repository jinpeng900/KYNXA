import { lexicalTerms, retrievalQueryConcepts } from '../../data/retrieval/retrieval-text.mjs';
import { normalizeRetrievalPath } from '../../data/retrieval/retrieval-contracts.mjs';
import { projectRetrievalModelView } from '../../data/retrieval/evidence-references.mjs';
import { estimateTokens } from '../../models/context-tokens.mjs';
import { buildRetrievalIntent } from './query-plan.mjs';

export const RETRIEVAL_CANDIDATE_LIMIT = 48;
const MAX_SELECTION_TOKENS = 32768;
const COMMON_QUERY_WORDS = new Set(['the', 'a', 'an', 'and', 'or', 'to', 'of', 'in', 'on', 'is', 'are',
  'what', 'how', 'where', 'when', 'which', 'does', 'do', 'can', 'should', 'please', 'this', 'that', 'it',
  'according', 'document', 'file', 'project']);

function normalizedText(text) {
  return String(text ?? '').normalize('NFKC').replace(/\s+/gu, ' ').trim().toLowerCase();
}

function informativeTerms(text) {
  const terms = lexicalTerms(String(text).replace(/根据资料|根据文档|帮我|请问|请你|如何|怎么|什么|这个|那个|之前|上次|资料|文档|文件|项目/gu, ' '), 512);
  const hasHanBigrams = terms.some(term => /^\p{Script=Han}{2}$/u.test(term));
  return new Set(terms.filter(term => !COMMON_QUERY_WORDS.has(term) &&
    (!hasHanBigrams || !/^\p{Script=Han}$/u.test(term))));
}

function tokenSimilarity(left, right) {
  if (!left.size || !right.size) return 0;
  let intersection = 0;
  for (const term of left) if (right.has(term)) intersection++;
  return intersection / (left.size + right.size - intersection);
}

function queryCoverage(terms, queryTerms, concepts) {
  let matchedTerms = 0;
  for (const term of queryTerms) if (terms.has(term)) matchedTerms++;
  const literal = queryTerms.size ? matchedTerms / queryTerms.size : 0;
  const bridged = concepts.length ? concepts.filter(aliases => aliases.some(term => terms.has(term))).length / concepts.length : 0;
  return Math.max(literal, bridged * 0.85);
}

function metadataText(item) {
  const structure = item.structure ?? {};
  return [item.title, item.locator?.relativePath, item.locator?.path, structure.symbolName,
    structure.qualifiedName, structure.parentSymbol, structure.sectionTitle, ...(structure.sectionPath ?? [])]
    .filter(value => typeof value === 'string').join(' ');
}

function candidateRole(item) {
  const path = String(item.locator?.relativePath ?? item.locator?.path ?? item.title ?? '').toLowerCase();
  if (/(?:^|[\\/._-])(?:tests?|spec)(?:[\\/._-]|$)/u.test(path)) return 'tests';
  if (/(?:config|settings)|\.(?:json|ya?ml|toml)$/u.test(path)) return 'configuration';
  if (item.structure?.domain === 'code' || /\.(?:cs|[cm]?js|jsx|[cm]?ts|tsx|py|rs|go)$/u.test(path)) return 'implementation';
  return 'source';
}

function requestedRoles(query) {
  const roles = new Set();
  if (/测试|验证|回归|\b(?:tests?|testing|verify|validation|regression)\b/iu.test(query)) roles.add('tests');
  if (/配置|设置|参数|\b(?:configuration|config|settings|parameters?)\b/iu.test(query)) roles.add('configuration');
  if (/实现|逻辑|流程|函数|代码|\b(?:implementation|logic|function|code|flow)\b/iu.test(query)) roles.add('implementation');
  return roles;
}

function metadataTargetMatch(item, intent) {
  const itemPath = item.locator?.relativePath ?? item.locator?.path ?? item.title;
  const normalizedPath = typeof itemPath === 'string' ? normalizeRetrievalPath(itemPath) : '';
  const pathMatch = Boolean(intent.path && (normalizedPath === intent.path || normalizedPath.endsWith(`/${intent.path}`)));
  const symbols = [item.structure?.symbolName, item.structure?.qualifiedName];
  const symbolMatch = Boolean(intent.symbol && symbols.some(value => typeof value === 'string' &&
    (value === intent.symbol || value.endsWith(`.${intent.symbol}`))));
  return { pathMatch, symbolMatch };
}

function rangeOverlap(left, right) {
  if (left.sourceId !== right.sourceId || left.scopeKey !== right.scopeKey ||
      left.contentHash !== right.contentHash) return 0;
  const a = left.locator, b = right.locator;
  if (![a?.startOffset, a?.endOffset, b?.startOffset, b?.endOffset].every(Number.isSafeInteger)) return 0;
  const shortest = Math.min(a.endOffset - a.startOffset, b.endOffset - b.startOffset);
  return shortest > 0 ? Math.max(0, Math.min(a.endOffset, b.endOffset) - Math.max(a.startOffset, b.startOffset)) / shortest : 0;
}

function contextTexts(value) {
  if (typeof value === 'string') return [normalizedText(value)];
  if (Array.isArray(value)) return value.flatMap(contextTexts);
  if (!value || typeof value !== 'object') return [];
  return contextTexts(value.content ?? value.Content ?? value.text ?? '');
}

/** Remove exact/overlapping repeats while retaining distinct evidence from one source.
 * 删除同一分块、重叠区间及重复正文，单份来源仍可贡献多段不同证据。 */
export function deduplicateCandidates(items, { existingContext = [], retrievalIntent } = {}) {
  const seenChunks = new Set(), seenText = new Set(), kept = [];
  const existing = contextTexts(existingContext);
  let duplicateCount = 0, alreadyPresentCount = 0;
  // Verified symbol/file intersections precede optional scores and identical copies in other files.
  // 已核实的符号与文件交集优先于可选重排分数和其他文件中的同文副本。
  const priority = item => {
    const target = retrievalIntent ? metadataTargetMatch(item, retrievalIntent) : {};
    return Number(item.exactTargetMatch === true) * 4 + Number(target.pathMatch === true) + Number(target.symbolMatch === true);
  };
  const prioritized = items.some(item => priority(item) > 0)
    ? [...items].sort((left, right) => priority(right) - priority(left)) : items;
  for (const item of prioritized) {
    const text = normalizedText(item.excerpt);
    if (!text) continue;
    const chunkKey = `${item.scopeKey}:${item.sourceId}:${item.chunkId ?? item.sourceRef ?? text}`;
    if (seenChunks.has(chunkKey) || seenText.has(text) || kept.some(previous => rangeOverlap(previous, item) >= 0.75)) {
      duplicateCount++; continue;
    }
    seenChunks.add(chunkKey); seenText.add(text);
    if (text.length >= 8 && existing.some(content => content.includes(text))) {
      alreadyPresentCount++; continue;
    }
    kept.push(item);
  }
  return { items: kept, duplicateCount, alreadyPresentCount };
}

export function evidenceRecord(item, reference) {
  return projectRetrievalModelView({ reference, sourceRef: item.modelSourceRef ?? item.sourceRef, title: item.title, scope: item.scopeKey,
    locator: item.locator, ...(item.structure ? { structure: item.structure } : {}), excerpt: item.excerpt });
}

export function evidenceItemTokens(item, reference = 1) {
  return estimateTokens(JSON.stringify(evidenceRecord(item, reference))) + 1;
}

/** RRF and reranker scores order candidates; they never become answer probabilities.
 * RRF 和重排分数只用于排序，证据状态根据可观察的查询覆盖与来源事实判定，不解释为回答概率。 */
export function assessEvidence(items, query, { requiresSourceRead = false, alreadyPresentCount = 0, retrievalIntent } = {}) {
  if (!items.length) return { state: 'empty', reason: alreadyPresentCount ? 'already-in-context' : 'no-current-evidence',
    requiresSourceRead: false, matchedTerms: 0, queryTerms: informativeTerms(query).size };
  const queryTerms = informativeTerms(query), matched = new Set();
  let bodyMatch = false;
  for (const item of items) {
    const terms = informativeTerms(`${item.title ?? ''} ${item.excerpt ?? ''}`);
    const bodyTerms = informativeTerms(item.excerpt ?? '');
    for (const term of queryTerms) if (terms.has(term)) {
      matched.add(term);
      if (bodyTerms.has(term)) bodyMatch = true;
    }
  }
  const coverage = queryTerms.size ? matched.size / queryTerms.size : 0;
  const usable = bodyMatch && coverage >= 0.4;
  const semanticOnly = !bodyMatch && items.some(item => Number.isSafeInteger(item.vectorRank) && item.vectorRank > 0);
  const intent = retrievalIntent ?? buildRetrievalIntent(query);
  const expectedRoles = requestedRoles(query), presentRoles = new Set(items.map(candidateRole));
  const missingEvidence = [...expectedRoles].filter(role => !presentRoles.has(role)).map(role => ({ kind: 'requested-role', role }));
  if (intent.symbol && !items.some(item => metadataTargetMatch(item, intent).symbolMatch || item.exactTargetMatch))
    missingEvidence.push({ kind: 'symbol-definition', symbol: intent.symbol });
  if (intent.path && !items.some(item => metadataTargetMatch(item, intent).pathMatch))
    missingEvidence.push({ kind: 'requested-file', path: intent.path });
  // Role/target presence is only a retrieval gap signal; it does not certify facts or executed tests.
  // 角色和目标是否出现只用于指示检索缺口，不能证明事实正确或测试已执行。
  return { state: usable ? 'usable' : 'weak', reason: usable ? 'query-terms-supported' : semanticOnly ? 'semantic-only-unverified' : 'partial-query-support',
    requiresSourceRead: requiresSourceRead || !usable, matchedTerms: matched.size, queryTerms: queryTerms.size,
    queryCoverage: coverage, missingEvidence, presentRoles: [...presentRoles], sufficiency: 'not-evaluated' };
}

/** Select a bounded diverse projection; original excerpts, references and scores stay intact.
 * 按预算选择具有多样性的请求投影，原文、回源引用和原始分数保持不变。 */
export function selectCandidates(items, { query = '', limit = 8, maximumTokens = 8192,
  existingContext = [], lambda = 0.7, requiresSourceRead = false, retrievalIntent, taskContext } = {}) {
  if (!Number.isSafeInteger(limit) || limit < 1 || limit > 60) throw new RangeError('Invalid retrieval result limit.');
  if (!Number.isSafeInteger(maximumTokens) || maximumTokens < 0 || maximumTokens > MAX_SELECTION_TOKENS)
    throw new RangeError('Invalid evidence token budget.');
  const intent = retrievalIntent ?? buildRetrievalIntent(query, { taskContext });
  const deduplicated = deduplicateCandidates(items, { existingContext, retrievalIntent: intent });
  const queryTerms = informativeTerms(query), concepts = retrievalQueryConcepts(query, intent);
  const roles = requestedRoles(query);
  const candidates = deduplicated.items.map((item, index) => {
    const terms = informativeTerms(item.excerpt), metadataTerms = informativeTerms(metadataText(item));
    const target = metadataTargetMatch(item, intent), role = candidateRole(item);
    const rankRelevance = 1 - index / Math.max(1, deduplicated.items.length);
    const relevance = rankRelevance * 0.55 + queryCoverage(terms, queryTerms, concepts) * 0.3 +
      queryCoverage(metadataTerms, queryTerms, concepts) * 0.1 + Number(roles.has(role)) * 0.05;
    return { item, terms, role, target, relevance, similarity: 0 };
  });
  const selected = [];
  const sourceCounts = new Map(), selectedRoles = new Set();
  let usedTokens = 0, omittedForBudget = 0;
  const relevanceWeight = Math.max(0, Math.min(1, lambda));
  while (candidates.length && selected.length < limit) {
    let bestIndex = 0, bestScore = -Infinity;
    const hasExactTarget = candidates.some(candidate => candidate.item.exactTargetMatch === true);
    for (const [index, candidate] of candidates.entries()) {
      if (hasExactTarget && candidate.item.exactTargetMatch !== true) continue;
      const sourceKey = `${candidate.item.scopeKey}:${candidate.item.sourceId}`;
      const sourceCount = sourceCounts.get(sourceKey) ?? 0;
      const targetBoost = Number(candidate.target.pathMatch) * 0.3 + Number(candidate.target.symbolMatch) * 0.35;
      const diversityWeight = intent.path ? 0 : requiresSourceRead || roles.size > 1 ? 0.22 : 0.14;
      const filePenalty = diversityWeight * sourceCount / (sourceCount + 1);
      const roleBoost = roles.has(candidate.role) && !selectedRoles.has(candidate.role) ? 0.06 : 0;
      // Diversity is a soft cost, so essential distinct fragments from the same file remain eligible without a hard cap.
      // 多样性只施加软代价，同一文件的必要独立片段始终可选，不设置固定分块数量上限。
      const score = relevanceWeight * candidate.relevance + targetBoost + roleBoost - filePenalty -
        (1 - relevanceWeight) * candidate.similarity;
      if (score > bestScore) { bestIndex = index; bestScore = score; }
    }
    const [candidate] = candidates.splice(bestIndex, 1);
    const cost = evidenceItemTokens(candidate.item, selected.length + 1);
    if (usedTokens + cost > maximumTokens) { omittedForBudget++; continue; }
    selected.push(candidate.item); usedTokens += cost;
    const sourceKey = `${candidate.item.scopeKey}:${candidate.item.sourceId}`;
    sourceCounts.set(sourceKey, (sourceCounts.get(sourceKey) ?? 0) + 1);
    selectedRoles.add(candidate.role);
    for (const remaining of candidates)
      remaining.similarity = Math.max(remaining.similarity, tokenSimilarity(candidate.terms, remaining.terms));
  }
  return { items: selected, selection: { candidateCount: items.length, uniqueCandidates: deduplicated.items.length,
    duplicateCount: deduplicated.duplicateCount, alreadyPresentCount: deduplicated.alreadyPresentCount,
    omittedForBudget, usedTokens, maximumTokens, method: 'metadata-query-mmr', sourceCount: sourceCounts.size },
    evidenceAssessment: assessEvidence(selected, query, { requiresSourceRead, alreadyPresentCount: deduplicated.alreadyPresentCount, retrievalIntent: intent }) };
}

export const selectEvidenceCandidates = selectCandidates;

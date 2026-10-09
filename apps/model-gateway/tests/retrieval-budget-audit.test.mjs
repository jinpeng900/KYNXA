import assert from 'node:assert/strict';
import { test } from 'node:test';
import { retrievalBudget } from '../orchestration/retrieval/retrieval-budget.mjs';
import { selectCandidates } from '../orchestration/retrieval/candidate-selection.mjs';
import { projectEvidence } from '../orchestration/retrieval/source-projection.mjs';
import { RetrievalCoordinator } from '../orchestration/retrieval/coordinator.mjs';
import { estimateTokens } from '../models/context-tokens.mjs';

function candidate(id, excerpt) {
  return { sourceId: id, scopeKey: 'user', sourceRef: id, title: id, contentHash: id,
    excerpt, locator: { startOffset: 0, endOffset: excerpt.length } };
}

test('candidate, final-reference and evidence limits retain separate requested and approved audit values', () => {
  for (const candidateLimit of [64, 128]) {
    const budget = retrievalBudget({ taskType: 'research', limit: 5, maximumTokens: 2048,
      suggestions: { candidateLimit, fusedCandidateLimit: 128, evidenceBudgetTokens: 32768, rerankCandidates: 40 } });
    assert.equal(budget.channelCandidates, candidateLimit);
    assert.equal(budget.fusedCandidates, 128);
    assert.deepEqual(budget.audit.requested, { limit: 5, maximumTokens: 2048 });
    assert.equal(budget.audit.configured.maximumTokens, 16384);
    assert.equal(budget.audit.resourceSuggestions.evidenceBudgetTokens, 32768);
    assert.equal(budget.audit.approved.maximumTokens, 2048);
    assert.equal(budget.audit.approved.rerankCandidates, 40);
    assert.equal(budget.audit.downstreamLimits.finalReferences, 60);
  }
});

test('selection audits distinguish duplicate, token and fragment-limit losses', () => {
  const short = candidate('short', 'alpha current evidence'), repeat = { ...short };
  const oversized = candidate('oversized', 'alpha enormous text '.repeat(300));
  const remaining = candidate('remaining', 'alpha separate remaining evidence');
  const selected = selectCandidates([oversized, short, repeat, remaining],
    { query: 'alpha', limit: 1, maximumTokens: 200 });
  assert.equal(selected.items.length, 1);
  assert.equal(selected.selection.duplicateCount, 1);
  assert.equal(selected.selection.omittedForBudget, 1);
  assert.equal(selected.selection.omittedForLimit, 1);
  assert.deepEqual(new Set(selected.selection.earlyCutReasons.map(item => item.reason)),
    new Set(['duplicate-or-overlapping-evidence', 'evidence-token-budget', 'selected-fragment-limit']));
  assert.equal(selected.evidenceAssessment.sufficiency, 'not-evaluated');
});

test('projection skips oversized evidence and retains later complete records with contiguous display numbers', () => {
  const oversized = candidate('oversized', 'alpha '.repeat(1500)), concise = candidate('concise', 'alpha concise source');
  const projected = projectEvidence([oversized, concise], 2000, { maximumTokens: 2000 });
  assert.deepEqual(projected.items.map(item => item.sourceId), ['concise']);
  assert.equal(JSON.parse(projected.prompt.split('\n').at(-1)).reference, 1);
  assert.equal(JSON.parse(projected.prompt.split('\n').at(-1)).excerpt, concise.excerpt);
  assert.ok(estimateTokens(projected.prompt) <= 2000);
  assert.deepEqual(projected.audit.earlyCutReasons, [{ reason: 'evidence-character-budget', count: 1 }]);
  assert.equal(projected.audit.inputCount, 2);
  assert.equal(projected.audit.selectedCount, 1);
  const exhausted = projectEvidence([concise], 2000, { maximumTokens: 0 });
  assert.equal(exhausted.prompt, '');
  assert.equal(exhausted.audit.earlyCutReasons[0].reason, 'model-context-exhausted');
});

test('reranker limits 20, 40 and 60 never discard candidates omitted from a partial response', async () => {
  const candidates = Array.from({ length: 128 }, (_, index) => candidate(`item-${index}`, `alpha distinct evidence ${index}`));
  const snapshot = { settings: { local: { rerankProfileId: 'builtin-multilingual-reranker' } } };
  for (const limit of [20, 40, 60]) {
    const owner = { reranker: { status: () => ({ state: 'ready', profileId: 'builtin-multilingual-reranker' }),
      rerank: async input => {
        assert.equal(input.limit, limit);
        return { items: input.candidates.slice(0, limit).toReversed().map(item => ({ ...item, rerankScore: 0.8 })) };
      } } };
    const result = await RetrievalCoordinator.prototype._rerank.call(owner, {}, 'alpha', candidates, snapshot,
      'research', undefined, { shouldRerank: true }, limit);
    assert.equal(result.items.length, 128);
    assert.equal(result.rerank.scoredCandidates, limit);
    assert.equal(result.items[0].sourceId, `item-${limit - 1}`);
    assert.ok(result.items.some(item => item.sourceId === 'item-127'));
  }
});

import assert from 'node:assert/strict';
import { test } from 'node:test';
import { retrievalBudget } from '../orchestration/retrieval/retrieval-budget.mjs';
import { selectCandidates } from '../orchestration/retrieval/candidate-selection.mjs';
import { projectEvidence } from '../orchestration/retrieval/source-projection.mjs';
import { RetrievalCoordinator } from '../orchestration/retrieval/coordinator.mjs';
import { estimateTokens } from '../models/context-tokens.mjs';
import { randomUUID } from 'node:crypto';
import { join } from 'node:path';
import { toolFixture } from './tool-fixture.mjs';

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
    assert.equal(budget.audit.configured.maximumTokens, 32768);
    assert.equal(budget.audit.resourceSuggestions.evidenceBudgetTokens, 32768);
    assert.equal(budget.audit.approved.maximumTokens, 2048);
    assert.equal(budget.audit.approved.rerankCandidates, 40);
    assert.equal(budget.audit.downstreamLimits.finalReferences, 60);
  }
});

test('resource reductions are retained while task defaults separate larger recall from final references', () => {
  const lookup = retrievalBudget(), research = retrievalBudget({ taskType: 'research' });
  assert.equal(lookup.channelCandidates, 64);
  assert.equal(lookup.fusedCandidates, 96);
  assert.equal(lookup.limit, 16);
  assert.equal(research.channelCandidates, 128);
  assert.equal(research.fusedCandidates, 160);
  assert.equal(research.limit, 40);
  const reduced = retrievalBudget({ taskType: 'research', suggestions: {
    candidateLimit: 32, fusedCandidateLimit: 48, evidenceBudgetTokens: 512 } });
  assert.equal(reduced.channelCandidates, 32);
  assert.equal(reduced.fusedCandidates, 48);
  assert.equal(reduced.maximumTokens, 512);
  const pressure = retrievalBudget({ taskType: 'research', suggestions: { candidateLimit: 16, fusedCandidateLimit: 19 } });
  assert.equal(pressure.channelCandidates, 16);
  assert.equal(pressure.fusedCandidates, 19);
  const ordinaryIdle = retrievalBudget({ suggestions: { candidateLimit: 160, fusedCandidateLimit: 192 } });
  assert.equal(ordinaryIdle.channelCandidates, 96);
  assert.equal(ordinaryIdle.fusedCandidates, 128);
});

test('small projections retain a legal navigation handle without inventing an excerpt or support', () => {
  const original = candidate('guide', 'alpha recovery conditions '.repeat(1000));
  const selected = selectCandidates([original], { query: 'alpha recovery', maximumTokens: 442 });
  assert.equal(selected.items.length, 1);
  assert.equal(selected.items[0].navigationOnly, true);
  assert.equal(selected.items[0].excerpt, '');
  assert.equal(selected.items[0].sourceRef, original.sourceRef);
  assert.equal(selected.evidenceAssessment.reason, 'navigation-only');
  const projected = projectEvidence(selected.items, 2000, { maximumTokens: 442, assessment: selected.evidenceAssessment });
  assert.equal(projected.items.length, 1);
  assert.ok(projected.usedTokens <= 442);
  const record = JSON.parse(projected.prompt.split('\n').at(-1));
  assert.equal(record.navigationOnly, true);
  assert.equal(record.next, 'knowledge.read');
  assert.equal(record.evidenceSupport, 'not-read');
});

test('a small real-coordinator projection publishes a current, authorized source-read navigation', async t => {
  const f = await toolFixture(t), retrieval = new RetrievalCoordinator({ conversations: f.conversations,
    memory: { contextFor: async () => ({ entries: [] }) }, tools: f.service,
    embeddings: { status: () => ({ state: 'unavailable' }), close: async () => {} } });
  f.service.retrieval = retrieval; t.after(() => retrieval.close());
  await retrieval.settings.patchGlobal({ expectedRevision: 0, patch: { local: { semantic: 'off' } } });
  const text = 'alpha recovery requires complete backup validation and authorized access. '.repeat(100);
  await retrieval.library.add([{ path: join(f.workspace, 'recovery.md'), title: 'alpha recovery', text }], { scope: 'user' });
  const requestId = randomUUID(), context = { conversationId: f.conversationId, projectId: f.projectId, requestId };
  const evidence = await retrieval.evidence(context, '根据资料解释 alpha recovery', { maximumTokens: 512 });
  assert.ok(evidence.references.length);
  assert.ok(evidence.resultRef);
  assert.ok(estimateTokens(evidence.prompt) <= 512);
  const record = JSON.parse(evidence.prompt.split('\n').at(-1));
  assert.equal(record.navigationOnly, true);
  assert.equal(record.excerpt, '');
  assert.equal(record.next, 'knowledge.read');
  assert.equal(evidence.evidenceAssessment.requiresSourceRead, true);
  await f.conversations.upsertMessage(f.conversationId, { Id: requestId, Role: 'assistant', Status: 'completed',
    Content: 'Source-read navigation only.', RetrievalResultRef: evidence.resultRef });
  const read = await retrieval.read(context, { sourceRef: record.sourceRef, limit: 8000 });
  assert.ok(JSON.stringify(read).includes('complete backup validation'));
  const toolContext = await f.context('full');
  await f.service.catalog(toolContext);
  f.service.setEvidenceBudget(toolContext, 512);
  const receipt = await f.run(toolContext, 'knowledge.search', { query: 'alpha recovery', maximumTokens: 32768 });
  assert.equal(receipt.isError, false, receipt.content);
  const archived = await f.service.results.get(toolContext, receipt.resultRef.id);
  const audit = archived.structuredContent.budget.audit;
  assert.equal(audit.modelRequested.maximumTokens, 32768);
  assert.equal(audit.availableContextTokens, 512);
  assert.equal(audit.approved.maximumTokens, 512);
  assert.ok(audit.earlyCutReasons.some(item => item.reason === 'remaining-context' && item.proposed === 32768));
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

import assert from 'node:assert/strict';
import { test } from 'node:test';
import { EvidenceAcquisition } from '../orchestration/retrieval/evidence-acquisition.mjs';

const item = (revision = 1, offset = 0) => ({ sourceId: 'synthetic-source', scopeKey: 'user', sourceRevision: revision,
  contentHash: String(revision).padStart(64, '0'), chunkId: `chunk-${offset}`, locator: { startOffset: offset, endOffset: offset + 200 } });

test('relevance, new observations and exhausted budgets never assert semantic sufficiency', () => {
  const plan = new EvidenceAcquisition(), ticket = plan.prepare({ query: 'ORION 2031', snapshotKey: 'snapshot', cacheKey: 'query' });
  const status = plan.observe(ticket, { items: [item()], evidenceAssessment: { state: 'usable' } });
  assert.equal(status.sufficiency, 'not-evaluated');
  assert.equal(status.newEvidenceCount, 1);
  assert.equal(status.explicitGap, false);
  assert.equal(plan.status(ticket, { blocked: true }).sufficiency, 'not-evaluated');
  assert.equal(plan.status(ticket, { blocked: true }).executed, false);
});

test('a repeated query can reuse evidence only within the same versioned snapshot and projection', () => {
  const plan = new EvidenceAcquisition();
  const first = plan.prepare({ query: 'reviewer', gap: 'Who reviewed ORION?', snapshotKey: 'v1', cacheKey: 'q1' });
  plan.observe(first, { items: [item()] });
  const repeated = plan.prepare({ query: 'reviewer', gap: 'Who reviewed ORION?', snapshotKey: 'v1', cacheKey: 'q1' });
  assert.ok(repeated.cached);
  assert.equal(plan.observe(repeated, repeated.cached, { reused: true }).newEvidenceCount, 0);
  assert.equal(plan.searches, 1);
  assert.equal(plan.prepare({ query: 'reviewer', snapshotKey: 'v2', cacheKey: 'q1' }).cached, undefined);
  assert.equal(plan.prepare({ query: 'reviewer', snapshotKey: 'v1', cacheKey: 'smaller-budget' }).cached, undefined);
});

test('new chapters and changed source versions remain new observations', () => {
  const plan = new EvidenceAcquisition();
  const first = plan.prepare({ query: 'constraints', snapshotKey: 'v1', cacheKey: 'q1' });
  plan.observe(first, { items: [item()] });
  const next = plan.prepare({ query: 'constraints details', snapshotKey: 'v1', cacheKey: 'q2' });
  assert.equal(plan.observe(next, { items: [item(1, 200), item(2)] }).newEvidenceCount, 2);
});

test('empty evidence is not reused or called a resolved gap; a research task retains larger budgets', () => {
  const standard = new EvidenceAcquisition(), research = new EvidenceAcquisition({ research: true });
  const input = { query: 'unknown', gap: 'NOVA launch date', snapshotKey: 'v1', cacheKey: 'empty' };
  for (let index = 0; index < standard.maximumSearchesPerGap; index++) {
    const ticket = standard.prepare(input);
    assert.equal(ticket.cached, undefined);
    assert.equal(standard.observe(ticket, { items: [] }).state, 'no-evidence');
  }
  assert.equal(standard.prepare(input).blocked, true);
  assert.ok(research.maximumSearches > standard.maximumSearches);
  assert.ok(research.maximumSearchesPerGap > standard.maximumSearchesPerGap);
  const newGap = standard.prepare({ ...input, gap: 'NOVA cancellation status', cacheKey: 'new-query' });
  assert.equal(newGap.blocked, false);
});

test('a corrupt or revoked cache is invalidated without treating its evidence as current', () => {
  const plan = new EvidenceAcquisition(), input = { query: 'ORION', snapshotKey: 'v1', cacheKey: 'query' };
  const ticket = plan.prepare(input);
  plan.observe(ticket, { items: [item()] });
  const repeated = plan.prepare(input);
  plan.invalidate(repeated);
  assert.equal(repeated.cached, undefined);
  assert.equal(plan.prepare(input).cached, undefined);
  assert.throws(() => plan.prepare({ ...input, gap: '' }), /missing fact/);
});

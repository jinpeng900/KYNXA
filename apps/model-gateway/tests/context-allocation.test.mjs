import assert from 'node:assert/strict';
import { test } from 'node:test';
import { contextAllocation, contextAllocationAudit } from '../orchestration/retrieval/context-allocation.mjs';

test('evidence-heavy tasks receive larger context shares while preserving output and history space', () => {
  const lookup = contextAllocation(8192, 'lookup'), file = contextAllocation(8192, 'file'), research = contextAllocation(8192, 'research');
  assert.ok(lookup.requestedEvidenceTokens >= 1024);
  assert.ok(file.requestedEvidenceTokens > lookup.requestedEvidenceTokens);
  assert.ok(research.requestedEvidenceTokens > file.requestedEvidenceTokens);
  assert.ok(research.schemaCeilingTokens < file.schemaCeilingTokens);
  for (const allocation of [lookup, file, research]) assert.ok(allocation.schemaCeilingTokens + allocation.requestedEvidenceTokens < 8192);
});

test('configured, approved, used and clipped evidence are distinct and unused schemas are observable', () => {
  const audit = contextAllocationAudit(contextAllocation(8192, 'research'), { configuredEvidenceTokens: 16384,
    finalEvidenceBudgetTokens: 1600, actualEvidenceTokens: 1200, actualSchemaTokens: 600 });
  assert.equal(audit.approvedEvidenceTokens, 1600);
  assert.equal(audit.actualEvidenceTokens, 1200);
  assert.equal(audit.unusedSchemaTokens, 1038);
  assert.deepEqual(audit.earlyCutReasons, ['task-context-share', 'remaining-context', 'available-selected-evidence']);
  assert.equal(contextAllocation(1_048_576, 'research').requestedEvidenceTokens, 32768);
});

test('a readable small-window allowance is approved only after mandatory input and safety fit', () => {
  const small = contextAllocation(3686, 'lookup', { mandatoryInputTokens: 400 });
  assert.ok(small.requestedEvidenceTokens >= 1024);
  assert.equal(small.approvedEvidenceTokens, small.requestedEvidenceTokens);
  const crowded = contextAllocation(3686, 'lookup', { mandatoryInputTokens: 3000 });
  assert.equal(crowded.approvedEvidenceTokens, 174);
  assert.ok(crowded.approvedEvidenceTokens + 3000 + crowded.safetyTokens <= 3686);
  assert.equal(contextAllocation(3686, 'lookup', { mandatoryInputTokens: 3500 }).approvedEvidenceTokens, 0);
});

test('non-retrieval requests do not claim an approved evidence allowance or a context clipping error', () => {
  const allocation = { ...contextAllocation(8192), evidenceRequested: false };
  const audit = contextAllocationAudit(allocation);
  assert.equal(audit.approvedEvidenceTokens, 0);
  assert.deepEqual(audit.earlyCutReasons, ['retrieval-not-requested']);
});

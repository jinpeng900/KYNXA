import assert from 'node:assert/strict';
import { test } from 'node:test';
import { contextAllocation, contextAllocationAudit } from '../orchestration/retrieval/context-allocation.mjs';

test('evidence-heavy tasks receive larger context shares while preserving output and history space', () => {
  const lookup = contextAllocation(8192, 'lookup'), file = contextAllocation(8192, 'file'), research = contextAllocation(8192, 'research');
  assert.equal(lookup.requestedEvidenceTokens, 983);
  assert.equal(file.requestedEvidenceTokens, 1474);
  assert.equal(research.requestedEvidenceTokens, 1966);
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
  assert.equal(contextAllocation(1_048_576, 'research').requestedEvidenceTokens, 16384);
});

test('non-retrieval requests do not claim an approved evidence allowance or a context clipping error', () => {
  const allocation = { ...contextAllocation(8192), evidenceRequested: false };
  const audit = contextAllocationAudit(allocation);
  assert.equal(audit.approvedEvidenceTokens, 0);
  assert.deepEqual(audit.earlyCutReasons, ['retrieval-not-requested']);
});

import assert from 'node:assert/strict';
import { test } from 'node:test';
import { planExternalModelDemand } from '../models/external-model-demand.mjs';

const loaded = { backend: 'ollama', loaded: true, runtimeContextTokens: 8192,
  modelMaximumContextTokens: 131072, estimatedWeightBytes: 5_000_000_000, kvBytesPerToken: 131072,
  observedMemoryBytes: 7_000_000_000, observedGpuMemoryBytes: 6_000_000_000, observedAt: 1234 };

test('loaded context does not reserve already observed weights, KV or a constant uncertainty twice', () => {
  for (const contextTokens of [4096, 8192]) {
    const plan = planExternalModelDemand(loaded, { contextTokens });
    assert.equal(plan.requiresAdmission, false);
    assert.equal(plan.memoryBytes, 0);
    assert.equal(plan.gpuMemoryBytes, 0);
    assert.equal(plan.uncertainty.marginBytes, 0);
    assert.equal(plan.breakdown.observedResidencyReservedAgain, false);
  }
});

test('loaded GPU model reserves only the new KV range plus a bounded scratch margin', () => {
  const plan = planExternalModelDemand(loaded, { contextTokens: 16384 });
  assert.equal(plan.requiresAdmission, true);
  assert.equal(plan.breakdown.weightIncrementBytes, 0);
  assert.equal(plan.breakdown.kvIncrementBytes, 8192 * loaded.kvBytesPerToken);
  assert.ok(plan.gpuMemoryBytes < loaded.observedGpuMemoryBytes);
  assert.equal(plan.gpuMemoryBytes, plan.breakdown.kvIncrementBytes + plan.uncertainty.marginBytes);
  assert.equal(plan.uncertainty.state, 'estimated-upper-bound');
  assert.ok(plan.uncertainty.reasons.includes('KV_OFFLOAD_DISTRIBUTION_UNKNOWN'));
});

test('verified CPU residency growth does not fabricate a GPU requirement', () => {
  const plan = planExternalModelDemand({ ...loaded, observedGpuMemoryBytes: 0 }, { contextTokens: 16384 });
  assert.ok(plan.memoryBytes > 0);
  assert.equal(plan.gpuMemoryBytes, 0);
  assert.equal(plan.breakdown.placement, 'cpu');
});

test('initial cold load includes estimated weights and KV but labels unknown offload as an upper bound', () => {
  const plan = planExternalModelDemand({ ...loaded, loaded: false, runtimeContextTokens: null,
    observedGpuMemoryBytes: null, observedMemoryBytes: null },
  { contextTokens: 32768, hardware: { gpu: { state: 'available' } } });
  assert.equal(plan.breakdown.weightIncrementBytes, loaded.estimatedWeightBytes);
  assert.equal(plan.breakdown.kvIncrementBytes, 32768 * loaded.kvBytesPerToken);
  assert.equal(plan.memoryBytes, plan.gpuMemoryBytes);
  assert.equal(plan.breakdown.memoryIncludesTransientStaging, true);
  assert.ok(plan.uncertainty.reasons.includes('INITIAL_OFFLOAD_DISTRIBUTION_UNKNOWN'));
});

test('unknown server-selected context still protects pending cold-load weights without claiming known KV', () => {
  const plan = planExternalModelDemand({ ...loaded, loaded: false, runtimeContextTokens: null,
    configuredContextTokens: null, observedGpuMemoryBytes: null, observedMemoryBytes: null },
  { hardware: { gpu: { state: 'available' } } });
  assert.equal(plan.state, 'partial');
  assert.equal(plan.requiresAdmission, true);
  assert.equal(plan.partialCoverage, true);
  assert.deepEqual(plan.unknownComponents, ['kv-cache']);
  assert.equal(plan.breakdown.weightIncrementBytes, loaded.estimatedWeightBytes);
  assert.equal(plan.breakdown.kvIncrementBytes, null);
  assert.equal(plan.breakdown.targetContextTokens, null);
  assert.equal(plan.breakdown.kvEstimateDtype, null);
  assert.equal(plan.gpuMemoryBytes, loaded.estimatedWeightBytes + plan.uncertainty.marginBytes);
  assert.ok(plan.uncertainty.reasons.includes('CONTEXT_TARGET_UNKNOWN'));
});

test('a partial CPU weight forecast remains partial and cannot reserve already loaded unknown-context weights', () => {
  const cold = planExternalModelDemand({ ...loaded, loaded: false, runtimeContextTokens: null,
    kvBytesPerToken: null }, { hardware: { gpu: { state: 'unavailable' } } });
  assert.equal(cold.state, 'partial');
  assert.equal(cold.gpuMemoryBytes, 0);
  assert.ok(cold.memoryBytes > loaded.estimatedWeightBytes);
  assert.equal(cold.breakdown.kvIncrementBytes, null);
  const warm = planExternalModelDemand(loaded, { hardware: { gpu: { state: 'available' } } });
  assert.equal(warm.state, 'unknown');
  assert.equal(warm.requiresAdmission, false);
  assert.equal(warm.memoryBytes, null);
});

test('missing load, baseline, KV, weights or GPU placement remains unknown and cannot claim admission', () => {
  const variants = [
    { ...loaded, loaded: null }, { ...loaded, runtimeContextTokens: null, configuredContextTokens: 8192 },
    { ...loaded, kvBytesPerToken: null }, { ...loaded, observedGpuMemoryBytes: null },
    { ...loaded, loaded: false, estimatedWeightBytes: null }, { ...loaded, loaded: false },
  ];
  for (const snapshot of variants) {
    const plan = planExternalModelDemand(snapshot, { contextTokens: 16384 });
    assert.equal(plan.state, 'unknown');
    assert.equal(plan.requiresAdmission, false);
    assert.equal(plan.memoryBytes, null);
    assert.equal(plan.gpuMemoryBytes, null);
  }
});

test('target is capped by verified model context while malicious demands cannot overflow resource requests', () => {
  const capped = planExternalModelDemand(loaded, { contextTokens: 262144 });
  assert.equal(capped.breakdown.targetContextTokens, 131072);
  for (const contextTokens of [-1, 1.5, Number.MAX_SAFE_INTEGER, Infinity]) {
    assert.equal(planExternalModelDemand(loaded, { contextTokens }).state, 'unknown');
  }
  const excessive = planExternalModelDemand({ ...loaded, kvBytesPerToken: 8 * 1024 ** 4 }, { contextTokens: 16384 });
  assert.equal(excessive.state, 'unknown');
  assert.equal(excessive.uncertainty.reasons[0], 'EXTERNAL_MODEL_DEMAND_EXCEEDS_BOUND');
});

test('a runtime without resource APIs retains an explicitly unsupported observation rather than a zero estimate', () => {
  const plan = planExternalModelDemand({ backend: 'external-openai', loaded: null }, { contextTokens: 16384 });
  assert.equal(plan.state, 'not-applicable');
  assert.equal(plan.memoryBytes, null);
});

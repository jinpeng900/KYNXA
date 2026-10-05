import test from 'node:test';
import assert from 'node:assert/strict';
import { pairedDifferenceBootstrap } from './compare-peer-scifact.mjs';

test('identical paired runs have an exactly zero interval', () => {
  const result = pairedDifferenceBootstrap([0, 0, 0], { resamples: 100 });
  assert.equal(result.meanDifference, 0);
  assert.deepEqual(result.percentile95Interval, [0, 0]);
  assert.deepEqual([result.wins, result.losses, result.ties], [0, 0, 3]);
});

test('a constant paired effect keeps the exact known interval without adding independent repeats', () => {
  const result = pairedDifferenceBootstrap([.5, .5, .5, .5], { resamples: 100 });
  assert.equal(result.queries, 4);
  assert.equal(result.meanDifference, .5);
  assert.deepEqual(result.percentile95Interval, [.5, .5]);
  assert.equal(result.wins, 4);
});

test('mixed paired differences are deterministic and expose both improvements and regressions', () => {
  const differences = [-1, 0, 1, .5];
  const result = pairedDifferenceBootstrap(differences, { seed: 12, resamples: 500 });
  assert.deepEqual(result, pairedDifferenceBootstrap(differences, { seed: 12, resamples: 500 }));
  assert.equal(result.meanDifference, .125);
  assert.ok(result.percentile95Interval[0] < 0 && result.percentile95Interval[1] > 0);
  assert.deepEqual([result.wins, result.losses, result.ties], [2, 1, 1]);
  assert.throws(() => pairedDifferenceBootstrap([]));
  assert.throws(() => pairedDifferenceBootstrap([NaN]));
});

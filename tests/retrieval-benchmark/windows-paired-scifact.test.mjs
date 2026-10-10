import assert from 'node:assert/strict';
import test from 'node:test';
import { freezeQuerySelection, validateCorpusIdentity } from './run-windows-paired-scifact.mjs';

test('paired selection preserves original twenty and freezes eighty without overlap', () => {
  const eligible = Array.from({ length: 300 }, (_, index) => String(index + 1));
  const original = eligible.slice(20, 40);
  const selected = freezeQuerySelection(original, eligible);
  assert.deepEqual(selected.originalQueryIds, original);
  assert.equal(selected.addedQueryIds.length, 80);
  assert.ok(selected.addedQueryIds.every(id => !original.includes(id)));
  assert.deepEqual(selected, freezeQuerySelection(original, [...eligible].reverse()));
  assert.throws(() => freezeQuerySelection(original.slice(0, 19), eligible), /Original twenty/u);
});

test('full corpus cannot silently substitute another chunk identity', () => {
  const manifest = { sample: { documentCount: 5183, chunkCount: 22858 },
    implementation: { dimensions: 384, actualLocalVectors: true } };
  assert.throws(() => validateCorpusIdentity(manifest, { sources: 5183, chunks: 22858 }), /different chunk corpus/u);
  assert.doesNotThrow(() => validateCorpusIdentity(manifest, { sources: 5183, chunks: 22858 }, 22858));
  assert.throws(() => validateCorpusIdentity({ ...manifest, sample: { documentCount: 500, chunkCount: 22858 } },
    { sources: 500, chunks: 22858 }, 22858), /Reduced corpus/u);
});

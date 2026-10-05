import test from 'node:test';
import assert from 'node:assert/strict';
import { hashText } from '../../apps/model-gateway/data/retrieval/retrieval-contracts.mjs';
import { chunkSource } from '../../apps/model-gateway/data/retrieval/retrieval-text.mjs';
import { meanPeerMetrics, peerDocumentMetrics, peerEvidenceProjection, peerChunkFromRow,
  verifyPeerSource } from './peer-scifact-metrics.mjs';

const archiveId = '10000000-0000-4000-8000-000000000001';

function publicRows() {
  const document = { _id: '12345', title: 'ORION launch report', text: 'ORION starts on 14 November 2031. '.repeat(45) };
  const text = document.title + '\n\n' + document.text;
  const source = { source_id: 'beir:scifact:12345', scope_key: 'user', source_type: 'public-benchmark',
    title: document.title, text, content_hash: hashText(text), source_revision: JSON.stringify(hashText(text)),
    locator: JSON.stringify({ beirDocumentId: document._id }) };
  const chunks = chunkSource({ sourceId: source.source_id, scopeKey: 'user', sourceType: 'public-benchmark',
    title: document.title, text, contentHash: source.content_hash, sourceRevision: source.content_hash,
    locator: { beirDocumentId: document._id } });
  const rows = chunks.map(chunk => ({ source_id: source.source_id, chunk_id: chunk.chunkId,
    chunk_index: chunk.chunkIndex, text: chunk.text, chunk_hash: chunk.chunkHash, start_offset: chunk.startOffset,
    end_offset: chunk.endOffset, start_line: chunk.startLine, end_line: chunk.endLine }));
  return { document, source, rows };
}

test('document metrics exclude the query ID before cutoff, collapse chunks and keep TREC denominators', () => {
  const relevance = new Map([['a', 1], ['b', 1], ['c', 1]]);
  const result = peerDocumentMetrics(['query', 'noise', 'a', 'a', 'b'], relevance, { queryId: 'query', cutoff: 3 });
  assert.equal(result.recall, 2 / 3);
  assert.equal(result.precision, 2 / 3);
  assert.equal(result.mrr, 1 / 2);
  assert.equal(result.map, (1 / 2 + 2 / 3) / 3);
  assert.ok(Math.abs(result.ndcg - ((1 / Math.log2(3) + .5) / (1 + 1 / Math.log2(3) + .5))) < 1e-12);
  const legacy = peerDocumentMetrics(['query', 'noise', 'a', 'a', 'b'], relevance,
    { queryId: 'query', cutoff: 3, ignoreIdenticalIds: false });
  assert.equal(legacy.recall, 1 / 3);
});

test('linear graded nDCG and empty rankings match independent manual values', () => {
  const relevance = new Map([['a', 3], ['b', 1]]);
  const result = peerDocumentMetrics(['b', 'a'], relevance, { queryId: 'q', cutoff: 10 });
  assert.ok(Math.abs(result.ndcg - ((1 + 3 / Math.log2(3)) / (3 + 1 / Math.log2(3)))) < 1e-12);
  assert.equal(result.precision, .2);
  assert.deepEqual(peerDocumentMetrics([], relevance, { queryId: 'q' }),
    { ndcg: 0, recall: 0, mrr: 0, map: 0, precision: 0, hit: 0 });
  assert.equal(meanPeerMetrics([result, result]).ndcg, result.ndcg);
  assert.throws(() => peerDocumentMetrics(['a'], new Map(), { queryId: 'q' }));
});

test('full source and chunk SHA-256 receipts reject changed original text or offset', () => {
  const { document, source, rows } = publicRows();
  assert.equal(verifyPeerSource(source, document).contentHash, hashText(source.text));
  assert.throws(() => verifyPeerSource({ ...source, text: source.text + ' altered' }, document));
  assert.throws(() => verifyPeerSource({ ...source, source_type: 'message' }, document));
  const item = peerChunkFromRow(rows[0], source);
  assert.equal(item.excerpt, rows[0].text);
  assert.throws(() => peerChunkFromRow({ ...rows[0], start_offset: rows[0].start_offset + 1 }, source));
  assert.throws(() => peerChunkFromRow({ ...rows[0], chunk_hash: '0'.repeat(64) }, source));
});

test('actual compact projection stays within all budgets and retains canonical hash provenance', () => {
  const { source, rows } = publicRows();
  const candidates = rows.map(row => peerChunkFromRow(row, source));
  const relevance = new Map([['12345', 1]]);
  for (const maximumTokens of [2048, 4096, 8192]) {
    const result = peerEvidenceProjection(candidates, 'When does ORION start?', relevance, maximumTokens,
      { queryId: 'query', archiveId });
    assert.equal(result.metrics.recall, 1);
    assert.ok(result.references.length <= 6 && result.references.length > 0);
    assert.ok(result.usedTokens <= maximumTokens);
    for (const reference of result.references) {
      assert.equal(reference.modelSourceRef.length, 29);
      assert.ok(reference.sourceRef.startsWith('rag1:'));
      assert.equal(reference.contentHash, source.content_hash);
      assert.equal(reference.chunkHash, reference.excerptSha256);
    }
  }
  assert.ok(candidates.every(item => item.modelSourceRef === undefined), 'Projection must not mutate canonical candidates.');
});

test('self-document exclusion and empty evidence cannot fabricate coverage or references', () => {
  const { source, rows } = publicRows();
  const candidates = rows.map(row => peerChunkFromRow(row, source));
  const relevance = new Map([['12345', 1]]);
  const result = peerEvidenceProjection(candidates, 'ORION?', relevance, 2048, { queryId: '12345', archiveId });
  assert.equal(result.metrics.recall, 0);
  assert.equal(result.metrics.selectedChunks, 0);
  assert.deepEqual(result.references, []);
});

import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFile, writeFile } from 'node:fs/promises';
import { DatabaseSync } from 'node:sqlite';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseSourceReference } from '../../apps/model-gateway/data/retrieval/retrieval-contracts.mjs';
import { parseEvidenceSourceRef } from '../../apps/model-gateway/data/retrieval/evidence-references.mjs';

const REPOSITORY_ROOT = fileURLToPath(new URL('../../', import.meta.url));
const QRELS_SHA256 = '0864bb985e0ca2367ba217977e72004d549054b2b06666ed9d4825ac7c21284c';
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');
const closeEnough = (actual, expected) => assert.ok(Math.abs(actual - expected) < 1e-12, `${actual} != ${expected}`);
const mean = values => values.reduce((sum, value) => sum + value, 0) / values.length;

/** Recalculate the recorded full run independently, without importing the scorer or projection helper.
 * 独立重算完整原始记录，不导入计分实现或投影 helper，避免用同一实现证明自己。 */
async function main() {
  assert.equal(process.argv[2], '--input');
  assert.ok(process.argv[3] && process.argv.length === 4, 'Use --input with one completed peer run.');
  const input = resolve(process.argv[3]);
  const manifestBytes = await readFile(join(input, 'manifest.json'));
  const manifest = JSON.parse(manifestBytes);
  const results = JSON.parse(await readFile(join(input, 'results.json'), 'utf8'));
  const rankingBytes = await readFile(join(input, 'rankings.jsonl'));
  const rankings = rankingBytes.toString('utf8').trim().split(/\r?\n/u).map(JSON.parse);
  const samples = JSON.parse(await readFile(join(input, 'latency-samples.json'), 'utf8'));
  const qrelsBytes = await readFile(join(REPOSITORY_ROOT, 'artifacts/verification/rag-benchmark/datasets/scifact/qrels/test.tsv'));
  assert.equal(sha256(qrelsBytes), QRELS_SHA256);
  assert.equal(results.sample.queries, 300);
  assert.equal(results.sample.documents, 5183);
  assert.equal(rankings.length, 300 * results.sample.repeats);
  const qrels = new Map();
  for (const line of qrelsBytes.toString('utf8').trim().split(/\r?\n/u).slice(1)) {
    const [queryId, documentId, grade] = line.split('\t');
    if (!qrels.has(queryId)) qrels.set(queryId, new Map());
    qrels.get(queryId).set(documentId, Number(grade));
  }
  assert.deepEqual(new Set(rankings.map(record => record.queryId)), new Set(qrels.keys()));
  assert.equal(sha256(JSON.stringify(manifest.sourceReceipts)), manifest.sourceReceiptsSha256);
  assert.equal(manifest.sourceReceipts.length, 5183);
  const sourceReceipts = new Map(manifest.sourceReceipts.map(item => [item.sourceId, item]));
  const database = new DatabaseSync(join(input, 'public-index/Index/retrieval.sqlite'), { readOnly: true });
  database.exec('PRAGMA query_only=ON');
  const chunk = database.prepare('SELECT c.chunk_hash,c.text,s.content_hash,s.source_revision FROM chunks c JOIN sources s ON s.source_id=c.source_id WHERE c.chunk_id=? AND c.source_id=?');
  let checkedValues = 0, verifiedShortReferences = 0;
  const evidenceRows = {}, documentRows = {};
  try {
    for (const record of rankings) for (const [name, method] of Object.entries(record.methods)) {
      const relevance = qrels.get(record.queryId), relevantCount = [...relevance.values()].filter(grade => grade > 0).length;
      const ordered = [...new Set(method.documents)].filter(id => id !== record.queryId).slice(0, 10);
      const matched = ordered.map((id, index) => ({ position: index + 1, grade: relevance.get(id) ?? 0 })).filter(item => item.grade > 0);
      const ideal = [...relevance.values()].filter(grade => grade > 0).sort((a, b) => b - a).slice(0, 10);
      const dcg = matched.reduce((sum, item) => sum + item.grade / Math.log2(item.position + 1), 0);
      const idcg = ideal.reduce((sum, grade, index) => sum + grade / Math.log2(index + 2), 0);
      const metrics = { ndcg: dcg / idcg, recall: matched.length / relevantCount,
        mrr: matched.length ? 1 / matched[0].position : 0,
        map: matched.reduce((sum, item, index) => sum + (index + 1) / item.position, 0) / relevantCount,
        precision: matched.length / 10, hit: matched.length ? 1 : 0 };
      for (const [key, value] of Object.entries(metrics)) {
        closeEnough(method.documentAt10[key], value);
        checkedValues++;
      }
      (documentRows[name] ??= []).push(metrics);
      for (const [budget, projection] of Object.entries(method.evidenceBudget)) {
        assert.ok(projection.usedTokens <= Number(budget));
        assert.ok(projection.usedCharacters <= 10000);
        assert.ok(projection.references.length <= 6);
        const identities = new Set(projection.references.map(item => item.documentId));
        const hits = [...identities].filter(id => (relevance.get(id) ?? 0) > 0);
        const relevantChunks = projection.references.filter(item => (relevance.get(item.documentId) ?? 0) > 0).length;
        const expected = { recall: hits.length / relevantCount, hit: hits.length ? 1 : 0,
          selectedChunks: projection.references.length, uniqueDocuments: identities.size,
          repeatedDocumentSlots: projection.references.length - identities.size, relevantChunks,
          chunkPrecision: projection.references.length ? relevantChunks / projection.references.length : 0 };
        for (const [key, value] of Object.entries(expected)) {
          closeEnough(projection.metrics[key], value);
          checkedValues++;
        }
        for (const reference of projection.references) {
          assert.equal(reference.modelSourceRef.length, 29);
          parseEvidenceSourceRef(reference.modelSourceRef);
          const descriptor = parseSourceReference(reference.sourceRef);
          assert.equal(descriptor.contentHash, sourceReceipts.get(descriptor.sourceId)?.contentHash);
          assert.equal(descriptor.contentHash, reference.contentHash);
          assert.equal(descriptor.chunkHash, reference.chunkHash);
          assert.equal(descriptor.chunkId, reference.chunkId);
          const actual = chunk.get(descriptor.chunkId, descriptor.sourceId);
          assert.equal(actual?.chunk_hash, reference.chunkHash);
          assert.equal(sha256(actual.text), reference.excerptSha256);
          assert.equal(actual.content_hash, reference.contentHash);
          assert.equal(JSON.parse(actual.source_revision), descriptor.sourceRevision);
          verifiedShortReferences++;
        }
        ((evidenceRows[name] ??= {})[budget] ??= []).push({ ...expected, meanUsedTokens: projection.usedTokens });
      }
    }
    for (const [name, method] of Object.entries(results.methods)) {
      for (const key of Object.keys(documentRows[name][0])) {
        closeEnough(method.documentAt10[key], mean(documentRows[name].map(item => item[key])));
        checkedValues++;
      }
      for (const [budget, values] of Object.entries(evidenceRows[name])) for (const key of Object.keys(values[0])) {
        closeEnough(method.evidenceBudget[budget][key], mean(values.map(item => item[key])));
        checkedValues++;
      }
      if (!method.latency) continue;
      for (const [kind, sampleName] of [['retrieval', 'retrieval'], ['endToEnd', 'endToEnd'], ['projectionAllBudgets', 'projection'], ['cpu', 'cpuMs']]) {
        const values = samples.methods[name][sampleName], ordered = [...values].sort((a, b) => a - b);
        assert.equal(values.length, rankings.length);
        const expected = { samples: values.length, p50Ms: ordered[Math.ceil(ordered.length * .5) - 1],
          p95Ms: ordered[Math.ceil(ordered.length * .95) - 1], meanMs: mean(values), maximumMs: ordered.at(-1) };
        for (const [key, value] of Object.entries(expected)) {
          closeEnough(method.latency[kind][key], value);
          checkedValues++;
        }
      }
    }
  } finally { database.close(); }
  const audit = { schemaVersion: 1, createdAt: new Date().toISOString(), input,
    manifestSha256: sha256(manifestBytes), rankingsSha256: sha256(rankingBytes), qrelsSha256: sha256(qrelsBytes),
    queries: 300, corpusDocuments: 5183, checkedValues, verifiedShortReferences, passed: true,
    method: 'Independent document/qrels arithmetic, budget coverage, native sample quantiles and read-only source/chunk hash checks',
    limitations: ['Not a third-party trec_eval executable run', 'Not provider request observation or formal-chat receipt authorization'] };
  const filename = join(input, 'independent-audit.json');
  await writeFile(filename, JSON.stringify(audit, null, 2) + '\n', { flag: 'wx' });
  process.stdout.write(JSON.stringify({ output: filename, checkedValues, verifiedShortReferences, passed: true }) + '\n');
}

try { await main(); }
catch (error) { process.stderr.write(`Independent peer audit failed: ${error.message}\n`); process.exitCode = 1; }

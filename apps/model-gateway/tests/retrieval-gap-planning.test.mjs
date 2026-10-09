import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { test } from 'node:test';
import { planEvidenceAcquisition, prioritizeEvidenceSources } from '../orchestration/retrieval/query-plan.mjs';
import { assessEvidence } from '../orchestration/retrieval/candidate-selection.mjs';
import { EvidenceAcquisition } from '../orchestration/retrieval/evidence-acquisition.mjs';
import { RetrievalCoordinator } from '../orchestration/retrieval/coordinator.mjs';
import { SourceIndexer } from '../orchestration/retrieval/source-indexer.mjs';
import { toolFixture } from './tool-fixture.mjs';

const evidence = (title, excerpt, extra = {}) => ({ title, excerpt, sourceId: title, scopeKey: 'user',
  sourceRevision: 1, contentHash: 'a'.repeat(64), sourceRef: title,
  locator: { relativePath: title, startOffset: 0, endOffset: excerpt.length }, ...extra });

test('an exact indexed declaration starts without vector inference or optional reranking', () => {
  const item = evidence('src/cancel.mjs', 'function CancelJob() {}', {
    exactTargetMatch: true, structure: { domain: 'code', symbolName: 'CancelJob' } });
  const decision = planEvidenceAcquisition({ query: 'Find `CancelJob` definition',
    items: [item], embeddingStatus: { state: 'ready', loaded: true }, rerankerStatus: { state: 'ready' } });
  assert.deepEqual(decision.channels, ['lexical', 'structure']);
  assert.equal(decision.shouldEmbed, false);
  assert.equal(decision.shouldRerank, false);
  assert.equal(decision.sufficiency, 'not-evaluated');
  const symbolOnly = planEvidenceAcquisition({ query: 'Find `CancelJob` definition',
    items: [{ ...item, exactTargetMatch: undefined, symbolRank: 1 }], embeddingStatus: { state: 'ready', loaded: true } });
  assert.equal(symbolOnly.shouldEmbed, false);
});

test('candidate support identifies missing roles without calling retrieved tests executed validation', () => {
  const items = [evidence('src/cancel.mjs', 'The cancellation implementation aborts queued jobs.')];
  const assessment = assessEvidence(items, 'Review cancellation implementation and tests');
  assert.deepEqual(assessment.missingEvidence, [{ kind: 'requested-role', role: 'tests' }]);
  const decision = planEvidenceAcquisition({ query: 'Fix cancellation implementation and tests', items, assessment,
    intent: { domain: 'code' } });
  assert.equal(decision.next, 'read-source');
  assert.equal(decision.requiresVerification, true);
  assert.deepEqual(decision.requiredChecks, ['re-read-current-source', 'run-relevant-validation']);
  assert.equal(decision.sufficiency, 'not-evaluated');
});

test('reranking runs only for ready optional models and an ambiguous or incomplete task', () => {
  const items = [evidence('a.md', 'release conditions'), evidence('b.md', 'deployment conditions')];
  const input = { query: 'Compare release and deployment', items, taskType: 'research',
    assessment: { state: 'weak' }, rerankerStatus: { state: 'ready' } };
  assert.equal(planEvidenceAcquisition(input).shouldRerank, true);
  assert.equal(planEvidenceAcquisition({ ...input, taskType: 'lookup' }).shouldRerank, false);
  assert.equal(planEvidenceAcquisition({ ...input, rerankerStatus: { state: 'loading' } }).shouldRerank, false);
  assert.equal(planEvidenceAcquisition({ ...input, assessment: { state: 'usable' } }).shouldRerank, false);
});

test('partial coverage and exhausted acquisition keep the gap explicit and do not certify absence', () => {
  assert.equal(planEvidenceAcquisition({ indexingPending: true }).next, 'direct-search-or-read');
  const stopped = planEvidenceAcquisition({ gap: 'current launch date', remainingSearches: 0 });
  assert.equal(stopped.next, 'state-unresolved-gap');
  assert.equal(stopped.shouldContinueSearch, false);
  assert.equal(stopped.sufficiency, 'not-evaluated');
});

test('repeated observations prefer source reads and a version-checked read does not certify the conclusion', () => {
  const acquisition = new EvidenceAcquisition();
  const item = evidence('reference.md', 'Conditions apply outside the excerpt.');
  const ticket = acquisition.prepare({ query: 'conditions', snapshotKey: 'v1', cacheKey: 'q' });
  const result = { items: [item], evidenceAssessment: { state: 'usable', requiresSourceRead: true } };
  assert.equal(acquisition.observe(ticket, result).decision.next, 'read-source');
  const repeated = acquisition.prepare({ query: 'conditions', snapshotKey: 'v1', cacheKey: 'q' });
  assert.equal(acquisition.observe(repeated, repeated.cached, { reused: true }).newEvidenceCount, 0);
  const read = acquisition.observeRead(item, { gap: 'omitted conditions', mode: 'section' });
  assert.equal(read.contentRead, true);
  assert.equal(read.sourceVersionChecked, true);
  assert.equal(read.sufficiency, 'not-evaluated');
});

test('a source version change invalidates only dependent observations and keeps the new evidence unverified', () => {
  const acquisition = new EvidenceAcquisition();
  const item = evidence('reference.md', 'Revision one.');
  const initial = acquisition.prepare({ query: 'revision', snapshotKey: 'v1', cacheKey: 'q' });
  acquisition.observe(initial, { items: [item] });
  acquisition.observeRead(item);
  const updated = { ...item, sourceRevision: 2, sourceRef: 'reference-v2', contentHash: 'b'.repeat(64) };
  const ticket = acquisition.prepare({ query: 'revision', snapshotKey: 'v2', cacheKey: 'q' });
  const status = acquisition.observe(ticket, { items: [updated] });
  assert.deepEqual(status.invalidatedSourceRefs, ['reference.md']);
  assert.equal(status.repairRequired, true);
  assert.equal(status.sufficiency, 'not-evaluated');
  assert.equal(acquisition.sourceReads.size, 0);
  assert.equal(acquisition.prepare({ query: 'revision', snapshotKey: 'v1', cacheKey: 'q' }).cached, undefined);
  const rederived = { ...updated, derivationSignature: 'c'.repeat(64), sourceRef: 'reference-derived-v3' };
  const derivationTicket = acquisition.prepare({ query: 'revision', snapshotKey: 'v3', cacheKey: 'q' });
  const derivation = acquisition.observe(derivationTicket, { items: [rederived] });
  assert.deepEqual(derivation.invalidatedSourceRefs, ['reference-v2']);
  assert.equal(derivation.newEvidenceCount, 1);
});

test('task priority reorders admitted metadata without adding sources, mutating originals or starving the tail', () => {
  const sources = ['large.csv', 'docs/notes.md', 'tests/cancel.test.mjs', 'src/cancel.mjs']
    .map(title => evidence(title, 'synthetic'));
  const reordered = prioritizeEvidenceSources(sources, new Set(['src/cancel.mjs']));
  assert.equal(reordered[0], sources[3]);
  assert.equal(reordered[1], sources[2]);
  assert.deepEqual(new Set(reordered), new Set(sources));
  assert.equal(sources[0].title, 'large.csv');
});

test('a newly requested path changes only remaining publication batches and reports their actual identities', async () => {
  const sources = ['a.md', 'b.md', 'c.md'].map((title, index) => ({ sourceId: `source-${index}`, scopeKey: 'user',
    sourceType: 'knowledge', title, text: `Evidence in ${title}`, sourceRevision: 1, locator: { relativePath: title } }));
  const published = [], progressed = [];
  const priorities = { revision: 0, paths: new Set() };
  const settings = { local: { semantic: 'off', embeddingProfileId: null, indexing: { batchSize: 1 } } };
  const indexer = new SourceIndexer({ library: {}, index: { upsertSources: async batch => {
    published.push(...batch.map(source => source.title));
    return { sources: batch.map(source => ({ sourceId: source.sourceId })) };
  } }, embeddings: { status: () => ({ state: 'unavailable' }) }, serialize: operation => operation() });
  await indexer.upsert(sources, settings, new AbortController().signal, async (_count, progress) => {
    progressed.push(...progress.processedSourceIds);
    if (progressed.length === 1) { priorities.paths.add('c.md'); priorities.revision++; }
  }, { semantic: false, priorities });
  assert.deepEqual(published, ['a.md', 'c.md', 'b.md']);
  assert.deepEqual(progressed, ['source-0', 'source-2', 'source-1']);
  assert.deepEqual(sources.map(source => source.title), ['a.md', 'b.md', 'c.md']);
});

test('automatic evidence preparation does not start a cold embedding model; explicit search can load it', async t => {
  const fixture = await toolFixture(t);
  let embeddingCalls = 0;
  const entry = { id: randomUUID(), scope: 'user', status: 'confirmed', active: true,
    kind: 'CALYX notes', revision: 1, content: 'CALYX documents describe cancellation conditions.' };
  const retrieval = new RetrievalCoordinator({ conversations: fixture.conversations, tools: fixture.service,
    memory: { contextFor: async () => ({ entries: [entry] }) },
    embeddings: { status: () => ({ state: 'ready', loaded: false, profileId: 'builtin-multilingual' }),
      embedQuery: async () => { embeddingCalls++; throw Object.assign(new Error('Synthetic failure'), { code: 'TEST_EMBEDDING_FAILURE' }); },
      close: async () => {} }, reranker: null });
  fixture.service.retrieval = retrieval;
  const context = await fixture.context('full');
  const automatic = await retrieval.evidence(context, 'Explain CALYX documents');
  assert.equal(embeddingCalls, 0);
  assert.ok(automatic.references.length);
  const explicit = await retrieval.search(context, { query: 'CALYX documents' });
  assert.equal(embeddingCalls, 1);
  assert.equal(explicit.embeddingDiagnostic, 'TEST_EMBEDDING_FAILURE');
  assert.ok(explicit.items.length);
});

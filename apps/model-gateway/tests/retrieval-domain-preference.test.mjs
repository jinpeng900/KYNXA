import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { RetrievalIndex } from '../data/retrieval/index.mjs';
import { validateRetrievalIntent } from '../data/retrieval/retrieval-contracts.mjs';

async function fixture(t, options = {}) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-domain-preference-'));
  const index = new RetrievalIndex({ root, ...options });
  t.after(async () => {
    await index.close();
    await rm(root, { recursive: true, force: true });
  });
  return index;
}

const source = (sourceId, sourceType, text, extra = {}) => ({ sourceId, sourceType, text,
  scopeKey: 'project:allowed', sourceRevision: 1, title: 'Evidence', locator: {}, ...extra });
const sourceIds = result => result.items.map(item => item.sourceId);

test('retrieval intent preserves a validated soft preference separately from its hard domain', () => {
  for (const preferredDomain of ['code', 'knowledge', 'mixed']) {
    assert.deepEqual(validateRetrievalIntent({ preferredDomain, path: 'Docs\\Review.md' }),
      { domain: 'mixed', preferredDomain, path: 'docs/review.md' });
  }
  assert.deepEqual(validateRetrievalIntent({ domain: 'code', preferredDomain: 'knowledge' }),
    { domain: 'code', preferredDomain: 'knowledge' });
  for (const preferredDomain of ['unknown', "code' OR 1=1 --", null, 0, [], {}]) {
    assert.throws(() => validateRetrievalIntent({ preferredDomain }), { code: 'INVALID_RETRIEVAL_INPUT' });
  }
});

test('lexical domain preference ranks shared terms without excluding the opposite domain or crossing scope', async t => {
  const index = await fixture(t, { vectorEnabled: false });
  await index.upsertSources([
    source('code-notes', 'code', 'method review network evidence'),
    source('research-notes', 'document', 'method review network evidence'),
    source('private-notes', 'code', 'method review network evidence', { scopeKey: 'project:private' })
  ]);
  const search = retrievalIntent => index.search({ query: 'method review', scopeKeys: ['project:allowed'], retrievalIntent });
  const original = await search({ domain: 'mixed' });
  const references = new Map(original.items.map(item => [item.sourceId, item.sourceRef]));
  for (const [preferredDomain, first] of [['code', 'code-notes'], ['knowledge', 'research-notes']]) {
    const result = await search({ domain: 'mixed', preferredDomain });
    assert.deepEqual(new Set(sourceIds(result)), new Set(['code-notes', 'research-notes']));
    assert.equal(result.items[0].sourceId, first);
    assert.equal(result.retrievalIntent.preferredDomain, preferredDomain);
    for (const item of result.items) {
      assert.equal(item.sourceRef, references.get(item.sourceId));
      assert.equal(item.excerpt, 'method review network evidence');
    }
  }
  assert.deepEqual(sourceIds(await search({ domain: 'mixed', preferredDomain: 'mixed' })), sourceIds(original));
  await assert.rejects(() => index.read({ sourceId: 'private-notes', scopeKeys: ['project:allowed'] }),
    { code: 'RETRIEVAL_SOURCE_NOT_FOUND' });
  await index.upsertSources([source('research-notes', 'document', 'Updated method review evidence', { sourceRevision: 2 })]);
  await assert.rejects(() => index.read({ sourceRef: references.get('research-notes'), scopeKeys: ['project:allowed'] }),
    { code: 'STALE_RETRIEVAL_SOURCE' });
});

test('an unmatched soft domain returns available evidence rather than a false absence', async t => {
  const index = await fixture(t, { vectorEnabled: false });
  await index.upsertSources([source('only-research', 'document', 'method compares experimental groups')]);
  const result = await index.search({ query: 'method', scopeKeys: ['project:allowed'],
    retrievalIntent: { domain: 'mixed', preferredDomain: 'code' } });
  assert.deepEqual(sourceIds(result), ['only-research']);
});

test('mixed-domain bilingual bridges retain code and document recall regardless of the soft preference', async t => {
  const index = await fixture(t, { vectorEnabled: false });
  const originals = new Map([
    ['english-code', 'cancellation releases resources'],
    ['english-document', 'cancellation changes the research protocol']
  ]);
  await index.upsertSources([
    source('english-code', 'code', originals.get('english-code')),
    source('english-document', 'document', originals.get('english-document')),
    source('private-chinese', 'document', '取消', { scopeKey: 'project:private' })
  ]);
  const query = '取消';
  const search = retrievalIntent => index.search({ query, scopeKeys: ['project:allowed'], retrievalIntent });
  const original = await search({ domain: 'mixed' });
  assert.deepEqual(new Set(sourceIds(original)), new Set(originals.keys()));
  const references = new Map(original.items.map(item => [item.sourceId, item.sourceRef]));
  for (const preferredDomain of ['code', 'knowledge']) {
    const result = await search({ domain: 'mixed', preferredDomain });
    assert.deepEqual(new Set(sourceIds(result)), new Set(originals.keys()));
    for (const item of result.items) {
      assert.equal(item.excerpt, originals.get(item.sourceId));
      assert.equal(item.sourceRef, references.get(item.sourceId));
      assert.equal((await index.read({ sourceRef: item.sourceRef, scopeKeys: ['project:allowed'] })).text,
        originals.get(item.sourceId));
    }
  }
});

test('soft lexical preference cannot displace a stronger exact phrase at the candidate limit', async t => {
  const index = await fixture(t, { vectorEnabled: false });
  await index.upsertSources([
    source('exact-research', 'document', 'study design'),
    source('weak-code', 'code', 'study additional implementation design')
  ]);
  const result = await index.search({ query: 'study design', scopeKeys: ['project:allowed'], channelCandidates: 1,
    retrievalIntent: { domain: 'mixed', preferredDomain: 'code' } });
  assert.deepEqual(sourceIds(result), ['exact-research']);
});

test('hybrid ranking applies a bounded preference while explicit domain retains its caller filter', async t => {
  const index = await fixture(t);
  await index.upsertSources([
    source('code-match', 'code', 'research review evidence'),
    source('research-match', 'document', 'research review evidence', { embeddingProfileId: 'fixture', vectors: [[1, 0]] }),
    source('private-match', 'code', 'research review evidence',
      { scopeKey: 'project:private', embeddingProfileId: 'fixture', vectors: [[1, 0]] }),
    source('legacy-match', 'work-file', 'research review evidence')
  ]);
  const search = retrievalIntent => index.search({ query: 'research review', scopeKeys: ['project:allowed'],
    queryVector: [1, 0], embeddingProfileId: 'fixture', retrievalIntent });
  const soft = await search({ domain: 'mixed', preferredDomain: 'code' });
  assert.equal(soft.strategy, 'hybrid');
  assert.deepEqual(new Set(sourceIds(soft)), new Set(['code-match', 'research-match', 'legacy-match']));
  assert.equal(soft.items[0].sourceId, 'research-match', 'two matching channels outweigh a weak domain preference');
  const hard = await search({ domain: 'code', preferredDomain: 'knowledge' });
  assert.deepEqual(new Set(sourceIds(hard)), new Set(['code-match', 'legacy-match']));
  assert.equal(hard.items[0].sourceId, 'code-match');
  const knowledge = await search({ domain: 'knowledge', preferredDomain: 'code' });
  assert.deepEqual(new Set(sourceIds(knowledge)), new Set(['research-match', 'legacy-match']));
});

test('vector-only retrieval preserves both domains and respects scope with soft preferences', async t => {
  const index = await fixture(t);
  await index.upsertSources([
    source('semantic-code', 'code', 'implementation evidence', { embeddingProfileId: 'fixture', vectors: [[1, 0]] }),
    source('semantic-research', 'document', 'experimental evidence', { embeddingProfileId: 'fixture', vectors: [[1, 0]] }),
    source('private-vector', 'code', 'private implementation',
      { scopeKey: 'project:private', embeddingProfileId: 'fixture', vectors: [[1, 0]] })
  ]);
  for (const [preferredDomain, first] of [['code', 'semantic-code'], ['knowledge', 'semantic-research']]) {
    const result = await index.search({ query: 'unmatchedterm', scopeKeys: ['project:allowed'],
      queryVector: [1, 0], embeddingProfileId: 'fixture', retrievalIntent: { domain: 'mixed', preferredDomain } });
    assert.equal(result.strategy, 'hybrid');
    assert.deepEqual(new Set(sourceIds(result)), new Set(['semantic-code', 'semantic-research']));
    assert.equal(result.items[0].sourceId, first);
  }
});

test('concurrent queued lexical queries retain independent scope statistics and deterministic scores', async t => {
  const index = await fixture(t, { vectorEnabled: false });
  await index.upsertSources(Array.from({ length: 100 }, (_, count) => source(`source-${count}`,
    count % 2 === 0 ? 'code' : 'document', `alpha ${count % 4 === 0 ? 'beta beta' : 'delta'} unique${count}`,
    { scopeKey: count % 3 === 0 ? 'project:private' : 'project:allowed' })));
  const inputs = Array.from({ length: 12 }, (_, count) => ({ query: count % 2 === 0 ? 'alpha beta' : 'alpha delta',
    scopeKeys: [count % 3 === 0 ? 'project:private' : 'project:allowed'],
    channelCandidates: 128, limit: 128, retrievalIntent: { domain: count % 4 === 0 ? 'code' : 'mixed' } }));
  const before = await Promise.all(inputs.map(input => index.search(input)));
  for (const [count, input] of inputs.entries()) {
    const repeated = await index.search(input);
    assert.deepEqual(repeated.items, before[count].items);
    assert.ok(repeated.items.every(item => input.scopeKeys.includes(item.scopeKey)));
    assert.ok(repeated.items.every(item => Number.isFinite(item.lexicalScore)));
  }
});

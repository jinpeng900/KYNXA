import assert from 'node:assert/strict';
import { DatabaseSync } from 'node:sqlite';
import { test } from 'node:test';
import { ScopedLexicalRanker } from '../data/retrieval/scoped-lexical-rank.mjs';

function fixture(t, { legacy = false, authorizedCount = 1 } = {}) {
  const database = new DatabaseSync(':memory:');
  t.after(() => database.close());
  database.exec(`CREATE TABLE sources(source_id TEXT PRIMARY KEY,scope_key TEXT,source_type TEXT,structure_json TEXT);
    CREATE TABLE chunks(id INTEGER PRIMARY KEY,chunk_id TEXT,source_id TEXT,text TEXT,structure_domain TEXT);
    CREATE TABLE scope_snapshots(scope_key TEXT PRIMARY KEY,generation INTEGER);
    CREATE TABLE retrieval_metadata(key TEXT PRIMARY KEY,value TEXT);
    CREATE VIRTUAL TABLE chunk_fts USING fts5(text);
    INSERT INTO scope_snapshots VALUES ('project:one',1),('project:two',1),('project:private',1);`);
  const source = database.prepare('INSERT INTO sources VALUES (?,?,?,?)');
  const chunk = database.prepare('INSERT INTO chunks VALUES (?,?,?,?,?)');
  const text = database.prepare('INSERT INTO chunk_fts(rowid,text) VALUES (?,?)');
  database.exec('BEGIN');
  for (let index = 0; index < 2200; index++) {
    const scope = index % 9 === 0 ? 'project:private' : index % 3 === 0 ? 'project:two' : 'project:one';
    const sourceId = `source-${index}`, content = index % 5 === 0 ? 'alpha delta scientific observation' :
      `${index % 2 === 0 ? 'alpha beta' : 'alpha novel beta'} observation ${'delta '.repeat(index % 13)}`;
    source.run(sourceId, scope, index % 4 === 0 ? 'code' : 'document', '');
    chunk.run(index + 1, `chunk-${String(index).padStart(5, '0')}`, sourceId, content, '');
    text.run(index + 1, content);
  }
  database.exec('COMMIT');
  const captured = [];
  const connection = { exec: (...args) => database.exec(...args), function: (...args) => database.function(...args),
    prepare: sql => {
      if (!sql.startsWith('WITH fts_matches')) return database.prepare(sql);
      captured.push(sql);
      if (!legacy) return database.prepare(sql);
      // Reconstruct the previous SQL only inside this fixture to compare actual rows, ranks and scores.
      // 仅在隔离夹具重建旧 SQL，对照真实返回行、排序和分数，不复制另一份产品排序器。
      const previous = sql.replace(/WITH fts_matches AS MATERIALIZED \([\s\S]*?\),\s*frequencies/u, 'WITH frequencies')
        .replace('FROM fts_matches matched JOIN scores score ON score.doc=matched.id',
          'FROM chunk_fts JOIN scores score ON score.doc=chunk_fts.rowid');
      const candidateStart = previous.indexOf('candidates AS MATERIALIZED');
      const oldSql = previous.slice(0, candidateStart) + previous.slice(candidateStart)
        .replace('WHERE s.scope_key IN', 'WHERE chunk_fts MATCH ? AND s.scope_key IN');
      const statement = database.prepare(oldSql);
      return { all: (...parameters) => statement.all(...parameters.slice(1, -authorizedCount), parameters[0],
        ...parameters.slice(-authorizedCount)) };
    } };
  const ranker = new ScopedLexicalRanker(connection);
  return { database, ranker, captured };
}

test('materialized MATCH preserves large candidate ordering and scoped BM25 scores', t => {
  for (const intent of [
    { scopes: ['project:one'] },
    { scopes: ['project:one', 'project:two'], preferredDomain: 'knowledge' },
    { scopes: ['project:one'], domain: 'code', preferredDomain: 'knowledge' }
  ]) {
    const authorizedCount = intent.scopes.length + Number(Boolean(intent.domain));
    const current = fixture(t), previous = fixture(t, { legacy: true, authorizedCount });
    for (const [expression, query] of [['"alpha" OR "beta"', 'alpha beta'], ['"delta"', 'delta'], ['"absent"', 'absent']]) {
      const input = { ...intent, expression, query, limit: 128,
        columns: 'c.chunk_id,s.scope_key,c.text' };
      const started = performance.now(), actual = current.ranker.search(input);
      const elapsedMs = performance.now() - started;
      assert.deepEqual(actual, previous.ranker.search(input));
      assert.ok(actual.every(item => intent.scopes.includes(item.scope_key)));
      if (query === 'alpha beta') assert.equal(actual.length, 128);
      t.diagnostic(`${JSON.stringify(intent)} ${query}: ${actual.length} rows, current ${elapsedMs.toFixed(2)} ms; synthetic fixture only`);
    }
  }
});

test('query plan materializes the single FTS match before per-document score joins', t => {
  const { ranker, database, captured } = fixture(t);
  ranker.search({ expression: '"alpha" OR "beta"', query: 'alpha beta', scopes: ['project:one'],
    columns: 'c.chunk_id', limit: 128 });
  const plan = database.prepare(`EXPLAIN QUERY PLAN ${captured.at(-1)}`)
    .all('"alpha" OR "beta"', 'alpha', 'beta', 'project:one', 'alpha beta', 'project:one');
  const details = plan.map(row => row.detail);
  assert.ok(details.includes('MATERIALIZE fts_matches'), JSON.stringify(details));
  assert.equal(details.filter(detail => /SCAN chunk_fts VIRTUAL TABLE/u.test(detail)).length, 1);
  t.diagnostic(details.join(' | '));
});

test('scope generation invalidation retains deterministic ranks after a corpus change', t => {
  const { ranker, database } = fixture(t);
  const input = { expression: '"alpha"', query: 'alpha', scopes: ['project:one'], columns: 'c.chunk_id', limit: 128 };
  ranker.search(input);
  database.exec(`DELETE FROM chunk_fts WHERE rowid IN (SELECT id FROM chunks WHERE source_id IN (SELECT source_id FROM sources WHERE scope_key='project:one') AND id%2=0);
    DELETE FROM chunks WHERE source_id IN (SELECT source_id FROM sources WHERE scope_key='project:one') AND id%2=0;
    UPDATE scope_snapshots SET generation=2 WHERE scope_key='project:one';`);
  const changed = ranker.search(input), fresh = new ScopedLexicalRanker(database).search(input);
  assert.deepEqual(changed, fresh);
});

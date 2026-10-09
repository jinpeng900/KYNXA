import { RETRIEVAL_DOMAIN_SQL } from './ann-store.mjs';

const MAX_SCOPE_STATISTICS = 32;
const MAX_TERM_STATISTICS = 1024;
const BM25_K1 = 1.2;
const BM25_B = 0.75;

function boundedCacheSet(cache, key, value, maximumEntries) {
  if (cache.has(key)) cache.delete(key);
  cache.set(key, value);
  while (cache.size > maximumEntries) cache.delete(cache.keys().next().value);
  return value;
}

/** FTS5's single-column docsize stores one unsigned varint, including repeated terms.
 * FTS5 单列 docsize 保存一个无符号变长整数，包含重复词；不能用去重查询词数代替原文长度。
 */
function documentTokens(bytes) {
  let tokens = 0;
  for (const byte of bytes ?? []) {
    tokens = tokens * 128 + (byte & 127);
    if (!(byte & 128)) return tokens;
  }
  return 0;
}

/** One postings index is shared, but BM25 statistics include only the authorized scope/domain.
 * 共用一份倒排索引，BM25 的语料长度与文档词频统计仅覆盖当前授权范围和领域。
 */
export class ScopedLexicalRanker {
  constructor(database) {
    this.database = database;
    this.corpusCache = new Map();
    this.termCache = new Map();
    this.activeStatistics = null;
    this.checkCancelled = null;
    this.scoredRows = 0;
    database.exec(`CREATE VIRTUAL TABLE IF NOT EXISTS temp.retrieval_term_instances USING fts5vocab(main,chunk_fts,'instance');
      CREATE VIRTUAL TABLE IF NOT EXISTS temp.retrieval_query_terms USING fts5(term,tokenize='unicode61');
      CREATE VIRTUAL TABLE IF NOT EXISTS temp.retrieval_query_vocabulary USING fts5vocab(temp,retrieval_query_terms,'row');`);
    database.function('retrieval_document_tokens', { deterministic: true }, bytes => {
      if (++this.scoredRows % 256 === 0) this.checkCancelled?.();
      return documentTokens(bytes);
    });
    database.function('retrieval_scoped_bm25', (term, frequency, size) => {
      if (++this.scoredRows % 256 === 0) this.checkCancelled?.();
      const statistics = this.activeStatistics;
      if (!statistics) return 0;
      const idf = statistics.inverseFrequency.get(term) ?? 0;
      const tokens = documentTokens(size);
      return -idf * frequency * (BM25_K1 + 1) /
        (frequency + BM25_K1 * (1 - BM25_B + BM25_B * tokens / statistics.averageTokens));
    });
  }

  queryTerms(expression) {
    const phrases = [...expression.matchAll(/"((?:[^"]|"")*)"/gu)].map(match => match[1].replaceAll('""', '"'));
    this.database.exec('DELETE FROM temp.retrieval_query_terms');
    this.database.prepare('INSERT INTO temp.retrieval_query_terms(term) VALUES (?)').run(phrases.join(' '));
    // Reuse the actual FTS tokenizer, including accent removal and identifier separators.
    // 复用实际 FTS 分词器，保持去重音和标识符分隔行为，避免查询词与索引词不一致。
    return this.database.prepare('SELECT term FROM temp.retrieval_query_vocabulary ORDER BY term').all().map(row => row.term);
  }

  search({ expression, query, scopes, domain, preferredDomain, columns, checkCancelled, limit = 40 }) {
    this.checkCancelled = checkCancelled;
    this.scoredRows = 0;
    const placeholders = scopes.map(() => '?').join(',');
    const predicate = `s.scope_key IN (${placeholders})${domain ? ` AND (${RETRIEVAL_DOMAIN_SQL}=? OR ${RETRIEVAL_DOMAIN_SQL}='')` : ''}`;
    const parameters = [...scopes, ...(domain ? [domain] : [])];
    const versions = this.database.prepare(`SELECT s.scope_key,COALESCE(m.value,CAST(s.generation AS TEXT)) AS generation
      FROM scope_snapshots s LEFT JOIN retrieval_metadata m ON m.key='lexical_generation:' || s.scope_key
      WHERE s.scope_key IN (${placeholders}) ORDER BY s.scope_key`).all(...scopes);
    const key = JSON.stringify([scopes.slice().sort(), domain ?? '', versions]);
    let corpus = this.corpusCache.get(key);
    checkCancelled?.();
    if (!corpus) {
      const row = this.database.prepare(`SELECT COUNT(*) AS documents,
        COALESCE(SUM(retrieval_document_tokens(d.sz)),0) AS tokens FROM chunks c
        JOIN sources s ON s.source_id=c.source_id JOIN chunk_fts_docsize d ON d.id=c.id WHERE ${predicate}`).get(...parameters);
      corpus = boundedCacheSet(this.corpusCache, key, { documents: row.documents,
        averageTokens: row.documents && row.tokens ? row.tokens / row.documents : 1 }, MAX_SCOPE_STATISTICS);
    }
    const terms = this.queryTerms(expression);
    if (!corpus.documents || !terms.length) return [];
    const inverseFrequency = new Map();
    const countTerm = this.database.prepare(`SELECT COUNT(DISTINCT v.doc) AS documents FROM temp.retrieval_term_instances v
      JOIN chunks c ON c.id=v.doc JOIN sources s ON s.source_id=c.source_id WHERE v.term=? AND ${predicate}`);
    for (const term of terms) {
      checkCancelled?.();
      const termKey = `${key}:${term}`;
      let frequency = this.termCache.get(termKey);
      if (frequency === undefined) frequency = boundedCacheSet(this.termCache, termKey, countTerm.get(term, ...parameters).documents, MAX_TERM_STATISTICS);
      inverseFrequency.set(term, Math.max(1e-6, Math.log((corpus.documents - frequency + 0.5) / (frequency + 0.5))));
    }
    this.activeStatistics = { ...corpus, inverseFrequency };
    // A soft preference only breaks relevance ties; it never changes eligibility or corpus statistics.
    // 软偏好只打破相关性并列，不改变候选资格或语料统计；明确领域筛选仍由 domain 决定。
    const rankingDomain = domain || preferredDomain;
    const domainRank = rankingDomain ? `CASE WHEN ${RETRIEVAL_DOMAIN_SQL}=? THEN 0 ELSE 1 END` : '0';
    const rankingParameters = rankingDomain ? [rankingDomain] : [];
    const candidateOrder = domain ? 'domain_rank,exact_rank,score.lexical_rank,c.chunk_id'
      : 'exact_rank,score.lexical_rank,domain_rank,c.chunk_id';
    const resultOrder = domain ? 'candidates.domain_rank,candidates.exact_rank,candidates.lexical_rank,candidates.chunk_id'
      : 'candidates.exact_rank,candidates.lexical_rank,candidates.domain_rank,candidates.chunk_id';
    try {
      // Rank every authorized match before applying the candidate limit; global top-k can lose valid rows.
      // 所有已授权命中先按范围统计排序，再限制候选数；全局 top-k 后筛范围会漏掉有效结果。
      return this.database.prepare(`WITH frequencies AS MATERIALIZED (
        SELECT v.doc,v.term,COUNT(*) AS frequency FROM temp.retrieval_term_instances v
        JOIN chunks c ON c.id=v.doc JOIN sources s ON s.source_id=c.source_id
        WHERE v.term IN (${terms.map(() => '?').join(',')}) AND ${predicate} GROUP BY v.doc,v.term),
        scores AS MATERIALIZED (SELECT f.doc,SUM(retrieval_scoped_bm25(f.term,f.frequency,d.sz)) AS lexical_rank
          FROM frequencies f JOIN chunk_fts_docsize d ON d.id=f.doc GROUP BY f.doc),
        candidates AS MATERIALIZED (SELECT c.id,c.chunk_id,score.lexical_rank,
          CASE WHEN instr(lower(c.text),lower(?)) > 0 THEN 0 ELSE 1 END AS exact_rank,${domainRank} AS domain_rank
          FROM chunk_fts JOIN scores score ON score.doc=chunk_fts.rowid JOIN chunks c ON c.id=score.doc
          JOIN sources s ON s.source_id=c.source_id WHERE chunk_fts MATCH ? AND ${predicate}
          ORDER BY ${candidateOrder} LIMIT ${limit})
        SELECT ${columns},candidates.lexical_rank FROM candidates
        JOIN chunks c ON c.id=candidates.id JOIN sources s ON s.source_id=c.source_id
        ORDER BY ${resultOrder}`)
        .all(...terms, ...parameters, query, ...rankingParameters, expression, ...parameters);
    } finally { this.activeStatistics = null; this.checkCancelled = null; }
  }
}

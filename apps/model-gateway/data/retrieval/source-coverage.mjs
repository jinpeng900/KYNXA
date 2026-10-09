import { retrievalFailure, retrievalSourceId } from './retrieval-contracts.mjs';
import { validateDocumentCoverage } from './document-coverage.mjs';

const STATES = new Set(['pending', 'ready', 'partial', 'failed', 'skipped', 'unverified', 'disabled']);

/** Coverage is a receipt of discovery/publication/failure, never a promise that every source was searched.
 * 覆盖状态记录发现、发布和失败回执，不保证所有来源已经检索或验证结论。
 */
export class SourceCoverageStore {
  constructor(database) {
    this.database = database;
    database.exec(`CREATE TABLE IF NOT EXISTS source_coverage (
      scope_key TEXT NOT NULL,source_id TEXT NOT NULL,relative_path TEXT NOT NULL,status TEXT NOT NULL,
      lexical TEXT NOT NULL,semantic TEXT NOT NULL,parser TEXT NOT NULL,error_code TEXT,
      source_revision TEXT,updated_at TEXT NOT NULL,PRIMARY KEY(scope_key,source_id));`);
    if (!database.prepare('PRAGMA table_info(source_coverage)').all().some(column => column.name === 'document_coverage'))
      database.exec('ALTER TABLE source_coverage ADD COLUMN document_coverage TEXT');
  }

  record(entries, scopes, checkCancelled) {
    if (!Array.isArray(entries) || entries.length > 100) throw retrievalFailure('Coverage batch is too large. / 来源覆盖批次过大。');
    const insert = this.database.prepare(`INSERT INTO source_coverage
      (scope_key,source_id,relative_path,status,lexical,semantic,parser,error_code,source_revision,updated_at,document_coverage)
      VALUES (?,?,?,?,?,?,?,?,?,?,?)
      ON CONFLICT(scope_key,source_id) DO UPDATE SET relative_path=excluded.relative_path,status=excluded.status,
      lexical=excluded.lexical,semantic=excluded.semantic,parser=excluded.parser,error_code=excluded.error_code,
      source_revision=excluded.source_revision,updated_at=excluded.updated_at,document_coverage=excluded.document_coverage`);
    for (const entry of entries) {
      checkCancelled?.(); retrievalSourceId(entry.sourceId);
      if (!scopes.includes(entry.scopeKey) || typeof entry.relativePath !== 'string' || entry.relativePath.length > 4096 ||
          ['status', 'lexical', 'semantic', 'parser'].some(key => !STATES.has(entry[key])) ||
          entry.errorCode !== undefined && !/^[A-Z][A-Z0-9_]{0,127}$/u.test(entry.errorCode))
        throw retrievalFailure('Invalid scoped source coverage. / 来源覆盖范围或状态无效。');
      insert.run(entry.scopeKey, entry.sourceId, entry.relativePath, entry.status, entry.lexical, entry.semantic, entry.parser,
        entry.errorCode ?? null, JSON.stringify(entry.sourceRevision ?? null), new Date().toISOString(),
        entry.documentCoverage ? JSON.stringify(validateDocumentCoverage(entry.documentCoverage)) : null);
    }
    return { recorded: entries.length };
  }

  query(scopes, { sourceId, limit = 100 } = {}, checkCancelled) {
    if (!Number.isSafeInteger(limit) || limit < 1 || limit > 1000) throw retrievalFailure('Invalid coverage limit. / 覆盖查询数量无效。');
    const placeholders = scopes.map(() => '?').join(',');
    const totals = this.database.prepare(`SELECT COUNT(*) AS discovered,COUNT(DISTINCT scope_key || ':' || relative_path) AS files,
      SUM(CASE WHEN status='failed' THEN 1 ELSE 0 END) AS failed,SUM(CASE WHEN status='skipped' THEN 1 ELSE 0 END) AS skipped
      FROM source_coverage WHERE scope_key IN (${placeholders})`).get(...scopes);
    const ready = this.database.prepare(`SELECT SUM(CASE WHEN v.lexical='ready' AND s.source_id IS NOT NULL AND v.source_revision=s.source_revision THEN 1 ELSE 0 END) AS lexical,
      SUM(CASE WHEN v.semantic='ready' AND s.source_id IS NOT NULL AND v.source_revision=s.source_revision
        AND EXISTS(SELECT 1 FROM chunks c WHERE c.source_id=s.source_id)
        AND NOT EXISTS(SELECT 1 FROM chunks c WHERE c.source_id=s.source_id AND c.vector IS NULL) THEN 1 ELSE 0 END) AS semantic
      FROM source_coverage v LEFT JOIN sources s ON s.source_id=v.source_id AND s.scope_key=v.scope_key WHERE v.scope_key IN (${placeholders})`).get(...scopes);
    const rows = this.database.prepare(`SELECT v.*,s.source_id AS published_source_id,s.source_revision AS published_revision,
      (EXISTS(SELECT 1 FROM chunks c WHERE c.source_id=s.source_id)
       AND NOT EXISTS(SELECT 1 FROM chunks c WHERE c.source_id=s.source_id AND c.vector IS NULL)) AS complete_vectors
      FROM source_coverage v LEFT JOIN sources s ON s.source_id=v.source_id AND s.scope_key=v.scope_key
      WHERE v.scope_key IN (${placeholders}) ${sourceId ? 'AND v.source_id=?' : ''}
      ORDER BY v.relative_path,v.source_id LIMIT ?`).all(...scopes, ...(sourceId ? [sourceId] : []), limit + 1);
    checkCancelled?.();
    return { counts: { ...Object.fromEntries(Object.entries(totals).map(([key, value]) => [key, value ?? 0])),
      lexical: ready.lexical ?? 0, semantic: ready.semantic ?? 0 }, hasMore: rows.length > limit,
      complete: false, items: rows.slice(0, limit).map(row => {
        const current = row.published_source_id && row.source_revision === row.published_revision;
        const lexical = row.lexical === 'ready' && !current ? 'unverified' : row.lexical;
        const semantic = row.semantic === 'ready' && (!current || !row.complete_vectors) ? 'unverified' : row.semantic;
        // Current publication controls ready counts and row status alike; old receipts cannot prove removed vectors.
        // 统计与单行状态同时以当前发布为准，旧回执不能证明已被移除的向量仍然可用。
        return { sourceId: row.source_id, scopeKey: row.scope_key, relativePath: row.relative_path,
          status: row.status === 'ready' && (lexical === 'unverified' || semantic === 'unverified') ? 'unverified' : row.status,
          lexical, semantic, parser: row.parser, ...(row.error_code ? { errorCode: row.error_code } : {}),
          ...(row.document_coverage ? { documentCoverage: validateDocumentCoverage(JSON.parse(row.document_coverage)) } : {}),
          sourceRevision: JSON.parse(row.source_revision), updatedAt: row.updated_at };
      }) };
  }
}

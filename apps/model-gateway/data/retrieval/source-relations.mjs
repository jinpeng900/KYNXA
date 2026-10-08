import { hashText, retrievalFailure, sourceReference, sourceEvidenceLocator } from './retrieval-contracts.mjs';

export const RELATION_DERIVATION_VERSION = 'structure-relations-v1';
const MAX_SOURCE_EDGES = 10000;
const KINDS = new Set(['definition', 'contains', 'lexical-reference', 'section']);

/** Syntax ownership is deterministic; textual mentions remain uncertain and never claim exhaustive references.
 * 语法归属确定性派生；文本提及始终标为不确定，不声称已得到穷举引用或语言服务调用图。
 */
export function deriveSourceRelations(source, chunks, checkCancelled) {
  const edges = [], seen = new Set(), definitions = new Set();
  const add = edge => {
    const key = hashText(JSON.stringify([source.sourceId, source.derivationSignature, RELATION_DERIVATION_VERSION, edge]));
    if (seen.has(key) || edges.length >= MAX_SOURCE_EDGES) return;
    seen.add(key); edges.push({ edgeId: key, ...edge });
  };
  for (const chunk of chunks) {
    checkCancelled?.();
    const structure = chunk.structure;
    if (structure?.parseStatus === 'unavailable') continue;
    const owner = structure?.qualifiedName ?? structure?.symbolName;
    const section = structure?.sectionPath?.join(' / ');
    const common = { chunkId: chunk.chunkId, chunkHash: chunk.chunkHash, startOffset: chunk.startOffset,
      endOffset: chunk.endOffset, offsetUnit: 'utf16-code-units' };
    const definitionKey = JSON.stringify([owner, structure?.unitStartOffset, structure?.unitEndOffset]);
    if (owner && !definitions.has(definitionKey)) {
      definitions.add(definitionKey);
      add({ ...common, kind: 'definition', fromSymbol: owner, toSymbol: structure.symbolName ?? owner, certainty: 'syntax' });
      if (structure.parentSymbol) add({ ...common, kind: 'contains', fromSymbol: structure.parentSymbol, toSymbol: owner, certainty: 'syntax' });
    }
    if (section) add({ ...common, kind: 'section', fromSymbol: source.title, toSymbol: section, certainty: 'syntax' });
    if (structure?.domain !== 'code') continue;
    for (const match of chunk.text.matchAll(/\b[A-Za-z_$][\w$]*(?:\.[A-Za-z_$][\w$]*)*\b/gu)) {
      if (edges.length >= MAX_SOURCE_EDGES) break;
      if (match[0].length < 3 || match[0] === structure.symbolName) continue;
      add({ ...common, kind: 'lexical-reference', fromSymbol: owner ?? source.title, toSymbol: match[0],
        startOffset: chunk.startOffset + match.index, endOffset: chunk.startOffset + match.index + match[0].length, certainty: 'lexical-unresolved' });
    }
  }
  return { edges, truncated: edges.length >= MAX_SOURCE_EDGES };
}

export class SourceRelationStore {
  constructor(database) {
    this.database = database;
    database.exec(`CREATE TABLE IF NOT EXISTS source_relations (
      edge_id TEXT PRIMARY KEY,source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE CASCADE,
      derivation_signature TEXT NOT NULL,kind TEXT NOT NULL,from_symbol TEXT NOT NULL,to_symbol TEXT NOT NULL,
      chunk_id TEXT NOT NULL,chunk_hash TEXT NOT NULL,start_offset INTEGER NOT NULL,end_offset INTEGER NOT NULL,certainty TEXT NOT NULL);
      CREATE INDEX IF NOT EXISTS relation_source ON source_relations(source_id,derivation_signature);
      CREATE INDEX IF NOT EXISTS relation_target ON source_relations(to_symbol,kind);
      CREATE INDEX IF NOT EXISTS relation_owner ON source_relations(from_symbol,kind);`);
  }

  publish(source, chunks, checkCancelled) {
    this.database.prepare('DELETE FROM source_relations WHERE source_id=?').run(source.sourceId);
    const result = deriveSourceRelations(source, chunks, checkCancelled);
    const insert = this.database.prepare('INSERT INTO source_relations VALUES (?,?,?,?,?,?,?,?,?,?,?)');
    for (const edge of result.edges) {
      checkCancelled?.();
      insert.run(edge.edgeId, source.sourceId, source.derivationSignature, edge.kind, edge.fromSymbol, edge.toSymbol,
        edge.chunkId, edge.chunkHash, edge.startOffset, edge.endOffset, edge.certainty);
    }
    return { edges: result.edges.length, truncated: result.truncated };
  }

  query({ scopes, sourceId, symbol, kind, limit = 40 }, checkCancelled) {
    if (kind !== undefined && !KINDS.has(kind) || symbol !== undefined && (typeof symbol !== 'string' || !symbol || symbol.length > 1024) ||
        !Number.isSafeInteger(limit) || limit < 1 || limit > 200)
      throw retrievalFailure('Invalid relation query. / 关系导航查询无效。');
    const rows = this.database.prepare(`SELECT e.*,s.scope_key,s.source_revision,s.content_hash,s.title,s.locator
      FROM source_relations e JOIN sources s ON s.source_id=e.source_id
      WHERE s.scope_key IN (${scopes.map(() => '?').join(',')}) AND e.derivation_signature=s.derivation_signature
      ${sourceId ? 'AND e.source_id=?' : ''} ${symbol ? 'AND (e.from_symbol=? OR e.to_symbol=?)' : ''} ${kind ? 'AND e.kind=?' : ''}
      ORDER BY CASE WHEN e.certainty='syntax' THEN 0 ELSE 1 END,e.source_id,e.start_offset,e.edge_id LIMIT ?`)
      .all(...scopes, ...(sourceId ? [sourceId] : []), ...(symbol ? [symbol, symbol] : []), ...(kind ? [kind] : []), limit + 1);
    checkCancelled?.();
    return { version: RELATION_DERIVATION_VERSION, complete: false, exhaustiveReferences: false, hasMore: rows.length > limit,
      items: rows.slice(0, limit).map(row => ({ edgeId: row.edge_id, kind: row.kind, fromSymbol: row.from_symbol, toSymbol: row.to_symbol,
        certainty: row.certainty, scopeKey: row.scope_key, sourceId: row.source_id, title: row.title,
        locator: { ...sourceEvidenceLocator(JSON.parse(row.locator), { startOffset: row.start_offset, endOffset: row.end_offset }),
          startOffset: row.start_offset, endOffset: row.end_offset, offsetUnit: 'utf16-code-units' },
        sourceRevision: JSON.parse(row.source_revision), derivationSignature: row.derivation_signature,
        sourceRef: sourceReference({ sourceId: row.source_id, scopeKey: row.scope_key, contentHash: row.content_hash,
          sourceRevision: JSON.parse(row.source_revision), derivationSignature: row.derivation_signature }, { chunkId: row.chunk_id, chunkHash: row.chunk_hash }) })) };
  }
}

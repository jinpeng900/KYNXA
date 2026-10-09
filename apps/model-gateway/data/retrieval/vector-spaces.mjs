import { retrievalFailure } from './retrieval-contracts.mjs';

const VECTOR_COLUMNS = ['embedding_profile_id', 'dimensions', 'vector', 'embedding_model_version', 'embedding_space_id'];
const MAX_RETAINED_SPACES = 2;

/** Alternate vectors belong to the same current chunks, never to a second source authority.
 * 多向量空间共享当前正式分块，不建立第二套来源；正文或实际嵌入输入改变后不保留旧向量。 */
export class RetrievalVectorSpaces {
  constructor(database) {
    this.database = database;
    database.exec(`CREATE TABLE IF NOT EXISTS chunk_vector_spaces (
      chunk_id TEXT NOT NULL REFERENCES chunks(chunk_id) ON DELETE CASCADE,
      embedding_profile_id TEXT NOT NULL,dimensions INTEGER NOT NULL,vector BLOB NOT NULL,
      embedding_model_version TEXT NOT NULL,embedding_space_id TEXT NOT NULL,updated_at TEXT NOT NULL,
      PRIMARY KEY(chunk_id,embedding_space_id));
      CREATE INDEX IF NOT EXISTS vector_spaces_profile ON chunk_vector_spaces(embedding_profile_id,embedding_space_id);
      INSERT OR IGNORE INTO chunk_vector_spaces SELECT chunk_id,embedding_profile_id,dimensions,vector,
        embedding_model_version,embedding_space_id,strftime('%Y-%m-%dT%H:%M:%fZ','now') FROM chunks
        WHERE vector IS NOT NULL AND embedding_profile_id IS NOT NULL;`);
    const columns = database.prepare('PRAGMA table_info(chunks)').all().map(row => row.name);
    const projection = columns.map(column => `${VECTOR_COLUMNS.includes(column) ? 'v' : 'c'}.${column}`).join(',');
    // A SQL view preserves the old row contract while filtering the exact requested space before ANN.
    // SQL 视图保持旧行合同，近邻检索前仍精确筛选请求空间，不能以相同维数代替空间校验。
    database.exec(`CREATE VIEW IF NOT EXISTS retrieval_vector_rows AS SELECT ${projection}
      FROM chunks c JOIN chunk_vector_spaces v ON v.chunk_id=c.chunk_id`);
  }

  capture(source, chunks, current) {
    if (!current || current.content_hash !== source.contentHash ||
      current.embedding_input_signature !== source.embeddingInputSignature) return new Map();
    const old = this.database.prepare('SELECT chunk_id,chunk_hash,start_offset,end_offset,embedding_projection FROM chunks WHERE source_id=?')
      .all(source.sourceId);
    const wanted = new Map(chunks.map(chunk => [chunk.chunkId, chunk]));
    if (old.length !== chunks.length || old.some(row => {
      const chunk = wanted.get(row.chunk_id);
      return !chunk || row.chunk_hash !== chunk.chunkHash || row.start_offset !== chunk.startOffset ||
        row.end_offset !== chunk.endOffset || row.embedding_projection !== (chunk.embeddingProjection ? JSON.stringify(chunk.embeddingProjection) : '');
    })) return new Map();
    const result = new Map();
    for (const row of this.database.prepare(`SELECT v.* FROM chunk_vector_spaces v JOIN chunks c ON c.chunk_id=v.chunk_id
      WHERE c.source_id=? ORDER BY v.updated_at DESC,v.embedding_space_id`).iterate(source.sourceId)) {
      const list = result.get(row.chunk_id) ?? [];
      if (list.length < MAX_RETAINED_SPACES) list.push(row);
      result.set(row.chunk_id, list);
    }
    return result;
  }

  publish(source, chunks, retained, timestamp) {
    const insert = this.database.prepare(`INSERT OR REPLACE INTO chunk_vector_spaces VALUES (?,?,?,?,?,?,?)`);
    for (const chunk of chunks) {
      const row = this.database.prepare('SELECT * FROM chunks WHERE chunk_id=?').get(chunk.chunkId);
      const previous = retained.get(chunk.chunkId) ?? [];
      const rows = row?.vector && row.embedding_profile_id ? [{ ...row, updated_at: timestamp },
        ...previous.filter(item => item.embedding_space_id !== row.embedding_space_id)] : previous;
      for (const vector of rows.slice(0, MAX_RETAINED_SPACES)) insert.run(chunk.chunkId, vector.embedding_profile_id,
        vector.dimensions, vector.vector, vector.embedding_model_version, vector.embedding_space_id, vector.updated_at);
    }
  }

  status(scopeKeys) {
    if (!Array.isArray(scopeKeys) || !scopeKeys.length) throw retrievalFailure('Vector scopes are required. / 必须提供向量范围。');
    const placeholders = scopeKeys.map(() => '?').join(',');
    const scopes = this.database.prepare(`SELECT s.scope_key,count(c.id) AS chunks FROM sources s JOIN chunks c ON c.source_id=s.source_id
      WHERE s.scope_key IN (${placeholders}) AND s.source_type NOT IN ('memory','message','conversation') GROUP BY s.scope_key`).all(...scopeKeys);
    const spaces = this.database.prepare(`SELECT s.scope_key,v.embedding_profile_id AS profileId,v.embedding_space_id AS spaceId,
      v.embedding_model_version AS modelVersion,v.dimensions,count(*) AS vectors
      FROM chunk_vector_spaces v JOIN chunks c ON c.chunk_id=v.chunk_id JOIN sources s ON s.source_id=c.source_id
      WHERE s.scope_key IN (${placeholders}) AND s.source_type NOT IN ('memory','message','conversation') GROUP BY s.scope_key,v.embedding_profile_id,v.embedding_space_id,v.embedding_model_version,v.dimensions`)
      .all(...scopeKeys);
    return { scopes: scopes.map(scope => ({ ...scope, spaces: spaces.filter(space => space.scope_key === scope.scope_key)
      .map(space => ({ ...space, complete: space.vectors === scope.chunks })) })), retainedSpacesPerChunk: MAX_RETAINED_SPACES,
      authority: 'current-source-and-embedding-input', cleanup: 'transactional-replacement-and-source-cascade' };
  }
}

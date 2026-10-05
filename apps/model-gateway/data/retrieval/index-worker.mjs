import { randomUUID } from 'node:crypto';
import { existsSync, lstatSync, mkdirSync, readFileSync, renameSync, unlinkSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { parentPort, workerData } from 'node:worker_threads';
import { DatabaseSync } from 'node:sqlite';
import { RETRIEVAL_SCHEMA_VERSION, hashText, parseSourceReference, retrievalFailure,
  retrievalScopeKeys, sourceReference, validateSource } from './retrieval-contracts.mjs';
import { chunkSource, lexicalText, matchExpression, CHUNKER_VERSION, TOKENIZER_VERSION } from './retrieval-text.mjs';
import { sourceWindow } from './source-window.mjs';

const root = resolve(workerData.root);
const directory = join(root, 'Index');
const filename = join(directory, 'retrieval.sqlite');
const identityDirectory = join(root, 'Retrieval');
const identityPath = join(identityDirectory, 'source-identities.json');
const MAX_VECTOR_SCAN_CHUNKS = 50000;
const CHUNK_ROW_COLUMNS = 'c.*, s.scope_key, s.source_type, s.title, s.locator, s.content_hash, s.source_revision, s.binding_revision, s.active_generation';
let database, vectorAvailable = false, vectorVersion = null, vectorError = null;

function checkPath(path, isDirectory = false) {
  if (!existsSync(path)) return;
  const info = lstatSync(path);
  if (info.isSymbolicLink() || (isDirectory ? !info.isDirectory() : !info.isFile() || info.nlink > 1))
    throw retrievalFailure('Unsafe retrieval storage path. / 检索存储路径包含链接或异常结构。', 'UNSAFE_RETRIEVAL_PATH', 409);
}

function initializeDirectories() {
  checkPath(root, true);
  if (!existsSync(root)) mkdirSync(root, { recursive: true, mode: 0o700 });
  for (const path of [directory, identityDirectory]) {
    checkPath(path, true);
    if (!existsSync(path)) mkdirSync(path, { mode: 0o700 });
  }
  for (const suffix of ['', '-wal', '-shm', '-journal']) checkPath(filename + suffix);
  checkPath(identityPath);
}

function generation() {
  return Number(database.prepare("SELECT value FROM retrieval_metadata WHERE key = 'generation'").get()?.value ?? 0);
}

function snapshotId(value = generation()) {
  return `${database.prepare("SELECT value FROM retrieval_metadata WHERE key='index_epoch'").get().value}:${value}`;
}

function nextGeneration(scopeKey) {
  const value = generation() + 1;
  database.prepare("UPDATE retrieval_metadata SET value = ? WHERE key = 'generation'").run(String(value));
  database.prepare('INSERT INTO scope_snapshots(scope_key, generation) VALUES (?, ?) ON CONFLICT(scope_key) DO UPDATE SET generation = excluded.generation')
    .run(scopeKey, value);
  return value;
}

async function openDatabase() {
  initializeDirectories();
  database = new DatabaseSync(filename, { allowExtension: true });
  const version = database.prepare('PRAGMA user_version').get().user_version;
  if (version > RETRIEVAL_SCHEMA_VERSION) {
    database.close();
    database = null;
    throw retrievalFailure('A newer retrieval index exists. / 检索索引由更新版本创建，原文件已保留。', 'UNSUPPORTED_RETRIEVAL_INDEX_VERSION', 409);
  }
  database.exec(`PRAGMA busy_timeout = 3000; PRAGMA journal_mode = WAL; PRAGMA synchronous = FULL;
    PRAGMA foreign_keys = ON;
    CREATE TABLE IF NOT EXISTS retrieval_metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
    INSERT OR IGNORE INTO retrieval_metadata VALUES ('generation', '0');
    CREATE TABLE IF NOT EXISTS sources (
      source_id TEXT PRIMARY KEY, scope_key TEXT NOT NULL, source_type TEXT NOT NULL, title TEXT NOT NULL,
      locator TEXT NOT NULL, text TEXT NOT NULL, content_hash TEXT NOT NULL, source_revision TEXT NOT NULL,
      binding_revision INTEGER NOT NULL, active_generation INTEGER NOT NULL, updated_at TEXT NOT NULL,
      embedding_signature TEXT NOT NULL DEFAULT '');
    CREATE INDEX IF NOT EXISTS sources_scope ON sources(scope_key);
    CREATE TABLE IF NOT EXISTS chunks (
      id INTEGER PRIMARY KEY, chunk_id TEXT UNIQUE NOT NULL, source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE CASCADE,
      chunk_index INTEGER NOT NULL, text TEXT NOT NULL, lexical_text TEXT NOT NULL, chunk_hash TEXT NOT NULL,
      start_offset INTEGER NOT NULL, end_offset INTEGER NOT NULL, start_line INTEGER NOT NULL, end_line INTEGER NOT NULL,
      embedding_profile_id TEXT, dimensions INTEGER, vector BLOB, chunker_version TEXT NOT NULL, tokenizer_version TEXT NOT NULL,
      embedding_model_version TEXT NOT NULL DEFAULT '');
    CREATE INDEX IF NOT EXISTS chunks_source ON chunks(source_id, chunk_index);
    CREATE INDEX IF NOT EXISTS chunks_profile ON chunks(embedding_profile_id, dimensions);
    CREATE TABLE IF NOT EXISTS scope_snapshots (scope_key TEXT PRIMARY KEY, generation INTEGER NOT NULL);
    CREATE VIRTUAL TABLE IF NOT EXISTS chunk_fts USING fts5(lexical_text, content='chunks', content_rowid='id', tokenize='unicode61');
    CREATE TRIGGER IF NOT EXISTS chunks_insert AFTER INSERT ON chunks BEGIN
      INSERT INTO chunk_fts(rowid, lexical_text) VALUES (new.id, new.lexical_text); END;
    CREATE TRIGGER IF NOT EXISTS chunks_delete AFTER DELETE ON chunks BEGIN
      INSERT INTO chunk_fts(chunk_fts, rowid, lexical_text) VALUES ('delete', old.id, old.lexical_text); END;
    CREATE TRIGGER IF NOT EXISTS chunks_update AFTER UPDATE ON chunks BEGIN
      INSERT INTO chunk_fts(chunk_fts, rowid, lexical_text) VALUES ('delete', old.id, old.lexical_text);
      INSERT INTO chunk_fts(rowid, lexical_text) VALUES (new.id, new.lexical_text); END;
    CREATE TABLE IF NOT EXISTS index_jobs (job_id TEXT PRIMARY KEY, state TEXT NOT NULL, source_count INTEGER NOT NULL,
      completed_count INTEGER NOT NULL, created_at TEXT NOT NULL, error_code TEXT);
    PRAGMA user_version = 1;`);
  if (!database.prepare('PRAGMA table_info(sources)').all().some(row => row.name === 'embedding_signature'))
    database.exec("ALTER TABLE sources ADD COLUMN embedding_signature TEXT NOT NULL DEFAULT ''");
  if (!database.prepare('PRAGMA table_info(chunks)').all().some(row => row.name === 'embedding_model_version'))
    database.exec("ALTER TABLE chunks ADD COLUMN embedding_model_version TEXT NOT NULL DEFAULT ''");
  database.prepare("INSERT OR IGNORE INTO retrieval_metadata(key,value) VALUES ('index_epoch',?)").run(randomUUID());
  // A deletion ledger is authoritative even after a crash before index cleanup.
  // 明确撤销登记是权威状态，崩溃后也不能保留尚未清掉的派生索引。
  const registry = identities();
  for (const entry of Object.values(registry.sources)) {
    if (entry.tombstone) database.prepare('DELETE FROM sources WHERE source_id=?').run(entry.sourceId);
  }
  await migrateLexicalIndex();
  try {
    if (workerData.vectorEnabled === false) throw retrievalFailure('Vector extension disabled. / 已关闭向量扩展。', 'RETRIEVAL_VECTOR_DISABLED');
    const sqliteVec = await import('sqlite-vec');
    sqliteVec.load(database);
    vectorVersion = database.prepare('SELECT vec_version() AS version').get().version;
    vectorAvailable = true;
  } catch (error) {
    vectorError = error.code ?? 'RETRIEVAL_VECTOR_UNAVAILABLE';
  } finally {
    // Extension paths are package-owned; arbitrary settings cannot load another DLL.
    // 原生扩展只从随包依赖加载，设置不能指定任意 DLL。
    database.enableLoadExtension(false);
  }
}

async function migrateLexicalIndex() {
  const staleChunks = database.prepare(`SELECT c.id,c.text,s.title,s.locator,s.scope_key FROM chunks c
    JOIN sources s ON s.source_id=c.source_id WHERE c.tokenizer_version=? ORDER BY c.id LIMIT 256`);
  const updateChunk = database.prepare('UPDATE chunks SET lexical_text=?,tokenizer_version=? WHERE id=?');
  for (;;) {
    const rows = staleChunks.all('han-bigram-code-v1');
    if (!rows.length) break;
    // Each batch changes derived lexical fields only; crashes can resume without replacing identities or vectors.
    // 每批只更新派生词法字段；崩溃后可续迁，不重写身份、撤销记录、原文、偏移或向量。
    transaction(() => {
      const scopes = new Set();
      for (const row of rows) {
        const locator = JSON.parse(row.locator);
        updateChunk.run(lexicalText(`${row.title} ${locator.relativePath ?? ''} ${row.text}`), TOKENIZER_VERSION, row.id);
        scopes.add(row.scope_key);
      }
      for (const scope of scopes) nextGeneration(scope);
    });
    await new Promise(resolveBatch => setImmediate(resolveBatch));
  }
  database.prepare("INSERT INTO retrieval_metadata(key,value) VALUES ('tokenizer_version',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
    .run(TOKENIZER_VERSION);
}

function cancelled(flag) {
  if (flag && Atomics.load(flag, 0)) throw Object.assign(new Error('Retrieval cancelled. / 检索已取消。'), { name: 'AbortError', code: 'ABORT_ERR' });
}

function transaction(operation) {
  database.exec('BEGIN IMMEDIATE');
  try { const result = operation(); database.exec('COMMIT'); return result; }
  catch (error) { database.exec('ROLLBACK'); throw error; }
}

function identities() {
  checkPath(identityPath);
  if (!existsSync(identityPath)) return { schemaVersion: 1, revision: 0, sources: {} };
  const info = lstatSync(identityPath);
  if (info.size > 32 * 1024 * 1024) throw retrievalFailure('Source identity registry is too large. / 资料身份登记过大。', 'CORRUPT_RETRIEVAL_IDENTITIES', 409);
  let value;
  try { value = JSON.parse(readFileSync(identityPath, 'utf8')); }
  catch { throw retrievalFailure('Invalid source identity registry. / 资料身份登记损坏，原文件已保留。', 'CORRUPT_RETRIEVAL_IDENTITIES', 409); }
  if (value.schemaVersion !== 1 || !Number.isSafeInteger(value.revision) || !value.sources || typeof value.sources !== 'object' || Array.isArray(value.sources))
    throw retrievalFailure('Unsupported source identity registry. / 资料身份登记版本无效，原文件已保留。', 'CORRUPT_RETRIEVAL_IDENTITIES', 409);
  return value;
}

function persistIdentities(value) {
  checkPath(identityPath);
  const temporary = `${identityPath}.${randomUUID()}.tmp`;
  try {
    writeFileSync(temporary, JSON.stringify(value, null, 2), { flag: 'wx', mode: 0o600 });
    checkPath(identityPath);
    renameSync(temporary, identityPath);
  } finally { if (existsSync(temporary)) unlinkSync(temporary); }
}

function prepareSource(input) {
  const source = validateSource(input);
  const chunks = input.chunks ?? chunkSource(source);
  if (!Array.isArray(chunks) || chunks.length > 40000) throw retrievalFailure('Invalid source chunks. / 资料分块无效。');
  for (const [index, chunk] of chunks.entries()) {
    if (!chunk || chunk.chunkIndex !== index || typeof chunk.text !== 'string' ||
        !Number.isSafeInteger(chunk.startOffset) || !Number.isSafeInteger(chunk.endOffset) ||
        chunk.startOffset < 0 || chunk.endOffset > source.text.length || chunk.endOffset <= chunk.startOffset ||
        source.text.slice(chunk.startOffset, chunk.endOffset) !== chunk.text || hashText(chunk.text) !== chunk.chunkHash)
      throw retrievalFailure('Source chunks do not match the original. / 资料分块与原文不一致。', 'RETRIEVAL_SOURCE_CHANGED', 409);
  }
  const vectors = input.vectors ?? [];
  if (!Array.isArray(vectors) || (vectors.length && vectors.length !== chunks.length))
    throw retrievalFailure('Vector and chunk counts differ. / 向量数量与分块数量不一致。');
  let dimensions = null;
  for (const vector of vectors) {
    if (vector === null || vector === undefined) continue;
    if (!(Array.isArray(vector) || vector instanceof Float32Array) || !vector.length || vector.length > 4096 ||
        !Array.from(vector).every(item => Number.isFinite(item)) || !Array.from(vector).some(item => item !== 0))
      throw retrievalFailure('Invalid embedding vector. / 嵌入向量无效。');
    if (dimensions !== null && dimensions !== vector.length) throw retrievalFailure('Embedding dimensions differ. / 嵌入维度不一致。');
    dimensions = vector.length;
  }
  if (dimensions !== null && (typeof input.embeddingProfileId !== 'string' || !input.embeddingProfileId || input.embeddingProfileId.length > 256))
    throw retrievalFailure('An embedding profile is required. / 向量必须关联嵌入模型配置。');
  if (input.embeddingModelVersion !== undefined && (typeof input.embeddingModelVersion !== 'string' || input.embeddingModelVersion.length > 512))
    throw retrievalFailure('Invalid embedding model version. / 嵌入模型版本无效。');
  const embeddingSignature = dimensions === null ? null : hashText(JSON.stringify({
    profile: source.embeddingProfileId, modelVersion: source.embeddingModelVersion ?? '', dimensions, chunks: chunks.map(chunk => chunk.chunkId),
    vectors: vectors.map(vector => vector ? Buffer.from(new Float32Array(vector).buffer).toString('base64') : null)
  }));
  return { source, chunks, vectors, dimensions, embeddingSignature };
}

function upsertSources({ sources }, flag) {
  if (!Array.isArray(sources) || sources.length > 100)
    throw retrievalFailure('A bounded source batch is required. / 必须提供有界资料批次。');
  if (sources.reduce((total, source) => total + (typeof source?.text === 'string' ? source.text.length : 0), 0) > 8 * 1024 * 1024)
    throw retrievalFailure('Source batch is too large. / 资料批次过大，请分批索引。', 'RETRIEVAL_BATCH_TOO_LARGE');
  cancelled(flag);
  const prepared = sources.map(prepareSource);
  const registry = identities();
  const jobId = randomUUID(), createdAt = new Date().toISOString();
  database.prepare('INSERT INTO index_jobs VALUES (?, ?, ?, ?, ?, ?)').run(jobId, 'running', sources.length, 0, createdAt, null);
  try {
    // Publish identity before disposable index data. A crash may leave an unindexed registered
    // source, but rebuilding can never invent another identity for the same reference.
    // 先登记身份再发布可重建索引；崩溃最多留下待索引资料，不会为已有引用另造身份。
    for (const { source } of prepared) {
      const key = hashText(source.sourceId);
      const old = registry.sources[key];
      if (old && old.scopeKey !== source.scopeKey)
        throw retrievalFailure('Source identity belongs to another scope. / 资料身份已属于其他范围。', 'RETRIEVAL_SCOPE_MISMATCH', 409);
      if (old?.tombstone)
        throw retrievalFailure('Source identity was permanently revoked. / 此资料身份已永久撤销，不能由迟到任务恢复。', 'RETRIEVAL_SOURCE_REVOKED', 409);
      const current = database.prepare('SELECT source_revision, content_hash, binding_revision FROM sources WHERE source_id = ?').get(source.sourceId);
      if (current) {
        const revision = JSON.parse(current.source_revision);
        if ((typeof source.sourceRevision === 'number' && typeof revision === 'number' && source.sourceRevision < revision) ||
            (source.bindingRevision ?? 0) < current.binding_revision)
          throw retrievalFailure('An outdated source update was rejected. / 已拒绝过期资料更新。', 'STALE_RETRIEVAL_SOURCE', 409);
        if (source.sourceRevision === revision && source.contentHash !== current.content_hash)
          throw retrievalFailure('Source changed without a new revision. / 资料内容已改变但版本未更新。', 'RETRIEVAL_SOURCE_CHANGED', 409);
      }
      registry.sources[key] = { sourceId: source.sourceId, scopeKey: source.scopeKey, sourceRevision: source.sourceRevision,
        contentHash: source.contentHash, locator: source.locator, sourceType: source.sourceType,
        bindingRevision: source.bindingRevision ?? 0, active: true, sourceRef: sourceReference(source) };
    }
    registry.revision++;
    persistIdentities(registry);
    const result = transaction(() => {
      const changedScopes = new Set(), published = [];
      for (const { source, chunks, vectors, dimensions, embeddingSignature } of prepared) {
        cancelled(flag);
        const current = database.prepare('SELECT * FROM sources WHERE source_id = ?').get(source.sourceId);
        // A lexical-only refresh cannot silently discard existing vectors for unchanged content.
        // 正文不变的词法刷新不能静默丢弃已经生成的向量。
        const hasNewVectors = vectors.some(vector => vector !== null && vector !== undefined);
        if (current?.content_hash === source.contentHash && current.source_revision === JSON.stringify(source.sourceRevision) &&
            current.binding_revision === (source.bindingRevision ?? 0) && (!hasNewVectors || current.embedding_signature === embeddingSignature) &&
            current.locator === JSON.stringify(source.locator) && current.title === source.title && current.source_type === source.sourceType) {
          published.push({ sourceId: source.sourceId, generation: current.active_generation, unchanged: true, sourceRef: sourceReference(source) });
          continue;
        }
        const value = nextGeneration(source.scopeKey);
        changedScopes.add(source.scopeKey);
        database.prepare(`INSERT INTO sources(source_id,scope_key,source_type,title,locator,text,content_hash,source_revision,binding_revision,active_generation,updated_at,embedding_signature)
          VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
          ON CONFLICT(source_id) DO UPDATE SET scope_key=excluded.scope_key, source_type=excluded.source_type,
          title=excluded.title, locator=excluded.locator, text=excluded.text, content_hash=excluded.content_hash,
          source_revision=excluded.source_revision, binding_revision=excluded.binding_revision,
          active_generation=excluded.active_generation, updated_at=excluded.updated_at, embedding_signature=excluded.embedding_signature`)
          .run(source.sourceId, source.scopeKey, source.sourceType, source.title, JSON.stringify(source.locator), source.text,
            source.contentHash, JSON.stringify(source.sourceRevision), source.bindingRevision ?? 0, value, createdAt, embeddingSignature ?? '');
        database.prepare('DELETE FROM chunks WHERE source_id = ?').run(source.sourceId);
        const insertChunk = database.prepare('INSERT INTO chunks(chunk_id,source_id,chunk_index,text,lexical_text,chunk_hash,start_offset,end_offset,start_line,end_line,embedding_profile_id,dimensions,vector,chunker_version,tokenizer_version,embedding_model_version) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)');
        for (const chunk of chunks) {
          cancelled(flag);
          const vector = vectors[chunk.chunkIndex];
          const blob = vector ? new Uint8Array(new Float32Array(vector).buffer) : null;
          insertChunk.run(chunk.chunkId, source.sourceId, chunk.chunkIndex, chunk.text,
            lexicalText(`${source.title} ${source.locator.relativePath ?? ''} ${chunk.text}`), chunk.chunkHash,
            chunk.startOffset, chunk.endOffset, chunk.startLine, chunk.endLine, vector ? source.embeddingProfileId : null,
            vector ? dimensions : null, blob, chunk.chunkerVersion ?? CHUNKER_VERSION, TOKENIZER_VERSION,
            vector ? source.embeddingModelVersion ?? '' : '');
        }
        published.push({ sourceId: source.sourceId, generation: value, chunks: chunks.length, sourceRef: sourceReference(source) });
      }
      cancelled(flag);
      database.prepare('UPDATE index_jobs SET state = ?, completed_count = ? WHERE job_id = ?').run('completed', sources.length, jobId);
      return { jobId, generation: generation(), indexSnapshotId: snapshotId(), sources: published, changedScopes: [...changedScopes] };
    });
    return result;
  } catch (error) {
    database.prepare('UPDATE index_jobs SET state = ?, error_code = ? WHERE job_id = ?').run('failed', error.code ?? 'RETRIEVAL_INDEX_FAILED', jobId);
    throw error;
  }
}

function publicChunk(row, score = null) {
  const source = { sourceId: row.source_id, scopeKey: row.scope_key, sourceRevision: JSON.parse(row.source_revision), contentHash: row.content_hash };
  return { ...source, sourceType: row.source_type, title: row.title,
    locator: { ...JSON.parse(row.locator), startLine: row.start_line, endLine: row.end_line,
      startOffset: row.start_offset, endOffset: row.end_offset },
    bindingRevision: row.binding_revision, generation: row.active_generation,
    chunkId: row.chunk_id, chunkIndex: row.chunk_index, chunkHash: row.chunk_hash,
    excerpt: row.text, score,
    ...(row.lexical_position === undefined ? {} : { lexicalRank: row.lexical_position, lexicalScore: row.lexical_rank ?? null }),
    ...(row.vector_position === undefined ? {} : { vectorRank: row.vector_position, distance: row.distance }),
    sourceRef: sourceReference(source, { chunkId: row.chunk_id, chunkHash: row.chunk_hash }) };
}

function search({ query, scopeKeys, queryVector, embeddingProfileId, embeddingModelVersion, limit = 8 }, flag) {
  const scopes = retrievalScopeKeys(scopeKeys), placeholders = scopes.map(() => '?').join(',');
  cancelled(flag);
  const expression = matchExpression(query);
  let lexical = [];
  // Materialize the narrow FTS result once; fetch text and vectors only for authorized top candidates.
  // 窄 FTS 结果只物化一次；范围筛选及排序后才读取候选正文与向量，避免逐分块重复 FTS 扫描。
  if (expression) lexical = database.prepare(`WITH matches AS MATERIALIZED (
    SELECT rowid, bm25(chunk_fts) AS lexical_rank FROM chunk_fts WHERE chunk_fts MATCH ?),
    candidates AS MATERIALIZED (
      SELECT c.id, c.chunk_id, m.lexical_rank,
        CASE WHEN instr(lower(c.text),lower(?)) > 0 THEN 0 ELSE 1 END AS exact_rank
      FROM matches m JOIN chunks c ON c.id=m.rowid JOIN sources s ON s.source_id=c.source_id
      WHERE s.scope_key IN (${placeholders}) ORDER BY exact_rank, m.lexical_rank, c.chunk_id LIMIT 40)
    SELECT ${CHUNK_ROW_COLUMNS}, candidates.lexical_rank FROM candidates
    JOIN chunks c ON c.id=candidates.id JOIN sources s ON s.source_id=c.source_id
    ORDER BY candidates.exact_rank, candidates.lexical_rank, candidates.chunk_id`)
    .all(expression, query, ...scopes);
  else if (query.trim()) lexical = database.prepare(`SELECT ${CHUNK_ROW_COLUMNS} FROM chunks c JOIN sources s ON s.source_id=c.source_id
    WHERE s.scope_key IN (${placeholders}) AND instr(lower(c.text),lower(?)) > 0 ORDER BY c.id LIMIT 40`).all(...scopes, query.trim());
  let semantic = [], degradedReason = null;
  if (queryVector && embeddingProfileId) {
    if (!vectorAvailable) degradedReason = vectorError;
    else {
      const versionClause = embeddingModelVersion === undefined ? '' : ' AND c.embedding_model_version=?';
      const versionParameters = embeddingModelVersion === undefined ? [] : [embeddingModelVersion];
      const count = database.prepare(`SELECT count(*) AS count FROM chunks c JOIN sources s ON s.source_id=c.source_id
        WHERE s.scope_key IN (${placeholders}) AND c.embedding_profile_id=? AND c.dimensions=? AND c.vector IS NOT NULL${versionClause}`)
        .get(...scopes, embeddingProfileId, queryVector.length, ...versionParameters).count;
      if (count > MAX_VECTOR_SCAN_CHUNKS) degradedReason = 'RETRIEVAL_VECTOR_SCAN_LIMIT';
      else {
        // Materialization proves permission filtering happens BEFORE distance computation.
        // 物化已授权集合，确保先按权限筛选，再计算向量距离，禁止全库 Top-K 后过滤。
        semantic = database.prepare(`WITH authorized AS MATERIALIZED (
          SELECT c.id,c.chunk_id,c.vector FROM chunks c JOIN sources s ON s.source_id=c.source_id
          WHERE s.scope_key IN (${placeholders}) AND c.embedding_profile_id=? AND c.dimensions=? AND c.vector IS NOT NULL${versionClause})
          , candidates AS MATERIALIZED (
            SELECT id,chunk_id,vec_distance_cosine(vector,?) AS distance FROM authorized ORDER BY distance,chunk_id LIMIT 40)
          SELECT ${CHUNK_ROW_COLUMNS},candidates.distance FROM candidates
          JOIN chunks c ON c.id=candidates.id JOIN sources s ON s.source_id=c.source_id
          ORDER BY candidates.distance,candidates.chunk_id`)
          .all(...scopes, embeddingProfileId, queryVector.length, ...versionParameters, new Uint8Array(new Float32Array(queryVector).buffer));
      }
    }
  }
  cancelled(flag);
  const combined = new Map();
  for (const list of [lexical, semantic]) for (const [rank, row] of list.entries()) {
    const old = combined.get(row.chunk_id);
    const diagnostics = list === lexical ? { lexical_position: rank + 1 } : { vector_position: rank + 1 };
    combined.set(row.chunk_id, { row: { ...old?.row, ...row, ...diagnostics }, score: (old?.score ?? 0) + 1 / (60 + rank + 1) });
  }
  const items = [...combined.values()].sort((a, b) => b.score - a.score || a.row.chunk_id.localeCompare(b.row.chunk_id))
    .slice(0, limit).map(item => publicChunk(item.row, item.score));
  const snapshots = database.prepare(`SELECT scope_key AS scopeKey,generation FROM scope_snapshots WHERE scope_key IN (${placeholders}) ORDER BY scope_key`).all(...scopes)
    .map(item => ({ ...item, snapshotId: snapshotId(item.generation) }));
  return { items, generation: generation(), indexSnapshotId: snapshotId(), scopeSnapshots: snapshots, strategy: semantic.length ? 'hybrid' : 'lexical',
    vectorAvailable, ...(degradedReason ? { degradedReason } : {}) };
}

function readSourceRow({ sourceId, sourceRef, scopeKeys }, flag) {
  const scopes = retrievalScopeKeys(scopeKeys), reference = sourceRef ? parseSourceReference(sourceRef) : null;
  if (reference && !scopes.includes(reference.scopeKey)) throw retrievalFailure('Source is outside the authorized scope. / 资料不在授权范围内。', 'RETRIEVAL_SOURCE_NOT_FOUND', 404);
  const id = reference?.sourceId ?? sourceId;
  const row = database.prepare(`SELECT * FROM sources WHERE source_id = ? AND scope_key IN (${scopes.map(() => '?').join(',')})`).get(id, ...scopes);
  if (!row) throw retrievalFailure('Source is unavailable. / 资料不存在或已撤销。', 'RETRIEVAL_SOURCE_NOT_FOUND', 404);
  if (reference && (reference.scopeKey !== row.scope_key || reference.contentHash !== row.content_hash || JSON.stringify(reference.sourceRevision) !== row.source_revision))
    throw retrievalFailure('Source reference is outdated. / 资料引用已过期，请重新检索。', 'STALE_RETRIEVAL_SOURCE', 409);
  let chunk;
  if (reference?.chunkId) {
    chunk = database.prepare('SELECT chunk_hash,start_offset,end_offset FROM chunks WHERE chunk_id=? AND source_id=?').get(reference.chunkId, id);
    if (!chunk || chunk.chunk_hash !== reference.chunkHash) throw retrievalFailure('Chunk reference is outdated. / 分块引用已过期。', 'STALE_RETRIEVAL_SOURCE', 409);
  }
  cancelled(flag);
  return { row, id, reference, chunk };
}

function readMetadata(row, id) {
  return { sourceId: id, scopeKey: row.scope_key, sourceRevision: JSON.parse(row.source_revision), contentHash: row.content_hash,
    sourceType: row.source_type, title: row.title, locator: JSON.parse(row.locator), bindingRevision: row.binding_revision,
    sourceRef: sourceReference({ sourceId: id, scopeKey: row.scope_key, sourceRevision: JSON.parse(row.source_revision), contentHash: row.content_hash }) };
}

function read(input, flag) {
  const { offset = 0, limit = 12000 } = input, { row, id } = readSourceRow(input, flag);
  const content = row.text.slice(offset, offset + limit);
  return { ...readMetadata(row, id),
    text: content, offset, totalCharacters: row.text.length, hasMore: offset + content.length < row.text.length,
    nextOffset: offset + content.length };
}

function readWindow(input, flag) {
  const { row, id, chunk } = readSourceRow(input, flag);
  const window = sourceWindow(row.text, { ...input, anchorOffset: input.anchorOffset ?? chunk?.start_offset ?? 0,
    ...(input.anchorOffset === undefined && chunk ? { referenceRange: { startOffset: chunk.start_offset, endOffset: chunk.end_offset } } : {}) }, () => cancelled(flag));
  return { ...readMetadata(row, id), ...window };
}

function removeSource({ sourceId, scopeKeys, permanent = true }) {
  const scopes = retrievalScopeKeys(scopeKeys);
  const owner = database.prepare('SELECT scope_key FROM sources WHERE source_id=?').get(sourceId);
  if (owner && !scopes.includes(owner.scope_key)) return { removed: false, generation: generation(), indexSnapshotId: snapshotId() };
  const current = database.prepare(`SELECT * FROM sources WHERE source_id=? AND scope_key IN (${scopes.map(() => '?').join(',')})`).get(sourceId, ...scopes);
  const registry = identities(), key = hashText(sourceId);
  const existing = registry.sources[key];
  if (!current && existing && !scopes.includes(existing.scopeKey)) return { removed: false, generation: generation(), indexSnapshotId: snapshotId() };
  if (!current && !permanent) return { removed: false, generation: generation(), indexSnapshotId: snapshotId() };
  const scope = current?.scope_key ?? existing?.scopeKey ?? scopes[0];
  registry.sources[key] = { ...existing, sourceId, scopeKey: scope, active: false,
    ...(permanent || existing?.tombstone ? { tombstone: true } : {}) };
  registry.revision++;
  persistIdentities(registry);
  return transaction(() => {
    database.prepare('DELETE FROM sources WHERE source_id=?').run(sourceId);
    return { removed: Boolean(current), generation: nextGeneration(scope), indexSnapshotId: snapshotId() };
  });
}

function listSources({ scopeKeys, sourceType }) {
  const scopes = retrievalScopeKeys(scopeKeys);
  const filter = sourceType === undefined ? '' : ' AND source_type=?';
  const parameters = sourceType === undefined ? [] : [sourceType];
  return database.prepare(`SELECT source_id AS sourceId,scope_key AS scopeKey,source_type AS sourceType,
    title,locator,content_hash AS contentHash,source_revision AS sourceRevision,binding_revision AS bindingRevision,
    active_generation AS generation FROM sources WHERE scope_key IN (${scopes.map(() => '?').join(',')})${filter}
    ORDER BY source_id`).all(...scopes, ...parameters).map(row => ({ ...row,
      sourceRevision: JSON.parse(row.sourceRevision), locator: JSON.parse(row.locator) }));
}

function invalidateScope({ scopeKey }) {
  retrievalScopeKeys([scopeKey]);
  const registry = identities();
  for (const value of Object.values(registry.sources)) if (value.scopeKey === scopeKey) value.active = false;
  registry.revision++;
  persistIdentities(registry);
  return transaction(() => {
    const result = database.prepare('DELETE FROM sources WHERE scope_key=?').run(scopeKey);
    return { removed: result.changes, generation: nextGeneration(scopeKey), indexSnapshotId: snapshotId(), scopeKey };
  });
}

function status() {
  return { schemaVersion: RETRIEVAL_SCHEMA_VERSION, generation: generation(), indexSnapshotId: snapshotId(),
    tokenizerVersion: TOKENIZER_VERSION, chunkerVersion: CHUNKER_VERSION,
    sources: database.prepare('SELECT count(*) AS count FROM sources').get().count,
    chunks: database.prepare('SELECT count(*) AS count FROM chunks').get().count,
    vectorChunks: database.prepare('SELECT count(*) AS count FROM chunks WHERE vector IS NOT NULL').get().count,
    vectorAvailable, vectorVersion, vectorError, indexPath: filename,
    scopeSnapshots: database.prepare('SELECT scope_key AS scopeKey,generation FROM scope_snapshots ORDER BY scope_key').all()
      .map(item => ({ ...item, snapshotId: snapshotId(item.generation) })),
    jobs: Object.fromEntries(database.prepare('SELECT state,count(*) AS count FROM index_jobs GROUP BY state').all().map(row => [row.state, row.count])) };
}

function close() {
  if (!database) return { closed: true };
  const checkpoint = database.prepare('PRAGMA wal_checkpoint(TRUNCATE)').get();
  if (checkpoint.busy) throw retrievalFailure('Retrieval index is busy. / 检索索引正被使用，无法安全迁移。', 'RETRIEVAL_INDEX_BUSY', 409);
  database.close();
  database = null;
  return { closed: true, checkpointed: true };
}

await openDatabase();
parentPort.on('message', message => {
  const flag = message.cancelBuffer ? new Int32Array(message.cancelBuffer) : null;
  try {
    cancelled(flag);
    const operations = { upsertSources, search, read, readWindow, removeSource, listSources, invalidateScope, status, close };
    const operation = operations[message.method];
    if (!operation || !database) throw retrievalFailure('Retrieval index is closed. / 检索索引已关闭。', 'RETRIEVAL_INDEX_CLOSED', 409);
    const result = operation(message.input ?? {}, flag);
    parentPort.postMessage({ id: message.id, result });
  } catch (error) {
    parentPort.postMessage({ id: message.id, error: { name: error.name, message: error.message,
      code: error.code ?? 'RETRIEVAL_INDEX_FAILED', statusCode: error.statusCode ?? 500 } });
  }
});

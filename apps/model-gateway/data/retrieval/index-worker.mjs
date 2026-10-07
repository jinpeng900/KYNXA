import { randomUUID } from 'node:crypto';
import { existsSync, lstatSync, mkdirSync, readFileSync, renameSync, unlinkSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { parentPort, workerData } from 'node:worker_threads';
import { DatabaseSync } from 'node:sqlite';
import { RETRIEVAL_INDEX_SCHEMA_VERSION, hashText, parseSourceReference, retrievalFailure,
  retrievalScopeKeys, sourceReference, validateSource, validateStructureDescriptor, validateChunkStructure,
  validateRetrievalIntent, normalizeRetrievalPath } from './retrieval-contracts.mjs';
import { chunkSource, lexicalText, matchExpression, CHUNKER_VERSION, TOKENIZER_VERSION } from './retrieval-text.mjs';
import { sourceWindow } from './source-window.mjs';
import { deriveSourceVersion, sourceDerivationVersions } from './derivation-version.mjs';
import { RetrievalVectorSearch } from './vector-search.mjs';
import { RETRIEVAL_DOMAIN_SQL } from './ann-store.mjs';

const root = resolve(workerData.root);
const directory = join(root, 'Index');
const filename = join(directory, 'retrieval.sqlite');
const identityDirectory = join(root, 'Retrieval');
const identityPath = join(identityDirectory, 'source-identities.json');
const CHUNK_ROW_COLUMNS = 'c.*, s.scope_key, s.source_type, s.title, s.locator, s.content_hash, s.source_revision, s.binding_revision, s.active_generation, s.derivation_signature';
let database, vectorSearch, vectorAvailable = false, vectorVersion = null, vectorError = null;

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

function isCorpusSource(sourceType) {
  return sourceType === 'knowledge' || sourceType === 'work-file';
}

function nextGeneration(scopeKey, { corpusChanged = false } = {}) {
  const value = generation() + 1;
  database.prepare("UPDATE retrieval_metadata SET value = ? WHERE key = 'generation'").run(String(value));
  database.prepare('INSERT INTO scope_snapshots(scope_key, generation) VALUES (?, ?) ON CONFLICT(scope_key) DO UPDATE SET generation = excluded.generation')
    .run(scopeKey, value);
  // Vector publication and conversation updates must not invalidate unchanged corpus bodies.
  // 向量发布和对话更新仍推进检索代次，但不能使未改变的资料原文缓存失效。
  const corpusKey = `corpus_generation:${scopeKey}`;
  if (corpusChanged) database.prepare('INSERT INTO retrieval_metadata(key,value) VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value')
    .run(corpusKey, String(value));
  else database.prepare('INSERT OR IGNORE INTO retrieval_metadata(key,value) VALUES (?,?)').run(corpusKey, '0');
  return value;
}

async function openDatabase() {
  initializeDirectories();
  database = new DatabaseSync(filename, { allowExtension: true });
  const version = database.prepare('PRAGMA user_version').get().user_version;
  if (version > RETRIEVAL_INDEX_SCHEMA_VERSION) {
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
      embedding_signature TEXT NOT NULL DEFAULT '', derivation_signature TEXT NOT NULL DEFAULT '',
      embedding_input_signature TEXT NOT NULL DEFAULT '', parser_version TEXT NOT NULL DEFAULT '',
      embedding_input_version TEXT NOT NULL DEFAULT '', chunker_version TEXT NOT NULL DEFAULT '',
      tokenizer_version TEXT NOT NULL DEFAULT '', structure_json TEXT NOT NULL DEFAULT '',
      relative_path TEXT NOT NULL DEFAULT '');
    CREATE INDEX IF NOT EXISTS sources_scope ON sources(scope_key);
    CREATE TABLE IF NOT EXISTS chunks (
      id INTEGER PRIMARY KEY, chunk_id TEXT UNIQUE NOT NULL, source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE CASCADE,
      chunk_index INTEGER NOT NULL, text TEXT NOT NULL, lexical_text TEXT NOT NULL, chunk_hash TEXT NOT NULL,
      start_offset INTEGER NOT NULL, end_offset INTEGER NOT NULL, start_line INTEGER NOT NULL, end_line INTEGER NOT NULL,
      embedding_profile_id TEXT, dimensions INTEGER, vector BLOB, chunker_version TEXT NOT NULL, tokenizer_version TEXT NOT NULL,
      embedding_model_version TEXT NOT NULL DEFAULT '', embedding_space_id TEXT NOT NULL DEFAULT '',
      structure_json TEXT NOT NULL DEFAULT '', structure_domain TEXT NOT NULL DEFAULT '',
      symbol_name TEXT NOT NULL DEFAULT '', qualified_name TEXT NOT NULL DEFAULT '',
      parse_status TEXT NOT NULL DEFAULT '', unit_start_offset INTEGER, unit_end_offset INTEGER);
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
      completed_count INTEGER NOT NULL, created_at TEXT NOT NULL, error_code TEXT);`);
  // Additive migration preserves old rows, identities, vectors and deletion ledgers.
  // 只增加派生版本列，保留旧行、身份、向量和撤销登记；下次发布时再验证旧派生数据。
  transaction(() => {
    const sourceColumns = new Set(database.prepare('PRAGMA table_info(sources)').all().map(row => row.name));
    for (const column of ['embedding_signature', 'derivation_signature', 'embedding_input_signature', 'parser_version', 'embedding_input_version', 'chunker_version', 'tokenizer_version', 'structure_json', 'relative_path']) {
      if (!sourceColumns.has(column)) database.exec(`ALTER TABLE sources ADD COLUMN ${column} TEXT NOT NULL DEFAULT ''`);
    }
    const chunkColumns = new Set(database.prepare('PRAGMA table_info(chunks)').all().map(row => row.name));
    for (const column of ['embedding_model_version', 'embedding_space_id', 'structure_json', 'structure_domain', 'symbol_name', 'qualified_name', 'parse_status']) {
      if (!chunkColumns.has(column)) database.exec(`ALTER TABLE chunks ADD COLUMN ${column} TEXT NOT NULL DEFAULT ''`);
    }
    for (const column of ['unit_start_offset', 'unit_end_offset']) {
      if (!chunkColumns.has(column)) database.exec(`ALTER TABLE chunks ADD COLUMN ${column} INTEGER`);
    }
    if (!sourceColumns.has('relative_path')) {
      const updatePath = database.prepare('UPDATE sources SET relative_path=? WHERE source_id=?');
      for (const row of database.prepare('SELECT source_id,locator FROM sources').iterate()) {
        const path = JSON.parse(row.locator).relativePath;
        if (typeof path === 'string') updatePath.run(normalizeRetrievalPath(path), row.source_id);
      }
    }
    database.exec('CREATE INDEX IF NOT EXISTS sources_relative_path ON sources(relative_path,scope_key)');
    database.exec('CREATE INDEX IF NOT EXISTS chunks_symbol ON chunks(symbol_name,structure_domain,parse_status)');
    database.exec('CREATE INDEX IF NOT EXISTS chunks_qualified_name ON chunks(qualified_name,structure_domain,parse_status)');
    database.exec('CREATE INDEX IF NOT EXISTS chunks_space ON chunks(embedding_space_id,embedding_profile_id,dimensions)');
    database.exec(`PRAGMA user_version = ${RETRIEVAL_INDEX_SCHEMA_VERSION}`);
  });
  database.prepare("INSERT OR IGNORE INTO retrieval_metadata(key,value) VALUES ('index_epoch',?)").run(randomUUID());
  // Existing indexes conservatively inherit their last scope generation once; fresh scopes begin at zero.
  // 既有索引只在首次升级时保守继承范围代次；新范围从零开始，不要求重建数据库。
  database.exec("INSERT OR IGNORE INTO retrieval_metadata(key,value) SELECT 'corpus_generation:' || scope_key,CAST(generation AS TEXT) FROM scope_snapshots");
  // A deletion ledger is authoritative even after a crash before index cleanup.
  // 明确撤销登记是权威状态，崩溃后也不能保留尚未清掉的派生索引。
  const registry = identities();
  transaction(() => {
    for (const entry of Object.values(registry.sources)) {
      if (!entry.tombstone) continue;
      const source = database.prepare('SELECT scope_key,source_type FROM sources WHERE source_id=?').get(entry.sourceId);
      if (source) {
        database.prepare('DELETE FROM sources WHERE source_id=?').run(entry.sourceId);
        nextGeneration(source.scope_key, { corpusChanged: isCorpusSource(source.source_type) });
      }
    }
  });
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
  vectorSearch = new RetrievalVectorSearch({ database, directory,
    epoch: database.prepare("SELECT value FROM retrieval_metadata WHERE key='index_epoch'").get().value, options: workerData.ann });
}

async function migrateLexicalIndex() {
  const staleChunks = database.prepare(`SELECT c.id,c.source_id,c.text,s.title,s.locator,s.scope_key,s.source_type FROM chunks c
    JOIN sources s ON s.source_id=c.source_id WHERE c.tokenizer_version=? ORDER BY c.id LIMIT 256`);
  const updateChunk = database.prepare('UPDATE chunks SET lexical_text=?,tokenizer_version=? WHERE id=?');
  for (;;) {
    const rows = staleChunks.all('han-bigram-code-v1');
    if (!rows.length) break;
    // Each batch changes derived lexical fields only; crashes can resume without replacing identities or vectors.
    // 每批只更新派生词法字段；崩溃后可续迁，不重写身份、撤销记录、原文、偏移或向量。
    transaction(() => {
      const scopes = new Set(), corpusScopes = new Set(), affectedSources = new Set();
      for (const row of rows) {
        const locator = JSON.parse(row.locator);
        updateChunk.run(lexicalText(`${row.title} ${locator.relativePath ?? ''} ${row.text}`), TOKENIZER_VERSION, row.id);
        scopes.add(row.scope_key);
        if (isCorpusSource(row.source_type)) corpusScopes.add(row.scope_key);
        affectedSources.add(row.source_id);
      }
      for (const sourceId of affectedSources) refreshMigratedDerivation(sourceId);
      for (const scope of scopes) nextGeneration(scope, { corpusChanged: corpusScopes.has(scope) });
    });
    await new Promise(resolveBatch => setImmediate(resolveBatch));
  }
  database.prepare("INSERT INTO retrieval_metadata(key,value) VALUES ('tokenizer_version',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
    .run(TOKENIZER_VERSION);
}

function refreshMigratedDerivation(sourceId) {
  const row = database.prepare('SELECT * FROM sources WHERE source_id=?').get(sourceId);
  // Legacy rows have unknown projection provenance; retain them until a verified upsert rederives them.
  // 旧行缺少投影来源，保留原状态，等经过验证的发布重新派生，不能凭迁移猜测旧向量身份。
  if (!row.derivation_signature) return;
  const source = { sourceId, scopeKey: row.scope_key, sourceType: row.source_type, title: row.title,
    locator: JSON.parse(row.locator), text: row.text, contentHash: row.content_hash,
    sourceRevision: JSON.parse(row.source_revision), bindingRevision: row.binding_revision,
    parserVersion: row.parser_version || undefined, chunkerVersion: row.chunker_version || undefined,
    tokenizerVersion: row.tokenizer_version === 'han-bigram-code-v1' ? TOKENIZER_VERSION : row.tokenizer_version || undefined,
    embeddingInputVersion: row.embedding_input_version || undefined,
    ...(row.structure_json ? { structure: JSON.parse(row.structure_json) } : {}) };
  const chunks = database.prepare('SELECT * FROM chunks WHERE source_id=? ORDER BY chunk_index').all(sourceId)
    .map(chunk => ({ chunkId: chunk.chunk_id, chunkIndex: chunk.chunk_index, chunkHash: chunk.chunk_hash,
      text: chunk.text, startOffset: chunk.start_offset, endOffset: chunk.end_offset,
      startLine: chunk.start_line, endLine: chunk.end_line, chunkerVersion: chunk.chunker_version,
      tokenizerVersion: chunk.tokenizer_version,
      ...(chunk.structure_json ? { structure: JSON.parse(chunk.structure_json) } : {}) }));
  const derived = deriveSourceVersion(source, chunks);
  database.prepare('UPDATE sources SET derivation_signature=?,tokenizer_version=? WHERE source_id=?')
    .run(derived.derivationSignature, derived.tokenizerVersion, sourceId);
}

function cancelled(flag) {
  if (flag && Atomics.load(flag, 0)) throw Object.assign(new Error('Retrieval cancelled. / 检索已取消。'), { name: 'AbortError', code: 'ABORT_ERR' });
}

function transaction(operation, flag) {
  database.exec('BEGIN IMMEDIATE');
  try { const result = operation(); cancelled(flag); database.exec('COMMIT'); return result; }
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

function prepareSource(input, flag) {
  cancelled(flag);
  let source = validateSource(input);
  const structure = validateStructureDescriptor(source.structure);
  if (structure) {
    if (source.parserVersion !== undefined && source.parserVersion !== structure.parserVersion)
      throw retrievalFailure('Source parser versions differ. / 资料解析版本不一致。', 'INVALID_RETRIEVAL_STRUCTURE');
    source = { ...source, structure, parserVersion: structure.parserVersion };
  }
  const versions = sourceDerivationVersions(source);
  const suppliedChunks = input.chunks ?? chunkSource(source);
  if (!Array.isArray(suppliedChunks) || suppliedChunks.length > 40000) throw retrievalFailure('Invalid source chunks. / 资料分块无效。');
  const chunks = suppliedChunks.map(chunk => {
    if (!chunk || typeof chunk !== 'object' || Array.isArray(chunk)) return null;
    if (chunk.chunkerVersion !== undefined && chunk.tokenizerVersion !== undefined) return chunk;
    return { ...chunk, chunkerVersion: chunk.chunkerVersion ?? versions.chunkerVersion,
      tokenizerVersion: chunk.tokenizerVersion ?? versions.tokenizerVersion };
  });
  if (!chunks.length && source.text.trim()) throw retrievalFailure('Source chunks are missing. / 资料正文缺少分块。', 'RETRIEVAL_SOURCE_CHANGED', 409);
  const chunkIds = new Set();
  let offsetCursor = 0, lineNumber = 1, previousEnd = 0;
  for (const [index, chunk] of chunks.entries()) {
    cancelled(flag);
    if (!chunk || chunk.chunkIndex !== index || typeof chunk.text !== 'string' ||
        typeof chunk.chunkId !== 'string' || !chunk.chunkId || chunk.chunkId.length > 256 || /[\x00-\x1f]/u.test(chunk.chunkId) || chunkIds.has(chunk.chunkId) ||
        !Number.isSafeInteger(chunk.startOffset) || !Number.isSafeInteger(chunk.endOffset) ||
        chunk.startOffset < previousEnd || chunk.endOffset > source.text.length || chunk.endOffset <= chunk.startOffset ||
        source.text.slice(chunk.startOffset, chunk.endOffset) !== chunk.text || hashText(chunk.text) !== chunk.chunkHash)
      throw retrievalFailure('Source chunks do not match the original. / 资料分块与原文不一致。', 'RETRIEVAL_SOURCE_CHANGED', 409);
    for (; offsetCursor < chunk.startOffset; offsetCursor++) if (source.text[offsetCursor] === '\n') lineNumber++;
    if (chunk.startLine !== lineNumber) throw retrievalFailure('Chunk line range is invalid. / 分块行号与原文不一致。', 'RETRIEVAL_SOURCE_CHANGED', 409);
    for (; offsetCursor < chunk.endOffset; offsetCursor++) if (source.text[offsetCursor] === '\n') lineNumber++;
    const splitsStartCodePoint = /[\uD800-\uDBFF]/u.test(source.text[chunk.startOffset - 1] ?? '') && /[\uDC00-\uDFFF]/u.test(chunk.text[0]);
    const splitsEndCodePoint = /[\uD800-\uDBFF]/u.test(chunk.text.at(-1)) && /[\uDC00-\uDFFF]/u.test(source.text[chunk.endOffset] ?? '');
    if (chunk.endLine !== lineNumber || splitsStartCodePoint || splitsEndCodePoint)
      throw retrievalFailure('Chunk line or Unicode range is invalid. / 分块行号或 Unicode 边界无效。', 'RETRIEVAL_SOURCE_CHANGED', 409);
    chunkIds.add(chunk.chunkId);
    previousEnd = chunk.endOffset;
    if (chunk.structure !== undefined) chunks[index] = { ...chunk, structure: validateChunkStructure(chunk.structure, source, chunk) };
  }
  const vectors = input.vectors ?? [];
  if (!Array.isArray(vectors) || (vectors.length && vectors.length !== chunks.length))
    throw retrievalFailure('Vector and chunk counts differ. / 向量数量与分块数量不一致。');
  let dimensions = null;
  for (const vector of vectors) {
    cancelled(flag);
    if (vector === null || vector === undefined) continue;
    if (!(Array.isArray(vector) || vector instanceof Float32Array) || !vector.length || vector.length > 4096 ||
        !Array.from(vector).every(item => Number.isFinite(item)) || !Array.from(vector).some(item => item !== 0))
      throw retrievalFailure('Invalid embedding vector. / 嵌入向量无效。');
    const encodedVector = new Float32Array(vector);
    if (!encodedVector.every(item => Number.isFinite(item)) || !encodedVector.some(item => item !== 0))
      throw retrievalFailure('Invalid float32 embedding vector. / float32 嵌入向量无效。');
    if (dimensions !== null && dimensions !== vector.length) throw retrievalFailure('Embedding dimensions differ. / 嵌入维度不一致。');
    dimensions = vector.length;
  }
  if ((dimensions !== null || input.embeddingProfileId !== undefined) &&
      (typeof input.embeddingProfileId !== 'string' || !input.embeddingProfileId.trim() || input.embeddingProfileId.length > 256 || /[\x00-\x1f]/u.test(input.embeddingProfileId)))
    throw retrievalFailure('An embedding profile is required. / 向量必须关联嵌入模型配置。');
  if (input.embeddingModelVersion !== undefined && (typeof input.embeddingModelVersion !== 'string' || input.embeddingModelVersion.length > 512))
    throw retrievalFailure('Invalid embedding model version. / 嵌入模型版本无效。');
  if (input.embeddingSpaceId !== undefined && (typeof input.embeddingSpaceId !== 'string' || !/^[a-f0-9]{64}$/u.test(input.embeddingSpaceId)))
    throw retrievalFailure('Invalid embedding space. / 嵌入空间身份无效。');
  const derivation = deriveSourceVersion(source, chunks, { checkCancelled: () => cancelled(flag) });
  source = { ...source, ...derivation };
  const embeddingSignature = dimensions === null ? null : hashText(JSON.stringify({
    profile: source.embeddingProfileId, modelVersion: source.embeddingModelVersion ?? '', space: source.embeddingSpaceId ?? '',
    dimensions, inputSignature: derivation.embeddingInputSignature, chunks: chunks.map(chunk => chunk.chunkId),
    vectors: vectors.map(vector => {
      cancelled(flag);
      return vector ? Buffer.from(new Float32Array(vector).buffer).toString('base64') : null;
    })
  }));
  return { source, chunks, vectors, dimensions, embeddingSignature };
}

async function upsertSources({ sources }, flag) {
  if (!Array.isArray(sources) || sources.length > 100)
    throw retrievalFailure('A bounded source batch is required. / 必须提供有界资料批次。');
  if (sources.reduce((total, source) => total + (typeof source?.text === 'string' ? source.text.length : 0), 0) > 8 * 1024 * 1024)
    throw retrievalFailure('Source batch is too large. / 资料批次过大，请分批索引。', 'RETRIEVAL_BATCH_TOO_LARGE');
  cancelled(flag);
  const sourceIds = new Set();
  const prepared = sources.map(source => {
    const item = prepareSource(source, flag);
    if (sourceIds.has(item.source.sourceId)) throw retrievalFailure('Duplicate source in batch. / 资料批次包含重复身份。');
    sourceIds.add(item.source.sourceId);
    return item;
  });
  const previousVectors = vectorSearch.sourceRows([...sourceIds]);
  const registry = identities();
  const jobId = randomUUID(), createdAt = new Date().toISOString();
  database.prepare('INSERT INTO index_jobs VALUES (?, ?, ?, ?, ?, ?)').run(jobId, 'running', sources.length, 0, createdAt, null);
  try {
    // Publish identity before disposable index data. A crash may leave an unindexed registered
    // source, but rebuilding can never invent another identity for the same reference.
    // 先登记身份再发布可重建索引；崩溃最多留下待索引资料，不会为已有引用另造身份。
    for (const { source } of prepared) {
      cancelled(flag);
      const key = hashText(source.sourceId);
      const old = registry.sources[key];
      if (old && old.scopeKey !== source.scopeKey)
        throw retrievalFailure('Source identity belongs to another scope. / 资料身份已属于其他范围。', 'RETRIEVAL_SCOPE_MISMATCH', 409);
      if (old?.tombstone)
        throw retrievalFailure('Source identity was permanently revoked. / 此资料身份已永久撤销，不能由迟到任务恢复。', 'RETRIEVAL_SOURCE_REVOKED', 409);
      const current = database.prepare('SELECT source_revision, content_hash, binding_revision,scope_key FROM sources WHERE source_id = ?').get(source.sourceId);
      if (current) {
        if (current.scope_key !== source.scopeKey) throw retrievalFailure('Source identity belongs to another scope. / 资料身份已属于其他范围。', 'RETRIEVAL_SCOPE_MISMATCH', 409);
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
    cancelled(flag);
    registry.revision++;
    persistIdentities(registry);
    const result = transaction(() => {
      const changedScopes = new Set(), published = [];
      for (const { source, chunks, vectors, dimensions, embeddingSignature } of prepared) {
        cancelled(flag);
        const current = database.prepare('SELECT * FROM sources WHERE source_id = ?').get(source.sourceId);
        // Content equality is not derivation equality; only identical embedding inputs may retain vectors.
        // 原文相同不代表派生相同；只有嵌入输入和空间兼容时，词法刷新才保留向量。
        const hasNewVectors = vectors.some(vector => vector !== null && vector !== undefined);
        const previousChunks = current && !hasNewVectors ? database.prepare('SELECT * FROM chunks WHERE source_id=? ORDER BY chunk_index').all(source.sourceId) : [];
        const canRetainVectors = current?.embedding_input_signature === source.embeddingInputSignature &&
          previousChunks.length === chunks.length && previousChunks.every((row, index) => {
            cancelled(flag);
            return row.chunk_id === chunks[index].chunkId && row.chunk_hash === chunks[index].chunkHash &&
              row.start_offset === chunks[index].startOffset && row.end_offset === chunks[index].endOffset &&
              (!row.vector || (source.embeddingProfileId === undefined || source.embeddingProfileId === row.embedding_profile_id) &&
                (source.embeddingModelVersion === undefined || source.embeddingModelVersion === row.embedding_model_version) &&
                (source.embeddingSpaceId === undefined || source.embeddingSpaceId === row.embedding_space_id));
          });
        const existingVectors = previousChunks.some(chunk => chunk.vector !== null);
        if (current?.derivation_signature === source.derivationSignature &&
            (hasNewVectors ? current.embedding_signature === embeddingSignature : !existingVectors || canRetainVectors)) {
          published.push({ sourceId: source.sourceId, generation: current.active_generation, unchanged: true, sourceRef: sourceReference(source) });
          continue;
        }
        const corpusChanged = (isCorpusSource(source.sourceType) || isCorpusSource(current?.source_type)) &&
          current?.derivation_signature !== source.derivationSignature;
        const value = nextGeneration(source.scopeKey, { corpusChanged });
        changedScopes.add(source.scopeKey);
        const retainedSignature = !hasNewVectors && canRetainVectors ? current.embedding_signature : '';
        database.prepare(`INSERT INTO sources(source_id,scope_key,source_type,title,locator,text,content_hash,source_revision,binding_revision,active_generation,updated_at,embedding_signature,derivation_signature,embedding_input_signature,parser_version,embedding_input_version,chunker_version,tokenizer_version,structure_json,relative_path)
          VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
          ON CONFLICT(source_id) DO UPDATE SET scope_key=excluded.scope_key, source_type=excluded.source_type,
          title=excluded.title, locator=excluded.locator, text=excluded.text, content_hash=excluded.content_hash,
          source_revision=excluded.source_revision, binding_revision=excluded.binding_revision,
          active_generation=excluded.active_generation, updated_at=excluded.updated_at, embedding_signature=excluded.embedding_signature,
          derivation_signature=excluded.derivation_signature,embedding_input_signature=excluded.embedding_input_signature,
          parser_version=excluded.parser_version,embedding_input_version=excluded.embedding_input_version,
          chunker_version=excluded.chunker_version,tokenizer_version=excluded.tokenizer_version,
          structure_json=excluded.structure_json,relative_path=excluded.relative_path`)
          .run(source.sourceId, source.scopeKey, source.sourceType, source.title, JSON.stringify(source.locator), source.text,
            source.contentHash, JSON.stringify(source.sourceRevision), source.bindingRevision ?? 0, value, createdAt, embeddingSignature ?? retainedSignature,
            source.derivationSignature, source.embeddingInputSignature, source.parserVersion, source.embeddingInputVersion,
            source.chunkerVersion, source.tokenizerVersion, source.structure ? JSON.stringify(source.structure) : '',
            typeof source.locator.relativePath === 'string' ? normalizeRetrievalPath(source.locator.relativePath) : '');
        database.prepare('DELETE FROM chunks WHERE source_id = ?').run(source.sourceId);
        const insertChunk = database.prepare('INSERT INTO chunks(chunk_id,source_id,chunk_index,text,lexical_text,chunk_hash,start_offset,end_offset,start_line,end_line,embedding_profile_id,dimensions,vector,chunker_version,tokenizer_version,embedding_model_version,embedding_space_id,structure_json,structure_domain,symbol_name,qualified_name,parse_status,unit_start_offset,unit_end_offset) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)');
        for (const chunk of chunks) {
          cancelled(flag);
          const vector = vectors[chunk.chunkIndex];
          const retained = !hasNewVectors && canRetainVectors ? previousChunks[chunk.chunkIndex] : null;
          const blob = vector ? new Uint8Array(new Float32Array(vector).buffer) : retained?.vector ?? null;
          insertChunk.run(chunk.chunkId, source.sourceId, chunk.chunkIndex, chunk.text,
            lexicalText(`${source.title} ${source.locator.relativePath ?? ''} ${chunk.text}`), chunk.chunkHash,
            chunk.startOffset, chunk.endOffset, chunk.startLine, chunk.endLine, vector ? source.embeddingProfileId : retained?.embedding_profile_id ?? null,
            vector ? dimensions : retained?.dimensions ?? null, blob, chunk.chunkerVersion, chunk.tokenizerVersion,
            vector ? source.embeddingModelVersion ?? '' : retained?.embedding_model_version ?? '',
            vector ? source.embeddingSpaceId ?? '' : retained?.embedding_space_id ?? '',
            chunk.structure ? JSON.stringify(chunk.structure) : '', chunk.structure?.domain ?? '',
            chunk.structure?.symbolName ?? '', chunk.structure?.qualifiedName ?? '', chunk.structure?.parseStatus ?? '',
            chunk.structure?.unitStartOffset ?? null, chunk.structure?.unitEndOffset ?? null);
        }
        published.push({ sourceId: source.sourceId, generation: value, chunks: chunks.length, sourceRef: sourceReference(source) });
      }
      cancelled(flag);
      database.prepare('UPDATE index_jobs SET state = ?, completed_count = ? WHERE job_id = ?').run('completed', sources.length, jobId);
      return { jobId, generation: generation(), indexSnapshotId: snapshotId(), sources: published, changedScopes: [...changedScopes] };
    }, flag);
    // Update disposable native graphs only after the authoritative SQLite commit receipt exists.
    // SQLite 正式提交回执存在后才更新可重建的原生图；缓存失败不能撤销已成功的索引发布。
    await vectorSearch.updateSources(previousVectors, [...sourceIds], result.changedScopes);
    return result;
  } catch (error) {
    database.prepare('UPDATE index_jobs SET state = ?, error_code = ? WHERE job_id = ?').run('failed', error.code ?? 'RETRIEVAL_INDEX_FAILED', jobId);
    throw error;
  }
}

function publicChunk(row, score = null) {
  const source = { sourceId: row.source_id, scopeKey: row.scope_key, sourceRevision: JSON.parse(row.source_revision), contentHash: row.content_hash,
    ...(row.derivation_signature ? { derivationSignature: row.derivation_signature } : {}) };
  return { ...source, sourceType: row.source_type, title: row.title,
    locator: { ...JSON.parse(row.locator), startLine: row.start_line, endLine: row.end_line,
      startOffset: row.start_offset, endOffset: row.end_offset },
    bindingRevision: row.binding_revision, generation: row.active_generation,
    chunkId: row.chunk_id, chunkIndex: row.chunk_index, chunkHash: row.chunk_hash,
    excerpt: row.text, score,
    ...(row.lexical_position === undefined ? {} : { lexicalRank: row.lexical_position, lexicalScore: row.lexical_rank ?? null }),
    ...(row.vector_position === undefined ? {} : { vectorRank: row.vector_position, distance: row.distance }),
    ...(row.symbol_position === undefined ? {} : { symbolRank: row.symbol_position }),
    ...(row.path_position === undefined ? {} : { pathRank: row.path_position }),
    ...(row.exact_target_match ? { exactTargetMatch: true } : {}),
    ...(row.structure_json ? { structure: JSON.parse(row.structure_json) } : {}),
    sourceRef: sourceReference(source, { chunkId: row.chunk_id, chunkHash: row.chunk_hash }) };
}

async function search({ query, scopeKeys, queryVector, embeddingProfileId, embeddingModelVersion, embeddingSpaceId, retrievalIntent, ann, limit = 8 }, flag) {
  const scopes = retrievalScopeKeys(scopeKeys), placeholders = scopes.map(() => '?').join(',');
  const intent = validateRetrievalIntent(retrievalIntent);
  const requestedDomain = intent?.domain !== 'mixed' ? intent?.domain : undefined;
  const domainExpression = RETRIEVAL_DOMAIN_SQL;
  // Unknown legacy sources remain eligible; exact known-domain candidates rank first.
  // 未知类别的旧来源仍可检索，明确匹配领域的候选优先；领域路由不能替代范围授权。
  const domainFilter = requestedDomain ? ` AND (${domainExpression}=? OR ${domainExpression}='')` : '';
  const domainRank = requestedDomain ? `CASE WHEN ${domainExpression}='${requestedDomain}' THEN 0 ELSE 1 END` : 'CASE WHEN 1 THEN 0 END';
  const authorizedParameters = [...scopes, ...(requestedDomain ? [requestedDomain] : [])];
  cancelled(flag);
  const expression = matchExpression(query);
  let lexical = [];
  // Materialize the narrow FTS result once; fetch text and vectors only for authorized top candidates.
  // 窄 FTS 结果只物化一次；范围筛选及排序后才读取候选正文与向量，避免逐分块重复 FTS 扫描。
  if (expression) lexical = database.prepare(`WITH matches AS MATERIALIZED (
    SELECT rowid, bm25(chunk_fts) AS lexical_rank FROM chunk_fts WHERE chunk_fts MATCH ?),
    candidates AS MATERIALIZED (
      SELECT c.id, c.chunk_id, m.lexical_rank,
        CASE WHEN instr(lower(c.text),lower(?)) > 0 THEN 0 ELSE 1 END AS exact_rank, ${domainRank} AS domain_rank
      FROM matches m JOIN chunks c ON c.id=m.rowid JOIN sources s ON s.source_id=c.source_id
      WHERE s.scope_key IN (${placeholders})${domainFilter} ORDER BY domain_rank, exact_rank, m.lexical_rank, c.chunk_id LIMIT 40)
    SELECT ${CHUNK_ROW_COLUMNS}, candidates.lexical_rank FROM candidates
    JOIN chunks c ON c.id=candidates.id JOIN sources s ON s.source_id=c.source_id
    ORDER BY candidates.domain_rank, candidates.exact_rank, candidates.lexical_rank, candidates.chunk_id`)
    .all(expression, query, ...authorizedParameters);
  else if (query.trim()) lexical = database.prepare(`SELECT ${CHUNK_ROW_COLUMNS} FROM chunks c JOIN sources s ON s.source_id=c.source_id
    WHERE s.scope_key IN (${placeholders})${domainFilter} AND instr(lower(c.text),lower(?)) > 0 ORDER BY ${domainRank},c.id LIMIT 40`).all(...authorizedParameters, query.trim());
  let symbols = [], paths = [];
  const escapedPath = intent?.path?.replace(/[\\%_]/gu, character => `\\${character}`);
  const pathPredicate = column => intent?.path?.endsWith('/') ? `${column} LIKE ? ESCAPE '\\'` : `(${column}=? OR ${column} LIKE ? ESCAPE '\\')`;
  const pathParameters = intent?.path?.endsWith('/') ? [`${escapedPath}%`] : [intent?.path, `%/${escapedPath}`];
  if (intent?.symbol) {
    const symbolHasQualifier = intent.symbol.includes('.');
    const escapedSymbol = intent.symbol.replace(/[\\%_]/gu, character => `\\${character}`);
    // A short Class.Member name may omit namespace components, but never part of a component.
    // Class.Member 可以省略命名空间，但必须按点分组件完整匹配，不能命中 OtherClass.Member。
    const suffixClause = symbolHasQualifier ? " OR (c.qualified_name LIKE ? ESCAPE '\\' AND substr(c.qualified_name,-length(?))=?)" : '';
    const symbolParameters = [intent.symbol, intent.symbol,
      ...(symbolHasQualifier ? [`%.${escapedSymbol}`, intent.symbol, intent.symbol] : [])];
    symbols = database.prepare(`WITH ranked AS MATERIALIZED (
      SELECT c.id,c.chunk_id,s.relative_path,ROW_NUMBER() OVER (PARTITION BY c.source_id,c.symbol_name,c.unit_start_offset,c.unit_end_offset ORDER BY c.chunk_index) AS unit_rank
      FROM chunks c JOIN sources s ON s.source_id=c.source_id WHERE s.scope_key IN (${placeholders})${domainFilter}
        AND c.structure_domain='code' AND c.parse_status IN ('parsed','partial') AND (c.symbol_name=? OR c.qualified_name=?${suffixClause})),
      candidates AS MATERIALIZED (SELECT ranked.id,
        ${intent.path ? `CASE WHEN ${pathPredicate('ranked.relative_path')} THEN 0 ELSE 1 END` : 'CASE WHEN 1 THEN 1 END'} AS target_rank
        FROM ranked WHERE ranked.unit_rank=1 ORDER BY target_rank,ranked.chunk_id LIMIT 40)
      SELECT ${CHUNK_ROW_COLUMNS},candidates.target_rank FROM candidates
      JOIN chunks c ON c.id=candidates.id JOIN sources s ON s.source_id=c.source_id
      ORDER BY candidates.target_rank,${domainRank},c.chunk_id`)
      .all(...authorizedParameters, ...symbolParameters, ...(intent.path ? pathParameters : []))
      .map(row => ({ ...row, ...(row.target_rank === 0 ? { exact_target_match: true } : {}) }));
  }
  if (intent?.path) {
    paths = database.prepare(`WITH ranked AS MATERIALIZED (
      SELECT c.id,ROW_NUMBER() OVER (PARTITION BY c.source_id ORDER BY c.chunk_index) AS source_rank
      FROM chunks c JOIN sources s ON s.source_id=c.source_id WHERE s.scope_key IN (${placeholders})${domainFilter} AND ${pathPredicate('s.relative_path')})
      SELECT ${CHUNK_ROW_COLUMNS} FROM ranked JOIN chunks c ON c.id=ranked.id JOIN sources s ON s.source_id=c.source_id
      WHERE ranked.source_rank=1 ORDER BY ${domainRank},s.relative_path,c.chunk_id LIMIT 40`)
      .all(...authorizedParameters, ...pathParameters);
  }
  let semantic = [], degradedReason = null, semanticBackend = null;
  if (queryVector && embeddingProfileId) {
    if (!vectorAvailable) degradedReason = vectorError;
    else {
      const vectorResult = await vectorSearch.search({ scopeKeys: scopes, queryVector, embeddingProfileId,
        embeddingModelVersion, embeddingSpaceId, requestedDomain, ann }, () => cancelled(flag));
      semantic = vectorResult.items;
      degradedReason = vectorResult.degradedReason;
      semanticBackend = vectorResult.semanticBackend;
    }
  }
  cancelled(flag);
  const combined = new Map();
  for (const [list, channel, weight] of [[lexical, 'lexical', 1], [semantic, 'vector', 1], [symbols, 'symbol', 2], [paths, 'path', 1.5]]) for (const [rank, row] of list.entries()) {
    const old = combined.get(row.chunk_id);
    const diagnostics = { [`${channel}_position`]: rank + 1 };
    combined.set(row.chunk_id, { row: { ...old?.row, ...row, ...diagnostics }, score: (old?.score ?? 0) + weight / (60 + rank + 1) });
  }
  const items = [...combined.values()].sort((a, b) => Number(Boolean(b.row.exact_target_match)) - Number(Boolean(a.row.exact_target_match)) ||
    b.score - a.score || a.row.chunk_id.localeCompare(b.row.chunk_id))
    .slice(0, limit).map(item => publicChunk(item.row, item.score));
  const snapshots = database.prepare(`SELECT scope_key AS scopeKey,generation FROM scope_snapshots WHERE scope_key IN (${placeholders}) ORDER BY scope_key`).all(...scopes)
    .map(item => ({ ...item, snapshotId: snapshotId(item.generation) }));
  return { items, generation: generation(), indexSnapshotId: snapshotId(), scopeSnapshots: snapshots, strategy: semantic.length ? 'hybrid' : 'lexical',
    ...(intent ? { retrievalIntent: intent, structuredChannels: { symbolCandidates: symbols.length, pathCandidates: paths.length },
      coverage: { symbols: 'indexed-definitions', references: false, callGraph: false } } : {}),
    vectorAvailable, semanticBackend, ...(degradedReason ? { degradedReason } : {}) };
}

function readSourceRow({ sourceId, sourceRef, scopeKeys }, flag) {
  const scopes = retrievalScopeKeys(scopeKeys), reference = sourceRef ? parseSourceReference(sourceRef) : null;
  if (reference && !scopes.includes(reference.scopeKey)) throw retrievalFailure('Source is outside the authorized scope. / 资料不在授权范围内。', 'RETRIEVAL_SOURCE_NOT_FOUND', 404);
  const id = reference?.sourceId ?? sourceId;
  const row = database.prepare(`SELECT * FROM sources WHERE source_id = ? AND scope_key IN (${scopes.map(() => '?').join(',')})`).get(id, ...scopes);
  if (!row) throw retrievalFailure('Source is unavailable. / 资料不存在或已撤销。', 'RETRIEVAL_SOURCE_NOT_FOUND', 404);
  if (reference && (reference.scopeKey !== row.scope_key || reference.contentHash !== row.content_hash || JSON.stringify(reference.sourceRevision) !== row.source_revision ||
      reference.derivationSignature !== undefined && reference.derivationSignature !== row.derivation_signature))
    throw retrievalFailure('Source reference is outdated. / 资料引用已过期，请重新检索。', 'STALE_RETRIEVAL_SOURCE', 409);
  let chunk;
  if (reference?.chunkId) {
    chunk = database.prepare('SELECT chunk_id,chunk_hash,start_offset,end_offset,structure_json FROM chunks WHERE chunk_id=? AND source_id=?').get(reference.chunkId, id);
    if (!chunk || chunk.chunk_hash !== reference.chunkHash) throw retrievalFailure('Chunk reference is outdated. / 分块引用已过期。', 'STALE_RETRIEVAL_SOURCE', 409);
  }
  cancelled(flag);
  return { row, id, reference, chunk };
}

/** Verify evidence metadata without loading stored original text or vector blobs.
 * 只核验证据元信息，不加载原文和向量；同文重新派生后旧证据也不能被再次发布。
 */
function verifyReference({ sourceRef, scopeKeys }, flag) {
  const scopes = retrievalScopeKeys(scopeKeys), reference = parseSourceReference(sourceRef);
  if (!scopes.includes(reference.scopeKey)) throw retrievalFailure('Source is outside the authorized scope. / 资料不在授权范围内。', 'RETRIEVAL_SOURCE_NOT_FOUND', 404);
  cancelled(flag);
  const stale = reason => { cancelled(flag); return { current: false, reason }; };
  const row = database.prepare(`SELECT source_id,scope_key,content_hash,source_revision,derivation_signature FROM sources
    WHERE source_id=? AND scope_key IN (${scopes.map(() => '?').join(',')})`).get(reference.sourceId, ...scopes);
  if (!row) return stale('RETRIEVAL_SOURCE_NOT_FOUND');
  if (reference.scopeKey !== row.scope_key || reference.contentHash !== row.content_hash ||
      JSON.stringify(reference.sourceRevision) !== row.source_revision ||
      reference.derivationSignature !== undefined && reference.derivationSignature !== row.derivation_signature)
    return stale('STALE_RETRIEVAL_SOURCE');
  if (reference.chunkId) {
    const chunk = database.prepare('SELECT chunk_hash FROM chunks WHERE chunk_id=? AND source_id=?').get(reference.chunkId, reference.sourceId);
    if (!chunk || chunk.chunk_hash !== reference.chunkHash) return stale('STALE_RETRIEVAL_SOURCE');
  }
  cancelled(flag);
  return { current: true, sourceId: row.source_id,
    ...(row.derivation_signature ? { derivationSignature: row.derivation_signature } : {}) };
}

function readMetadata(row, id, chunk) {
  const source = { sourceId: id, scopeKey: row.scope_key, sourceRevision: JSON.parse(row.source_revision), contentHash: row.content_hash,
    ...(row.derivation_signature ? { derivationSignature: row.derivation_signature } : {}) };
  return { ...source,
    sourceType: row.source_type, title: row.title, locator: JSON.parse(row.locator), bindingRevision: row.binding_revision,
    ...(row.structure_json ? { structure: JSON.parse(row.structure_json) } : {}),
    sourceRef: sourceReference(source, chunk ? { chunkId: chunk.chunk_id, chunkHash: chunk.chunk_hash } : null) };
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
  if (input.mode === 'unit') return readStructuredUnit(row, id, chunk, input, flag);
  const window = sourceWindow(row.text, { ...input, anchorOffset: input.anchorOffset ?? chunk?.start_offset ?? 0,
    ...(input.anchorOffset === undefined && chunk ? { referenceRange: { startOffset: chunk.start_offset, endOffset: chunk.end_offset } } : {}) }, () => cancelled(flag));
  return { ...readMetadata(row, id, chunk), ...window };
}

/** Expand only the verified matched unit; large units stay bounded and missing structure is explicit.
 * 只展开已核验命中单元；大单元仍有长度上限，旧索引缺少结构时明确回退，不能冒称完整函数。
 */
function readStructuredUnit(row, id, chunk, input, flag) {
  const metadata = readMetadata(row, id, chunk);
  if (!chunk?.structure_json) {
    const fallback = sourceWindow(row.text, { ...input, mode: 'window', anchorOffset: input.anchorOffset ?? chunk?.start_offset ?? 0 }, () => cancelled(flag));
    return { ...metadata, ...fallback, unitUnavailable: true };
  }
  const structure = validateChunkStructure(JSON.parse(chunk.structure_json), { text: row.text,
    ...(row.structure_json ? { structure: JSON.parse(row.structure_json) } : {}) },
  { startOffset: chunk.start_offset, endOffset: chunk.end_offset });
  const lower = structure.unitStartOffset, upper = structure.unitEndOffset;
  const anchorOffset = input.anchorOffset ?? chunk.start_offset;
  if (anchorOffset < lower || anchorOffset >= upper) throw retrievalFailure('Anchor is outside the matched unit. / 回读锚点不在命中单元内。', 'INVALID_RETRIEVAL_OFFSET');
  const text = row.text.slice(lower, upper), shouldReadWholeUnit = text.length <= input.limit;
  const window = sourceWindow(text, { ...input, mode: 'window', anchorOffset: shouldReadWholeUnit ? 0 : anchorOffset - lower,
    beforeCharacters: shouldReadWholeUnit ? 0 : input.beforeCharacters,
    referenceRange: { startOffset: chunk.start_offset - lower, endOffset: chunk.end_offset - lower } }, () => cancelled(flag));
  const offset = lower + window.offset, nextOffset = lower + window.nextOffset;
  return { ...metadata, ...window, offset, nextOffset, totalCharacters: row.text.length,
    window: { ...window.window, mode: 'unit', anchorOffset, startOffset: offset, endOffset: nextOffset,
      unit: { ...structure, startOffset: lower, endOffset: upper },
      referenceRange: { startOffset: chunk.start_offset, endOffset: chunk.end_offset },
      referenceRangeCovered: offset <= chunk.start_offset && nextOffset >= chunk.end_offset } };
}

async function removeSource({ sourceId, scopeKeys, permanent = true }) {
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
  const previousVectors = vectorSearch.sourceRows([sourceId]);
  const result = transaction(() => {
    database.prepare('DELETE FROM sources WHERE source_id=?').run(sourceId);
    return { removed: Boolean(current), generation: nextGeneration(scope,
      { corpusChanged: isCorpusSource(current?.source_type ?? existing?.sourceType) }), indexSnapshotId: snapshotId() };
  });
  await vectorSearch.updateSources(previousVectors, [], [scope]);
  return result;
}

function listSources({ scopeKeys, sourceType }) {
  const scopes = retrievalScopeKeys(scopeKeys);
  const filter = sourceType === undefined ? '' : ' AND source_type=?';
  const parameters = sourceType === undefined ? [] : [sourceType];
  return database.prepare(`SELECT source_id AS sourceId,scope_key AS scopeKey,source_type AS sourceType,
    title,locator,content_hash AS contentHash,source_revision AS sourceRevision,binding_revision AS bindingRevision,
    derivation_signature AS derivationSignature,embedding_input_signature AS embeddingInputSignature,
    parser_version AS parserVersion,embedding_input_version AS embeddingInputVersion,
    chunker_version AS chunkerVersion,tokenizer_version AS tokenizerVersion,
    structure_json AS structure,
    active_generation AS generation,
    (SELECT count(*) FROM chunks c WHERE c.source_id=sources.source_id) AS chunkCount,
    (SELECT count(*) FROM chunks c WHERE c.source_id=sources.source_id AND c.vector IS NOT NULL) AS vectorChunks,
    (SELECT CASE WHEN count(DISTINCT c.embedding_profile_id)=1 THEN min(c.embedding_profile_id) END FROM chunks c WHERE c.source_id=sources.source_id AND c.vector IS NOT NULL) AS embeddingProfileId,
    (SELECT CASE WHEN count(DISTINCT c.embedding_model_version)=1 THEN min(c.embedding_model_version) END FROM chunks c WHERE c.source_id=sources.source_id AND c.vector IS NOT NULL) AS embeddingModelVersion,
    (SELECT CASE WHEN count(DISTINCT c.embedding_space_id)=1 THEN min(c.embedding_space_id) END FROM chunks c WHERE c.source_id=sources.source_id AND c.vector IS NOT NULL) AS embeddingSpaceId,
    (SELECT CASE WHEN count(DISTINCT c.dimensions)=1 THEN min(c.dimensions) END FROM chunks c WHERE c.source_id=sources.source_id AND c.vector IS NOT NULL) AS vectorDimensions
    FROM sources WHERE scope_key IN (${scopes.map(() => '?').join(',')})${filter}
    ORDER BY source_id`).all(...scopes, ...parameters).map(row => ({ ...row,
      sourceRevision: JSON.parse(row.sourceRevision), locator: JSON.parse(row.locator),
      ...(row.structure ? { structure: JSON.parse(row.structure) } : { structure: undefined }) }));
}

async function invalidateScope({ scopeKey }) {
  retrievalScopeKeys([scopeKey]);
  const registry = identities();
  const corpusChanged = Boolean(database.prepare("SELECT 1 FROM sources WHERE scope_key=? AND source_type IN ('knowledge','work-file') LIMIT 1").get(scopeKey)) ||
    Object.values(registry.sources).some(value => value.scopeKey === scopeKey && value.active && isCorpusSource(value.sourceType));
  for (const value of Object.values(registry.sources)) if (value.scopeKey === scopeKey) value.active = false;
  registry.revision++;
  persistIdentities(registry);
  const result = transaction(() => {
    const result = database.prepare('DELETE FROM sources WHERE scope_key=?').run(scopeKey);
    return { removed: result.changes, generation: nextGeneration(scopeKey, { corpusChanged }), indexSnapshotId: snapshotId(), scopeKey };
  });
  await vectorSearch.invalidateScopes([scopeKey]);
  return result;
}

function scopeVersion({ scopeKeys }, flag) {
  const scopes = retrievalScopeKeys(scopeKeys);
  cancelled(flag);
  // Scope generations and the database epoch prove freshness without scanning corpus rows.
  // 范围版本和数据库代次用于核验缓存新鲜度，不扫描来源、分块或向量。
  const rows = database.prepare(`SELECT scope_key,generation FROM scope_snapshots
    WHERE scope_key IN (${scopes.map(() => '?').join(',')})`).all(...scopes);
  const generations = new Map(rows.map(row => [row.scope_key, row.generation]));
  const corpusKeys = scopes.map(scope => `corpus_generation:${scope}`);
  const corpusRows = database.prepare(`SELECT key,value FROM retrieval_metadata WHERE key IN (${corpusKeys.map(() => '?').join(',')})`).all(...corpusKeys);
  const corpusGenerations = new Map(corpusRows.map(row => [row.key, Number(row.value)]));
  return { indexEpoch: database.prepare("SELECT value FROM retrieval_metadata WHERE key='index_epoch'").get().value,
    scopes: scopes.map(scopeKey => ({ scopeKey, generation: generations.get(scopeKey) ?? 0,
      corpusGeneration: corpusGenerations.get(`corpus_generation:${scopeKey}`) ?? 0 })) };
}

function status() {
  return { schemaVersion: RETRIEVAL_INDEX_SCHEMA_VERSION, generation: generation(), indexSnapshotId: snapshotId(),
    tokenizerVersion: TOKENIZER_VERSION, chunkerVersion: CHUNKER_VERSION,
    sources: database.prepare('SELECT count(*) AS count FROM sources').get().count,
    chunks: database.prepare('SELECT count(*) AS count FROM chunks').get().count,
    vectorChunks: database.prepare('SELECT count(*) AS count FROM chunks WHERE vector IS NOT NULL').get().count,
    vectorAvailable, vectorVersion, vectorError, ann: vectorSearch.status(), indexPath: filename,
    scopeSnapshots: database.prepare('SELECT scope_key AS scopeKey,generation FROM scope_snapshots ORDER BY scope_key').all()
      .map(item => ({ ...item, snapshotId: snapshotId(item.generation) })),
    jobs: Object.fromEntries(database.prepare('SELECT state,count(*) AS count FROM index_jobs GROUP BY state').all().map(row => [row.state, row.count])) };
}

async function close() {
  if (!database) return { closed: true };
  const checkpoint = database.prepare('PRAGMA wal_checkpoint(TRUNCATE)').get();
  if (checkpoint.busy) throw retrievalFailure('Retrieval index is busy. / 检索索引正被使用，无法安全迁移。', 'RETRIEVAL_INDEX_BUSY', 409);
  await vectorSearch.close();
  database.close();
  database = null;
  return { closed: true, checkpointed: true };
}

await openDatabase();
let operationsQueue = Promise.resolve();
parentPort.on('message', message => {
  // One async owner preserves SQLite transaction order across background ANN responses.
  // 异步队列维持单一 SQLite 所有者；等待后台向量结果期间也不能让另一请求交错事务。
  operationsQueue = operationsQueue.catch(() => {}).then(async () => {
    const flag = message.cancelBuffer ? new Int32Array(message.cancelBuffer) : null;
    try {
      cancelled(flag);
      const operations = { upsertSources, search, read, readWindow, verifyReference, removeSource, listSources, invalidateScope, scopeVersion, status, close };
      const operation = operations[message.method];
      if (!operation || !database) throw retrievalFailure('Retrieval index is closed. / 检索索引已关闭。', 'RETRIEVAL_INDEX_CLOSED', 409);
      const result = await operation(message.input ?? {}, flag);
      parentPort.postMessage({ id: message.id, result });
    } catch (error) {
      parentPort.postMessage({ id: message.id, error: { name: error.name, message: error.message,
        code: error.code ?? 'RETRIEVAL_INDEX_FAILED', statusCode: error.statusCode ?? 500 } });
    }
  });
});

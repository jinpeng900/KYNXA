pub const RETRIEVAL_SCHEMA: &str = "PRAGMA busy_timeout=3000;PRAGMA journal_mode=WAL;PRAGMA synchronous=FULL;PRAGMA foreign_keys=ON;
CREATE TABLE IF NOT EXISTS retrieval_metadata(key TEXT NOT NULL PRIMARY KEY,value TEXT NOT NULL);
INSERT OR IGNORE INTO retrieval_metadata VALUES ('generation','0');
CREATE TABLE IF NOT EXISTS sources(
    source_id TEXT NOT NULL PRIMARY KEY,
    scope_key TEXT NOT NULL,
    source_type TEXT NOT NULL,
    title TEXT NOT NULL,
    locator TEXT NOT NULL,
    text TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    source_revision TEXT NOT NULL,
    binding_revision INTEGER NOT NULL,
    active_generation INTEGER NOT NULL,
    updated_at TEXT NOT NULL,
    embedding_signature TEXT NOT NULL DEFAULT '',
    derivation_signature TEXT NOT NULL DEFAULT '',
    embedding_input_signature TEXT NOT NULL DEFAULT '',
    parser_version TEXT NOT NULL DEFAULT '',
    embedding_input_version TEXT NOT NULL DEFAULT '',
    chunker_version TEXT NOT NULL DEFAULT '',
    tokenizer_version TEXT NOT NULL DEFAULT '',
    structure_json TEXT NOT NULL DEFAULT '',
    relative_path TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS sources_scope ON sources(scope_key);
CREATE TABLE IF NOT EXISTS chunks(
    id INTEGER PRIMARY KEY,
    chunk_id TEXT UNIQUE NOT NULL,
    source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE CASCADE,
    chunk_index INTEGER NOT NULL,
    text TEXT NOT NULL,
    lexical_text TEXT NOT NULL,
    chunk_hash TEXT NOT NULL,
    start_offset INTEGER NOT NULL,
    end_offset INTEGER NOT NULL,
    start_line INTEGER NOT NULL,
    end_line INTEGER NOT NULL,
    embedding_profile_id TEXT,
    dimensions INTEGER,
    vector BLOB,
    chunker_version TEXT NOT NULL,
    tokenizer_version TEXT NOT NULL,
    embedding_model_version TEXT NOT NULL DEFAULT '',
    embedding_space_id TEXT NOT NULL DEFAULT '',
    structure_json TEXT NOT NULL DEFAULT '',
    structure_domain TEXT NOT NULL DEFAULT '',
    symbol_name TEXT NOT NULL DEFAULT '',
    qualified_name TEXT NOT NULL DEFAULT '',
    parse_status TEXT NOT NULL DEFAULT '',
    unit_start_offset INTEGER,
    unit_end_offset INTEGER,
    embedding_projection TEXT NOT NULL DEFAULT '',
    UNIQUE(source_id,chunk_index)
);
CREATE INDEX IF NOT EXISTS chunks_source ON chunks(source_id,chunk_index);
CREATE INDEX IF NOT EXISTS chunks_profile ON chunks(embedding_profile_id,dimensions);
CREATE TABLE IF NOT EXISTS scope_snapshots(scope_key TEXT NOT NULL PRIMARY KEY,generation INTEGER NOT NULL);
CREATE VIRTUAL TABLE IF NOT EXISTS chunk_fts USING fts5(lexical_text,content='chunks',content_rowid='id',tokenize='unicode61');
CREATE TRIGGER IF NOT EXISTS chunks_insert AFTER INSERT ON chunks BEGIN INSERT INTO chunk_fts(rowid,lexical_text) VALUES (new.id,new.lexical_text); END;
CREATE TRIGGER IF NOT EXISTS chunks_delete AFTER DELETE ON chunks BEGIN INSERT INTO chunk_fts(chunk_fts,rowid,lexical_text) VALUES ('delete',old.id,old.lexical_text); END;
CREATE TRIGGER IF NOT EXISTS chunks_update AFTER UPDATE ON chunks BEGIN INSERT INTO chunk_fts(chunk_fts,rowid,lexical_text) VALUES ('delete',old.id,old.lexical_text); INSERT INTO chunk_fts(rowid,lexical_text) VALUES (new.id,new.lexical_text); END;
CREATE TABLE IF NOT EXISTS index_jobs(job_id TEXT NOT NULL PRIMARY KEY,state TEXT NOT NULL,source_count INTEGER NOT NULL,completed_count INTEGER NOT NULL,created_at TEXT NOT NULL,error_code TEXT);";

pub const RELATIONS_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS source_relations(edge_id TEXT NOT NULL PRIMARY KEY,source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE CASCADE,derivation_signature TEXT NOT NULL,kind TEXT NOT NULL,from_symbol TEXT NOT NULL,to_symbol TEXT NOT NULL,chunk_id TEXT NOT NULL,chunk_hash TEXT NOT NULL,start_offset INTEGER NOT NULL,end_offset INTEGER NOT NULL,certainty TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS relation_source ON source_relations(source_id,derivation_signature);
CREATE INDEX IF NOT EXISTS relation_target ON source_relations(to_symbol,kind);
CREATE INDEX IF NOT EXISTS relation_owner ON source_relations(from_symbol,kind);";

pub const VECTOR_SPACES_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS chunk_vector_spaces(chunk_id TEXT NOT NULL REFERENCES chunks(chunk_id) ON DELETE CASCADE,embedding_profile_id TEXT NOT NULL,dimensions INTEGER NOT NULL,vector BLOB NOT NULL,embedding_model_version TEXT NOT NULL,embedding_space_id TEXT NOT NULL,updated_at TEXT NOT NULL,PRIMARY KEY(chunk_id,embedding_space_id));
CREATE INDEX IF NOT EXISTS vector_spaces_profile ON chunk_vector_spaces(embedding_profile_id,embedding_space_id);
INSERT OR IGNORE INTO chunk_vector_spaces SELECT chunk_id,embedding_profile_id,dimensions,vector,embedding_model_version,embedding_space_id,strftime('%Y-%m-%dT%H:%M:%fZ','now') FROM chunks WHERE vector IS NOT NULL AND embedding_profile_id IS NOT NULL;";

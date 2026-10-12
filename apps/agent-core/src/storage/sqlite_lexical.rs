use crate::storage::{ContractError, ContractResult};
use regex::Regex;
use rusqlite::{Connection, functions::FunctionFlags, params_from_iter, types::Value as SqlValue};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

pub const DOMAIN_SQL: &str = "CASE WHEN c.structure_domain<>'' THEN c.structure_domain WHEN s.structure_json<>'' THEN json_extract(s.structure_json,'$.domain') WHEN s.source_type='code' THEN 'code' WHEN s.source_type IN ('document','memory','conversation','web') THEN 'knowledge' ELSE '' END";

fn columns(arguments: &Value) -> Result<String, ContractError> {
    if arguments["metadataOnly"] == true {
        return Ok(super::sqlite_channels::candidate_columns());
    }
    if arguments["fullRows"] == true {
        return Ok(super::sqlite_channels::chunk_columns());
    }
    let full = format!(
        "c.*, s.scope_key, s.source_type, s.title, s.locator, s.content_hash, s.source_revision, s.binding_revision, s.active_generation, s.derivation_signature, {DOMAIN_SQL} AS actual_domain"
    );
    let value = arguments["columns"].as_str().unwrap_or("c.chunk_id");
    if value.split_whitespace().collect::<Vec<_>>() == full.split_whitespace().collect::<Vec<_>>() {
        return Ok(full);
    }
    if value.split(',').all(|field| {
        let field = field.trim();
        let Some(name) = field
            .strip_prefix("c.")
            .or_else(|| field.strip_prefix("s."))
        else {
            return false;
        };
        name == "*"
            || !name.is_empty()
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
    }) {
        Ok(value.to_owned())
    } else {
        Err(failure("invalid row projection"))
    }
}

#[derive(Clone)]
struct Corpus {
    documents: i64,
    average_tokens: f64,
}

#[derive(Default)]
struct StatisticsCache {
    corpora: HashMap<String, Corpus>,
    corpus_order: VecDeque<String>,
    frequencies: HashMap<String, i64>,
    frequency_order: VecDeque<String>,
}

static CACHE: OnceLock<Mutex<StatisticsCache>> = OnceLock::new();

#[derive(Default)]
pub(crate) struct RankingContext {
    average_tokens: f64,
    frequencies: HashMap<String, f64>,
}

/// Connection-local functions borrow only the current query's ranking inputs.
/// 连接局部函数只借用当前查询的排序输入，函数与临时表只初始化一次。
pub(crate) fn initialize_connection(
    connection: &Connection,
    ranking: Arc<RwLock<RankingContext>>,
) -> Result<(), ContractError> {
    connection.execute_batch("CREATE VIRTUAL TABLE IF NOT EXISTS temp.retrieval_term_instances USING fts5vocab(main,chunk_fts,'instance'); CREATE VIRTUAL TABLE IF NOT EXISTS temp.retrieval_query_terms USING fts5(term,tokenize='unicode61'); CREATE VIRTUAL TABLE IF NOT EXISTS temp.retrieval_query_vocabulary USING fts5vocab(temp,retrieval_query_terms,'row');").map_err(failure)?;
    connection
        .create_scalar_function(
            "retrieval_document_tokens",
            1,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
            |context| Ok(document_tokens(context.get_raw(0).as_blob()?)),
        )
        .map_err(failure)?;
    connection
        .create_scalar_function(
            "retrieval_scoped_bm25",
            3,
            FunctionFlags::SQLITE_UTF8,
            move |context| {
                let term = context.get::<String>(0)?;
                let frequency = context.get::<f64>(1)?;
                let tokens = document_tokens(context.get_raw(2).as_blob()?);
                let ranking = ranking.read().map_err(|_| {
                    rusqlite::Error::UserFunctionError(Box::new(std::io::Error::other(
                        "lexical ranking context poisoned",
                    )))
                })?;
                let idf = ranking.frequencies.get(&term).copied().unwrap_or(0.0);
                Ok(-idf * frequency * 2.2
                    / (frequency + 1.2 * (0.25 + 0.75 * tokens / ranking.average_tokens)))
            },
        )
        .map_err(failure)?;
    Ok(())
}

fn failure(error: impl std::fmt::Display) -> ContractError {
    ContractError::new(
        format!("SQLite lexical retrieval failed. / SQLite 词法检索失败：{error}"),
        "RETRIEVAL_LEXICAL_FAILED",
        500,
    )
}

fn document_tokens(bytes: &[u8]) -> f64 {
    let mut tokens = 0_u64;
    for byte in bytes {
        tokens = tokens
            .saturating_mul(128)
            .saturating_add(u64::from(byte & 127));
        if byte & 128 == 0 {
            return tokens as f64;
        }
    }
    0.0
}

fn placeholders(count: usize) -> String {
    vec!["?"; count].join(",")
}

fn strings(arguments: &Value, field: &str) -> Vec<String> {
    arguments[field]
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn text_parameter(value: impl Into<String>) -> SqlValue {
    SqlValue::Text(value.into())
}

/// A read transaction binds MATCH, authorization, corpus statistics and ranking to one SQLite snapshot.
/// 读取事务将全文匹配、授权语料统计与排序绑定到同一个 SQLite 快照。
pub fn apply(arguments: &Value) -> ContractResult {
    let root = Path::new(arguments["root"].as_str().unwrap_or(""));
    let path = Path::new(arguments["path"].as_str().unwrap_or(""));
    let mut connection = super::sqlite_connections::acquire(
        root,
        path,
        arguments["timeoutMs"].as_u64().unwrap_or(120_000),
    )?;
    connection.execute_batch("BEGIN").map_err(failure)?;
    let result = query(&mut connection, arguments);
    super::sqlite_store::finish(&connection, result)
}

/// Caller-owned snapshots combine lexical statistics and other channels without another connection checkout.
/// 调用方持有的快照合并词法统计与其他通道，不再次借用连接或核验相同数据库身份。
pub(crate) fn query(
    connection: &mut super::sqlite_connections::SqliteConnectionLease,
    arguments: &Value,
) -> ContractResult {
    let path = Path::new(arguments["path"].as_str().unwrap_or(""));
    let ranking = connection.lexical_context()?;
    connection
        .execute_batch("DELETE FROM temp.retrieval_query_terms;")
        .map_err(failure)?;
    let scopes = strings(arguments, "scopes");
    if scopes.is_empty() || scopes.len() > 1000 {
        return Err(failure("invalid scopes"));
    }
    let domain = arguments["domain"]
        .as_str()
        .filter(|value| !value.is_empty());
    let preferred_domain = arguments["preferredDomain"]
        .as_str()
        .filter(|value| !value.is_empty());
    let expression = arguments["expression"].as_str().unwrap_or("");
    let query = arguments["query"].as_str().unwrap_or("");
    let limit = arguments["limit"].as_u64().unwrap_or(40).clamp(1, 160);
    let columns = columns(arguments)?;
    let scope_placeholders = placeholders(scopes.len());
    let predicate = format!(
        "s.scope_key IN ({scope_placeholders}){}",
        if domain.is_some() {
            format!(" AND ({DOMAIN_SQL}=? OR {DOMAIN_SQL}='')")
        } else {
            String::new()
        }
    );
    let mut parameters: Vec<SqlValue> = scopes.iter().cloned().map(text_parameter).collect();
    if let Some(domain) = domain {
        parameters.push(text_parameter(domain));
    }
    let version_sql = format!(
        "SELECT s.scope_key,COALESCE(m.value,CAST(s.generation AS TEXT)) AS generation FROM scope_snapshots s LEFT JOIN retrieval_metadata m ON m.key='lexical_generation:' || s.scope_key WHERE s.scope_key IN ({scope_placeholders}) ORDER BY s.scope_key"
    );
    let versions: Vec<Value> = connection
        .prepare_cached(&version_sql)
        .map_err(failure)?
        .query_map(params_from_iter(scopes.iter()), |row| {
            Ok(json!([row.get::<_, String>(0)?, row.get::<_, String>(1)?]))
        })
        .map_err(failure)?
        .collect::<Result<_, _>>()
        .map_err(failure)?;
    let epoch: Option<String> = connection
        .prepare_cached("SELECT value FROM retrieval_metadata WHERE key='index_epoch'")
        .map_err(failure)?
        .query_row([], |row| row.get(0))
        .ok();
    let mut sorted_scopes = scopes.clone();
    sorted_scopes.sort();
    let key = json!([
        path.to_string_lossy(),
        connection.database_identity(),
        epoch,
        sorted_scopes,
        domain,
        versions
    ])
    .to_string();
    let cached_corpus = CACHE
        .get_or_init(|| Mutex::new(StatisticsCache::default()))
        .lock()
        .map_err(failure)?
        .corpora
        .get(&key)
        .cloned();
    let corpus = if let Some(corpus) = cached_corpus {
        corpus
    } else {
        let sql = format!(
            "SELECT COUNT(*),COALESCE(SUM(retrieval_document_tokens(d.sz)),0) FROM chunks c JOIN sources s ON s.source_id=c.source_id JOIN chunk_fts_docsize d ON d.id=c.id WHERE {predicate}"
        );
        let (documents, tokens): (i64, f64) = connection
            .prepare_cached(&sql)
            .map_err(failure)?
            .query_row(params_from_iter(parameters.iter()), |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .map_err(failure)?;
        let corpus = Corpus {
            documents,
            average_tokens: if documents > 0 && tokens > 0.0 {
                tokens / documents as f64
            } else {
                1.0
            },
        };
        let mut cache = CACHE
            .get()
            .expect("initialized statistics cache")
            .lock()
            .map_err(failure)?;
        cache.corpora.insert(key.clone(), corpus.clone());
        cache.corpus_order.push_back(key.clone());
        while cache.corpus_order.len() > 32 {
            if let Some(key) = cache.corpus_order.pop_front() {
                cache.corpora.remove(&key);
            }
        }
        corpus
    };
    static PHRASE_PATTERN: OnceLock<Regex> = OnceLock::new();
    let phrase_pattern = PHRASE_PATTERN
        .get_or_init(|| Regex::new(r#""((?:[^"]|"")*)""#).expect("fixed phrase pattern"));
    let phrases: Vec<String> = phrase_pattern
        .captures_iter(expression)
        .map(|capture| capture[1].replace("\"\"", "\""))
        .collect();
    connection
        .execute(
            "INSERT INTO temp.retrieval_query_terms(term) VALUES (?)",
            [phrases.join(" ")],
        )
        .map_err(failure)?;
    let terms: Vec<String> = connection
        .prepare_cached("SELECT term FROM temp.retrieval_query_vocabulary ORDER BY term")
        .map_err(failure)?
        .query_map([], |row| row.get(0))
        .map_err(failure)?
        .collect::<Result<_, _>>()
        .map_err(failure)?;
    if corpus.documents == 0 || terms.is_empty() {
        return Ok(json!({"items": [], "queryPlan": []}));
    }
    let mut inverse_frequency = HashMap::new();
    let count_sql = format!(
        "SELECT COUNT(DISTINCT v.doc) FROM temp.retrieval_term_instances v JOIN chunks c ON c.id=v.doc JOIN sources s ON s.source_id=c.source_id WHERE v.term=? AND {predicate}"
    );
    for term in &terms {
        let term_key = format!("{key}:{term}");
        let cached_frequency = CACHE
            .get()
            .expect("initialized statistics cache")
            .lock()
            .map_err(failure)?
            .frequencies
            .get(&term_key)
            .copied();
        let frequency = if let Some(frequency) = cached_frequency {
            frequency
        } else {
            let term_parameters: Vec<SqlValue> = std::iter::once(text_parameter(term.clone()))
                .chain(parameters.iter().cloned())
                .collect();
            let frequency: i64 = connection
                .prepare_cached(&count_sql)
                .map_err(failure)?
                .query_row(params_from_iter(term_parameters.iter()), |row| row.get(0))
                .map_err(failure)?;
            let mut cache = CACHE
                .get()
                .expect("initialized statistics cache")
                .lock()
                .map_err(failure)?;
            cache.frequencies.insert(term_key.clone(), frequency);
            cache.frequency_order.push_back(term_key);
            while cache.frequency_order.len() > 1024 {
                if let Some(key) = cache.frequency_order.pop_front() {
                    cache.frequencies.remove(&key);
                }
            }
            frequency
        };
        inverse_frequency.insert(
            term.clone(),
            (((corpus.documents - frequency) as f64 + 0.5) / (frequency as f64 + 0.5))
                .ln()
                .max(1e-6),
        );
    }
    *ranking.write().map_err(failure)? = RankingContext {
        average_tokens: corpus.average_tokens,
        frequencies: inverse_frequency,
    };
    let ranking_domain = domain.or(preferred_domain);
    let domain_rank = if ranking_domain.is_some() {
        format!("CASE WHEN {DOMAIN_SQL}=? THEN 0 ELSE 1 END")
    } else {
        "0".into()
    };
    let candidate_order = if domain.is_some() {
        "domain_rank,exact_rank,score.lexical_rank,c.chunk_id"
    } else {
        "exact_rank,score.lexical_rank,domain_rank,c.chunk_id"
    };
    let result_order = if domain.is_some() {
        "candidates.domain_rank,candidates.exact_rank,candidates.lexical_rank,candidates.chunk_id"
    } else {
        "candidates.exact_rank,candidates.lexical_rank,candidates.domain_rank,candidates.chunk_id"
    };
    let sql = format!(
        "WITH fts_matches AS MATERIALIZED (SELECT rowid AS id FROM chunk_fts WHERE chunk_fts MATCH ?), frequencies AS MATERIALIZED (SELECT v.doc,v.term,COUNT(*) AS frequency FROM temp.retrieval_term_instances v JOIN chunks c ON c.id=v.doc JOIN sources s ON s.source_id=c.source_id WHERE v.term IN ({}) AND {predicate} GROUP BY v.doc,v.term), scores AS MATERIALIZED (SELECT f.doc,SUM(retrieval_scoped_bm25(f.term,f.frequency,d.sz)) AS lexical_rank FROM frequencies f JOIN chunk_fts_docsize d ON d.id=f.doc GROUP BY f.doc), candidates AS MATERIALIZED (SELECT c.id,c.chunk_id,score.lexical_rank,CASE WHEN instr(lower(c.text),lower(?)) > 0 THEN 0 ELSE 1 END AS exact_rank,{domain_rank} AS domain_rank FROM fts_matches matched JOIN scores score ON score.doc=matched.id JOIN chunks c ON c.id=score.doc JOIN sources s ON s.source_id=c.source_id WHERE {predicate} ORDER BY {candidate_order} LIMIT {limit}) SELECT candidates.id,candidates.lexical_rank FROM candidates ORDER BY {result_order}",
        placeholders(terms.len())
    );
    let mut search_parameters = vec![text_parameter(expression)];
    search_parameters.extend(terms.iter().cloned().map(text_parameter));
    search_parameters.extend(parameters.iter().cloned());
    search_parameters.push(text_parameter(query));
    if let Some(ranking_domain) = ranking_domain {
        search_parameters.push(text_parameter(ranking_domain));
    }
    search_parameters.extend(parameters);
    let sql = sql.replace("SELECT candidates.id,candidates.lexical_rank FROM candidates ORDER BY", &format!("SELECT {columns},candidates.lexical_rank FROM candidates JOIN chunks c ON c.id=candidates.id JOIN sources s ON s.source_id=c.source_id ORDER BY"));
    let query_plan = if arguments["explain"] == true {
        connection
            .prepare_cached(&format!("EXPLAIN QUERY PLAN {sql}"))
            .map_err(failure)?
            .query_map(params_from_iter(search_parameters.iter()), |row| {
                row.get::<_, String>(3)
            })
            .map_err(failure)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(failure)?
    } else {
        Vec::new()
    };
    let items = super::sqlite_store::rows(&connection, &sql, search_parameters)?;
    Ok(json!({"items": items, "queryPlan": query_plan}))
}

use std::collections::HashSet;
use std::sync::OnceLock;

use regex::Regex;
use serde_json::{Value, json};

const TECHNICAL_CONCEPTS: &[(&[&str], &[&str])] = &[
    (
        &["取消", "中断", "终止"],
        &[
            "cancel",
            "cancellation",
            "cancelled",
            "abort",
            "aborted",
            "terminate",
        ],
    ),
    (
        &["恢复", "续传"],
        &["resume", "resumed", "restore", "recovery"],
    ),
    (&["重试"], &["retry", "retries"]),
    (&["超时"], &["timeout", "timedout"]),
    (&["并发"], &["concurrency", "concurrent"]),
    (&["队列"], &["queue", "queued"]),
    (&["检查点", "断点"], &["checkpoint", "checkpoints"]),
    (&["索引"], &["index", "indexing", "indexed"]),
    (&["缓存"], &["cache", "cached", "caching"]),
    (
        &["权限", "审批"],
        &[
            "permission",
            "permissions",
            "approval",
            "authorize",
            "authorization",
        ],
    ),
    (&["绑定", "挂载"], &["binding", "bound", "mount", "mounted"]),
    (
        &["持久化", "存储", "保存"],
        &[
            "persist",
            "persisted",
            "persistence",
            "storage",
            "store",
            "save",
            "saved",
        ],
    ),
    (&["序列化"], &["serialize", "serialization"]),
    (&["压缩"], &["compact", "compaction", "compression"]),
    (&["分块"], &["chunk", "chunks", "chunking"]),
    (&["分词"], &["tokenizer", "tokenization"]),
    (&["嵌入"], &["embedding", "embeddings"]),
    (&["重排"], &["rerank", "reranking"]),
    (&["上下文"], &["context"]),
    (
        &["引用", "调用方"],
        &["reference", "references", "caller", "callers"],
    ),
    (&["配置", "设置"], &["configuration", "config", "settings"]),
    (&["内存"], &["memory"]),
    (&["磁盘"], &["disk"]),
];

fn han_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"\p{sc=Han}+").expect("static Han pattern"))
}

fn token_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN
        .get_or_init(|| Regex::new(r"\p{sc=Han}+|[\p{L}\p{N}_$]+").expect("static token pattern"))
}

fn is_han(text: &str) -> bool {
    han_pattern()
        .find(text)
        .is_some_and(|found| found.start() == 0 && found.end() == text.len())
}

fn append(
    term: String,
    limit: usize,
    deduplicate: bool,
    result: &mut Vec<String>,
    seen: &mut HashSet<String>,
) {
    if result.len() >= limit || deduplicate && !seen.insert(term.clone()) {
        return;
    }
    result.push(term);
}

/// Han is separated before Latin token matching; document counts retain repeated occurrences.
/// 中文先与相邻英文分隔；文档词频保留重复出现，查询词按同一规则去重。
pub fn collect_terms(text: &str, limit: usize, deduplicate: bool) -> Vec<String> {
    let separated = han_pattern().replace_all(text, " $0 ");
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    for found in token_pattern().find_iter(&separated) {
        let token = found.as_str();
        if is_han(token) {
            let characters: Vec<char> = token.chars().collect();
            for (index, character) in characters.iter().enumerate() {
                append(
                    character.to_string(),
                    limit,
                    deduplicate,
                    &mut result,
                    &mut seen,
                );
                if let Some(next) = characters.get(index + 1) {
                    append(
                        format!("{character}{next}"),
                        limit,
                        deduplicate,
                        &mut result,
                        &mut seen,
                    );
                }
                if result.len() >= limit {
                    break;
                }
            }
        } else {
            let normalized = token.to_lowercase();
            append(
                normalized.clone(),
                limit,
                deduplicate,
                &mut result,
                &mut seen,
            );
            let characters: Vec<char> = token.chars().collect();
            let mut split = String::new();
            for (index, character) in characters.iter().enumerate() {
                if index > 0
                    && character.is_ascii_uppercase()
                    && (characters[index - 1].is_ascii_lowercase()
                        || characters[index - 1].is_ascii_digit()
                        || characters[index - 1].is_ascii_uppercase()
                            && characters
                                .get(index + 1)
                                .is_some_and(char::is_ascii_lowercase))
                {
                    split.push(' ');
                }
                split.push(*character);
            }
            let mut parts = HashSet::new();
            for part in split
                .split(|character| character == '_' || character == '$' || character == ' ')
                .filter(|part| !part.is_empty())
            {
                let part = part.to_lowercase();
                if part != normalized && parts.insert(part.clone()) {
                    append(part, limit, deduplicate, &mut result, &mut seen);
                }
            }
        }
        if result.len() >= limit {
            break;
        }
    }
    result
}

fn concepts(text: &str, domain: &str) -> Vec<Vec<String>> {
    if domain == "knowledge" {
        return Vec::new();
    }
    let terms: HashSet<String> = collect_terms(text, 512, true).into_iter().collect();
    TECHNICAL_CONCEPTS
        .iter()
        .filter(|(phrases, aliases)| {
            phrases.iter().any(|phrase| text.contains(phrase))
                || aliases.iter().any(|alias| terms.contains(*alias))
        })
        .map(|(_, aliases)| aliases.iter().map(|alias| (*alias).to_owned()).collect())
        .collect()
}

fn query_terms(args: &Value, default_limit: usize) -> Vec<String> {
    let text = args["text"].as_str().unwrap_or("");
    let normalized = args["normalizedText"].as_str().unwrap_or(text);
    let limit = term_limit(args, default_limit);
    let mut terms = collect_terms(text, limit, true);
    let mut seen: HashSet<String> = terms.iter().cloned().collect();
    for aliases in concepts(normalized, args["domain"].as_str().unwrap_or("mixed")) {
        for alias in aliases {
            if terms.len() >= limit {
                return terms;
            }
            if seen.insert(alias.clone()) {
                terms.push(alias);
            }
        }
    }
    terms
}

fn term_limit(args: &Value, default: usize) -> usize {
    match args.get("maximumTerms") {
        None => default,
        Some(value) => value
            .as_f64()
            .map(|value| value.ceil().max(0.0) as usize)
            .unwrap_or(usize::MAX),
    }
}

pub fn apply(operation: &str, args: &Value) -> Result<Value, String> {
    let text = args["text"].as_str().unwrap_or("");
    Ok(match operation {
        "retrieval.lexical-terms" => json!(collect_terms(text, term_limit(args, 20000), true)),
        "retrieval.lexical-text" => json!(collect_terms(text, 20000, false).join(" ")),
        "retrieval.query-concepts" => json!(concepts(
            args["normalizedText"].as_str().unwrap_or(text),
            args["domain"].as_str().unwrap_or("mixed")
        )),
        "retrieval.query-terms" => json!(query_terms(args, 64)),
        "retrieval.match-expression" => {
            let mut bounded = args.clone();
            bounded["maximumTerms"] = json!(64);
            let terms = query_terms(&bounded, 64);
            let bigrams = terms
                .iter()
                .any(|term| is_han(term) && term.chars().count() == 2);
            json!(
                terms
                    .iter()
                    .filter(|term| !bigrams || !is_han(term) || term.chars().count() != 1)
                    .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
                    .collect::<Vec<_>>()
                    .join(" OR ")
            )
        }
        _ => return Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixed_identifiers_han_and_frequency_keep_order() {
        assert_eq!(
            collect_terms("SQLite索引 HTTPServer cancelTask cancelTask", 30, true),
            vec![
                "sqlite",
                "sq",
                "lite",
                "索",
                "索引",
                "引",
                "httpserver",
                "http",
                "server",
                "canceltask",
                "cancel",
                "task"
            ]
        );
        assert_eq!(
            collect_terms("alpha alpha cancelTask", 30, false),
            vec!["alpha", "alpha", "canceltask", "cancel", "task"]
        );
        assert!(collect_terms("内容", 0, true).is_empty());
    }

    #[test]
    fn query_aliases_never_change_knowledge_terms_or_execute_punctuation() {
        assert!(
            apply(
                "retrieval.query-concepts",
                &json!({"text":"取消", "domain":"knowledge"})
            )
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
        );
        let expression = apply(
            "retrieval.match-expression",
            &json!({"text":"取消 OR * \""}),
        )
        .unwrap();
        assert!(expression.as_str().unwrap().contains("\"cancel\""));
        assert!(!expression.as_str().unwrap().contains('*'));
    }
}

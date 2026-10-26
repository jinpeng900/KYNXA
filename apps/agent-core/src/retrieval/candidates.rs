use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use regex::Regex;
use serde_json::{Value, json};

use super::lexical;

const COMMON_WORDS: &[&str] = &[
    "the",
    "a",
    "an",
    "and",
    "or",
    "to",
    "of",
    "in",
    "on",
    "is",
    "are",
    "what",
    "how",
    "where",
    "when",
    "which",
    "does",
    "do",
    "can",
    "should",
    "please",
    "this",
    "that",
    "it",
    "according",
    "document",
    "file",
    "project",
];

fn informative_terms(text: &str) -> Vec<String> {
    static STRIP: OnceLock<Regex> = OnceLock::new();
    static HAN: OnceLock<Regex> = OnceLock::new();
    let stripped = STRIP.get_or_init(|| Regex::new("根据资料|根据文档|帮我|请问|请你|如何|怎么|什么|这个|那个|之前|上次|资料|文档|文件|项目").expect("static query filler")).replace_all(text, " ");
    let terms = lexical::collect_terms(&stripped, 512, true);
    let han = HAN.get_or_init(|| Regex::new(r"^\p{sc=Han}+$").expect("static Han terms"));
    let bigrams = terms
        .iter()
        .any(|term| han.is_match(term) && term.chars().count() == 2);
    terms
        .into_iter()
        .filter(|term| {
            !COMMON_WORDS.contains(&term.as_str())
                && (!bigrams || !han.is_match(term) || term.chars().count() != 1)
        })
        .collect()
}

fn text<'a>(value: &'a Value, field: &str) -> &'a str {
    value[field].as_str().unwrap_or("")
}

fn role(item: &Value) -> &'static str {
    static TEST: OnceLock<Regex> = OnceLock::new();
    static CONFIG: OnceLock<Regex> = OnceLock::new();
    static CODE: OnceLock<Regex> = OnceLock::new();
    let path = text(item, "rolePath");
    if TEST
        .get_or_init(|| {
            Regex::new(r"(?:^|[\\/._-])(?:tests?|spec)(?:[\\/._-]|$)").expect("static test path")
        })
        .is_match(path)
    {
        return "tests";
    }
    if CONFIG
        .get_or_init(|| {
            Regex::new(r"(?:config|settings)|\.(?:json|ya?ml|toml)$").expect("static config path")
        })
        .is_match(path)
    {
        return "configuration";
    }
    if item["structure"]["domain"] == "code"
        || CODE
            .get_or_init(|| {
                Regex::new(r"\.(?:cs|[cm]?js|jsx|[cm]?ts|tsx|py|rs|go)$")
                    .expect("static source path")
            })
            .is_match(path)
    {
        "implementation"
    } else {
        "source"
    }
}

fn requested_roles(query: &str) -> Vec<&'static str> {
    static TEST: OnceLock<Regex> = OnceLock::new();
    static CONFIG: OnceLock<Regex> = OnceLock::new();
    static CODE: OnceLock<Regex> = OnceLock::new();
    let mut roles = Vec::new();
    if TEST
        .get_or_init(|| {
            Regex::new(r"(?i)测试|验证|回归|\b(?:tests?|testing|verify|validation|regression)\b")
                .expect("static requested tests")
        })
        .is_match(query)
    {
        roles.push("tests");
    }
    if CONFIG
        .get_or_init(|| {
            Regex::new(r"(?i)配置|设置|参数|\b(?:configuration|config|settings|parameters?)\b")
                .expect("static requested configuration")
        })
        .is_match(query)
    {
        roles.push("configuration");
    }
    if CODE
        .get_or_init(|| {
            Regex::new(
                r"(?i)实现|逻辑|流程|函数|代码|\b(?:implementation|logic|function|code|flow)\b",
            )
            .expect("static requested implementation")
        })
        .is_match(query)
    {
        roles.push("implementation");
    }
    roles
}

pub fn target_match(item: &Value, intent: &Value) -> (bool, bool) {
    let path = text(intent, "path");
    let item_path = text(item, "normalizedPath");
    let symbol = text(intent, "symbol");
    let path_match =
        !path.is_empty() && (item_path == path || item_path.ends_with(&format!("/{path}")));
    let symbol_match = !symbol.is_empty()
        && ["symbolName", "qualifiedName"].iter().any(|key| {
            item["structure"][key]
                .as_str()
                .is_some_and(|value| value == symbol || value.ends_with(&format!(".{symbol}")))
        });
    (path_match, symbol_match)
}

fn range_overlap(left: &Value, right: &Value) -> f64 {
    if ["sourceId", "scopeKey", "contentHash"]
        .iter()
        .any(|key| left[key] != right[key])
    {
        return 0.0;
    }
    let (Some(a), Some(b), Some(c), Some(d)) = (
        left["startOffset"].as_i64(),
        left["endOffset"].as_i64(),
        right["startOffset"].as_i64(),
        right["endOffset"].as_i64(),
    ) else {
        return 0.0;
    };
    let shortest = (b - a).min(d - c);
    if shortest <= 0 {
        0.0
    } else {
        ((b.min(d) - a.max(c)).max(0) as f64) / shortest as f64
    }
}

#[derive(Clone)]
struct Kept {
    index: usize,
    item: Value,
    navigation: bool,
}

struct Deduplicated {
    items: Vec<Kept>,
    duplicate_count: usize,
    present_count: usize,
    covered_count: usize,
}

/// Return original indices without changing source objects, offsets, receipts or authorization.
/// 返回原始下标，保留来源对象、偏移、回执及授权边界。
fn deduplicate(args: &Value) -> Result<Deduplicated, String> {
    let items = args["items"]
        .as_array()
        .ok_or("INVALID_RETRIEVAL_CANDIDATES")?;
    let intent = &args["intent"];
    let priority = |item: &Value| {
        let (path, symbol) = target_match(item, intent);
        i32::from(item["exactTargetMatch"] == true) * 4 + i32::from(path) + i32::from(symbol)
    };
    let mut indices: Vec<usize> = (0..items.len()).collect();
    indices.sort_by_key(|index| std::cmp::Reverse(priority(&items[*index])));
    let existing: Vec<&str> = args["existing"]
        .as_array()
        .map(|items| items.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut seen_chunks = HashSet::new();
    let mut seen_text = HashSet::new();
    let mut result = Deduplicated {
        items: Vec::new(),
        duplicate_count: 0,
        present_count: 0,
        covered_count: 0,
    };
    for index in indices {
        let original = &items[index];
        let mut item = original.clone();
        let original_text = text(&item, "normalizedExcerpt");
        let covered = args["navigateCoveredMessages"] == true
            && original["sourceType"] == "message"
            && items.iter().any(|source| {
                let source_text = text(source, "normalizedExcerpt");
                source["sourceType"] != "message"
                    && source["navigationOnly"] != true
                    && source_text.encode_utf16().count() >= 8
                    && original_text.contains(source_text)
            });
        if covered {
            item["normalizedExcerpt"] = json!("");
            item["excerpt"] = json!("");
            item["structure"] = Value::Null;
            item["navigationOnly"] = json!(true);
            result.covered_count += 1;
        }
        let body = text(&item, "normalizedExcerpt");
        if body.is_empty() && item["navigationOnly"] != true {
            continue;
        }
        let chunk = [item.get("chunkId"), item.get("sourceRef")]
            .into_iter()
            .flatten()
            .find(|value| !value.is_null())
            .and_then(Value::as_str)
            .unwrap_or(body);
        let chunk_key = format!(
            "{}:{}:{chunk}",
            text(&item, "scopeKey"),
            text(&item, "sourceId")
        );
        if seen_chunks.contains(&chunk_key)
            || !body.is_empty() && seen_text.contains(body)
            || result
                .items
                .iter()
                .any(|previous| range_overlap(&previous.item, &item) >= 0.75)
        {
            result.duplicate_count += 1;
            continue;
        }
        seen_chunks.insert(chunk_key);
        if !body.is_empty() {
            seen_text.insert(body.to_owned());
        }
        if body.encode_utf16().count() >= 8 && existing.iter().any(|context| context.contains(body))
        {
            result.present_count += 1;
            continue;
        }
        result.items.push(Kept {
            index,
            navigation: covered,
            item,
        });
    }
    Ok(result)
}

fn assessment(items: &[Kept], args: &Value, already_present: usize) -> Value {
    let query = text(args, "query");
    let query_terms = informative_terms(query);
    if items.is_empty() {
        return json!({"state":"empty", "reason": if already_present > 0 { "already-in-context" } else { "no-current-evidence" }, "requiresSourceRead":false,"matchedTerms":0,"queryTerms":query_terms.len()});
    }
    let mut matched = HashSet::new();
    let mut body_match = false;
    for kept in items {
        let terms: HashSet<String> = informative_terms(&format!(
            "{} {}",
            text(&kept.item, "title"),
            text(&kept.item, "excerpt")
        ))
        .into_iter()
        .collect();
        let body: HashSet<String> = informative_terms(text(&kept.item, "excerpt"))
            .into_iter()
            .collect();
        for term in &query_terms {
            if terms.contains(term) {
                matched.insert(term.clone());
                body_match |= body.contains(term);
            }
        }
    }
    let coverage = if query_terms.is_empty() {
        0.0
    } else {
        matched.len() as f64 / query_terms.len() as f64
    };
    let usable = body_match && coverage >= 0.4;
    let navigation = items.iter().all(|kept| kept.item["navigationOnly"] == true);
    let semantic_only = !body_match
        && items.iter().any(|kept| {
            kept.item["vectorRank"]
                .as_u64()
                .is_some_and(|rank| rank > 0)
        });
    let mut present_roles = Vec::new();
    for kept in items {
        let value = role(&kept.item);
        if !present_roles.contains(&value) {
            present_roles.push(value);
        }
    }
    let mut missing: Vec<Value> = requested_roles(query)
        .iter()
        .filter(|role| !present_roles.contains(role))
        .map(|role| json!({"kind":"requested-role", "role":role}))
        .collect();
    let intent = &args["intent"];
    if !text(intent, "symbol").is_empty()
        && !items
            .iter()
            .any(|kept| target_match(&kept.item, intent).1 || kept.item["exactTargetMatch"] == true)
    {
        missing.push(json!({"kind":"symbol-definition", "symbol":intent["symbol"]}));
    }
    if !text(intent, "path").is_empty()
        && !items.iter().any(|kept| target_match(&kept.item, intent).0)
    {
        missing.push(json!({"kind":"requested-file", "path":intent["path"]}));
    }
    json!({"state":if usable {"usable"} else {"weak"}, "reason":if navigation {"navigation-only"} else if usable {"query-terms-supported"} else if semantic_only {"semantic-only-unverified"} else {"partial-query-support"},
        "requiresSourceRead": args["requiresSourceRead"] == true || !usable || navigation, "matchedTerms":matched.len(), "queryTerms":query_terms.len(), "queryCoverage":coverage,
        "missingEvidence":missing, "presentRoles":present_roles, "sufficiency":"not-evaluated"})
}

fn coverage(terms: &HashSet<String>, query: &[String], concepts: &[Value]) -> f64 {
    let literal = if query.is_empty() {
        0.0
    } else {
        query.iter().filter(|term| terms.contains(*term)).count() as f64 / query.len() as f64
    };
    let bridged = if concepts.is_empty() {
        0.0
    } else {
        concepts
            .iter()
            .filter(|aliases| {
                aliases.as_array().is_some_and(|aliases| {
                    aliases
                        .iter()
                        .filter_map(Value::as_str)
                        .any(|alias| terms.contains(alias))
                })
            })
            .count() as f64
            / concepts.len() as f64
    };
    literal.max(bridged * 0.85)
}

pub fn apply(operation: &str, args: &Value) -> Result<Value, String> {
    if operation == "retrieval.candidate-assess" {
        let items = args["items"]
            .as_array()
            .ok_or("INVALID_RETRIEVAL_CANDIDATES")?
            .iter()
            .enumerate()
            .map(|(index, item)| Kept {
                index,
                item: item.clone(),
                navigation: false,
            })
            .collect::<Vec<_>>();
        return Ok(assessment(
            &items,
            args,
            args["alreadyPresentCount"].as_u64().unwrap_or(0) as usize,
        ));
    }
    let deduplicated = deduplicate(args)?;
    if operation == "retrieval.candidate-deduplicate" {
        return Ok(
            json!({"items":deduplicated.items.iter().map(|kept| json!({"index":kept.index,"navigation":kept.navigation})).collect::<Vec<_>>(),
            "duplicateCount":deduplicated.duplicate_count, "alreadyPresentCount":deduplicated.present_count,"coveredMessageCount":deduplicated.covered_count}),
        );
    }
    if operation != "retrieval.candidate-select" {
        return Err("AGENT_CORE_UNKNOWN_OPERATION".into());
    }
    let limit = args["limit"]
        .as_u64()
        .filter(|limit| (1..=60).contains(limit))
        .ok_or("INVALID_RETRIEVAL_RESULT_LIMIT")? as usize;
    let maximum_tokens = args["maximumTokens"]
        .as_u64()
        .filter(|tokens| *tokens <= 32768)
        .ok_or("INVALID_RETRIEVAL_TOKEN_BUDGET")?;
    let query = text(args, "query");
    let query_terms = informative_terms(query);
    let concepts = lexical::apply(
        "retrieval.query-concepts",
        &json!({"normalizedText":args["normalizedQuery"],"domain":args["intent"]["domain"]}),
    )?;
    let concepts = concepts.as_array().ok_or("INVALID_RETRIEVAL_CANDIDATES")?;
    let roles = requested_roles(query);
    struct Candidate {
        kept: Kept,
        terms: HashSet<String>,
        role: &'static str,
        target: (bool, bool),
        relevance: f64,
        similarity: f64,
    }
    let mut candidates = deduplicated
        .items
        .iter()
        .enumerate()
        .map(|(index, kept)| {
            let terms = informative_terms(text(&kept.item, "excerpt"))
                .into_iter()
                .collect();
            let metadata = informative_terms(text(&kept.item, "metadataText"))
                .into_iter()
                .collect();
            let role = role(&kept.item);
            let relevance = (1.0 - index as f64 / deduplicated.items.len().max(1) as f64) * 0.55
                + coverage(&terms, &query_terms, concepts) * 0.3
                + coverage(&metadata, &query_terms, concepts) * 0.1
                + u8::from(roles.contains(&role)) as f64 * 0.05;
            Candidate {
                kept: kept.clone(),
                terms,
                role,
                target: target_match(&kept.item, &args["intent"]),
                relevance,
                similarity: 0.0,
            }
        })
        .collect::<Vec<_>>();
    let weight = args["lambda"].as_f64().unwrap_or(0.7).clamp(0.0, 1.0);
    let mut selected = Vec::new();
    let mut source_counts: HashMap<String, usize> = HashMap::new();
    let mut selected_roles = HashSet::new();
    let mut used_tokens = 0;
    let mut omitted_budget = 0;
    while !candidates.is_empty() && selected.len() < limit {
        let exact = candidates
            .iter()
            .any(|candidate| candidate.kept.item["exactTargetMatch"] == true);
        let mut best_index = 0;
        let mut best_score = f64::NEG_INFINITY;
        for (index, candidate) in candidates.iter().enumerate() {
            if exact && candidate.kept.item["exactTargetMatch"] != true {
                continue;
            }
            let key = format!(
                "{}:{}",
                text(&candidate.kept.item, "scopeKey"),
                text(&candidate.kept.item, "sourceId")
            );
            let count = *source_counts.get(&key).unwrap_or(&0) as f64;
            let diversity = if !text(&args["intent"], "path").is_empty() {
                0.0
            } else if args["requiresSourceRead"] == true || roles.len() > 1 {
                0.22
            } else {
                0.14
            };
            let boost =
                if roles.contains(&candidate.role) && !selected_roles.contains(candidate.role) {
                    0.06
                } else {
                    0.0
                };
            let score = weight * candidate.relevance
                + u8::from(candidate.target.0) as f64 * 0.3
                + u8::from(candidate.target.1) as f64 * 0.35
                + boost
                - diversity * count / (count + 1.0)
                - (1.0 - weight) * candidate.similarity;
            if score > best_score {
                best_index = index;
                best_score = score;
            }
        }
        let candidate = candidates.remove(best_index);
        let cost = candidate.kept.item["cost"]
            .as_u64()
            .ok_or("INVALID_RETRIEVAL_TOKEN_BUDGET")?;
        if used_tokens + cost > maximum_tokens {
            omitted_budget += 1;
            continue;
        }
        let key = format!(
            "{}:{}",
            text(&candidate.kept.item, "scopeKey"),
            text(&candidate.kept.item, "sourceId")
        );
        *source_counts.entry(key).or_default() += 1;
        selected_roles.insert(candidate.role);
        for remaining in &mut candidates {
            let intersection = candidate.terms.intersection(&remaining.terms).count();
            let similarity = if candidate.terms.is_empty() || remaining.terms.is_empty() {
                0.0
            } else {
                intersection as f64
                    / (candidate.terms.len() + remaining.terms.len() - intersection) as f64
            };
            remaining.similarity = remaining.similarity.max(similarity);
        }
        selected.push(candidate.kept);
        used_tokens += cost;
    }
    let mut navigation_count = 0;
    if selected.is_empty() && maximum_tokens > 0 {
        for kept in &deduplicated.items {
            let cost = kept.item["navigationCost"]
                .as_u64()
                .ok_or("INVALID_RETRIEVAL_TOKEN_BUDGET")?;
            if cost > maximum_tokens {
                continue;
            }
            let mut navigation = kept.clone();
            navigation.navigation = true;
            navigation.item["excerpt"] = json!("");
            navigation.item["structure"] = Value::Null;
            navigation.item["navigationOnly"] = json!(true);
            source_counts.insert(
                format!(
                    "{}:{}",
                    text(&navigation.item, "scopeKey"),
                    text(&navigation.item, "sourceId")
                ),
                1,
            );
            selected.push(navigation);
            used_tokens = cost;
            navigation_count = 1;
            break;
        }
    }
    let mut cuts = Vec::new();
    for (reason, count) in [
        ("excerpt-replaced-by-read-navigation", navigation_count),
        (
            "duplicate-or-overlapping-evidence",
            deduplicated.duplicate_count,
        ),
        ("already-in-model-context", deduplicated.present_count),
        ("evidence-token-budget", omitted_budget),
        ("selected-fragment-limit", candidates.len()),
    ] {
        if count > 0 {
            cuts.push(json!({"reason":reason,"count":count}));
        }
    }
    Ok(
        json!({"items":selected.iter().map(|kept| json!({"index":kept.index,"navigation":kept.navigation})).collect::<Vec<_>>(),
        "selection":{"candidateCount":args["items"].as_array().map_or(0,Vec::len),"uniqueCandidates":deduplicated.items.len(),"duplicateCount":deduplicated.duplicate_count,
            "alreadyPresentCount":deduplicated.present_count,"omittedForBudget":omitted_budget,"omittedForLimit":candidates.len(),"selectedCount":selected.len(),"limit":limit,"usedTokens":used_tokens,
            "maximumTokens":maximum_tokens,"earlyCutReasons":cuts,"method":"metadata-query-mmr","sourceCount":source_counts.len()},
        "evidenceAssessment":assessment(&selected,args,deduplicated.present_count)}),
    )
}

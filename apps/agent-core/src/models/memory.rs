use std::collections::HashSet;
use std::sync::OnceLock;

use regex::Regex;
use serde_json::{Value, json};

use super::protocol::{array, estimate_tokens, quote, string, trim};

fn equal_id(left: &Value, right: &Value) -> bool {
    left.as_str()
        .zip(right.as_str())
        .is_some_and(|(left, right)| left.to_lowercase() == right.to_lowercase())
}

fn memory_line(entry: &Value, excerpt: Option<&str>) -> String {
    let label = match string(&entry["scope"]) {
        "chat" => "当前聊天",
        "project" => "当前工作",
        _ => "用户全局",
    };
    let source = if entry["source"]["type"] == "user-message" {
        format!(
            "用户消息 {}/{}",
            string(&entry["source"]["conversationId"]),
            string(&entry["source"]["messageId"])
        )
    } else {
        "用户手工确认".to_owned()
    };
    format!(
        "[{label}已确认资料；来源：{source}] {}",
        excerpt
            .map(str::to_owned)
            .unwrap_or_else(|| quote(string(&entry["content"])))
    )
}

fn memory_system(lines: &[String], notice: &str) -> String {
    if lines.is_empty() {
        return String::new();
    }
    std::iter::once(notice)
        .chain(lines.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

fn system_tokens(lines: &[String], notice: &str) -> u64 {
    let system = memory_system(lines, notice);
    if system.is_empty() {
        0
    } else {
        8 + estimate_tokens(&system)
    }
}

fn clip(text: &str, budget: u64) -> String {
    if estimate_tokens(text) <= budget {
        return text.to_owned();
    }
    let chars: Vec<_> = text.chars().collect();
    let (mut low, mut high) = (0, chars.len());
    while low < high {
        let middle = (low + high).div_ceil(2);
        if estimate_tokens(&format!("{}…", chars[..middle].iter().collect::<String>())) <= budget
        {
            low = middle
        } else {
            high = middle - 1;
        }
    }
    if low == 0 {
        String::new()
    } else {
        format!("{}…", chars[..low].iter().collect::<String>())
    }
}

fn excerpt(entry: &Value, lines: &[String], budget: u64, notice: &str) -> Option<String> {
    let prefix = "仅首尾原文摘录，完整记忆仍已保存：";
    let mut candidates = lines.to_vec();
    candidates.push(memory_line(entry, Some(prefix)));
    let mut content_budget = budget.saturating_sub(system_tokens(&candidates, notice) + 12);
    let content = string(&entry["content"]);
    let reversed: String = content.chars().rev().collect();
    while content_budget >= 48 {
        let beginning = clip(content, content_budget / 2);
        let ending: String = clip(&reversed, content_budget / 2).chars().rev().collect();
        let line = memory_line(
            entry,
            Some(&format!(
                "{prefix}{} … {}",
                quote(&beginning),
                quote(&ending)
            )),
        );
        let mut candidates = lines.to_vec();
        candidates.push(line.clone());
        if system_tokens(&candidates, notice) <= budget {
            return Some(line);
        }
        content_budget = (content_budget as f64 * 0.8).floor() as u64;
    }
    None
}

/// Confirmed scope precedes ranking; relevance never turns candidate facts into authority.
/// 已确认状态和作用域先于排序；相关性不能把候选事实提升为权威记忆。
fn prepare(args: &Value) -> Value {
    let entries = array(&args["entries"]);
    let mut seen = HashSet::new();
    let mut valid_indexes = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let Some(id) = entry["id"].as_str() else {
            continue;
        };
        if entry["status"] != "confirmed"
            || entry["active"] == false
            || seen.contains(id)
            || !entry["content"].is_string()
            || trim(string(&entry["content"])).is_empty()
            || !matches!(string(&entry["kind"]), "fact" | "preference" | "decision")
        {
            continue;
        }
        let allowed = match string(&entry["scope"]) {
            "chat" => equal_id(&entry["scopeId"], &args["conversationId"]),
            "project" => {
                !string(&args["projectId"]).is_empty()
                    && equal_id(&entry["scopeId"], &args["projectId"])
            }
            "user" => entry["scopeId"] == "user",
            _ => false,
        };
        if allowed {
            seen.insert(id);
            valid_indexes.push(index);
        }
    }
    let budget = args["memoryBudgetTokens"].as_u64().unwrap_or(0);
    let notice = string(&args["referenceNotice"]);
    let mut ordered = valid_indexes.clone();
    let all_lines: Vec<_> = ordered
        .iter()
        .map(|index| memory_line(&entries[*index], None))
        .collect();
    if system_tokens(&all_lines, notice) > budget {
        static TERMS: OnceLock<Regex> = OnceLock::new();
        static HAN: OnceLock<Regex> = OnceLock::new();
        let pattern = TERMS.get_or_init(|| Regex::new(r"[a-z0-9_]{3,}|\p{Han}{2,}").unwrap());
        let han = HAN.get_or_init(|| Regex::new(r"\p{Han}").unwrap());
        let query = string(&args["query"]).to_lowercase();
        let generic: HashSet<Vec<u16>> = [
            "继续", "工作", "这个", "那个", "问题", "请问", "一下", "怎么", "如何", "什么",
        ]
        .iter()
        .map(|term| term.encode_utf16().collect())
        .collect();
        let mut terms: HashSet<Vec<u16>> = HashSet::new();
        for matched in pattern.find_iter(&query) {
            let term = matched.as_str();
            let units: Vec<_> = term.encode_utf16().collect();
            if han.is_match(term) {
                for pair in units.windows(2) {
                    if !generic.contains(pair) {
                        terms.insert(pair.to_vec());
                    }
                }
            } else if !generic.contains(&units) {
                terms.insert(units);
            }
        }
        let priority = |index: usize| {
            let entry = &entries[index];
            let text: Vec<_> = string(&entry["content"])
                .to_lowercase()
                .encode_utf16()
                .collect();
            let matches = terms
                .iter()
                .filter(|term| {
                    text.windows(term.len())
                        .any(|window| window == term.as_slice())
                })
                .count();
            (if matches > 0 {
                100 + matches.min(50)
            } else {
                0
            }) + if entry["kind"] == "preference" {
                20
            } else if entry["kind"] == "decision" {
                15
            } else {
                0
            }
        };
        ordered.sort_by(|left, right| {
            priority(*right)
                .cmp(&priority(*left))
                .then(
                    (estimate_tokens(&memory_line(&entries[*right], None)) as f64
                        <= budget as f64 * 0.5)
                        .cmp(
                            &(estimate_tokens(&memory_line(&entries[*left], None)) as f64
                                <= budget as f64 * 0.5),
                        ),
                )
                .then(left.cmp(right))
        });
    }
    let mut lines = Vec::new();
    let mut included = Vec::new();
    let mut truncated = Vec::new();
    let mut projection = Vec::new();
    for index in ordered {
        let entry = &entries[index];
        let mut candidates = lines.clone();
        candidates.push(memory_line(entry, None));
        let full = system_tokens(&candidates, notice) <= budget;
        let Some(line) = (if full {
            candidates.last().cloned()
        } else {
            excerpt(entry, &lines, budget, notice)
        }) else {
            continue;
        };
        included.push(entry["id"].clone());
        if !full {
            truncated.push(entry["id"].clone());
        }
        let mut item = json!({"memoryId":entry["id"],"line":line});
        if full {
            item["content"] = entry["content"].clone();
        }
        projection.push(item);
        lines.push(line);
    }
    let omitted: Vec<_> = valid_indexes
        .iter()
        .filter(|index| !included.contains(&entries[**index]["id"]))
        .map(|index| entries[*index]["id"].clone())
        .collect();
    json!({"validIndexes":valid_indexes,"memoryLines":lines,"includedMemoryIds":included,"truncatedMemoryIds":truncated,"memoryProjection":projection,"omittedMemoryIds":omitted})
}

pub fn calculate(operation: &str, args: &Value) -> Result<Value, String> {
    match operation {
        "models.memory.prepare" => Ok(prepare(args)),
        _ => Err("AGENT_CORE_UNKNOWN_OPERATION".to_owned()),
    }
}

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

fn query_terms(query: &str) -> HashSet<Vec<u16>> {
    static TERMS: OnceLock<Regex> = OnceLock::new();
    static HAN: OnceLock<Regex> = OnceLock::new();
    static GENERIC_HAN: OnceLock<Regex> = OnceLock::new();
    static GENERIC_ENGLISH: OnceLock<HashSet<&'static str>> = OnceLock::new();
    let pattern = TERMS.get_or_init(|| Regex::new(r"[a-z0-9_]{3,}|\p{Han}{2,}").unwrap());
    let han = HAN.get_or_init(|| Regex::new(r"\p{Han}").unwrap());
    let generic_han = GENERIC_HAN.get_or_init(|| Regex::new(
        r"继续|工作|这个|那个|这些|那些|问题|请问|一下|怎么|如何|什么|生成|文档|文件|当前|项目|要求|使用|帮我|帮忙|给我|一个|一份|需要|请把|打开|写个|编写|进行|相关|任务|处理"
    ).unwrap());
    let generic_english = GENERIC_ENGLISH.get_or_init(|| [
        "about", "and", "are", "can", "could", "create", "current", "document", "documents",
        "file", "files", "for", "from", "generate", "get", "has", "have", "help", "into",
        "its", "make", "need", "new", "now", "open", "please", "project", "projects", "set",
        "should", "show", "some", "task", "tasks", "that", "the", "their", "them", "there",
        "these", "this", "those", "use", "using", "want", "was", "were", "what", "when",
        "where", "which", "will", "with", "would", "write", "you", "your",
    ].into_iter().collect());
    let mut terms = HashSet::new();
    for matched in pattern.find_iter(query).take(128) {
        let term = matched.as_str();
        if han.is_match(term) {
            // Split generic actions before forming pairs, so removed words cannot leave cross-word matches.
            // 先切除通用动作再提取双字词，避免被删除的词留下跨词误匹配。
            let meaningful = generic_han.replace_all(term, " ");
            for segment in pattern.find_iter(&meaningful) {
                let characters: Vec<_> = segment.as_str().chars().collect();
                for pair in characters.windows(2).take(128) {
                    terms.insert(pair.iter().collect::<String>().encode_utf16().collect());
                }
                if terms.len() >= 128 { break; }
            }
        } else if !generic_english.contains(term) {
            terms.insert(term.encode_utf16().collect());
        }
        if terms.len() >= 128 { break; }
    }
    terms
}

fn relevance(entry: &Value, terms: &HashSet<Vec<u16>>) -> usize {
    let content:Vec<_>=string(&entry["content"]).to_lowercase().encode_utf16().collect();
    terms.iter().filter(|term|content.windows(term.len()).any(|window|window==term.as_slice())).count()
}

fn recalls_memory(query: &str) -> bool {
    static RECALL: OnceLock<Regex> = OnceLock::new();
    // Only explicit saved-memory inspection widens recall; opening a preference UI keeps relevance filtering.
    // 只有明确查看已保存记忆才扩大回忆范围；打开偏好设置界面仍按相关性筛选。
    RECALL.get_or_init(|| Regex::new(r"(?:查看|读取|检查|列出|显示|回忆).{0,12}(?:(?:已保存|已确认|全局|用户|我的|当前聊天|当前项目)的?记忆|记忆内容|(?:你|我们)?(?:已)?记住的(?:内容|事情|信息))|(?:查看|读取|列出|显示).{0,12}(?:已保存|已确认|记住)的(?:内容|偏好|决定)|^(?:你|我们)(?:还)?记得(?:什么|哪些|的内容)[？?。\s]*$|\b(?:read|inspect|list|show|recall)\b.{0,24}\b(?:(?:saved|stored|confirmed)\s+(?:memory|memories|preferences|decisions)|remembered\s+(?:content|facts|preferences|decisions))\b|\bwhat\s+(?:do|did|have)\s+you\s+(?:remember|remembered)\b").unwrap()).is_match(query)
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
    let query=string(&args["query"]).to_lowercase();
    let terms=query_terms(&query);
    let inspect_all=query.trim().is_empty() || recalls_memory(&query);
    let mut preference_budget=(budget/4).min(256);
    let mut decision_budget=(budget/4).min(384);
    // Relevance is applied even when everything would fit; only small scoped defaults may accompany another topic.
    // 即使总量未超预算也先做相关筛选；仅允许少量稳定偏好和当前范围决策随其它话题注入。
    let mut eligible=HashSet::new();
    for index in &valid_indexes {
        let entry=&entries[*index];
        if inspect_all || relevance(entry,&terms)>0 { eligible.insert(*index); continue; }
        let allocation=if entry["scope"]=="user" && entry["kind"]=="preference" {
            Some(&mut preference_budget)
        } else if matches!(string(&entry["scope"]),"chat"|"project")
            && matches!(string(&entry["kind"]),"decision"|"preference") { Some(&mut decision_budget) } else { None };
        if let Some(remaining)=allocation {
            if *remaining==0 { continue; }
            let cost=system_tokens(&[memory_line(entry,None)],"");
            if cost<=*remaining { *remaining-=cost; eligible.insert(*index); }
        }
    }
    let excluded=valid_indexes.iter().filter(|index|!eligible.contains(*index))
        .map(|index|entries[*index]["id"].clone()).collect::<Vec<_>>();
    let mut ordered = valid_indexes.clone();
    ordered.retain(|index|eligible.contains(index));
    let all_lines: Vec<_> = ordered
        .iter()
        .map(|index| memory_line(&entries[*index], None))
        .collect();
    if !query.trim().is_empty() || system_tokens(&all_lines, notice) > budget {
        let priority = |index: usize| {
            let entry = &entries[index];
            let matches = relevance(entry,&terms);
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
        .filter(|index| eligible.contains(*index) && !included.contains(&entries[**index]["id"]))
        .map(|index| entries[*index]["id"].clone())
        .collect();
    json!({"validIndexes":valid_indexes,"memoryLines":lines,"includedMemoryIds":included,"truncatedMemoryIds":truncated,"memoryProjection":projection,"omittedMemoryIds":omitted,
        "excludedMemoryIds":excluded,"selectionStrategy":"confirmed-scope-relevance-with-bounded-defaults"})
}

pub fn calculate(operation: &str, args: &Value) -> Result<Value, String> {
    match operation {
        "models.memory.prepare" => Ok(prepare(args)),
        _ => Err("AGENT_CORE_UNKNOWN_OPERATION".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str,scope: &str,scope_id: &str,kind: &str,content: &str) -> Value {
        json!({"id":id,"scope":scope,"scopeId":scope_id,"kind":kind,"content":content,"status":"confirmed","active":true})
    }

    #[test]
    fn memory_selection_filters_unrelated_facts_even_below_budget_and_preserves_explicit_recall() {
        let entries=json!([
            entry("database","user","user","fact","PostgreSQL migrations target version 16."),
            entry("gardening","user","user","fact","Tomatoes grow in the sunny garden. The current project document for gardeners uses 文件生成项目文档。")]);
        let mut arguments=json!({"entries":entries,"conversationId":"chat","projectId":"project","memoryBudgetTokens":4096,
            "query":"Review PostgreSQL migrations","referenceNotice":"Confirmed selected memory"});
        let selected=prepare(&arguments);
        assert_eq!(selected["includedMemoryIds"],json!(["database"]));
        assert_eq!(selected["excludedMemoryIds"],json!(["gardening"]));
        assert!(array(&selected["omittedMemoryIds"]).is_empty());
        for query in [
            "Please write a document for the current project",
            "当前项目要求生成文件",
            "显示用户偏好设置页面",
            "Show preferences settings",
        ] {
            arguments["query"] = json!(query);
            let unrelated = prepare(&arguments);
            assert!(array(&unrelated["includedMemoryIds"]).is_empty(), "{query}");
            assert_eq!(array(&unrelated["excludedMemoryIds"]).len(), 2, "{query}");
        }
        for query in ["查看全局记忆", "show saved memories", "你还记得什么"] {
            arguments["query"] = json!(query);
            let recalled = prepare(&arguments);
            assert_eq!(array(&recalled["includedMemoryIds"]).len(), 2, "{query}");
            assert!(array(&recalled["excludedMemoryIds"]).is_empty(), "{query}");
        }
        arguments["query"]=json!("");
        assert_eq!(array(&prepare(&arguments)["includedMemoryIds"]).len(),2);
    }

    #[test]
    fn memory_selection_bounds_default_preferences_and_retains_only_confirmed_current_scope() {
        let mut entries=vec![
            entry("language","user","user","preference","Always answer in Chinese."),
            entry("current-decision","project","project","decision","Keep the agreed deployment directory."),
            entry("other-project","project","elsewhere","decision","Unrelated project release requirements."),
            entry("other-chat","chat","elsewhere","fact","PostgreSQL belongs to another chat."),
            entry("inactive","user","user","fact","PostgreSQL inactive fixture."),
            entry("draft","user","user","fact","PostgreSQL unconfirmed fixture."),
            entry("current-fact","chat","chat","fact","The current project contains unrelated general records.")];
        entries[4]["active"]=json!(false);
        entries[5]["status"]=json!("draft");
        for index in 0..30 { entries.push(entry(&format!("optional-{index}"),"user","user","preference",
            &format!("Keep optional presentation preference number {index} with concise labels."))); }
        let selected=prepare(&json!({"entries":entries,"conversationId":"chat","projectId":"project",
            "memoryBudgetTokens":4096,"query":"Review PostgreSQL migrations","referenceNotice":"Confirmed selected memory"}));
        let included=array(&selected["includedMemoryIds"]);
        assert!(included.contains(&json!("language")));
        assert!(included.contains(&json!("current-decision")));
        for id in ["other-project","other-chat","inactive","draft"] { assert!(!included.contains(&json!(id)),"{id}"); }
        let preference_lines=array(&selected["memoryProjection"]).iter().filter(|projection|
            projection["memoryId"]!="current-decision").map(|projection|string(&projection["line"]).to_owned()).collect::<Vec<_>>();
        assert!(system_tokens(&preference_lines,"")<=256);
        assert!(!array(&selected["excludedMemoryIds"]).is_empty());
        assert!(array(&selected["excludedMemoryIds"]).contains(&json!("current-fact")));
    }
}

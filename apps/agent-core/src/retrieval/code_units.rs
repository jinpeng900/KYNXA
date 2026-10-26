use std::collections::{BinaryHeap, HashSet};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::chunking;

/// Map selected current evidence only, not a complete language-service graph.
/// 仅映射当前选中代码证据，不作为完整语言服务图。
pub fn navigation(items: &[Value], maximum_tokens: u64) -> Value {
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    let mut omitted = 0usize;
    for item in items {
        let structure = &item["structure"];
        let Some(symbol) = structure["qualifiedName"].as_str().or_else(|| structure["symbolName"].as_str())
            .filter(|symbol| !symbol.is_empty()) else { continue; };
        if structure["domain"] != "code" || structure["parseStatus"] == "unavailable" { continue; }
        let path = item["locator"]["relativePath"].as_str().unwrap_or("");
        let key = (item["locator"].to_string(), symbol.to_owned(), structure["unitStartOffset"].to_string());
        if !seen.insert(key) { continue; }
        let mut entry = json!({"sourceRef":item["sourceRef"],"path":path,"mountId":item["locator"]["mountId"],
            "symbol":symbol,"kind":structure["kind"],"startLine":item["startLine"],"endLine":item["endLine"]});
        if let Some(signature) = structure["signature"].as_str().filter(|signature| !signature.is_empty()) {
            entry["signature"] = json!(signature.chars().take(321).collect::<String>());
        }
        entries.push(entry);
        if crate::text_metrics::estimate_tokens(&json!(&entries).to_string()) > maximum_tokens {
            entries.pop();
            omitted += 1;
        }
    }
    json!({"entries":entries,"omitted":omitted,"coverage":"selected-current-evidence", "resolvedCallGraph":false})
}

/// Assign AST spans to the deepest declaration without claiming resolved references.
/// AST 区间归最内层声明，语法归属不代表引用已解析。
pub fn select(args: &Value) -> Result<Value, String> {
    let text = args["text"]
        .as_str()
        .ok_or("INVALID_CODE_SOURCE")?
        .encode_utf16()
        .collect::<Vec<_>>();
    let declarations = args["declarations"]
        .as_array()
        .filter(|items| items.len() <= 10000)
        .ok_or("INVALID_RETRIEVAL_STRUCTURE")?;
    let mut events = Vec::with_capacity(declarations.len() * 2);
    for (owner, declaration) in declarations.iter().enumerate() {
        let unit = &declaration["unit"];
        let start = unit["startOffset"]
            .as_u64()
            .filter(|offset| *offset <= text.len() as u64)
            .ok_or("INVALID_RETRIEVAL_STRUCTURE")? as usize;
        let end = unit["endOffset"]
            .as_u64()
            .filter(|offset| *offset <= text.len() as u64)
            .ok_or("INVALID_RETRIEVAL_STRUCTURE")? as usize;
        if end <= start || chunking::split_pair(&text, start) || chunking::split_pair(&text, end) {
            return Err("INVALID_RETRIEVAL_STRUCTURE".into());
        }
        events.push((start, true, owner));
        events.push((end, false, owner));
    }
    events.sort_by_key(|(offset, active, _)| (*offset, *active));
    let mut active = vec![false; declarations.len()];
    let mut heap: BinaryHeap<(u64, usize)> = BinaryHeap::new();
    let mut units = Vec::new();
    let mut previous_offset = events.first().map_or(0, |event| event.0);
    let mut fragment: Option<(usize, usize, usize)> = None;
    let mut scanned_offset = 0;
    let mut line = 1;
    let deadline = Instant::now() + Duration::from_millis(250);
    let limited = std::cell::Cell::new(false);
    let mut finish = |fragment: &mut Option<(usize, usize, usize)>| -> bool {
        let Some((owner, start, end)) = fragment.take() else {
            return true;
        };
        if text[start..end]
            .iter()
            .all(|character| chunking::whitespace(*character))
        {
            return true;
        }
        if units.len() >= 5000 {
            limited.set(true);
            return false;
        }
        while scanned_offset < start {
            if text[scanned_offset] == 10 {
                line += 1;
            }
            scanned_offset += 1;
        }
        let start_line = line;
        while scanned_offset < end {
            if text[scanned_offset] == 10 {
                line += 1;
            }
            scanned_offset += 1;
        }
        let mut unit = declarations[owner]["unit"].clone();
        unit["unitStartOffset"] = unit["startOffset"].clone();
        unit["unitEndOffset"] = unit["endOffset"].clone();
        unit["startOffset"] = json!(start);
        unit["endOffset"] = json!(end);
        unit["startLine"] = json!(start_line);
        unit["endLine"] = json!(line);
        units.push(unit);
        true
    };
    let mut index = 0;
    while index < events.len() {
        if Instant::now() > deadline {
            limited.set(true);
            break;
        }
        while heap.peek().is_some_and(|(_, owner)| !active[*owner]) {
            heap.pop();
        }
        let owner = heap.peek().map(|(_, owner)| *owner);
        let offset = events[index].0;
        if offset > previous_offset {
            if let Some(owner) = owner {
                if fragment.is_some_and(|(previous_owner, _, end)| {
                    previous_owner == owner && end == previous_offset
                }) {
                    if let Some((_, _, end)) = &mut fragment {
                        *end = offset;
                    }
                } else {
                    if !finish(&mut fragment) {
                        return Ok(json!({"units":units,"limited":true}));
                    }
                    fragment = Some((owner, previous_offset, offset));
                }
            } else if !finish(&mut fragment) {
                return Ok(json!({"units":units,"limited":true}));
            }
        }
        while index < events.len() && events[index].0 == offset {
            let (_, is_active, owner) = events[index];
            index += 1;
            active[owner] = is_active;
            if is_active {
                heap.push((declarations[owner]["depth"].as_u64().unwrap_or(0), owner));
            }
        }
        previous_offset = offset;
    }
    finish(&mut fragment);
    Ok(json!({"units":units,"limited":limited.get()}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_navigation_keeps_mounts_distinct_and_reuses_split_declarations() {
        let first = json!({"sourceRef":"e1","locator":{"relativePath":"src/main.rs","mountId":"frontend"},
            "startLine":2,"endLine":8,"structure":{"domain":"code","kind":"function","qualifiedName":"App.start",
                "unitStartOffset":12,"signature":"fn start(value: usize) -> bool"}});
        let mut repeated = first.clone();
        repeated["sourceRef"] = json!("e2");
        let mut other = first.clone();
        other["sourceRef"] = json!("e3");
        other["locator"]["mountId"] = json!("backend");
        let view = navigation(&[first.clone(), repeated, other], 1024);
        assert_eq!(view["entries"].as_array().unwrap().len(), 2);
        assert_eq!(view["entries"][0]["sourceRef"], "e1");
        assert_eq!(view["entries"][1]["mountId"], "backend");
        assert_eq!(view["resolvedCallGraph"], false);
        let tiny = navigation(&[first], 0);
        assert!(tiny["entries"].as_array().unwrap().is_empty());
        assert_eq!(tiny["omitted"], 1);
        assert!(navigation(&[json!({"structure":{"domain":"knowledge"}})], 1024)["entries"].as_array().unwrap().is_empty());
    }
}

use std::collections::HashSet;
use std::sync::OnceLock;

use regex::Regex;
use serde_json::{Value, json};

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}

/// Derived syntax ownership is precise; textual names keep unresolved certainty and bounded coverage.
/// 派生语法归属保持确定性；文本名称仍标为未解析关系，数量边界不冒充穷举结果。
pub fn derive(args: &Value) -> Result<Value, String> {
    static IDENTIFIER: OnceLock<Regex> = OnceLock::new();
    let chunks = args["chunks"]
        .as_array()
        .ok_or("INVALID_RETRIEVAL_STRUCTURE")?;
    let mut edges = Vec::new();
    let mut seen = HashSet::new();
    let mut definitions = HashSet::new();
    let mut add = |edge: Value| {
        if edges.len() >= 10000 {
            return;
        }
        let key = serde_json::to_string(&edge).expect("derived relation JSON");
        if seen.insert(key) {
            edges.push(edge);
        }
    };
    for chunk in chunks {
        let structure = &chunk["structure"];
        if structure["parseStatus"] == "unavailable" {
            continue;
        }
        let owner = structure
            .get("qualifiedName")
            .filter(|value| !value.is_null())
            .or_else(|| structure.get("symbolName"))
            .and_then(Value::as_str);
        let common = json!({"chunkId":chunk["chunkId"],"chunkHash":chunk["chunkHash"],"startOffset":chunk["startOffset"],"endOffset":chunk["endOffset"],"offsetUnit":"utf16-code-units"});
        let definition = serde_json::to_string(&json!([
            owner,
            structure["unitStartOffset"],
            structure["unitEndOffset"]
        ]))
        .expect("definition tuple");
        if let Some(owner) = owner.filter(|owner| !owner.is_empty()) {
            if definitions.insert(definition) {
                let mut edge = common.clone();
                edge["kind"] = json!("definition");
                edge["fromSymbol"] = json!(owner);
                edge["toSymbol"] = structure
                    .get("symbolName")
                    .filter(|value| !value.is_null())
                    .cloned()
                    .unwrap_or(json!(owner));
                edge["certainty"] = json!("syntax");
                add(edge);
                if !text(structure, "parentSymbol").is_empty() {
                    let mut edge = common.clone();
                    edge["kind"] = json!("contains");
                    edge["fromSymbol"] = structure["parentSymbol"].clone();
                    edge["toSymbol"] = json!(owner);
                    edge["certainty"] = json!("syntax");
                    add(edge);
                }
            }
        }
        if let Some(path) = structure["sectionPath"].as_array() {
            let section = path
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" / ");
            if !section.is_empty() {
                let mut edge = common.clone();
                edge["kind"] = json!("section");
                edge["fromSymbol"] = args["title"].clone();
                edge["toSymbol"] = json!(section);
                edge["certainty"] = json!("syntax");
                add(edge);
            }
        }
        if structure["domain"] != "code" {
            continue;
        }
        let raw = text(chunk, "text");
        let mut previous_byte = 0;
        let mut previous_offset = 0;
        for found in IDENTIFIER
            .get_or_init(|| {
                Regex::new(
                    r"(?-u:\b)[A-Za-z_$][A-Za-z0-9_$]*(?:\.[A-Za-z_$][A-Za-z0-9_$]*)*(?-u:\b)",
                )
                .expect("static lexical references")
            })
            .find_iter(raw)
        {
            let name = found.as_str();
            if name.len() < 3 || name == text(structure, "symbolName") {
                continue;
            }
            previous_offset += raw[previous_byte..found.start()].encode_utf16().count();
            previous_byte = found.start();
            let start = chunk["startOffset"]
                .as_u64()
                .ok_or("INVALID_RETRIEVAL_STRUCTURE")?
                + previous_offset as u64;
            let mut edge = common.clone();
            edge["kind"] = json!("lexical-reference");
            edge["fromSymbol"] = json!(owner.unwrap_or(text(args, "title")));
            edge["toSymbol"] = json!(name);
            edge["startOffset"] = json!(start);
            edge["endOffset"] = json!(start + name.len() as u64);
            edge["certainty"] = json!("lexical-unresolved");
            add(edge);
        }
    }
    Ok(json!({"truncated":edges.len()>=10000,"edges":edges}))
}

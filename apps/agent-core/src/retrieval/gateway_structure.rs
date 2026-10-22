//! Native syntax adapters feed the existing bounded structural selection and relationship rules.
//! 原生语法适配器接入既有有界结构选择与关系规则，不把词法候选升级为已解析调用。

use crate::storage::{ContractError, gateway};
use serde_json::{Value, json};

pub(crate) const CHUNKER_VERSION: &str = "structure-unit-groups-v2";
pub fn parser_version(path: &str) -> &'static str {
    if grammar(
        std::path::Path::new(path)
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or(""),
    )
    .is_some()
    {
        "rust-tree-sitter-v1"
    } else {
        "rust-document-blocks-v1"
    }
}
use std::path::Path;

fn grammar(extension: &str) -> Option<(tree_sitter::Language, &'static str)> {
    Some(match extension {
        "cs" => (tree_sitter_c_sharp::LANGUAGE.into(), "csharp"),
        "js" | "mjs" | "cjs" | "jsx" => (tree_sitter_javascript::LANGUAGE.into(), "javascript"),
        "ts" | "mts" | "cts" => (
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            "typescript",
        ),
        "tsx" => (tree_sitter_typescript::LANGUAGE_TSX.into(), "tsx"),
        "rs" => (tree_sitter_rust::LANGUAGE.into(), "rust"),
        "py" | "pyi" => (tree_sitter_python::LANGUAGE.into(), "python"),
        "java" => (tree_sitter_java::LANGUAGE.into(), "java"),
        "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hxx" => {
            (tree_sitter_cpp::LANGUAGE.into(), "cpp")
        }
        "go" => (tree_sitter_go::LANGUAGE.into(), "go"),
        _ => return None,
    })
}
fn declaration(kind: &str) -> Option<&'static str> {
    match kind {
        "class_declaration" | "class_definition" | "class_specifier" | "struct_item"
        | "struct_specifier" | "struct_declaration" => Some("class"),
        "interface_declaration" | "trait_item" => Some("interface"),
        "enum_declaration" | "enum_item" | "enum_specifier" => Some("enum"),
        "namespace_declaration" | "module" | "mod_item" => Some("namespace"),
        "method_declaration"
        | "method_definition"
        | "function_definition"
        | "function_declaration"
        | "function_item"
        | "constructor_declaration" => Some("function"),
        "impl_item" => Some("class"),
        "property_declaration" | "field_declaration" => Some("property"),
        _ => None,
    }
}
fn name<'a>(node: tree_sitter::Node<'a>, text: &'a str) -> Option<&'a str> {
    let target = node
        .child_by_field_name("name")
        .or_else(|| node.child_by_field_name("type"))
        .or_else(|| node.child_by_field_name("declarator"))?;
    let mut node = target;
    for _ in 0..8 {
        if [
            "identifier",
            "field_identifier",
            "type_identifier",
            "namespace_identifier",
            "qualified_identifier",
        ]
        .contains(&node.kind())
        {
            break;
        }
        let next = node
            .child_by_field_name("declarator")
            .or_else(|| node.child_by_field_name("name"));
        match next {
            Some(next) => node = next,
            None => break,
        }
    }
    node.utf8_text(text.as_bytes())
        .ok()
        .filter(|name| !name.is_empty() && name.len() <= 1024)
}

/// Read the actual declaration header; this optional preview does not change chunk or embedding identities.
/// 提取真实声明头部；可选预览不改变分块和向量身份，旧缓存缺少该字段仍然有效。
fn signature(node: tree_sitter::Node<'_>, text: &str) -> String {
    let end = node.child_by_field_name("body").map_or(node.end_byte(), |body| body.start_byte());
    let header = text.get(node.start_byte()..end).unwrap_or("").trim();
    let mut characters = header.chars();
    let preview = characters.by_ref().take(320).collect::<String>();
    if characters.next().is_some() { format!("{preview}…") } else { preview }
}

#[cfg(test)]
mod signature_tests {
    use super::*;

    #[test]
    fn code_signature_is_the_original_header_without_function_body() {
        let text = "pub fn 合计(\n    value: usize,\n) -> usize {\n value + 1\n}\n";
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_rust::LANGUAGE.into()).unwrap();
        let tree = parser.parse(text, None).unwrap();
        let node = tree.root_node().named_child(0).unwrap();
        let actual = signature(node, text);
        assert_eq!(actual, "pub fn 合计(\n    value: usize,\n) -> usize");
        assert!(!actual.contains("value + 1"));
        assert_eq!(parser_version("main.rs"), "rust-tree-sitter-v1");
    }
}
fn utf16_index(text: &str) -> Vec<usize> {
    let mut offsets = vec![0; text.len() + 1];
    let mut count = 0;
    for (start, character) in text.char_indices() {
        for offset in &mut offsets[start..start + character.len_utf8()] {
            *offset = count;
        }
        count += character.len_utf16();
        offsets[start + character.len_utf8()] = count;
    }
    offsets
}
fn line_entries(text: &str) -> Vec<Value> {
    let mut offset = 0;
    let mut lines = Vec::new();
    for (number, line) in text.split_inclusive('\n').enumerate() {
        let length = line.encode_utf16().count();
        lines.push(json!({"text":line.trim_end_matches(['\r','\n']),"startOffset":offset,"endOffset":offset+length,"startLine":number+1,"endLine":number+1}));
        offset += length;
    }
    lines
}
pub fn parse(text: &str, path: &str) -> Result<Value, ContractError> {
    let extension = Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let Some((language, language_name)) = grammar(&extension) else {
        let document=super::document::parse(&json!({"extension":format!(".{extension}"),"lines":line_entries(text),"complete":true}))
            .map_err(|_|gateway::error("文档结构解析失败。","INVALID_RETRIEVAL_STRUCTURE",422))?;
        return Ok(
            json!({"domain":"knowledge","language":"text","parserVersion":"rust-document-blocks-v1","parseStatus":document["parseStatus"],
            "units":document["units"],"diagnostics":document["diagnostics"],"offsetUnit":"utf16-code-units"}),
        );
    };
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&language)
        .map_err(|_| gateway::error("代码语法库无法加载。", "CODE_PARSER_UNAVAILABLE", 503))?;
    let tree = parser
        .parse(text, None)
        .ok_or_else(|| gateway::error("代码解析已取消。", "ABORT_ERR", 499))?;
    let offsets = utf16_index(text);
    let mut declarations = Vec::new();
    let mut pending = vec![(tree.root_node(), 0usize, None::<String>)];
    let mut limited = false;
    while let Some((node, depth, parent)) = pending.pop() {
        crate::storage::sqlite_store::cancelled()?;
        if declarations.len() >= 10000 {
            limited = true;
            break;
        }
        let mut next_parent = parent.clone();
        if let Some(kind) =
            declaration(node.kind()).filter(|_| !node.is_error() && !node.is_missing())
        {
            if let Some(symbol) = name(node, text) {
                let qualified = parent
                    .as_ref()
                    .map(|parent| format!("{parent}.{symbol}"))
                    .unwrap_or(symbol.to_string());
                declarations.push(json!({"depth":depth,"unit":{"kind":kind,"symbolName":symbol,"qualifiedName":qualified,"parentSymbol":parent,"signature":signature(node, text),
                    "startOffset":offsets[node.start_byte()],"endOffset":offsets[node.end_byte()],"startLine":node.start_position().row+1,"endLine":node.end_position().row+1}}));
                next_parent = Some(qualified);
            }
        }
        let mut cursor = node.walk();
        let children: Vec<_> = node.named_children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            pending.push((child, depth + 1, next_parent.clone()));
        }
    }
    let selected = super::code_units::select(&json!({"text":text,"declarations":declarations}))
        .map_err(|_| gateway::error("代码结构选择失败。", "INVALID_RETRIEVAL_STRUCTURE", 422))?;
    let partial = limited || selected["limited"] == true || tree.root_node().has_error();
    Ok(
        json!({"domain":"code","language":language_name,"parserVersion":"rust-tree-sitter-v1","parseStatus":if partial{"partial"}else{"parsed"},
        "units":selected["units"],"diagnostics":if partial{json!(["CODE_STRUCTURE_PARTIAL"])}else{json!([])},"offsetUnit":"utf16-code-units"}),
    )
}
pub fn chunks(text: &str, structure: &Value, maximum: usize) -> Result<Vec<Value>, ContractError> {
    let total = text.encode_utf16().count();
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut spans = structure["units"].as_array().cloned().unwrap_or_default();
    spans.sort_by_key(|span| span["startOffset"].as_u64().unwrap_or(0));
    // Retain uncovered text as explicit context instead of silently dropping imports, comments or parse gaps.
    // 未覆盖文本作为明确上下文保留，不能静默遗漏导入、注释或解析缺口。
    let mut ranges = Vec::new();
    let mut position = 0;
    for span in spans {
        let start = span["startOffset"].as_u64().unwrap_or(0) as usize;
        let end = span["endOffset"].as_u64().unwrap_or(0) as usize;
        if start < position || end <= start || end > total {
            continue;
        }
        if start > position {
            ranges.push((position, start, None));
        }
        ranges.push((start, end, Some(span)));
        position = end;
    }
    if position < total {
        ranges.push((position, total, None));
    }
    let mut output = Vec::new();
    for (start, end, span) in ranges {
        let body = String::from_utf16_lossy(&units[start..end]);
        let chunks = super::chunking::apply(&json!({"text":body,"maxChars":maximum}))
            .map_err(|_| gateway::error("资料分块失败。", "INVALID_RETRIEVAL_STRUCTURE", 422))?;
        let first_line = units[..start].iter().filter(|unit| **unit == 10).count();
        for chunk in chunks.as_array().unwrap() {
            let mut chunk = chunk.clone();
            chunk["startOffset"] = json!(start + chunk["startOffset"].as_u64().unwrap() as usize);
            chunk["endOffset"] = json!(start + chunk["endOffset"].as_u64().unwrap() as usize);
            chunk["startLine"] = json!(first_line + chunk["startLine"].as_u64().unwrap() as usize);
            chunk["endLine"] = json!(first_line + chunk["endLine"].as_u64().unwrap() as usize);
            let mut metadata = span.clone().unwrap_or(json!({"kind":"context"}));
            if let Some(span) = &span {
                metadata["unitStartOffset"] = span["unitStartOffset"].clone();
                metadata["unitEndOffset"] = span["unitEndOffset"].clone();
                if metadata["unitStartOffset"].is_null() {
                    metadata["unitStartOffset"] = span["startOffset"].clone();
                    metadata["unitEndOffset"] = span["endOffset"].clone();
                }
            }
            metadata["domain"] = structure["domain"].clone();
            metadata["language"] = structure["language"].clone();
            metadata["parseStatus"] = structure["parseStatus"].clone();
            metadata["offsetUnit"] = json!("utf16-code-units");
            chunk["structure"] = metadata;
            output.push(chunk);
        }
    }
    Ok(merge_adjacent_chunks(&units, output, maximum))
}

fn merge_boundary_matches(previous: &Value, next: &Value) -> bool {
    let left = &previous["structure"];
    let right = &next["structure"];
    // Publication calls this per source; retain explicit identities too when future adapters supply them.
    // 发布始终逐来源调用；适配器显式提供身份时仍核对，不能把跨范围内容拼成同一证据。
    for field in ["sourceId", "scopeKey", "sourceRevision", "bindingRevision", "locator"] {
        if previous[field] != next[field] { return false; }
    }
    for field in ["domain", "language", "parseStatus", "kind", "sectionPath", "sectionTitle",
        "page", "pageIndex", "pageNumber", "slide", "slideNumber", "sheetName", "tableId"] {
        if left[field] != right[field] { return false; }
    }
    if left["parseStatus"] != "parsed" { return false; }
    match left["domain"].as_str() {
        Some("code") => {
            left["kind"] != "context" && left["unitStartOffset"].as_u64().is_some()
                && left["unitEndOffset"].as_u64().is_some()
                && ["unitStartOffset", "unitEndOffset", "symbolName", "qualifiedName", "parentSymbol"]
                    .iter().all(|field| left[*field] == right[*field])
        }
        Some("knowledge") => {
            // Paragraphs may share a verified section; tables, headings and fenced code stay separate.
            // 段落只有在已识别的同一章节中才合并；表格、标题和围栏代码保持独立。
            left["kind"] == "paragraph" && (left["sectionPath"].as_array().is_some_and(|path| !path.is_empty())
                || left["sectionTitle"].as_str().is_some_and(|title| !title.is_empty()))
        }
        _ => false,
    }
}

/// Coalesce compatible small pieces without changing source coordinates or inventing a language-sized policy.
/// 合并结构兼容的小片段，保留来源坐标；沿用调用方粗边界，最终嵌入仍走真实分词器的 token 拟合。
fn merge_adjacent_chunks(text: &[u16], chunks: Vec<Value>, maximum: usize) -> Vec<Value> {
    let mut merged: Vec<Value> = Vec::with_capacity(chunks.len());
    for mut next in chunks {
        next["chunkerVersion"] = json!(CHUNKER_VERSION);
        let Some(previous) = merged.last_mut() else { merged.push(next); continue; };
        let range = previous["startOffset"].as_u64().zip(previous["endOffset"].as_u64())
            .zip(next["startOffset"].as_u64().zip(next["endOffset"].as_u64()));
        let can_merge = range.is_some_and(|((start, previous_end), (next_start, end))| {
            start <= previous_end && previous_end <= next_start && next_start < end && end <= text.len() as u64
                && end.saturating_sub(start) <= maximum as u64
                && !super::chunking::split_pair(text, start as usize)
                && !super::chunking::split_pair(text, end as usize)
                && text[previous_end as usize..next_start as usize].iter().all(|unit| super::chunking::whitespace(*unit))
                && !text[start as usize..end as usize].contains(&0x000c)
                && merge_boundary_matches(previous, &next)
        });
        if !can_merge { merged.push(next); continue; }
        previous["endOffset"] = next["endOffset"].clone();
        previous["endLine"] = next["endLine"].clone();
        let count = previous["structure"]["mergedUnitCount"].as_u64().unwrap_or(1)
            .saturating_add(next["structure"]["mergedUnitCount"].as_u64().unwrap_or(1));
        previous["structure"]["mergedUnitCount"] = json!(count);
        if previous["structure"]["domain"] == "knowledge" {
            previous["structure"]["unitStartOffset"] = previous["startOffset"].clone();
            previous["structure"]["unitEndOffset"] = next["endOffset"].clone();
            previous["structure"]["endOffset"] = next["structure"]["endOffset"].clone();
            previous["structure"]["endLine"] = next["structure"]["endLine"].clone();
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structural_merge_preserves_utf16_text_and_section_table_boundaries() {
        let text = "# First\n\nAlpha 😀.\n\nBeta 中文.\n\n| a | b |\n| - | - |\n| c | d |\n\nGamma.\n\n# Second\n\nDelta.\n\nEpsilon.\n";
        let parsed = parse(text, "fixture.md").unwrap();
        let output = chunks(text, &parsed, 384).unwrap();
        let units: Vec<u16> = text.encode_utf16().collect();
        let groups = output.iter().filter(|chunk| chunk["structure"]["mergedUnitCount"].as_u64().unwrap_or(1) > 1).collect::<Vec<_>>();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0]["structure"]["sectionTitle"], "First");
        assert_eq!(groups[1]["structure"]["sectionTitle"], "Second");
        let body = |chunk: &Value| String::from_utf16(&units[chunk["startOffset"].as_u64().unwrap() as usize..chunk["endOffset"].as_u64().unwrap() as usize]).unwrap();
        assert_eq!(body(groups[0]), "Alpha 😀.\n\nBeta 中文.\n");
        assert!(!body(groups[0]).contains('|'));
        assert_eq!(output.iter().filter(|chunk| chunk["structure"]["kind"] == "table").count(), 1);
        for chunk in &output {
            assert_eq!(chunk["chunkerVersion"], CHUNKER_VERSION);
            assert!(!super::super::chunking::split_pair(&units, chunk["startOffset"].as_u64().unwrap() as usize));
            assert!(chunk["endOffset"].as_u64().unwrap() - chunk["startOffset"].as_u64().unwrap() <= 384);
        }
    }

    #[test]
    fn structural_merge_requires_same_function_page_scope_and_budget() {
        let text: Vec<u16> = "left\n\nright".encode_utf16().collect();
        let left = json!({"startOffset":0,"endOffset":5,"startLine":1,"endLine":2,"scopeKey":"project:a",
            "structure":{"domain":"code","language":"rust","parseStatus":"parsed","kind":"function","unitStartOffset":0,"unitEndOffset":11,"symbolName":"same"}});
        let mut right = left.clone(); right["startOffset"]=json!(6);right["endOffset"]=json!(11);right["startLine"]=json!(3);right["endLine"]=json!(3);
        assert_eq!(merge_adjacent_chunks(&text, vec![left.clone(),right.clone()], 64).len(), 1);
        assert_eq!(merge_adjacent_chunks(&text, vec![left.clone(),right.clone()], 8).len(), 2);
        for (field, value) in [("symbolName",json!("other")),("unitStartOffset",json!(6)),("pageNumber",json!(2)),("parseStatus",json!("partial"))] {
            let mut changed=right.clone();changed["structure"][field]=value;
            assert_eq!(merge_adjacent_chunks(&text, vec![left.clone(),changed], 64).len(), 2);
        }
        let mut other_scope=right.clone();other_scope["scopeKey"]=json!("project:b");
        assert_eq!(merge_adjacent_chunks(&text, vec![left.clone(),other_scope], 64).len(), 2);
        let mut page_break=text;page_break[5]=12;
        assert_eq!(merge_adjacent_chunks(&page_break, vec![left,right], 64).len(), 2);
    }
}

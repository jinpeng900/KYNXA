use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value, json};

use super::intent::{js_trim, matches};
use super::service_error::{ServiceError, ServiceResult};
use super::service_http::public_url;
use crate::models::protocol::{array, slice, string};

const SOURCE_FIELDS: &[&str] = &[
    "url",
    "title",
    "name",
    "text",
    "description",
    "snippet",
    "content",
    "highlights",
    "summary",
    "excerpt",
    "excerpts",
];

const TEMPORAL_FIELDS: &[&str] = &[
    "publishedAt",
    "publishedDate",
    "published_date",
    "datePublished",
    "publicationDate",
    "publishedTime",
    "published_time",
    "updatedAt",
    "updatedDate",
    "dateModified",
    "lastModified",
    "last_modified",
    "modifiedAt",
    "date",
    "age",
    "page_age",
    "firstCrawledDate",
    "lastCrawledDate",
    "indexedDate",
];

fn temporal_value(value: &Value) -> Option<Value> {
    match value {
        Value::String(text) if !text.trim().is_empty() => Some(json!(slice(text.trim(), 0, 256))),
        Value::Number(_) => Some(value.clone()),
        _ => None,
    }
}

/// Preserve provider date labels as evidence; retrieval time never becomes publication time.
/// 保留来源原有时间字段及含义；不能把本次检索时间当成文章发布日期。
pub(crate) fn preserve_temporal_metadata(source: &Value, target: &mut Value) {
    for field in TEMPORAL_FIELDS {
        if target[*field].is_null() {
            if let Some(value) = temporal_value(&source[*field]) {
                target[*field] = value;
            }
        }
    }
}

struct Sources<'a> {
    nodes: HashMap<u64, &'a Value>,
    visited: HashSet<u64>,
    sources: Vec<Value>,
    maximum: usize,
}

impl<'a> Sources<'a> {
    fn resolve(&self, value: &'a Value) -> &'a Value {
        value["reference"]
            .as_u64()
            .and_then(|id| self.nodes.get(&id).copied())
            .unwrap_or(value)
    }
    fn field(&self, value: &'a Value, name: &str) -> &'a Value {
        let value = self.resolve(value);
        array(&value["fields"])
            .iter()
            .find(|field| field[0] == name)
            .map(|field| &field[1])
            .unwrap_or(&Value::Null)
    }
    fn string_value(&self, value: &'a Value) -> String {
        self.string_value_checked(value, 0, &mut HashSet::new())
    }
    fn string_value_checked(
        &self,
        value: &'a Value,
        depth: usize,
        visiting: &mut HashSet<u64>,
    ) -> String {
        if depth > 64 {
            return String::new();
        }
        match string(&value["kind"]) {
            "string" => string(&value["text"]).into(),
            "number" | "boolean" => value["value"].to_string(),
            "null" => "null".into(),
            "undefined" => "undefined".into(),
            "array" => {
                let id = value["id"].as_u64();
                if id.is_some_and(|id| !visiting.insert(id)) {
                    return String::new();
                }
                let text = array(&value["items"])
                    .iter()
                    .map(|item| {
                        if ["null", "undefined"].contains(&string(&item["kind"])) {
                            String::new()
                        } else {
                            self.string_value_checked(self.resolve(item), depth + 1, visiting)
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                if let Some(id) = id {
                    visiting.remove(&id);
                }
                text
            }
            _ => "[object Object]".into(),
        }
    }
    fn source_text(&self, value: &'a Value, depth: usize) -> String {
        if depth > 64 {
            return String::new();
        }
        let value = self.resolve(value);
        if value["kind"] == "string" {
            return string(&value["text"]).into();
        }
        if value["kind"] == "array" {
            return array(&value["items"])
                .iter()
                .map(|item| self.source_text(item, depth + 1))
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
        }
        let text = self.field(value, "text");
        if text["kind"] == "string" {
            return string(&text["text"]).into();
        }
        String::new()
    }
    fn add(&mut self, url: &Value, title: &str, excerpt: &str, metadata: &Value) {
        let Ok(url) = public_url(url) else {
            return;
        };
        let url = url.to_string();
        let title = slice(js_trim(title), 0, 512);
        let excerpt = slice(js_trim(excerpt), 0, 2500);
        if let Some(previous) = self.sources.iter_mut().find(|source| source["url"] == url) {
            if string(&previous["title"]).is_empty() && !title.is_empty() {
                previous["title"] = json!(title);
            }
            if string(&previous["excerpt"]).is_empty() && !excerpt.is_empty() {
                previous["excerpt"] = json!(excerpt);
            }
            preserve_temporal_metadata(metadata, previous);
        } else if self.sources.len() < self.maximum {
            let mut source = json!({"url":url,"title":title,"excerpt":excerpt});
            preserve_temporal_metadata(metadata, &mut source);
            self.sources.push(source);
        }
    }
    fn text(&mut self, text: &str) {
        let title = regex::Regex::new(r"(?i)^\s*Title:\s*(.*)$").unwrap();
        let url = regex::Regex::new(r"(?i)^\s*URL:\s*(.*)$").unwrap();
        let body =
            regex::Regex::new(r"(?i)^\s*(?:Content|Text|Highlights|Summary|Description):\s*(.*)$")
                .unwrap();
        let date = regex::Regex::new(
            r"(?i)^\s*(Published(?: Date| Time)?|Date|(?:Last )?Updated(?: Date)?|Modified(?: Date)?|Last Modified):\s*(.*)$",
        )
        .unwrap();
        let mut labelled = false;
        let mut current: Option<(String, String, Vec<String>, bool, Map<String, Value>)> = None;
        let mut sources = Vec::new();
        let finish = |current: &mut Option<(String, String, Vec<String>, bool, Map<String, Value>)>,
                      sources: &mut Vec<(String, String, String, Value)>| {
            if let Some((title, url, lines, _, metadata)) = current.take() {
                if !url.is_empty() {
                    sources.push((
                        url,
                        title,
                        js_trim(&lines.join("\n")).into(),
                        Value::Object(metadata),
                    ));
                }
            }
        };
        for line in text
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line))
        {
            if let Some(captures) = title.captures(line) {
                labelled = true;
                finish(&mut current, &mut sources);
                current = Some((
                    js_trim(&captures[1]).into(),
                    String::new(),
                    Vec::new(),
                    false,
                    Map::new(),
                ));
            } else if let Some(captures) = url.captures(line) {
                labelled = true;
                if current
                    .as_ref()
                    .is_some_and(|(_, url, _, _, _)| !url.is_empty())
                {
                    finish(&mut current, &mut sources);
                }
                let current = current.get_or_insert_with(|| {
                    (String::new(), String::new(), Vec::new(), false, Map::new())
                });
                current.1 = js_trim(&captures[1]).into();
            } else if matches(r"^\s*(?:---+|\*\*\*+)\s*$", line) {
                finish(&mut current, &mut sources);
            } else if let Some((_, url, lines, has_body, metadata)) = &mut current {
                if url.is_empty() {
                    continue;
                }
                if !*has_body {
                    if let Some(captures) = date.captures(line) {
                        let label = captures[1].to_ascii_lowercase();
                        let field = if label.starts_with("published") {
                            "publishedDate"
                        } else if label.contains("updated") {
                            "updatedAt"
                        } else if label.contains("modified") {
                            "lastModified"
                        } else {
                            "date"
                        };
                        if let Some(value) = temporal_value(&json!(&captures[2])) {
                            metadata.entry(field).or_insert(value);
                        }
                        continue;
                    }
                }
                if let Some(captures) = body.captures(line) {
                    *has_body = true;
                    if !captures[1].is_empty() {
                        lines.push(captures[1].into());
                    }
                } else if !*has_body && matches(r"(?i)^\s*Author:", line) {
                    continue;
                } else {
                    if !js_trim(line).is_empty() {
                        *has_body = true;
                    }
                    lines.push(line.into());
                }
            }
        }
        finish(&mut current, &mut sources);
        if labelled {
            for (url, title, excerpt, metadata) in sources {
                self.add(&json!(url), &title, &excerpt, &metadata);
            }
            return;
        }
        let markdown = regex::Regex::new(r"\[([^\]\r\n]*)\]\((https?://[^\s<>]+)\)").unwrap();
        let link = regex::Regex::new(r#"https?://[^\s<>"\]\)]+"#).unwrap();
        for line in text.split('\n') {
            for capture in markdown.captures_iter(line) {
                self.add(&json!(&capture[2]), &capture[1], "", &Value::Null);
            }
            for capture in link.find_iter(line) {
                self.add(
                    &json!(capture.as_str().trim_end_matches(['.', ',', ';'])),
                    "",
                    "",
                    &Value::Null,
                );
            }
        }
    }
    fn visit(&mut self, value: &'a Value, depth: usize) {
        if depth > 12 {
            return;
        }
        let value = self.resolve(value);
        if value["kind"] == "string" {
            if let Some(parsed) = value.get("parsed") {
                self.visit(parsed, depth + 1);
            } else {
                self.text(string(&value["text"]));
            }
            return;
        }
        if !["object", "array"].contains(&string(&value["kind"])) {
            return;
        }
        if let Some(id) = value["id"].as_u64() {
            if !self.visited.insert(id) {
                return;
            }
        }
        if value["kind"] == "array" {
            for item in array(&value["items"]) {
                self.visit(item, depth + 1);
            }
            return;
        }
        let url = self.field(value, "url");
        let is_source = url["kind"] == "string";
        if is_source {
            let title = self.field(value, "title");
            let title =
                if title.is_null() || ["null", "undefined"].contains(&string(&title["kind"])) {
                    self.field(value, "name")
                } else {
                    title
                };
            let title =
                if title.is_null() || ["null", "undefined"].contains(&string(&title["kind"])) {
                    String::new()
                } else {
                    self.string_value(self.resolve(title))
                };
            let mut excerpt = String::new();
            for field in [
                "text",
                "description",
                "snippet",
                "content",
                "highlights",
                "summary",
                "excerpt",
                "excerpts",
            ] {
                let value = self.source_text(self.field(value, field), 0);
                if !js_trim(&value).is_empty() {
                    excerpt = value;
                    break;
                }
            }
            let metadata = TEMPORAL_FIELDS
                .iter()
                .filter_map(|field| {
                    let value = self.resolve(self.field(value, field));
                    let value = match string(&value["kind"]) {
                        "string" => &value["text"],
                        "number" => &value["value"],
                        _ => &Value::Null,
                    };
                    temporal_value(value).map(|value| ((*field).to_owned(), value))
                })
                .collect::<Map<_, _>>();
            self.add(&url["text"], &title, &excerpt, &Value::Object(metadata));
        }
        for field in array(&value["fields"]) {
            let name = string(&field[0]);
            if ["_meta", "data", "blob"].contains(&name)
                || is_source && SOURCE_FIELDS.contains(&name)
            {
                continue;
            }
            self.visit(&field[1], depth + 1);
        }
    }
    fn visit_json(&mut self, value: &Value, depth: usize, remaining: &mut usize) {
        if depth > 12 || *remaining == 0 || self.sources.len() >= self.maximum {
            return;
        }
        *remaining -= 1;
        if let Some(text) = value.as_str() {
            if let Ok(parsed) = serde_json::from_str::<Value>(text) {
                self.visit_json(&parsed, depth + 1, remaining);
            } else {
                self.text(text);
            }
            return;
        }
        if let Some(items) = value.as_array() {
            for item in items {
                if *remaining == 0 || self.sources.len() >= self.maximum {
                    break;
                }
                self.visit_json(item, depth + 1, remaining);
            }
            return;
        }
        let Some(fields) = value.as_object() else {
            return;
        };
        let is_source = value["url"].is_string();
        if is_source {
            let title = value["title"]
                .as_str()
                .or_else(|| value["name"].as_str())
                .unwrap_or("");
            let excerpt = [
                "text",
                "description",
                "snippet",
                "content",
                "highlights",
                "summary",
                "excerpt",
                "excerpts",
            ]
            .into_iter()
            .find_map(|field| {
                if let Some(text) = value[field].as_str().filter(|text| !text.trim().is_empty()) {
                    return Some(slice(text, 0, 2500));
                }
                let items = value[field].as_array()?;
                let text = items
                    .iter()
                    .filter_map(Value::as_str)
                    .take(20)
                    .map(|text| slice(text, 0, 2500))
                    .collect::<Vec<_>>()
                    .join("\n");
                (!text.trim().is_empty()).then_some(text)
            })
            .unwrap_or_default();
            self.add(&value["url"], title, &excerpt, value);
        }
        for (field, item) in fields {
            if *remaining == 0 || self.sources.len() >= self.maximum {
                break;
            }
            if ["_meta", "data", "blob"].contains(&field.as_str())
                || is_source && SOURCE_FIELDS.contains(&field.as_str())
            {
                continue;
            }
            self.visit_json(item, depth + 1, remaining);
        }
    }
}

/// Native JSON results use the same URL, excerpt and deduplication rules without a second graph projection.
/// 原生 JSON 回执复用 URL、摘录与去重规则，不再建立另一份图投影。
pub(crate) fn normalize_result(value: &Value, maximum: usize) -> Vec<Value> {
    let mut sources = Sources {
        nodes: HashMap::new(),
        visited: HashSet::new(),
        sources: Vec::new(),
        maximum,
    };
    sources.visit_json(value, 0, &mut 2048);
    sources.sources
}

pub fn apply(operation: &str, arguments: &Value) -> ServiceResult<Value> {
    match operation {
        "sources.projection" => {
            Ok(json!({"skipKeys":["_meta","data","blob"],"projectionDepth":64}))
        }
        "sources.normalize" => {
            let maximum = arguments["maximum"]
                .as_f64()
                .filter(|value| value.is_finite())
                .map(|value| value.trunc().max(0.0) as usize)
                .unwrap_or(6);
            let graph = &arguments["graph"];
            let mut sources = Sources {
                nodes: array(&graph["nodes"])
                    .iter()
                    .filter_map(|node| node["id"].as_u64().map(|id| (id, node)))
                    .collect(),
                visited: HashSet::new(),
                sources: Vec::new(),
                maximum,
            };
            if maximum > 0 {
                sources.visit(&graph["root"], 0);
            }
            Ok(json!(sources.sources))
        }
        _ => Err(ServiceError::new(
            "AGENT_CORE_UNKNOWN_OPERATION",
            "Unknown web source operation.",
            400,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_sources_preserve_publication_and_modification_dates_separately() {
        let sources = normalize_result(&json!({"results":[{
            "url":"https://example.org/news", "title":"News", "excerpts":["Original evidence"],
            "publishedDate":"2024-01-02", "dateModified":"2026-10-17", "age":"yesterday",
            "fetchedAt":"2026-10-18T09:00:00Z", "unknownMetadata":"omit"
        }]}), 6);
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0]["publishedDate"], "2024-01-02");
        assert_eq!(sources[0]["dateModified"], "2026-10-17");
        assert_eq!(sources[0]["age"], "yesterday");
        assert_eq!(sources[0]["excerpt"], "Original evidence");
        assert!(sources[0].get("publishedAt").is_none());
        assert!(sources[0].get("fetchedAt").is_none());
        assert!(sources[0].get("unknownMetadata").is_none());
    }

    #[test]
    fn labelled_sources_retain_dates_without_confusing_generic_date_with_publication() {
        let sources = normalize_result(&json!({"content":[{"type":"text","text":
            "Title: First article\nURL: https://example.org/first\nPublished Date: 2024-01-02\nLast Updated: 2026-10-17\nText: Original first evidence\n---\nTitle: Second article\nURL: https://example.org/second\nDate: 2025-03-04\nText: Original second evidence"
        }]}), 6);
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0]["publishedDate"], "2024-01-02");
        assert_eq!(sources[0]["updatedAt"], "2026-10-17");
        assert_eq!(sources[0]["excerpt"], "Original first evidence");
        assert_eq!(sources[1]["date"], "2025-03-04");
        assert!(sources[1].get("publishedDate").is_none());
    }

    #[test]
    fn repeated_source_only_fills_missing_dates_and_missing_metadata_stays_unknown() {
        let sources = normalize_result(&json!({"results":[
            {"url":"https://example.org/news","publishedAt":"2024-01-02","text":"Original"},
            {"url":"https://example.org/news","publishedAt":"2026-10-18","updatedAt":"2026-10-17"},
            {"url":"https://example.org/undated","publishedAt":{},"age":false}
        ]}), 6);
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0]["publishedAt"], "2024-01-02");
        assert_eq!(sources[0]["updatedAt"], "2026-10-17");
        assert!(sources[1].get("publishedAt").is_none());
        assert!(sources[1].get("age").is_none());
    }

    #[test]
    fn graph_sources_preserve_provider_dates_without_relabelling_them() {
        let source = json!({"id":1,"kind":"object","fields":[
            ["url",{"kind":"string","text":"https://example.org/news"}],
            ["publishedAt",{"kind":"string","text":"2024-01-02"}],
            ["lastCrawledDate",{"kind":"string","text":"2026-10-17"}],
            ["excerpt",{"kind":"string","text":"Original evidence"}]
        ]});
        let normalized = apply(
            "sources.normalize",
            &json!({"maximum":6,"graph":{"root":source,"nodes":[]}}),
        ).unwrap();
        assert_eq!(normalized[0]["publishedAt"], "2024-01-02");
        assert_eq!(normalized[0]["lastCrawledDate"], "2026-10-17");
        assert_eq!(normalized[0]["excerpt"], "Original evidence");
    }
}

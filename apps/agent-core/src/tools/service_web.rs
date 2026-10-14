use std::collections::HashSet;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use url::Url;

use super::intent::{self, matches};
use super::local_files::sha256;
use super::service_error::{ServiceError, ServiceResult};
use crate::models::protocol::{array, slice, string};

pub fn provider_kind(descriptor: &Value) -> Option<&'static str> {
    let name = descriptor["toolName"]
        .as_str()
        .unwrap_or_else(|| string(&descriptor["name"]));
    if matches(r"(?:^|[._])web_search_exa$", name) {
        Some("exa")
    } else if matches(r"(?:^|[._])brave_web_search$", name) {
        Some("brave")
    } else if name.ends_with("web_search")
        && (string(&descriptor["serverId"]).contains("parallel")
            || descriptor["originalInputSchema"]["properties"]["search_queries"].is_object())
    {
        Some("parallel")
    } else {
        None
    }
}

/// Recognize configured capabilities without connecting or enabling another service.
/// 只识别已有配置能力，不为候选发现建立连接或启用其他服务。
pub fn search_server(server: &Value) -> bool {
    matches(
        r"(?i)(?:^|[-_.])(?:exa|brave(?:-search)?|parallel)(?:[-_.]|$)",
        string(&server["id"]),
    ) || server["url"]
        .as_str()
        .and_then(|value| Url::parse(value).ok())
        .is_some_and(|url| matches!(url.host_str(), Some("mcp.exa.ai" | "search.parallel.ai")))
        || array(&server["args"])
            .iter()
            .filter_map(Value::as_str)
            .any(|argument| {
                argument.contains("exa-mcp-server")
                    || argument.contains("brave-search-mcp")
                    || argument.contains("server-brave-search")
            })
        || array(&server["tools"])
            .iter()
            .any(|tool| provider_kind(tool).is_some())
}
pub fn public_fetch(descriptor: &Value) -> bool {
    descriptor["name"] == "web.fetch"
        || matches(
            r"^mcp\.(?:official-)?fetch(?:-[1-9]\d*)?\.fetch$",
            string(&descriptor["name"]),
        )
}
pub fn automatic_browser(context: &Value, descriptor: &Value) -> bool {
    let browser_intent = context
        .get("browserTaskIntent")
        .cloned()
        .unwrap_or_else(|| {
            intent::calculate(
                "tools.browserIntent",
                &json!({"message":context["message"]}),
            )
            .unwrap_or(Value::Null)
        });
    browser_intent["explicitBrowserTask"] != true
        && matches(
            r"^mcp\.(?:official-)?(?:chrome-devtools|playwright)(?:-[1-9]\d*)?\.(?:take_snapshot|evaluate_script|browser_snapshot|browser_evaluate)$",
            string(&descriptor["name"]),
        )
}
fn bounded(value: &Value, fallback: usize, minimum: usize, maximum: usize) -> ServiceResult<usize> {
    if value.is_null() {
        return Ok(fallback);
    }
    value
        .as_u64()
        .filter(|value| (minimum as u64..=maximum as u64).contains(value))
        .map(|value| value as usize)
        .ok_or_else(|| {
            ServiceError::new(
                "INVALID_TOOL_ARGUMENTS",
                &format!("整数参数须在 {minimum}–{maximum} 范围内。"),
                400,
            )
        })
}

struct Stage {
    started: Instant,
    paused: Duration,
    duration_ms: f64,
    queries: usize,
    pages: usize,
    query_count: usize,
    page_count: usize,
    seen: HashSet<String>,
}

#[derive(Default)]
pub struct WebState {
    stage: Option<Stage>,
}

impl WebState {
    fn elapsed(stage: &Stage) -> f64 {
        stage.started.elapsed().as_secs_f64() * 1000.0 - stage.paused.as_secs_f64() * 1000.0
    }
    fn check_stage(stage: &Stage, kind: &str) -> ServiceResult<()> {
        if Self::elapsed(stage) >= stage.duration_ms
            || if kind == "query" {
                stage.query_count >= stage.queries
            } else {
                stage.page_count >= stage.pages
            }
        {
            return Err(ServiceError::new(
                "WEB_STAGE_BUDGET_EXHAUSTED",
                "本次网页检索预算已用完，请根据已有证据回答或说明不足；其他代码任务可继续。",
                409,
            ));
        }
        Ok(())
    }
    fn snapshot(stage: &Stage) -> Value {
        json!({"elapsedMs":Self::elapsed(stage),"durationMs":stage.duration_ms,"queries":stage.queries,"pages":stage.pages,
            "queryCount":stage.query_count,"pageCount":stage.page_count})
    }
    pub fn apply(&mut self, operation: &str, arguments: &Value) -> ServiceResult<Value> {
        match operation {
            "web.providerKind" => Ok(provider_kind(&arguments["descriptor"])
                .map(|kind| json!(kind))
                .unwrap_or(Value::Null)),
            "web.publicFetch" => Ok(json!(public_fetch(&arguments["descriptor"]))),
            "web.automaticBrowser" => Ok(json!(automatic_browser(
                &arguments["context"],
                &arguments["descriptor"]
            ))),
            "web.check" => {
                if arguments["mode"] == "off" {
                    return Err(ServiceError::new(
                        "WEB_SEARCH_DISABLED",
                        "网页检索已关闭。",
                        409,
                    ));
                }
                let deep = arguments["depth"] == "deep";
                let stage = self.stage.get_or_insert_with(|| Stage {
                    started: Instant::now(),
                    paused: Duration::ZERO,
                    duration_ms: if deep { 180000.0 } else { 45000.0 },
                    queries: if deep { 12 } else { 2 },
                    pages: if deep { 20 } else { 6 },
                    query_count: 0,
                    page_count: 0,
                    seen: HashSet::new(),
                });
                Self::check_stage(stage, string(&arguments["kind"]))?;
                Ok(
                    json!({"remainingMs":(stage.duration_ms-Self::elapsed(stage)).max(1.0),"stage":Self::snapshot(stage)}),
                )
            }
            "web.take" => {
                let stage = self.stage.as_mut().ok_or_else(|| {
                    ServiceError::new("WEB_STAGE_NOT_STARTED", "网页检索阶段尚未开始。", 409)
                })?;
                Self::check_stage(stage, string(&arguments["kind"]))?;
                if arguments["kind"] == "query" {
                    stage.query_count += 1;
                } else {
                    stage.page_count += 1;
                }
                Ok(
                    json!({"remainingMs":(stage.duration_ms-Self::elapsed(stage)).max(1.0),"stage":Self::snapshot(stage)}),
                )
            }
            "web.available" => {
                let Some(stage) = &self.stage else {
                    return Ok(json!(true));
                };
                let descriptor = &arguments["descriptor"];
                let query =
                    descriptor["name"] == "web.search" || provider_kind(descriptor).is_some();
                let page = public_fetch(descriptor);
                Ok(json!(
                    !query && !page
                        || Self::elapsed(stage) < stage.duration_ms
                            && if query {
                                stage.query_count < stage.queries
                            } else {
                                stage.page_count < stage.pages
                            }
                ))
            }
            "web.snapshot" => Ok(self
                .stage
                .as_ref()
                .map(Self::snapshot)
                .unwrap_or(Value::Null)),
            "web.pause" => {
                if let Some(stage) = self.stage.as_mut() {
                    stage.paused += Duration::from_secs_f64(
                        arguments["durationMs"].as_f64().unwrap_or(0.0).max(0.0) / 1000.0,
                    );
                }
                Ok(Value::Null)
            }
            "web.setDuration" => {
                if let Some(stage) = self.stage.as_mut() {
                    stage.duration_ms = arguments["durationMs"].as_f64().unwrap_or(0.0);
                }
                Ok(Value::Null)
            }
            "web.remaining" => Ok(self
                .stage
                .as_ref()
                .map(|stage| json!(stage.duration_ms - Self::elapsed(stage)))
                .unwrap_or(Value::Null)),
            "web.results" => {
                let stage = self.stage.as_mut().ok_or_else(|| {
                    ServiceError::new("WEB_STAGE_NOT_STARTED", "网页检索阶段尚未开始。", 409)
                })?;
                let mut new_evidence = 0;
                for source in array(&arguments["sources"]) {
                    if stage.seen.insert(sha256(
                        format!("{}{}", string(&source["url"]), string(&source["excerpt"]))
                            .as_bytes(),
                    )) {
                        new_evidence += 1;
                    }
                }
                let sources = array(&arguments["sources"]);
                Ok(
                    json!({"sources":sources,"providerId":arguments["providerId"],"originalResultRef":arguments["resultRef"],"newEvidence":new_evidence,
                    "stopSuggested":!sources.is_empty() && new_evidence==0,"remainingQueries":stage.queries.saturating_sub(stage.query_count),
                    "evidenceDecision":{"sufficiency":"not-evaluated","missingInformation":arguments["gap"],
                        "next":if sources.is_empty(){"state-the-gap-or-adjust-the-query"}else{"answer-if-supported-otherwise-read-the-specific-missing-page"},
                        "notice":"Check entity, time, scope and omitted conditions. Answer when those requested facts have sources. New URLs or matching keywords alone do not prove sufficiency. Another search should address a concrete gap; failed search is not permission to open a local browser."}}),
                )
            }
            "web.providerPlan" => {
                let descriptors = array(&arguments["descriptors"]);
                let provider = string(&arguments["providerId"]);
                let index = descriptors
                    .iter()
                    .position(|descriptor| {
                        provider_kind(descriptor).is_some()
                            && (provider == "auto" || descriptor["serverId"] == provider)
                    })
                    .ok_or_else(|| {
                        ServiceError::new(
                            "WEB_SEARCH_PROVIDER_UNAVAILABLE",
                            "没有就绪的搜索服务，请启用 Exa、Parallel 或 Brave MCP。",
                            503,
                        )
                    })?;
                let descriptor = &descriptors[index];
                let properties = descriptor["originalInputSchema"]
                    .get("properties")
                    .unwrap_or(&descriptor["inputSchema"]["properties"]["arguments"]["properties"]);
                let count = arguments["limit"]
                    .as_u64()
                    .unwrap_or(6)
                    .min(if arguments["depth"] == "deep" { 20 } else { 6 });
                let mut input = json!({"query":arguments["query"]});
                if provider_kind(descriptor) == Some("parallel") {
                    // Preserve the original query and stable conversation identity; never rotate identities to evade limits.
                    // 保留原查询与稳定聊天身份，不能通过轮换身份规避限流。
                    input = json!({"objective":arguments["query"],"search_queries":[arguments["query"]]});
                    if properties.get("session_id").is_some() {
                        input["session_id"] =
                            json!(sha256(string(&arguments["conversationId"]).as_bytes()));
                    }
                }
                if properties.get("numResults").is_some() {
                    input["numResults"] = json!(count);
                }
                if properties.get("count").is_some() {
                    input["count"] = json!(count);
                }
                if properties.get("search_lang").is_some() && arguments["language"] != "auto" {
                    input["search_lang"] = json!(if arguments["language"] == "zh-CN" {
                        "zh-hans"
                    } else {
                        "en"
                    });
                }
                Ok(json!({"index":index,"count":count,"arguments":input}))
            }
            "web.fetchTimeout" => Ok(json!(bounded(&arguments["timeoutMs"], 20000, 100, 60000)?)),
            "web.decodePlan" => {
                let content_type = string(&arguments["contentType"]);
                let label = regex::Regex::new(r#"(?i)charset\s*=\s*["']?([^\s;"']+)"#)
                    .unwrap()
                    .captures(content_type)
                    .and_then(|captures| captures.get(1))
                    .map(|value| value.as_str())
                    .unwrap_or("utf-8");
                let media_type = content_type
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_lowercase();
                let html = ["text/html", "application/xhtml+xml"].contains(&media_type.as_str());
                Ok(json!({"label":label,"mediaType":media_type,"html":html}))
            }
            "web.href" => {
                let url = Url::parse(string(&arguments["base"]))
                    .ok()
                    .and_then(|base| base.join(string(&arguments["href"])).ok());
                Ok(json!(
                    url.filter(|url| ["http", "https"].contains(&url.scheme())
                        && url.username().is_empty()
                        && url.password().is_none())
                        .map(|url| url.to_string())
                        .unwrap_or_default()
                ))
            }
            "web.fetchReceipt" => fetch_receipt(arguments),
            _ => Err(ServiceError::new(
                "AGENT_CORE_UNKNOWN_OPERATION",
                "Unknown web service operation.",
                400,
            )),
        }
    }
}

fn fetch_receipt(arguments: &Value) -> ServiceResult<Value> {
    let extracted = string(&arguments["extracted"]);
    if arguments["binary"] == true {
        return Err(ServiceError::new(
            "WEB_UNSUPPORTED_CONTENT",
            "网页不是受支持的文本。",
            415,
        ));
    }
    let units = extracted.encode_utf16().collect::<Vec<_>>();
    let mut full_end = units.len().min(262144);
    if full_end > 0 && (0xd800..=0xdbff).contains(&units[full_end - 1]) {
        full_end -= 1;
    }
    let full = String::from_utf16_lossy(&units[..full_end]);
    let truncated = full_end < units.len() || extracted.contains("[KYNXA_WEB_EXTRACTION_LIMIT]");
    let input = &arguments["input"];
    let offset = bounded(&input["offset"], 0, 0, 262144)?;
    let limit = bounded(&input["limit"], 16000, 1, 16000)?;
    if offset > full_end
        || offset > 0
            && offset < full_end
            && (0xdc00..=0xdfff).contains(&units[offset])
            && (0xd800..=0xdbff).contains(&units[offset - 1])
    {
        return Err(ServiceError::new(
            "WEB_INVALID_OFFSET",
            "网页分页位置无效，请使用返回的 nextOffset。",
            400,
        ));
    }
    let mut end = full_end.min(offset + limit);
    if end > 0 && end < full_end && (0xd800..=0xdbff).contains(&units[end - 1]) {
        end -= 1;
    }
    if end == offset && end < full_end {
        end = (end + 2).min(full_end);
    }
    let mut metadata = arguments["metadata"].clone();
    metadata["title"] = json!(slice(string(&arguments["title"]).trim(), 0, 512));
    metadata["extractedCharacters"] = json!(full_end);
    metadata["extractionTruncated"] = json!(truncated);
    metadata["javascriptExecuted"] = json!(false);
    let mut value = metadata.clone();
    value["content"] = json!(String::from_utf16_lossy(&units[offset..end]));
    value["offset"] = json!(offset);
    value["nextOffset"] = json!(end);
    value["hasMore"] = json!(end < full_end);
    value["truncated"] = json!(offset > 0 || end < full_end || truncated);
    let mut complete = metadata.clone();
    complete["content"] = json!(full);
    let title = string(&metadata["title"]);
    let url = string(&metadata["url"]);
    Ok(
        json!({"value":value,"canonical":{"content":[{"type":"resource_link","uri":url,"name":if title.is_empty(){url}else{title},"mimeType":"text/plain"}],"structuredContent":complete,"isError":false},"isError":false,"outsideWorkspace":true}),
    )
}

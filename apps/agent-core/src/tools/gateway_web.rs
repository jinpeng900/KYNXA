//! Public webpage retrieval, executed from the configured gateway host.
//! 从实际网关主机读取公共网页；不猜测执行环境、不自动打开用户浏览器。
//!
//! Extraction and UTF-16 pagination live here; proxy selection and destination validation live in gateway_web_proxy.
//! 本模块负责正文提取与 UTF-16 分页；代理选择和目的地核验由 gateway_web_proxy 负责。

use super::gateway::{GatewayToolContext, check_cancelled, failure, wait_for_cancellation};
use crate::gateway_contract::GatewayError;
use futures_util::StreamExt;
use scraper::{Html, Selector};
use serde_json::{Value, json};
use std::{collections::HashSet, time::Duration};

const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_EXTRACTED_CHARS: usize = 262144;

async fn decoded_body_bounded(
    response: reqwest::Response,
    maximum_bytes: usize,
) -> Result<Vec<u8>, GatewayError> {
    // Cap reqwest's decoded stream, not the compressed Content-Length.
    // 限制 reqwest 解压后的正文大小，不能按压缩 Content-Length 计算。
    if response
        .content_length()
        .is_some_and(|size| size > maximum_bytes as u64)
    {
        return Err(failure(
            "WEB_RESPONSE_TOO_LARGE",
            "响应超过本次读取字节额度。",
            413,
        ));
    }
    if response
        .headers()
        .get(reqwest::header::CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value != "identity")
    {
        return Err(failure(
            "WEB_UNSUPPORTED_ENCODING",
            "服务器使用未支持的压缩格式。",
            415,
        ));
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|_| failure("WEB_NETWORK_FAILED", "网页解压或响应读取中断。", 502))?;
        if bytes.len().saturating_add(chunk.len()) > maximum_bytes {
            return Err(failure(
                "WEB_RESPONSE_TOO_LARGE",
                "响应超过本次读取字节额度。",
                413,
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[cfg(test)]
async fn decoded_body(response: reqwest::Response) -> Result<Vec<u8>, GatewayError> {
    decoded_body_bounded(response, MAX_RESPONSE_BYTES).await
}

pub(crate) fn extract_html(input: &str, url: &url::Url) -> (String, String) {
    let document = Html::parse_document(input);
    let title = document
        .select(&Selector::parse("title").expect("title selector"))
        .next()
        .map(|element| element.text().collect::<String>())
        .unwrap_or_default();
    let excluded = Selector::parse(
        "script,style,noscript,template,svg,img,input,textarea,[hidden],[aria-hidden='true']",
    )
    .expect("excluded selector");
    let mut output = String::new();
    let mut characters = 0;
    for node in document.tree.nodes() {
        if let scraper::node::Node::Text(text) = node.value() {
            if node
                .ancestors()
                .filter_map(scraper::ElementRef::wrap)
                .any(|parent| excluded.matches(&parent))
            {
                continue;
            }
            let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
            if text.is_empty() {
                continue;
            }
            if !output.is_empty() {
                output.push('\n');
                characters += 1;
            }
            output.push_str(&text);
            characters += text.chars().count();
            if let Some(link) = node
                .ancestors()
                .filter_map(scraper::ElementRef::wrap)
                .find(|element| element.value().name() == "a")
            {
                if let Some(href) = link
                    .value()
                    .attr("href")
                    .and_then(|href| url.join(href).ok())
                    .filter(|href| ["http", "https"].contains(&href.scheme()) && href.has_host())
                {
                    if href.as_str() != text {
                        output.push_str(" (");
                        output.push_str(href.as_str());
                        output.push(')');
                        characters += href.as_str().chars().count() + 3;
                    }
                }
            }
            if characters >= MAX_EXTRACTED_CHARS {
                output = output.chars().take(MAX_EXTRACTED_CHARS).collect();
                output.push_str("\n[KYNXA_WEB_EXTRACTION_LIMIT]\n");
                break;
            }
        }
    }
    (title, output)
}

pub(crate) fn decode_text(
    bytes: &[u8],
    content_type: &str,
    mime: &str,
) -> Result<(String, Value), GatewayError> {
    let header_charset = content_type.split(';').skip(1).find_map(|part| {
        let (name, value) = part.trim().split_once('=')?;
        name.eq_ignore_ascii_case("charset")
            .then(|| value.trim().trim_matches(['\'', '"']).to_owned())
    });
    let meta_charset = if mime == "text/html" || mime == "application/xhtml+xml" {
        // Read only the bounded HTML header to discover declared charset; do not treat bad bytes as an unreadable page.
        // 只检查有界 HTML 头中的编码声明；局部无效字节不能导致整页无法读取。
        let header = String::from_utf8_lossy(&bytes[..bytes.len().min(8192)]);
        let document = Html::parse_document(&header);
        document
            .select(&Selector::parse("meta").expect("meta selector"))
            .find_map(|meta| {
                if let Some(charset) = meta.value().attr("charset") {
                    return Some(charset.trim().to_owned());
                }
                if !meta
                    .value()
                    .attr("http-equiv")
                    .is_some_and(|value| value.eq_ignore_ascii_case("content-type"))
                {
                    return None;
                }
                meta.value().attr("content")?.split(';').find_map(|part| {
                    let (name, value) = part.trim().split_once('=')?;
                    name.eq_ignore_ascii_case("charset")
                        .then(|| value.trim().trim_matches(['\'', '"']).to_owned())
                })
            })
    } else {
        None
    };
    // Encoding precedence is BOM, explicit HTTP charset, HTML metadata, then UTF-8.
    // 编码依次采用 BOM、明确 HTTP 声明、HTML 元数据和 UTF-8 默认值。
    let (encoding, source) = if let Some((encoding, _)) = encoding_rs::Encoding::for_bom(bytes) {
        (encoding, "bom")
    } else if let Some(charset) = header_charset {
        (
            encoding_rs::Encoding::for_label(charset.as_bytes()).ok_or_else(|| {
                failure("WEB_UNSUPPORTED_ENCODING", "网页字符编码不受支持。", 415)
            })?,
            "http-charset",
        )
    } else if let Some(encoding) =
        meta_charset.and_then(|charset| encoding_rs::Encoding::for_label(charset.as_bytes()))
    {
        (encoding, "html-meta")
    } else {
        (encoding_rs::UTF_8, "default-utf8")
    };
    let (decoded, _, had_replacements) = encoding.decode(bytes);
    Ok((
        decoded.into_owned(),
        json!({
            "encoding": encoding.name(),
            "source": source,
            "hadReplacements": had_replacements,
        }),
    ))
}

/// One guarded transport serves webpage tools and explicitly imported document URLs.
/// 网页工具与明确导入的文档链接共用目的地核验、代理和逐跳重定向路径。
pub(crate) struct PublicDownload {
    pub bytes: Vec<u8>,
    pub final_url: url::Url,
    pub requested_url: String,
    pub content_type: String,
    pub media_type: String,
    pub status: u16,
    pub redirects: Vec<Value>,
    pub fetched_at: String,
    pub response_hash: String,
    pub route: &'static str,
    pub resolution: &'static str,
}

pub(crate) fn normalize_public_url(requested_url: &str) -> Result<url::Url, GatewayError> {
    super::service_http::public_url(&json!(requested_url))
        .map_err(|error| failure(&error.code, &error.message, error.status_code))
}

struct GuardedResponse {
    response: reqwest::Response,
    route: &'static str,
    resolution: &'static str,
    _transport: Option<super::gateway_web_proxy::PublicClient>,
}

async fn send_public(url: url::Url, timeout: Duration) -> Result<GuardedResponse, GatewayError> {
    let transport = super::gateway_web_proxy::client(&url, timeout).await?;
    let response = transport.client.get(url)
        .header(reqwest::header::USER_AGENT, "KYNXA/0.1 (+native-public-fetch)")
        .header(reqwest::header::ACCEPT, "text/html,application/xhtml+xml,application/pdf,application/octet-stream;q=0.9,*/*;q=0.8")
        .send().await.map_err(|error| failure(if error.is_timeout() { "WEB_TIMEOUT" } else { "WEB_NETWORK_FAILED" }, "网页请求未完成。", 502))?;
    Ok(GuardedResponse {
        response,
        route: transport.route,
        resolution: transport.resolution,
        _transport: Some(transport),
    })
}

fn follows_location(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

fn redirect_target(
    base: &url::Url,
    location: Option<&reqwest::header::HeaderValue>,
) -> Result<url::Url, GatewayError> {
    let location = location
        .and_then(|value| std::str::from_utf8(value.as_bytes()).ok())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 8192
                && !value
                    .chars()
                    .any(|character| character.is_control() || character == '\\')
        })
        .ok_or_else(|| failure("WEB_REDIRECT_FAILED", "网页重定向缺少有效 Location。", 502))?;
    // Encode UTF-8 Location values through URL joining, then recheck the public destination.
    // 合并 URL 时编码 UTF-8 Location，再核验公共目的地址。
    let target = base
        .join(location)
        .map_err(|_| failure("WEB_REDIRECT_FAILED", "网页重定向地址无效。", 502))?;
    normalize_public_url(target.as_str())
}

fn diagnostic_url(value: &str) -> Option<String> {
    let mut url = normalize_public_url(value).ok()?;
    url.set_query(None);
    url.set_fragment(None);
    Some(url.as_str().chars().take(1024).collect())
}

fn download_failure(
    error: GatewayError,
    url: &url::Url,
    status: Option<u16>,
    redirects: &[Value],
) -> GatewayError {
    let safe_url = diagnostic_url(url.as_str());
    let safe_redirects = redirects
        .iter()
        .take(8)
        .map(|hop| {
            json!({
                "from":hop["from"].as_str().and_then(diagnostic_url),
                "to":hop["to"].as_str().and_then(diagnostic_url),
                "status":hop["status"],
            })
        })
        .collect::<Vec<_>>();
    let retryable = matches!(error.code.as_str(), "WEB_TIMEOUT" | "WEB_NETWORK_FAILED")
        || status.is_some_and(|status| status >= 500);
    let recovery = if matches!(error.code.as_str(), "WEB_URL_BLOCKED" | "TOOL_CANCELLED") {
        json!({"action":"stop-this-read","retrySameUrl":false,
            "notice":"Keep the original permission boundary; do not bypass it with another transport."})
    } else {
        // Recovery is a scoped suggestion, never an automatic search or permission to navigate a user's page.
        // 恢复只提供当前来源的建议；不会自动搜索，也不授权改变用户页面或复制登录凭据。
        json!({"action":if status==Some(404){"locate-current-source"}else{"use-another-enabled-read-path"},
            "retrySameUrl":retryable,"tool":"web.search",
            "arguments":{"query":format!("site:{}",url.host_str().unwrap_or("")),
                "reason":"Locate a readable current source after the requested public page could not be read."},
            "notice":"Preserve the original task and this failure. Prefer saved result pages when available; otherwise use an enabled source extractor or an authorized existing browser. Login and human verification require the user; do not repeatedly retry the same unchanged URL."})
    };
    error.with_details(json!({"upstreamStatus":status,"failedUrl":safe_url,
        "redirectCount":redirects.len(),"redirects":safe_redirects,"retryable":retryable,"recovery":recovery}))
}

/// Keep one deadline and one redirect policy for production transport and deterministic loopback checks.
/// 生产传输与确定性回环验证共用总截止和跳转规则；测试注入不改变正式目的地准入。
async fn download_with_sender<F, Pending>(
    requested_url: &str,
    maximum_bytes: usize,
    timeout_ms: u64,
    text_only: bool,
    mut send: F,
) -> Result<PublicDownload, GatewayError>
where
    F: FnMut(url::Url, Duration) -> Pending,
    Pending: std::future::Future<Output = Result<GuardedResponse, GatewayError>>,
{
    let mut url = normalize_public_url(requested_url)?;
    let mut redirects = Vec::new();
    let mut visited = HashSet::from([url.to_string()]);
    let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
    for _ in 0..9 {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(download_failure(
                failure("WEB_TIMEOUT", "网页总读取预算已到期。", 504),
                &url,
                None,
                &redirects,
            ));
        }
        let guarded = tokio::time::timeout_at(deadline, send(url.clone(), remaining))
            .await
            .unwrap_or_else(|_| Err(failure("WEB_TIMEOUT", "网页总读取预算已到期。", 504)))
            .map_err(|error| download_failure(error, &url, None, &redirects))?;
        // The private relay must remain owned until the response body has been consumed or this hop ends.
        // 私有转接在响应正文读取完成或当前跳结束前保持所有权，不能收到响应头就提前关闭。
        let GuardedResponse {
            response,
            route,
            resolution,
            _transport,
        } = guarded;
        let status = response.status().as_u16();
        if follows_location(status) {
            let target =
                redirect_target(&url, response.headers().get(reqwest::header::LOCATION))
                    .map_err(|error| download_failure(error, &url, Some(status), &redirects))?;
            redirects.push(json!({"from":url.as_str(),"to":target.as_str(),"status":status}));
            if !visited.insert(target.to_string()) {
                return Err(download_failure(
                    failure(
                        "WEB_REDIRECT_LOOP",
                        "网页跳转回已访问地址，请改用其他有效来源或受控浏览器读取。",
                        502,
                    ),
                    &target,
                    Some(status),
                    &redirects,
                ));
            }
            if redirects.len() >= 9 {
                return Err(download_failure(
                    failure("WEB_REDIRECT_LIMIT", "网页重定向次数超过限额。", 502),
                    &url,
                    Some(status),
                    &redirects,
                ));
            }
            url = target;
            continue;
        }
        if status == 304 {
            return Err(download_failure(
                failure(
                    "WEB_NOT_MODIFIED_WITHOUT_CACHE",
                    "服务器返回 304，但本次没有可复用的缓存正文；请读取原归档或使用其他已启用读取方式。",
                    502,
                ),
                &url,
                Some(status),
                &redirects,
            ));
        }
        if !response.status().is_success() {
            let error = failure(
                "WEB_HTTP_STATUS",
                &format!("网页返回 HTTP {status}。"),
                if status < 400 { 502 } else { status },
            )
            .with_retry_after(super::gateway_mcp::retry_after_ms(response.headers()));
            return Err(download_failure(error, &url, Some(status), &redirects));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_owned();
        let media_type = content_type
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_lowercase();
        if text_only
            && ![
                "text/html",
                "application/xhtml+xml",
                "text/plain",
                "text/markdown",
                "application/json",
            ]
            .contains(&media_type.as_str())
        {
            return Err(download_failure(
                failure(
                    "WEB_UNSUPPORTED_CONTENT",
                    "网页响应不是可直接读取的文本；文档需显式导入后专用解析。",
                    415,
                ),
                &url,
                Some(status),
                &redirects,
            ));
        }
        let bytes =
            tokio::time::timeout_at(deadline, decoded_body_bounded(response, maximum_bytes))
                .await
                .unwrap_or_else(|_| Err(failure("WEB_TIMEOUT", "网页总读取预算已到期。", 504)))
                .map_err(|error| download_failure(error, &url, Some(status), &redirects))?;
        use sha2::Digest;
        let response_hash = format!("{:x}", sha2::Sha256::digest(&bytes));
        return Ok(PublicDownload {
            bytes,
            final_url: url,
            requested_url: requested_url.to_owned(),
            content_type,
            media_type,
            status,
            redirects,
            fetched_at: chrono::Utc::now().to_rfc3339(),
            response_hash,
            route,
            resolution,
        });
    }
    Err(download_failure(
        failure("WEB_REDIRECT_LIMIT", "网页重定向次数超过限额。", 502),
        &url,
        redirects
            .last()
            .and_then(|hop| hop["status"].as_u64())
            .map(|status| status as u16),
        &redirects,
    ))
}

async fn download_inner(
    requested_url: &str,
    maximum_bytes: usize,
    timeout_ms: u64,
    text_only: bool,
) -> Result<PublicDownload, GatewayError> {
    download_with_sender(
        requested_url,
        maximum_bytes,
        timeout_ms,
        text_only,
        send_public,
    )
    .await
}

/// Cancellation drops the same guarded request and private relay; imported URLs do not gain a different network policy.
/// 取消释放同一受控请求及私有转接；导入链接不会取得另一套网络权限。
pub(crate) async fn download_public(
    requested_url: &str,
    maximum_bytes: usize,
    timeout_ms: u64,
    cancellation: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<PublicDownload, GatewayError> {
    let timeout_ms = timeout_ms.clamp(100, 120_000);
    let maximum_bytes = maximum_bytes.min(128 * 1024 * 1024);
    tokio::select! {
        result = tokio::time::timeout(Duration::from_millis(timeout_ms),download_inner(requested_url,maximum_bytes,timeout_ms,false)) => {
            result.unwrap_or_else(|_|Err(failure("WEB_TIMEOUT","链接读取总预算已到期。",504)))
        },
        _ = async {
            loop {
                if cancellation.load(std::sync::atomic::Ordering::Acquire) {break;}
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        } => Err(failure("TOOL_CANCELLED","链接读取已取消。",409)),
    }
}

fn static_page_state(decoded: &str, url: &url::Url, mime: &str, title: &str, text: &str) -> Value {
    if !["text/html", "application/xhtml+xml"].contains(&mime) {
        return json!({"evidenceUsable":true,"needsHumanVerification":false,"requiresRendering":false});
    }
    let document = Html::parse_document(decoded);
    let google = url.host_str().is_some_and(|host| {
        host == "google.com"
            || host.starts_with("www.google.")
            || host.starts_with("ipv4.google.")
            || host.starts_with("ipv6.google.")
    });
    let summary = text.chars().take(3000).collect::<String>().to_lowercase();
    let google_challenge = google
        && (url.path() == "/sorry"
            || url.path().starts_with("/sorry/")
            || summary.contains("our systems have detected unusual traffic")
            || (summary.contains("我们的系统检测到") && summary.contains("异常流量")));
    let cloudflare_form = document
        .select(
            &Selector::parse("#challenge-form,#cf-challenge-running").expect("challenge selector"),
        )
        .next()
        .is_some();
    let challenge_title = format!("{title} {summary}").to_lowercase();
    let cloudflare_challenge = cloudflare_form
        && [
            "just a moment",
            "verify you are human",
            "verify that you are human",
            "人机验证",
            "验证您是人类",
        ]
        .iter()
        .any(|marker| challenge_title.contains(marker));
    let needs_human = google_challenge || cloudflare_challenge;
    // Only a bounded, identifiable empty script shell requests rendering; ordinary short factual pages remain usable.
    // 仅识别有明确脚本空壳特征的短页；正常简短正文仍可使用，验证码从不当作来源证据。
    let scripts = document
        .select(&Selector::parse("script").expect("script selector"))
        .next()
        .is_some();
    let empty_app = document
        .select(
            &Selector::parse("#app:empty,#root:empty,#__next:empty").expect("application selector"),
        )
        .next()
        .is_some();
    let javascript_notice = document
        .select(&Selector::parse("noscript").expect("noscript selector"))
        .any(|element| {
            let value = element.text().collect::<String>().to_lowercase();
            value.contains("enable javascript")
                || value.contains("启用 javascript")
                || value.contains("开启javascript")
        });
    let requires_rendering =
        !needs_human && text.chars().count() < 160 && scripts && (empty_app || javascript_notice);
    json!({
        "evidenceUsable":!needs_human && !requires_rendering,
        "needsHumanVerification":needs_human,
        "verificationProvider":if google_challenge {Some("google")} else if cloudflare_challenge {Some("cloudflare")} else {None},
        "requiresRendering":requires_rendering,
        "recovery":if needs_human {json!({"action":"user-verification-or-another-enabled-source","retrySameUrl":false})}
            else if requires_rendering {json!({"action":"use-authorized-browser-or-enabled-content-extractor","retrySameUrl":false})}
            else {Value::Null},
    })
}

async fn fetch_document(input: &Value) -> Result<Value, GatewayError> {
    let requested_url = input["url"].as_str().unwrap_or("");
    let timeout_ms = input["timeoutMs"]
        .as_u64()
        .unwrap_or(30_000)
        .clamp(100, 60_000);
    let download = download_inner(requested_url, MAX_RESPONSE_BYTES, timeout_ms, true).await?;
    let url = &download.final_url;
    let bytes = &download.bytes;
    let content_type = &download.content_type;
    let mime = &download.media_type;
    let status = download.status;
    let redirects = &download.redirects;
    if ![
        "text/html",
        "application/xhtml+xml",
        "text/plain",
        "text/markdown",
        "application/json",
    ]
    .contains(&mime.as_str())
    {
        return Err(failure(
            "WEB_UNSUPPORTED_CONTENT",
            "网页响应不是可直接读取的文本；文档需显式导入后专用解析。",
            415,
        ));
    }
    let (decoded, text_decoding) = decode_text(&bytes, &content_type, &mime)?;
    let (title, text) = if mime == "text/html" || mime == "application/xhtml+xml" {
        extract_html(&decoded, &url)
    } else {
        (
            String::new(),
            decoded
                .chars()
                .take(MAX_EXTRACTED_CHARS)
                .collect::<String>(),
        )
    };
    let page_state = static_page_state(&decoded, url, mime, &title, &text);
    return Ok(json!({
        "title": title,
        "url": url.as_str(),
        "requestedUrl": requested_url,
        "finalUrl": url.as_str(),
        "status": status,
        "mediaType": mime,
        "responseBytes": bytes.len(),
        "textDecoding": text_decoding,
        "redirects": redirects,
        "fetchedAt": download.fetched_at,
        "responseHash": download.response_hash,
        "fullText": text,
        "pageState": page_state,
        "evidenceUsable": page_state["evidenceUsable"],
        "needsHumanVerification": page_state["needsHumanVerification"],
        "requiresRendering": page_state["requiresRendering"],
        "recovery": page_state["recovery"],
        "executionEnvironment": {
            "schemaVersion": 1,
            "executorKind": "builtin-http",
            "executorLocation": "gateway-host",
            "operationLocation": "gateway-host",
            "gatewayHostMeaning": "machine-running-the-gateway",
            "userDeviceRelationship": "unverified",
            "grantsPermission": false,
            "network": {
                "requestOrigin": "gateway-host",
                "route": download.route,
                "addressVerification": download.resolution,
            },
        },
    }));
}

fn fetch_key(input: &Value, ctx: &GatewayToolContext) -> Result<String, GatewayError> {
    let mut url = normalize_public_url(input["url"].as_str().unwrap_or(""))?;
    url.set_fragment(None);
    Ok(super::local_files::sha256(
        json!({
            "url":url.as_str(),"conversation":ctx.conversation_id,"project":ctx.project_id,
            "permission":ctx.permission_mode,"roots":ctx.authorized_roots,"dataHome":ctx.data_home,
            "configurationGeneration":ctx.config["configurationGeneration"],
        })
        .to_string()
        .as_bytes(),
    ))
}

async fn fetch_scoped(input: &Value, ctx: &GatewayToolContext) -> Result<Value, GatewayError> {
    let key = fetch_key(input, ctx)?;
    if input["offset"].as_u64().unwrap_or(0) > 0 && input["refresh"] != true {
        if let Some(snapshot) = ctx.runtime.web_fetch.get(&key) {
            super::gateway::check_configuration_generation(ctx)?;
            return Ok(snapshot.page(input, true));
        }
    }
    let snapshot = super::gateway_web_snapshot::PageSnapshot::new(fetch_document(input).await?);
    super::gateway::check_configuration_generation(ctx)?;
    let page = snapshot.page(input, false);
    ctx.runtime.web_fetch.insert(key, snapshot);
    Ok(page)
}

#[cfg(test)]
async fn fetch_inner(input: &Value) -> Result<Value, GatewayError> {
    Ok(
        super::gateway_web_snapshot::PageSnapshot::new(fetch_document(input).await?)
            .page(input, false),
    )
}

pub(crate) async fn fetch(input: Value, ctx: &GatewayToolContext) -> Result<Value, GatewayError> {
    check_cancelled(ctx)?;
    let mut cancellation = ctx.cancellation.clone();
    let timeout_ms = input["timeoutMs"]
        .as_u64()
        .unwrap_or(30_000)
        .clamp(100, 60_000);
    tokio::select! {
        result = tokio::time::timeout(Duration::from_millis(timeout_ms), fetch_scoped(&input, ctx)) => {
            result.unwrap_or_else(|_| {
                Err(failure("WEB_TIMEOUT", "网页总读取预算已到期。", 504))
            })
        },
        _ = wait_for_cancellation(&mut cancellation) => {
            Err(failure("TOOL_CANCELLED", "网页读取已取消。", 409))
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct RedirectFixture {
        address: std::net::SocketAddr,
        requests: std::sync::Arc<tokio::sync::Mutex<Vec<String>>>,
        server: tokio::task::JoinHandle<()>,
    }

    impl Drop for RedirectFixture {
        fn drop(&mut self) {
            self.server.abort();
        }
    }

    impl RedirectFixture {
        async fn start() -> Self {
            use axum::{
                body::Body,
                extract::State,
                http::{Response, Uri},
            };
            let requests = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
            let service = axum::Router::new()
                .fallback(
                    |State(requests): State<std::sync::Arc<tokio::sync::Mutex<Vec<String>>>>,
                     uri: Uri| async move {
                        requests.lock().await.push(uri.path().to_owned());
                        let mut status = 200;
                        let mut location = None;
                        let mut retry_after = None;
                        match uri.path() {
                            "/missing" => status = 302,
                            "/multiple" => status = 300,
                            "/not-modified" => status = 304,
                            "/not-found" => status = 404,
                            "/rate-limited" => {
                                status = 429;
                                retry_after = Some("60");
                            }
                            "/cycle-a" => {
                                status = 302;
                                location = Some("/cycle-b?token=fixture-secret".to_owned());
                            }
                            "/cycle-b" => {
                                status = 307;
                                location = Some("/cycle-a".to_owned());
                            }
                            "/private" => {
                                status = 301;
                                location = Some("http://127.0.0.1/fixture-secret".to_owned());
                            }
                            "/slow-a" => {
                                tokio::time::sleep(Duration::from_millis(300)).await;
                                status = 302;
                                location = Some("/slow-b".to_owned());
                            }
                            "/slow-b" => {
                                tokio::time::sleep(Duration::from_millis(300)).await;
                            }
                            path if path.starts_with("/redirect/") => {
                                status = path
                                    .trim_start_matches("/redirect/")
                                    .parse::<u16>()
                                    .unwrap();
                                location = Some("/英雄图鉴".to_owned());
                            }
                            path if path.starts_with("/limit/") => {
                                let step =
                                    path.trim_start_matches("/limit/").parse::<u16>().unwrap();
                                status = 302;
                                location = Some(format!("/limit/{}", step + 1));
                            }
                            _ => {}
                        }
                        let mut response = Response::builder()
                            .status(status)
                            .header("content-type", "text/plain");
                        if let Some(location) = location {
                            response = response.header(
                                "location",
                                reqwest::header::HeaderValue::from_bytes(location.as_bytes())
                                    .unwrap(),
                            );
                        }
                        if let Some(retry_after) = retry_after {
                            response = response.header("retry-after", retry_after);
                        }
                        response
                            .body(Body::from(
                                "Public fixture body; never copy an error body's fixture-secret.",
                            ))
                            .unwrap()
                    },
                )
                .with_state(requests.clone());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                axum::serve(listener, service).await.unwrap();
            });
            Self {
                address,
                requests,
                server,
            }
        }

        async fn download(
            &self,
            path: &str,
            timeout_ms: u64,
        ) -> Result<PublicDownload, GatewayError> {
            let client = reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap();
            let address = self.address;
            // Only this test sender maps an already validated public URL to its owned loopback HTTP fixture.
            // 仅测试发送器把已核验公共 URL 映射到自有回环 HTTP 服务；生产发送器和逐跳准入不变。
            download_with_sender(
                &format!("http://example.org{path}"),
                4096,
                timeout_ms,
                true,
                move |url, _| {
                    let client = client.clone();
                    async move {
                        assert_eq!(url.host_str(), Some("example.org"));
                        let mut local = url.clone();
                        local.set_host(Some("127.0.0.1")).unwrap();
                        local.set_port(Some(address.port())).unwrap();
                        let response = client.get(local).send().await.map_err(|_| {
                            failure("WEB_NETWORK_FAILED", "Fixture request failed.", 502)
                        })?;
                        Ok(GuardedResponse {
                            response,
                            route: "loopback-fixture",
                            resolution: "fixture-public-url",
                            _transport: None,
                        })
                    }
                },
            )
            .await
        }
    }

    #[test]
    fn redirect_targets_encode_utf8_without_accepting_unsafe_controls_or_destinations() {
        let base = url::Url::parse("https://example.org/wiki/index").unwrap();
        let unicode = reqwest::header::HeaderValue::from_bytes("/英雄图鉴".as_bytes()).unwrap();
        let target = redirect_target(&base, Some(&unicode)).unwrap();
        assert_eq!(
            target.as_str(),
            "https://example.org/%E8%8B%B1%E9%9B%84%E5%9B%BE%E9%89%B4"
        );
        for target in [
            "/%0d%0aX-Header:secret",
            "http://127.0.0.1/",
            "https://user:secret@example.org/",
            "file:///secret",
            "\\private",
        ] {
            let location = reqwest::header::HeaderValue::from_bytes(target.as_bytes()).unwrap();
            assert!(redirect_target(&base, Some(&location)).is_err(), "{target}");
        }
        let tab = reqwest::header::HeaderValue::from_bytes(b"/a\tb").unwrap();
        assert!(redirect_target(&base, Some(&tab)).is_err());
        let invalid_utf8 = reqwest::header::HeaderValue::from_bytes(b"/\xff").unwrap();
        assert!(redirect_target(&base, Some(&invalid_utf8)).is_err());
    }

    #[tokio::test]
    async fn actual_redirect_transport_distinguishes_headers_loops_status_and_safe_recovery() {
        let fixture = RedirectFixture::start().await;
        for status in [301, 302, 303, 307, 308] {
            let result = fixture
                .download(&format!("/redirect/{status}"), 5000)
                .await
                .unwrap();
            assert_eq!(result.status, 200);
            assert_eq!(result.redirects.len(), 1);
            assert_eq!(result.redirects[0]["status"], status);
            assert_eq!(
                result.final_url.path(),
                "/%E8%8B%B1%E9%9B%84%E5%9B%BE%E9%89%B4"
            );
        }
        for (path, code, status) in [
            ("/missing", "WEB_REDIRECT_FAILED", 302),
            ("/multiple", "WEB_HTTP_STATUS", 300),
            ("/not-modified", "WEB_NOT_MODIFIED_WITHOUT_CACHE", 304),
            ("/not-found", "WEB_HTTP_STATUS", 404),
            ("/rate-limited", "WEB_HTTP_STATUS", 429),
        ] {
            let error = fixture
                .download(&format!("{path}?token=fixture-secret"), 5000)
                .await
                .err()
                .unwrap();
            assert_eq!(error.code, code);
            let response = error.response();
            assert_eq!(response["details"]["upstreamStatus"], status);
            assert_eq!(
                response["details"]["failedUrl"],
                format!("http://example.org{path}")
            );
            assert_eq!(response["details"]["recovery"]["tool"], "web.search");
            assert!(!response.to_string().contains("fixture-secret"));
            assert_eq!(response["details"]["retryable"], false);
            if status == 429 {
                assert_eq!(error.retry_after_ms, Some(60_000));
            }
        }
        let before = fixture.requests.lock().await.len();
        let error = fixture.download("/cycle-a", 5000).await.err().unwrap();
        assert_eq!(error.code, "WEB_REDIRECT_LOOP");
        assert_eq!(fixture.requests.lock().await.len() - before, 2);
        assert_eq!(error.response()["details"]["redirectCount"], 2);
        assert!(!error.response().to_string().contains("fixture-secret"));
        let before = fixture.requests.lock().await.len();
        let error = fixture.download("/private", 5000).await.err().unwrap();
        assert_eq!(error.code, "WEB_URL_BLOCKED");
        assert_eq!(fixture.requests.lock().await.len() - before, 1);
        assert!(!error.response().to_string().contains("127.0.0.1"));
        assert!(error.response()["details"]["recovery"]["tool"].is_null());
        let before = fixture.requests.lock().await.len();
        let error = fixture.download("/limit/0", 5000).await.err().unwrap();
        assert_eq!(error.code, "WEB_REDIRECT_LIMIT");
        assert_eq!(fixture.requests.lock().await.len() - before, 9);
        assert_eq!(
            error.response()["details"]["redirects"]
                .as_array()
                .unwrap()
                .len(),
            8
        );
    }

    #[tokio::test]
    async fn redirect_transport_keeps_one_total_deadline_and_never_dispatches_after_it() {
        let fixture = RedirectFixture::start().await;
        let error = fixture
            .download("/slow-a?token=fixture-secret", 500)
            .await
            .err()
            .unwrap();
        assert_eq!(error.code, "WEB_TIMEOUT");
        assert!(!error.response().to_string().contains("fixture-secret"));
        let before = fixture.requests.lock().await.len();
        let error = fixture.download("/redirect/301", 0).await.err().unwrap();
        assert_eq!(error.code, "WEB_TIMEOUT");
        assert_eq!(fixture.requests.lock().await.len(), before);
    }

    #[test]
    fn static_content_diagnostics_reject_verification_and_script_shells_without_rejecting_short_pages()
     {
        let google = url::Url::parse("https://www.google.com/sorry/index").unwrap();
        let value = static_page_state("<p>Verify</p>", &google, "text/html", "Verify", "Verify");
        assert_eq!(value["evidenceUsable"], false);
        assert_eq!(value["needsHumanVerification"], true);
        let public = url::Url::parse("https://example.org/page").unwrap();
        let shell = static_page_state(
            "<div id='app'></div><script src='/app.js'></script>",
            &public,
            "text/html",
            "App",
            "App",
        );
        assert_eq!(shell["requiresRendering"], true);
        assert_eq!(shell["evidenceUsable"], false);
        let short = static_page_state(
            "<p>42</p><script>analytics()</script>",
            &public,
            "text/html",
            "Answer",
            "42",
        );
        assert_eq!(short["evidenceUsable"], true);
        let challenge = static_page_state(
            "<form id='challenge-form'></form>",
            &public,
            "text/html",
            "Just a moment",
            "Verify you are human",
        );
        assert_eq!(challenge["verificationProvider"], "cloudflare");
        assert_eq!(challenge["evidenceUsable"], false);
    }

    #[tokio::test]
    async fn continuation_uses_scoped_snapshot_and_rechecks_configuration_without_network() {
        use std::sync::Arc;
        let (_keep, cancellation) = tokio::sync::watch::channel(false);
        let mut ctx = GatewayToolContext {
            data_home: "fixture-data".into(),
            extension_root: "fixture-tools".into(),
            workdir: "fixture-work".into(),
            toolhost_exe: "fixture-toolhost".into(),
            permission_mode: "smart".into(),
            conversation_id: "first".into(),
            project_id: None,
            authorized_roots: vec![],
            cancellation,
            runtime: Arc::new(super::super::gateway::ToolRuntime::default()),
            config: json!({}),
            approved: false,
            events: None,
            external_execute: None,
        };
        let input = json!({"url":"https://example.org/live?topic=first","offset":4,"limit":6});
        let key = fetch_key(&input, &ctx).unwrap();
        ctx.runtime.web_fetch.insert(
            key.clone(),
            super::super::gateway_web_snapshot::PageSnapshot::new(
                json!({"fullText":"old cached content","responseHash":"original"}),
            ),
        );
        let page = fetch_scoped(&input, &ctx).await.unwrap();
        assert_eq!(page["content"], "cached");
        assert_eq!(page["cache"]["hit"], true);
        assert_eq!(page["contentHash"], "original");
        ctx.conversation_id = "another".into();
        assert_ne!(fetch_key(&input, &ctx).unwrap(), key);
        ctx.conversation_id = "first".into();
        ctx.permission_mode = "full".into();
        assert_ne!(fetch_key(&input, &ctx).unwrap(), key);
        ctx.config["configurationGeneration"] = json!(1);
        let stale_key = fetch_key(&input, &ctx).unwrap();
        ctx.runtime.web_fetch.insert(
            stale_key,
            super::super::gateway_web_snapshot::PageSnapshot::new(json!({"fullText":"stale"})),
        );
        assert_eq!(
            fetch_scoped(&input, &ctx).await.unwrap_err().code,
            "TOOL_CONFIGURATION_CHANGED"
        );
    }

    #[tokio::test]
    #[ignore = "Explicit public-page smoke uses the user's configured network proxy"]
    async fn configured_proxy_fetches_public_page_without_fake_ip_rejection() {
        let started = std::time::Instant::now();
        let value = fetch_inner(&json!({
            "url": "https://pvp.qq.com/web201605/herolist.shtml",
            "limit": 1000,
            "timeoutMs": 60000,
        }))
        .await
        .unwrap();
        assert_eq!(value["status"], 200);
        assert!(!value["content"].as_str().unwrap().is_empty());
        assert_eq!(
            value["executionEnvironment"]["network"]["route"],
            "configured-http-proxy"
        );
        println!(
            "public fetch: elapsedMs={}, status={}, bytes={}, route={}, verification={}, decoding={}",
            started.elapsed().as_millis(),
            value["status"],
            value["responseBytes"],
            value["executionEnvironment"]["network"]["route"],
            value["executionEnvironment"]["network"]["addressVerification"],
            value["textDecoding"]
        );
    }
    #[test]
    fn html_charset_and_partial_invalid_bytes_remain_readable_and_observable() {
        let source =
            "<meta http-equiv='Content-Type' content='text/html; charset=gb2312'><p>王者荣耀</p>";
        let (bytes, _, _) = encoding_rs::GBK.encode(source);
        let (decoded, diagnostics) = decode_text(&bytes, "text/html", "text/html").unwrap();
        assert!(decoded.contains("王者荣耀"));
        assert_eq!(diagnostics["encoding"], "GBK");
        assert_eq!(diagnostics["source"], "html-meta");
        let (_, diagnostics) = decode_text(
            b"hello\xffworld",
            "text/plain; CHARSET='UTF-8'",
            "text/plain",
        )
        .unwrap();
        assert_eq!(diagnostics["source"], "http-charset");
        assert_eq!(diagnostics["hadReplacements"], true);
    }
    #[tokio::test]
    async fn native_compression_decodes_all_formats_and_bounds_decoded_bytes() {
        use axum::{Router, routing::get};
        use base64::Engine;
        let mut service = Router::new();
        for (name, value) in [
            (
                "gzip",
                "H4sIAAAAAAAACnu6fdOL5Yuf9nUr5CWWZJalKiTn55Wk5pUAAOEhRywYAAAA",
            ),
            ("deflate", "eJx7un3Ti+WLn/Z1K+QllmSWpSok5+eVpOaVAACvYwxB"),
            ("br", "iwuA5bey6Kej5Y6LIG5hdGl2ZSBjb250ZW50Aw=="),
        ] {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(value)
                .unwrap();
            service = service.route(
                &format!("/{name}"),
                get(move || {
                    let bytes = bytes.clone();
                    async move {
                        (
                            [("content-encoding", name), ("content-type", "text/plain")],
                            bytes,
                        )
                    }
                }),
            );
        }
        // A 2 KiB gzip fixture expands beyond 2 MiB; its compressed Content-Length cannot bypass the decoded cap.
        // 约 2 KiB 的 gzip 夹具解压后超过 2 MiB；较小的 Content-Length 不能绕过正文限额。
        service = service.route(
            "/limit",
            get(|| async {
                (
                    [("content-encoding", "gzip")],
                    include_bytes!("../../tests/fixtures/web-decoded-limit.gz").to_vec(),
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, service).await.unwrap();
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .gzip(true)
            .brotli(true)
            .deflate(true)
            .build()
            .unwrap();
        for name in ["gzip", "deflate", "br"] {
            let response = client
                .get(format!("http://{address}/{name}"))
                .send()
                .await
                .unwrap();
            assert_eq!(
                String::from_utf8(decoded_body(response).await.unwrap()).unwrap(),
                "已解压 native content"
            );
        }
        assert_eq!(
            decoded_body(
                client
                    .get(format!("http://{address}/limit"))
                    .send()
                    .await
                    .unwrap()
            )
            .await
            .unwrap_err()
            .code,
            "WEB_RESPONSE_TOO_LARGE"
        );
        server.abort();
        let _ = server.await;
    }
    #[test]
    fn html_excludes_secret_controls_and_preserves_public_links() {
        let (title, text) = extract_html(
            "<html><title>Title</title><body><script>secret()</script><input value='password'><p hidden>hidden</p><a href='/doc'>Document</a><p>Hello</p></body></html>",
            &url::Url::parse("https://example.org/page").unwrap(),
        );
        assert_eq!(title, "Title");
        assert!(text.contains("https://example.org/doc"));
        assert!(text.contains("Hello"));
        assert!(!text.contains("secret"));
        assert!(!text.contains("password"));
        assert!(!text.contains("hidden"));
    }
}

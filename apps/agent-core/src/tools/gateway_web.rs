//! Public webpage retrieval and search, executed from the configured gateway host.
//! 从实际网关主机读取公共网页与检索；不猜测执行环境、不自动打开用户浏览器。
//!
//! Extraction and UTF-16 pagination live here; proxy selection and destination validation live in gateway_web_proxy.
//! 本模块负责正文提取与 UTF-16 分页；代理选择和目的地核验由 gateway_web_proxy 负责。

use super::gateway::{GatewayToolContext, check_cancelled, failure};
use crate::gateway_contract::GatewayError;
use futures_util::StreamExt;
use scraper::{Html, Selector};
use serde_json::{Value, json};
use std::time::Duration;

const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_EXTRACTED_CHARS: usize = 262144;

async fn decoded_body(response: reqwest::Response) -> Result<Vec<u8>, GatewayError> {
    // Reqwest removes accepted compression headers and yields decoded bytes; cap that stream, not the compressed wire length.
    // reqwest 解压受支持的响应并移除压缩头；限制解压后正文，不能用较小的压缩长度绕过内存额度。
    if response
        .content_length()
        .is_some_and(|size| size > MAX_RESPONSE_BYTES as u64)
    {
        return Err(failure(
            "WEB_RESPONSE_TOO_LARGE",
            "网页响应超过 2 MiB。",
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
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(failure(
                "WEB_RESPONSE_TOO_LARGE",
                "网页响应超过 2 MiB。",
                413,
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn extract_html(input: &str, url: &url::Url) -> (String, String) {
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

fn decode_text(
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

async fn fetch_inner(input: &Value) -> Result<Value, GatewayError> {
    let requested_url = input["url"].as_str().unwrap_or("");
    let mut url = super::service_http::public_url(&json!(requested_url))
        .map_err(|error| failure(&error.code, &error.message, error.status_code))?;
    let timeout_ms = input["timeoutMs"]
        .as_u64()
        .unwrap_or(30_000)
        .clamp(100, 60_000);
    let mut redirects = Vec::new();
    for _ in 0..9 {
        // Revalidate and pin every redirect, while preserving the actual configured proxy.
        // 每次重定向重新核验并绑定公共地址，同时保留用户真正配置的代理。
        let transport =
            super::gateway_web_proxy::client(&url, Duration::from_millis(timeout_ms)).await?;
        let response = transport
            .client
            .get(url.clone())
            .header(
                reqwest::header::USER_AGENT,
                "KYNXA/0.1 (+native-public-fetch)",
            )
            .header(
                reqwest::header::ACCEPT,
                "text/html,application/xhtml+xml,text/plain,application/json;q=0.9",
            )
            .send()
            .await
            .map_err(|error| {
                failure(
                    if error.is_timeout() {
                        "WEB_TIMEOUT"
                    } else {
                        "WEB_NETWORK_FAILED"
                    },
                    "网页请求未完成。",
                    502,
                )
            })?;
        if response.status().is_redirection() {
            let target = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| {
                    failure("WEB_REDIRECT_FAILED", "网页重定向缺少有效 Location。", 502)
                })?;
            let target = url
                .join(target)
                .map_err(|_| failure("WEB_REDIRECT_FAILED", "网页重定向地址无效。", 502))?;
            let target = super::service_http::public_url(&json!(target.as_str()))
                .map_err(|error| failure(&error.code, &error.message, error.status_code))?;
            redirects.push(json!({
                "from": url.as_str(),
                "to": target.as_str(),
                "status": response.status().as_u16(),
            }));
            url = target;
            continue;
        }
        let status = response.status().as_u16();
        if !response.status().is_success() {
            return Err(failure(
                "WEB_HTTP_STATUS",
                &format!("网页返回 HTTP {status}。"),
                status,
            ));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("text/plain")
            .to_owned();
        let mime = content_type
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_lowercase();
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
                "网页响应不是可直接读取的文本；文档需下载后专用解析。",
                415,
            ));
        }
        let bytes = decoded_body(response).await?;
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
        let units = text.encode_utf16().collect::<Vec<_>>();
        let offset = input["offset"].as_u64().unwrap_or(0) as usize;
        if offset > units.len() {
            return Err(failure(
                "WEB_OFFSET_OUT_OF_RANGE",
                "网页 offset 超出当前内容。",
                400,
            ));
        }
        // Public offsets count UTF-16 units; avoid ending a page inside a surrogate pair.
        // 公开偏移按 UTF-16 单元计数；页尾不能截断代理对。
        let mut end = (offset + input["limit"].as_u64().unwrap_or(16000).clamp(1, 16000) as usize)
            .min(units.len());
        if end < units.len() && end > 0 && (0xd800..=0xdbff).contains(&units[end - 1]) {
            if end - offset == 1 {
                end += 1;
            } else {
                end -= 1;
            }
        }
        let page = String::from_utf16(&units[offset..end]).map_err(|_| {
            failure(
                "WEB_INVALID_OFFSET",
                "网页 offset 位于 UTF-16 代理对中间。",
                400,
            )
        })?;
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
            "fetchedAt": chrono::Utc::now().to_rfc3339(),
            "content": page,
            "offset": offset,
            "nextOffset": end,
            "totalChars": units.len(),
            "hasMore": end < units.len(),
            "fullText": text,
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
                    "route": transport.route,
                    "addressVerification": transport.resolution,
                },
            },
        }));
    }
    Err(failure(
        "WEB_REDIRECT_LIMIT",
        "网页重定向次数超过限额。",
        502,
    ))
}

async fn wait_for_cancellation(cancellation: &mut tokio::sync::watch::Receiver<bool>) {
    loop {
        if *cancellation.borrow_and_update() {
            break;
        }
        if cancellation.changed().await.is_err() {
            // A closed signal channel is not an explicit cancellation request.
            // 信号通道关闭不等于用户明确取消，继续等待外层执行或超时结束。
            std::future::pending::<()>().await;
        }
    }
}

pub(crate) async fn fetch(input: Value, ctx: &GatewayToolContext) -> Result<Value, GatewayError> {
    check_cancelled(ctx)?;
    let mut cancellation = ctx.cancellation.clone();
    let timeout_ms = input["timeoutMs"]
        .as_u64()
        .unwrap_or(30_000)
        .clamp(100, 60_000);
    tokio::select! {
        result = tokio::time::timeout(Duration::from_millis(timeout_ms), fetch_inner(&input)) => {
            result.unwrap_or_else(|_| {
                Err(failure("WEB_TIMEOUT", "网页总读取预算已到期。", 504))
            })
        },
        _ = wait_for_cancellation(&mut cancellation) => {
            Err(failure("TOOL_CANCELLED", "网页读取已取消。", 409))
        },
    }
}

async fn search_inner(input: Value, ctx: &GatewayToolContext) -> Result<Value, GatewayError> {
    check_cancelled(ctx)?;
    if ctx.config["retrieval"]["web"]["mode"] == "off" {
        return Err(failure("WEB_SEARCH_DISABLED", "网页检索已关闭。", 409));
    }
    let query = input["query"]
        .as_str()
        .filter(|query| !query.trim().is_empty())
        .ok_or_else(|| failure("INVALID_TOOL_ARGUMENTS", "搜索 query 不能为空。", 400))?;
    let count = input["limit"].as_u64().unwrap_or(10).clamp(1, 20);
    let selected = ctx.config["retrieval"]["web"]["providerId"]
        .as_str()
        .unwrap_or("auto");
    let mut servers = super::gateway_mcp::servers(ctx);
    let loaded = ctx.runtime.schemas.read().await.clone();
    for server in &mut servers {
        server["tools"] = json!(
            loaded
                .iter()
                .filter(|tool| tool["serverId"] == server["id"])
                .cloned()
                .collect::<Vec<_>>()
        );
    }
    let index = super::service_broker_completion::apply(
        "broker.completion.searchProvider",
        &json!({"servers": servers, "providerId": selected}),
    )
    .map_err(|error| failure(&error.code, &error.message, error.status_code))?;
    let server = index
        .as_u64()
        .and_then(|index| servers.get(index as usize))
        .cloned()
        .ok_or_else(|| {
            failure(
                "WEB_SEARCH_PROVIDER_UNAVAILABLE",
                "选定搜索服务未启用；未绕过设置启用其他远程服务。",
                503,
            )
        })?;
    let brave = server["id"].as_str().is_some_and(|id| id.contains("brave"));
    let command = std::path::Path::new(server["command"].as_str().unwrap_or(""))
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_lowercase();
    if brave && ["npx", "npx.cmd", "npm", "npm.cmd", "node", "node.exe"].contains(&command.as_str())
    {
        if server["disabledTools"]
            .as_array()
            .is_some_and(|tools| tools.contains(&json!("brave_web_search")))
        {
            return Err(failure(
                "WEB_SEARCH_PROVIDER_UNAVAILABLE",
                "Brave 搜索工具已禁用。",
                403,
            ));
        }
        let key = std::env::var("KYNXA_BRAVE_API_KEY").map_err(|_| {
            failure(
                "WEB_SEARCH_CREDENTIAL_MISSING",
                "Brave API key 未配置。",
                401,
            )
        })?;
        let mut url = url::Url::parse("https://api.search.brave.com/res/v1/web/search")
            .expect("fixed search endpoint");
        url.query_pairs_mut()
            .append_pair("q", query)
            .append_pair("count", &count.to_string());
        let transport = super::gateway_web_proxy::client(&url, Duration::from_secs(30)).await?;
        let response = transport
            .client
            .get(url)
            .header("X-Subscription-Token", key)
            .send()
            .await
            .map_err(|_| failure("WEB_SEARCH_FAILED", "Brave 搜索请求失败。", 502))?
            .error_for_status()
            .map_err(|_| failure("WEB_SEARCH_FAILED", "Brave 搜索被服务拒绝。", 502))?;
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk =
                chunk.map_err(|_| failure("WEB_SEARCH_FAILED", "Brave 搜索读取中断。", 502))?;
            if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err(failure(
                    "WEB_RESPONSE_TOO_LARGE",
                    "Brave 搜索响应超出读取边界。",
                    413,
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| failure("WEB_SEARCH_INVALID_RESULT", "Brave 搜索回执无效。", 502))?;
        let sources = value["web"]["results"]
            .as_array()
            .into_iter()
            .flatten()
            .take(count as usize)
            .map(|source| {
                json!({
                    "title": source["title"],
                    "url": source["url"],
                    "snippet": source["description"],
                })
            })
            .collect::<Vec<_>>();
        return Ok(json!({
            "query": query,
            "providerId": server["id"],
            "sources": sources,
            "executionEnvironment": super::contracts::calculate(
                "tools.resolveEnvironment",
                &json!({"descriptor": {"name": "web.fetch", "source": "builtin"}}),
            ).map_err(|message| failure("TOOL_ENVIRONMENT_FAILED", &message, 500))?,
        }));
    }
    let descriptors = super::gateway_mcp::catalog_selected(ctx, vec![server.clone()]).await?;
    let plan = super::gateway::native(
        ctx,
        "web.providerPlan",
        json!({
            "descriptors": descriptors,
            "providerId": server["id"],
            "depth": if count > 6 {
                "deep"
            } else {
                ctx.config["retrieval"]["web"]["depth"].as_str().unwrap_or("standard")
            },
            "language": ctx.config["retrieval"]["web"]["language"].as_str().unwrap_or("auto"),
            "query": query,
            "limit": count,
        }),
    )
    .await?;
    let tool = descriptors[plan["index"].as_u64().unwrap_or(0) as usize].clone();
    let value = super::gateway_mcp::execute(
        tool["name"].as_str().unwrap_or(""),
        json!({
            "arguments": plan["arguments"],
            "policy": {
                "reason": input["reason"].as_str().unwrap_or("Public web search requested by current user"),
            },
        }),
        ctx,
    ).await?;
    Ok(json!({
        "query": query,
        "providerId": server["id"],
        "canonical": value,
        "content": value["content"],
        "isError": value["isError"],
        "status": value["status"],
        "code": value["code"],
        "executionEnvironment": super::contracts::calculate(
            "tools.resolveEnvironment",
            &json!({"descriptor": tool, "server": server}),
        ).map_err(|message| failure("TOOL_ENVIRONMENT_FAILED", &message, 500))?,
    }))
}

pub(crate) async fn search(input: Value, ctx: &GatewayToolContext) -> Result<Value, GatewayError> {
    check_cancelled(ctx)?;
    let mut cancellation = ctx.cancellation.clone();
    tokio::select! {
        result = tokio::time::timeout(Duration::from_secs(60), search_inner(input, ctx)) => {
            result.unwrap_or_else(|_| {
                Err(failure("WEB_TIMEOUT", "网页搜索总预算已到期。", 504))
            })
        },
        _ = wait_for_cancellation(&mut cancellation) => {
            Err(failure("TOOL_CANCELLED", "网页搜索已取消。", 409))
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    #[tokio::test]
    async fn disabled_search_provider_never_creates_a_fallback_connection() {
        let (mut ctx, _sender) = super::super::gateway::test_context();
        let root = ctx.workdir.parent().unwrap().to_owned();
        ctx.config["retrieval"] = json!({
            "web": {"providerId": "official-exa", "mode": "auto"},
        });
        ctx.config["mcpServers"] = json!([{
            "id": "official-exa",
            "enabled": false,
            "transport": "streamable-http",
            "url": "http://127.0.0.1:1/mcp",
        }]);
        assert_eq!(
            search(json!({"query":"public fixture"}), &ctx)
                .await
                .unwrap_err()
                .code,
            "WEB_SEARCH_PROVIDER_UNAVAILABLE"
        );
        assert!(ctx.runtime.mcp.lock().await.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}

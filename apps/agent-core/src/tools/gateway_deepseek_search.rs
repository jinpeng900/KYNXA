//! DeepSeek's hosted search as the built-in last-resort source for a configured official connection.
//! 已配置官方 DeepSeek 连接时接入其托管搜索，作为内置的最后备用来源；不新增用户配置或复制密钥。

use super::gateway::{GatewayToolContext, failure};
use crate::gateway_contract::GatewayError;
use crate::models::gateway_config::ModelConfigStore;
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::time::Duration;

pub(super) const SOURCE_ID: &str = "deepseek-search";
const SEARCH_ENDPOINT: &str = "https://api.deepseek.com/v1/tools/search";
const MAX_QUERY_CHARACTERS: usize = 128;
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

/// Only the official endpoint shares the model key with search; compatible gateways never receive it.
/// 只有官方端点的模型密钥可用于搜索；兼容网关或自建代理的密钥不会发往 DeepSeek 搜索。
fn official_endpoint(base_url: &str) -> bool {
    url::Url::parse(base_url).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str() == Some("api.deepseek.com")
            && url.port().is_none()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && ["", "/", "/v1", "/v1/"].contains(&url.path())
    })
}

/// Read the saved key on demand so tool configuration, cache keys and receipts never hold it.
/// 按需读取已保存密钥，工具配置、缓存键和回执都不持有密钥。
async fn api_key(ctx: &GatewayToolContext) -> Option<String> {
    let store = ModelConfigStore::new(ctx.runtime.model_home.get()?);
    let providers = store.list().await.ok()?;
    for provider in providers.as_array().into_iter().flatten() {
        let (Some(id), Some(base_url)) = (provider["providerId"].as_str(), provider["baseUrl"].as_str()) else {
            continue;
        };
        if !official_endpoint(base_url) {
            continue;
        }
        if let Ok(Some(key)) = store.saved_key(id, base_url).await {
            if !key.trim().is_empty() {
                return Some(key);
            }
        }
    }
    None
}

/// Describe the built-in source only while an official DeepSeek connection has a saved key.
/// 仅在官方 DeepSeek 连接已保存密钥时提供该内置来源；描述中不含密钥。
pub(super) async fn source(ctx: &GatewayToolContext) -> Option<Value> {
    api_key(ctx).await?;
    Some(json!({"id":SOURCE_ID,"name":"DeepSeek Search","builtin":SOURCE_ID,"url":SEARCH_ENDPOINT}))
}

/// Project DeepSeek results into the shared source shape, keeping provider dates as evidence.
/// 将 DeepSeek 结果投影为统一来源格式，保留来源自带日期，不把检索时间当作发布时间。
fn sources(value: &Value, count: usize) -> Vec<Value> {
    value["results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|result| result["url"].as_str().is_some_and(|url| !url.is_empty()))
        .take(count)
        .map(|result| {
            let snippet = result["summary"]
                .as_str()
                .filter(|summary| !summary.trim().is_empty())
                .map(|summary| json!(summary))
                .unwrap_or_else(|| result["snippet"].clone());
            let mut projected = json!({"title":result["title"],"url":result["url"],"snippet":snippet});
            if let Some(site) = result["site_name"].as_str().filter(|site| !site.is_empty()) {
                projected["siteName"] = json!(site);
            }
            super::service_web_sources::preserve_temporal_metadata(result, &mut projected);
            projected
        })
        .collect()
}

pub(super) async fn search(
    query: &str,
    count: u64,
    server: &Value,
    ctx: &GatewayToolContext,
) -> Result<Value, GatewayError> {
    let key = api_key(ctx).await.ok_or_else(|| {
        failure("WEB_SEARCH_CREDENTIAL_MISSING", "DeepSeek 官方连接未保存 API Key。", 401)
    })?;
    // The service accepts at most 128 characters; keep the leading terms and operators instead of failing the search.
    // 服务只接受最多 128 个字符；保留前部关键词与搜索运算符，而不是让整次搜索失败。
    let query = query.trim().chars().take(MAX_QUERY_CHARACTERS).collect::<String>();
    let url = url::Url::parse(SEARCH_ENDPOINT).expect("fixed search endpoint");
    let transport = super::gateway_web_proxy::client(&url, Duration::from_secs(30)).await?;
    let response = transport
        .client
        .post(url)
        .bearer_auth(key)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(json!({"query":query,"count":count,"need_content":false}).to_string())
        .send()
        .await
        .map_err(|_| failure("WEB_SEARCH_FAILED", "DeepSeek 搜索请求失败。", 502))?;
    let status = response.status();
    if !status.is_success() {
        return Err(failure("WEB_SEARCH_HTTP_REJECTED", "DeepSeek 搜索被服务拒绝。", status.as_u16())
            .with_retry_after(super::gateway_mcp::retry_after_ms(response.headers()))
            .with_details(json!({"failureOrigin":"provider-http","httpStatus":status.as_u16()})));
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| failure("WEB_SEARCH_FAILED", "DeepSeek 搜索读取中断。", 502))?;
        if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(failure("WEB_RESPONSE_TOO_LARGE", "DeepSeek 搜索响应超出读取边界。", 413));
        }
        bytes.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| failure("WEB_SEARCH_INVALID_RESULT", "DeepSeek 搜索回执无效。", 502))?;
    Ok(json!({
        "query": query,
        "providerId": server["id"],
        "sources": sources(&value, count as usize),
        "executionEnvironment": super::contracts::calculate(
            "tools.resolveEnvironment",
            &json!({"descriptor": {"name": "web.search", "source": "builtin"}}),
        ).map_err(|message| failure("TOOL_ENVIRONMENT_FAILED", &message, 500))?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_exact_official_endpoint_can_lend_its_key_to_search() {
        for allowed in ["https://api.deepseek.com", "https://api.deepseek.com/", "https://api.deepseek.com/v1"] {
            assert!(official_endpoint(allowed), "{allowed}");
        }
        for denied in [
            "http://api.deepseek.com",
            "https://api.deepseek.com:8443",
            "https://api.deepseek.com/anthropic",
            "https://proxy.example.com/v1",
            "https://api.deepseek.com.evil.example",
            "https://user:pass@api.deepseek.com",
            "https://api.deepseek.com/?route=1",
        ] {
            assert!(!official_endpoint(denied), "{denied}");
        }
    }

    #[test]
    fn results_keep_model_summaries_sites_and_provider_dates_only() {
        let value = json!({"results":[
            {"title":"A","url":"https://a.example/1","snippet":"short","summary":"model summary","site_name":"A Site","date":"2026-10-01T00:00:00Z","rerank_score":2.5},
            {"title":"No URL","url":"","summary":"dropped"},
            {"title":"B","url":"https://b.example/2","snippet":"fallback snippet","summary":"  "},
            {"title":"C","url":"https://c.example/3","summary":"over limit"}
        ]});
        let projected = sources(&value, 2);
        assert_eq!(projected.len(), 2);
        assert_eq!(projected[0]["snippet"], "model summary");
        assert_eq!(projected[0]["siteName"], "A Site");
        assert_eq!(projected[0]["date"], "2026-10-01T00:00:00Z");
        assert!(projected[0].get("rerank_score").is_none());
        assert_eq!(projected[1]["url"], "https://b.example/2");
        assert_eq!(projected[1]["snippet"], "fallback snippet");
        assert!(projected[1].get("date").is_none());
    }

    #[tokio::test]
    #[ignore = "Explicit paid probe against the configured official DeepSeek connection named by KYNXA_DEEPSEEK_SEARCH_MODEL_HOME"]
    async fn configured_official_connection_returns_real_search_sources() {
        let (ctx, _keep) = super::super::gateway::test_context();
        let home = std::env::var_os("KYNXA_DEEPSEEK_SEARCH_MODEL_HOME").expect("model home of a configured connection");
        ctx.runtime.model_home.set(std::path::PathBuf::from(home)).unwrap();
        let server = source(&ctx).await.expect("official DeepSeek connection with a saved key");
        let value = search("DeepSeek API pricing", 5, &server, &ctx).await.unwrap();
        let sources = value["sources"].as_array().unwrap();
        assert!(!sources.is_empty(), "{value}");
        assert!(sources.iter().all(|source| source["url"].as_str().is_some_and(|url| url.starts_with("http"))));
        println!("{}", json!({"count":sources.len(),"first":sources[0]["url"],"date":sources[0]["date"]}));
    }

    #[tokio::test]
    async fn the_source_appears_only_for_an_official_connection_with_a_saved_key() {
        let (ctx, _keep) = super::super::gateway::test_context();
        assert!(source(&ctx).await.is_none(), "No model home means no built-in DeepSeek source.");
        let home = ctx.workdir.parent().unwrap().join("models-home");
        std::fs::create_dir_all(&home).unwrap();
        ctx.runtime.model_home.set(home.clone()).unwrap();
        let write = |providers: Value| std::fs::write(home.join("connections.json"),
            json!({"version":1,"providers":providers}).to_string()).unwrap();
        write(json!([{"providerId":"relay","displayName":"Relay","baseUrl":"https://proxy.example.com/v1",
            "apiKey":"fixture-relay-key","models":["deepseek-flash"],"protocol":"openai-completions"}]));
        assert!(source(&ctx).await.is_none(), "A compatible relay key must not be sent to DeepSeek search.");
        write(json!([{"providerId":"deepseek","displayName":"DeepSeek","baseUrl":"https://api.deepseek.com",
            "apiKey":"","models":["deepseek-flash"],"protocol":"openai-completions"}]));
        assert!(source(&ctx).await.is_none(), "An official connection without a key stays disabled.");
        write(json!([{"providerId":"deepseek","displayName":"DeepSeek","baseUrl":"https://api.deepseek.com",
            "apiKey":"fixture-official-key","models":["deepseek-flash"],"protocol":"openai-completions"}]));
        let described = source(&ctx).await.expect("official connection with a key");
        assert_eq!(described["id"], SOURCE_ID);
        assert!(!described.to_string().contains("fixture-official-key"), "The source description never carries the key.");
    }
}

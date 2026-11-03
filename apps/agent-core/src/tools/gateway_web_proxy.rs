//! 保留用户配置的 HTTP 代理，并把公共网页目的地绑定到已检查地址。
//!
//! 本传输模块拥有地址检查与单次请求的转接生命周期；gateway_web 拥有正文提取和分页。

use super::gateway::failure;
use crate::gateway_contract::GatewayError;
use base64::Engine;
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use url::Url;

/// 传输对象释放时中止自有转接任务，覆盖调用取消和超时路径。
pub(crate) struct PublicClient {
    pub client: reqwest::Client,
    pub route: &'static str,
    pub resolution: &'static str,
    relay: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for PublicClient {
    fn drop(&mut self) {
        if let Some(relay) = self.relay.take() {
            relay.abort();
        }
    }
}

fn bypass_matches(host: &str, rules: &str) -> bool {
    let host = host
        .trim_matches(['[', ']'])
        .trim_end_matches('.')
        .to_ascii_lowercase();
    rules.split([',', ';']).any(|rule| {
        let rule = rule.trim().to_ascii_lowercase();
        if rule == "*"
            || rule == "<local>" && !host.contains('.') && host.parse::<IpAddr>().is_err()
        {
            return true;
        }
        if let Some((network, prefix)) = rule.split_once('/') {
            return match (
                host.parse::<IpAddr>(),
                network.parse::<IpAddr>(),
                prefix.parse::<u32>(),
            ) {
                (Ok(IpAddr::V4(host)), Ok(IpAddr::V4(network)), Ok(prefix)) if prefix <= 32 => {
                    let mask = u32::MAX.checked_shl(32 - prefix).unwrap_or(0);
                    u32::from(host) & mask == u32::from(network) & mask
                }
                (Ok(IpAddr::V6(host)), Ok(IpAddr::V6(network)), Ok(prefix)) if prefix <= 128 => {
                    let mask = u128::MAX.checked_shl(128 - prefix).unwrap_or(0);
                    u128::from(host) & mask == u128::from(network) & mask
                }
                _ => false,
            };
        }
        let rule = rule.trim_start_matches("*.").trim_start_matches('.');
        !rule.is_empty() && (host == rule || host.ends_with(&format!(".{rule}")))
    })
}

fn parse_proxy(value: &str, scheme: &str) -> Result<Url, GatewayError> {
    let value = if value.contains('=') {
        value
            .split(';')
            .find_map(|entry| {
                let (kind, endpoint) = entry.trim().split_once('=')?;
                (kind.eq_ignore_ascii_case(scheme)).then_some(endpoint)
            })
            .ok_or_else(|| {
                failure(
                    "WEB_PROXY_UNSUPPORTED",
                    "系统代理没有当前协议的可用地址。",
                    503,
                )
            })?
    } else {
        value.trim()
    };
    let proxy = Url::parse(
        if value.contains("://") {
            value.to_owned()
        } else {
            format!("http://{value}")
        }
        .as_str(),
    )
    .map_err(|_| failure("WEB_PROXY_INVALID", "已配置代理地址无效。", 503))?;
    if proxy.scheme() != "http"
        || !proxy.has_host()
        || proxy.path() != "/"
        || proxy.query().is_some()
        || proxy.fragment().is_some()
    {
        return Err(failure(
            "WEB_PROXY_UNSUPPORTED",
            "当前网页转接支持 HTTP 代理；未绕过其他代理配置直连。",
            503,
        ));
    }
    Ok(proxy)
}

#[cfg(windows)]
fn system_proxy() -> Result<Option<(String, String)>, GatewayError> {
    #[repr(C)]
    struct ProxyConfig {
        auto_detect: i32,
        auto_config_url: *mut u16,
        proxy: *mut u16,
        bypass: *mut u16,
    }
    #[link(name = "winhttp")]
    unsafe extern "system" {
        fn WinHttpGetIEProxyConfigForCurrentUser(config: *mut ProxyConfig) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalFree(memory: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
    }
    unsafe fn take_text(pointer: *mut u16) -> String {
        if pointer.is_null() {
            return String::new();
        }
        let mut length = 0;
        while length < 32768 && unsafe { *pointer.add(length) } != 0 {
            length += 1;
        }
        // WinHTTP 返回 GlobalAlloc 所有的文本；先复制内容，再仅释放该分配一次。
        let text = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(pointer, length) });
        unsafe {
            GlobalFree(pointer.cast());
        }
        text
    }
    let mut config = ProxyConfig {
        auto_detect: 0,
        auto_config_url: std::ptr::null_mut(),
        proxy: std::ptr::null_mut(),
        bypass: std::ptr::null_mut(),
    };
    if unsafe { WinHttpGetIEProxyConfigForCurrentUser(&mut config) } == 0 {
        return Ok(None);
    }
    let auto_url = unsafe { take_text(config.auto_config_url) };
    let proxy = unsafe { take_text(config.proxy) };
    let bypass = unsafe { take_text(config.bypass) };
    if !proxy.is_empty() {
        return Ok(Some((proxy, bypass)));
    }
    if !auto_url.is_empty() {
        return Err(failure(
            "WEB_PROXY_UNSUPPORTED",
            "网页已识别 PAC 自动代理；当前不能核验其目的地，请使用受控浏览器读取。",
            503,
        ));
    }
    Ok(None)
}

#[cfg(not(windows))]
fn system_proxy() -> Result<Option<(String, String)>, GatewayError> {
    Ok(None)
}

pub(crate) fn proxy_for(url: &Url) -> Result<Option<Url>, GatewayError> {
    // 明确绕过规则和环境代理配置优先于当前用户的 Windows 代理配置。
    let bypass = std::env::var("NO_PROXY")
        .or_else(|_| std::env::var("no_proxy"))
        .unwrap_or_default();
    if bypass_matches(url.host_str().unwrap_or(""), &bypass) {
        return Ok(None);
    }
    let variable = if url.scheme() == "https" {
        "HTTPS_PROXY"
    } else {
        "HTTP_PROXY"
    };
    for variable in [
        variable.to_owned(),
        variable.to_ascii_lowercase(),
        "ALL_PROXY".to_owned(),
        "all_proxy".to_owned(),
    ] {
        if let Ok(value) = std::env::var(variable) {
            if !value.trim().is_empty() {
                return parse_proxy(&value, url.scheme()).map(Some);
            }
        }
    }
    if let Some((proxy, bypass)) = system_proxy()? {
        if !bypass_matches(url.host_str().unwrap_or(""), &bypass) {
            return parse_proxy(&proxy, url.scheme()).map(Some);
        }
    }
    Ok(None)
}

async fn public_dns(
    host: &str,
    proxy: &Url,
    timeout: Duration,
) -> Result<Vec<SocketAddr>, GatewayError> {
    type CachedAnswers = std::collections::HashMap<[u8; 32], (Instant, Vec<SocketAddr>)>;
    static ANSWERS: OnceLock<Mutex<CachedAnswers>> = OnceLock::new();
    use sha2::{Digest, Sha256};
    let key: [u8; 32] = Sha256::digest(format!("{}\n{host}", proxy.as_str()).as_bytes()).into();
    let cache = ANSWERS.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    if let Ok(cache) = cache.lock() {
        if let Some((_, addresses)) = cache
            .get(&key)
            .filter(|(expires, _)| *expires > Instant::now())
        {
            return Ok(addresses.clone());
        }
    }
    // 检查解析经过现有代理，只发送当前公共网页域名。
    let client = reqwest::Client::builder()
        .no_proxy()
        .proxy(
            reqwest::Proxy::all(proxy.as_str())
                .map_err(|_| failure("WEB_PROXY_INVALID", "已配置代理地址无效。", 503))?,
        )
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .build()
        .map_err(|_| failure("WEB_HTTP_FAILED", "公共地址核验客户端初始化失败。", 500))?;
    let response = client
        .get("https://cloudflare-dns.com/dns-query")
        .header("accept", "application/dns-json")
        .query(&[("name", host), ("type", "A")])
        .send()
        .await
        .map_err(|_| {
            failure(
                "WEB_PUBLIC_DNS_UNAVAILABLE",
                "代理虚拟地址的公共解析核验未完成；可改用受控浏览器。",
                503,
            )
        })?;
    if !response.status().is_success() {
        return Err(failure(
            "WEB_PUBLIC_DNS_UNAVAILABLE",
            "公共解析服务未返回有效结果。",
            503,
        ));
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk
            .map_err(|_| failure("WEB_PUBLIC_DNS_UNAVAILABLE", "公共解析回执读取失败。", 503))?;
        if bytes.len() + chunk.len() > 64 * 1024 {
            return Err(failure(
                "WEB_PUBLIC_DNS_UNAVAILABLE",
                "公共解析回执超过核验限额。",
                503,
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| failure("WEB_PUBLIC_DNS_UNAVAILABLE", "公共解析回执无效。", 503))?;
    let addresses = public_dns_addresses(&value)?;
    let ttl_seconds = value["Answer"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|answer| answer["TTL"].as_u64())
        .min()
        .unwrap_or(0)
        .min(120);
    // 连续分页只在 DNS TTL 内复用已检查地址；限量缓存不授予私网访问能力。
    if ttl_seconds > 0 {
        if let Ok(mut cache) = cache.lock() {
            cache.retain(|_, (expires, _)| *expires > Instant::now());
            if cache.len() >= 128 {
                if let Some(oldest) = cache
                    .iter()
                    .min_by_key(|(_, (expires, _))| expires)
                    .map(|(key, _)| *key)
                {
                    cache.remove(&oldest);
                }
            }
            cache.insert(
                key,
                (
                    Instant::now() + Duration::from_secs(ttl_seconds),
                    addresses.clone(),
                ),
            );
        }
    }
    Ok(addresses)
}

fn public_dns_addresses(value: &Value) -> Result<Vec<SocketAddr>, GatewayError> {
    if value["Status"] != 0 {
        return Err(failure(
            "WEB_PUBLIC_DNS_UNAVAILABLE",
            "公共解析未找到有效地址。",
            503,
        ));
    }
    let addresses = value["Answer"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|answer| answer["type"] == 1 || answer["type"] == 28)
        .map(|answer| answer["data"].as_str().unwrap_or("").parse::<IpAddr>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| failure("WEB_PUBLIC_DNS_UNAVAILABLE", "公共解析地址无效。", 503))?;
    if addresses.is_empty()
        || addresses.len() > 64
        || addresses
            .iter()
            .any(|address| !super::service_http::public_address(&address.to_string()))
    {
        return Err(failure(
            "WEB_URL_BLOCKED",
            "公共解析结果包含非公共地址，网页读取已拒绝。",
            403,
        ));
    }
    Ok(addresses
        .into_iter()
        .map(|address| SocketAddr::new(address, 443))
        .collect())
}

async fn read_http_header(stream: &mut TcpStream) -> Result<Vec<u8>, std::io::Error> {
    let mut bytes = Vec::new();
    // 只读取协议头，随后 TLS 隧道的首批字节留给隧道。
    while bytes.len() < 16384 {
        let byte = stream.read_u8().await?;
        bytes.push(byte);
        if bytes.ends_with(b"\r\n\r\n") {
            return Ok(bytes);
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "proxy header too large",
    ))
}

async fn relay_public_request(
    listener: TcpListener,
    proxy: Url,
    url: Url,
    destinations: Vec<SocketAddr>,
    authorization_token: String,
) -> Result<(), std::io::Error> {
    let (mut incoming, _) = listener.accept().await?;
    let request = read_http_header(&mut incoming).await?;
    let request_text =
        std::str::from_utf8(&request).map_err(|_| std::io::Error::other("invalid proxy header"))?;
    let first_line = request_text.lines().next().unwrap_or("");
    if !request_text.lines().any(|line| {
        line.split_once(':').is_some_and(|(name, value)| {
            name.eq_ignore_ascii_case("proxy-authorization") && value.trim() == authorization_token
        })
    }) {
        return Err(std::io::Error::other("invalid private relay token"));
    }
    let authority = format!(
        "{}:{}",
        url.host_str().unwrap_or(""),
        url.port_or_known_default().unwrap_or(443)
    );
    let is_secure = url.scheme() == "https";
    if is_secure && first_line != format!("CONNECT {authority} HTTP/1.1")
        || !is_secure && first_line != format!("GET {} HTTP/1.1", url.as_str())
    {
        return Err(std::io::Error::other("unexpected proxy destination"));
    }
    let mut upstream = TcpStream::connect((
        proxy.host_str().unwrap_or(""),
        proxy.port_or_known_default().unwrap_or(80),
    ))
    .await?;
    // 上游代理只连接已检查公共 IP，防止第二次解析时把网页域名重绑到内网。
    let target = destinations[0].to_string();
    let mut connect = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
    if !proxy.username().is_empty() || proxy.password().is_some() {
        let credentials = format!("{}:{}", proxy.username(), proxy.password().unwrap_or(""));
        connect.push_str(&format!(
            "Proxy-Authorization: Basic {}\r\n",
            base64::engine::general_purpose::STANDARD.encode(credentials)
        ));
    }
    connect.push_str("\r\n");
    upstream.write_all(connect.as_bytes()).await?;
    let response = read_http_header(&mut upstream).await?;
    if std::str::from_utf8(&response)
        .ok()
        .and_then(|text| text.lines().next())
        .and_then(|line| line.split_whitespace().nth(1))
        != Some("200")
    {
        incoming
            .write_all(
                b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
            )
            .await?;
        return Err(std::io::Error::other("upstream proxy rejected tunnel"));
    }
    if is_secure {
        incoming
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?;
    } else {
        let path = format!(
            "{}{}",
            url.path(),
            url.query()
                .map(|query| format!("?{query}"))
                .unwrap_or_default()
        );
        let mut forwarded = format!("GET {path} HTTP/1.1\r\n");
        for line in request_text
            .split("\r\n")
            .skip(1)
            .filter(|line| !line.is_empty())
        {
            if !line.to_ascii_lowercase().starts_with("proxy-") {
                forwarded.push_str(line);
                forwarded.push_str("\r\n");
            }
        }
        forwarded.push_str("\r\n");
        upstream.write_all(forwarded.as_bytes()).await?;
    }
    tokio::io::copy_bidirectional(&mut incoming, &mut upstream).await?;
    Ok(())
}

pub(crate) async fn client(url: &Url, timeout: Duration) -> Result<PublicClient, GatewayError> {
    super::service_http::public_url(&json!(url.as_str()))
        .map_err(|error| failure(&error.code, &error.message, error.status_code))?;
    let proxy = proxy_for(url)?;
    let host = url.host_str().unwrap_or("").trim_matches(['[', ']']);
    let port = url.port_or_known_default().unwrap_or(443);
    let gateway_addresses = tokio::net::lookup_host((host, port))
        .await
        .map(|addresses| addresses.collect::<Vec<_>>());
    let is_public_resolution = gateway_addresses.as_ref().is_ok_and(|addresses| {
        !addresses.is_empty()
            && addresses.len() <= 64
            && addresses
                .iter()
                .all(|address| super::service_http::public_address(&address.ip().to_string()))
    });
    // 配了代理时本地 DNS 可能是虚拟地址或被污染，优先经代理查公共解析；结果同样必须是公网地址。
    let verified = match proxy.as_ref().filter(|_| host.contains('.')) {
        Some(proxy) => Some(public_dns(host, proxy, timeout).await),
        None => None,
    };
    let (mut addresses, resolution) = match verified {
        Some(Ok(addresses)) => (addresses, "public-dns-via-configured-proxy"),
        Some(Err(error)) if error.code == "WEB_URL_BLOCKED" || !is_public_resolution => {
            return Err(error);
        }
        _ if is_public_resolution => (
            gateway_addresses.unwrap_or_default(),
            "gateway-dns-public",
        ),
        _ if gateway_addresses.is_err() => {
            return Err(failure("WEB_DNS_FAILED", "网页主机解析失败。", 502));
        }
        _ => {
            return Err(failure(
                "WEB_URL_BLOCKED",
                "DNS 包含无法核验的非公共地址；网页读取已拒绝。",
                403,
            ));
        }
    };
    for address in &mut addresses {
        address.set_port(port);
    }
    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .gzip(true)
        .brotli(true)
        .deflate(true)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout);
    let (relay_task, route) = if let Some(proxy) = proxy {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|_| failure("WEB_PROXY_FAILED", "受控代理转接初始化失败。", 503))?;
        let token = uuid::Uuid::new_v4().to_string();
        let authorization = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("kynxa:{token}"))
        );
        builder = builder.proxy(
            reqwest::Proxy::all(format!("http://{}", listener.local_addr().unwrap()))
                .map_err(|_| failure("WEB_PROXY_FAILED", "受控代理转接配置失败。", 503))?
                .basic_auth("kynxa", &token),
        );
        let url = url.clone();
        (
            Some(tokio::spawn(async move {
                let _ = tokio::time::timeout(
                    timeout,
                    relay_public_request(listener, proxy, url, addresses, authorization),
                )
                .await;
            })),
            "configured-http-proxy",
        )
    } else {
        builder = builder.resolve_to_addrs(host, &addresses);
        (None, "direct-validated-public")
    };
    let client = match builder.build() {
        Ok(client) => client,
        Err(_) => {
            if let Some(relay) = relay_task {
                relay.abort();
            }
            return Err(failure("WEB_HTTP_FAILED", "网页客户端初始化失败。", 500));
        }
    };
    Ok(PublicClient {
        client,
        route,
        resolution,
        relay: relay_task,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn proxy_bypass_does_not_expand_private_access() {
        assert!(bypass_matches("example.org", "*.example.org"));
        assert!(bypass_matches("part.example.org", ".example.org"));
        assert!(!bypass_matches("fakeexample.org", "example.org"));
        assert!(bypass_matches("192.168.1.1", "192.168.0.0/16"));
        assert!(!bypass_matches("198.18.0.10", "192.168.0.0/16"));
        assert_eq!(
            parse_proxy("http=127.0.0.1:7890;https=127.0.0.1:7891", "https")
                .unwrap()
                .port(),
            Some(7891)
        );
        assert_eq!(
            parse_proxy("socks5://127.0.0.1:7890", "https")
                .unwrap_err()
                .code,
            "WEB_PROXY_UNSUPPORTED"
        );
    }
    #[test]
    fn public_dns_verification_rejects_private_mixed_and_invalid_answers() {
        assert_eq!(
            public_dns_addresses(&json!({
                "Status": 0,
                "Answer": [{"type": 1, "data": "93.184.216.34"}],
            }))
            .unwrap()[0]
                .ip()
                .to_string(),
            "93.184.216.34"
        );
        for address in [
            "127.0.0.1",
            "10.0.0.1",
            "192.168.1.1",
            "198.18.0.10",
            "fdfe:dcba:9876::a",
        ] {
            assert_eq!(
                public_dns_addresses(&json!({
                    "Status": 0,
                    "Answer": [
                        {"type": 1, "data": "93.184.216.34"},
                        {"type": 1, "data": address},
                    ],
                }))
                .unwrap_err()
                .code,
                "WEB_URL_BLOCKED"
            );
        }
        assert_eq!(
            public_dns_addresses(&json!({"Status": 3, "Answer": []}))
                .unwrap_err()
                .code,
            "WEB_PUBLIC_DNS_UNAVAILABLE"
        );
    }
    #[tokio::test]
    async fn proxy_tunnel_pins_address_and_preserves_host_and_response() {
        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_address = upstream.local_addr().unwrap();
        let proxy = tokio::spawn(async move {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let request = String::from_utf8(read_http_header(&mut stream).await.unwrap()).unwrap();
            assert!(request.starts_with("CONNECT 93.184.216.34:80 HTTP/1.1\r\n"));
            stream
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .unwrap();
            let request = String::from_utf8(read_http_header(&mut stream).await.unwrap()).unwrap();
            assert!(request.starts_with("GET /fixture HTTP/1.1\r\n"));
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("host: example.org\r\n")
            );
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nfixture",
                )
                .await
                .unwrap();
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let token = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode("kynxa:fixture")
        );
        let tunnel = tokio::spawn(relay_public_request(
            listener,
            Url::parse(&format!("http://{proxy_address}")).unwrap(),
            Url::parse("http://example.org/fixture").unwrap(),
            vec!["93.184.216.34:80".parse().unwrap()],
            token,
        ));
        let client = reqwest::Client::builder()
            .no_proxy()
            .proxy(
                reqwest::Proxy::all(format!("http://{address}"))
                    .unwrap()
                    .basic_auth("kynxa", "fixture"),
            )
            .build()
            .unwrap();
        assert_eq!(
            client
                .get("http://example.org/fixture")
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap(),
            "fixture"
        );
        proxy.await.unwrap();
        tunnel.await.unwrap().unwrap();
    }
    #[tokio::test]
    async fn private_url_is_blocked_before_proxy_or_dns_access() {
        for url in [
            "http://127.0.0.1/",
            "https://10.0.0.1/",
            "http://service.local/",
            "http://[::1]/",
        ] {
            assert_eq!(
                client(&Url::parse(url).unwrap(), Duration::from_millis(100))
                    .await
                    .err()
                    .unwrap()
                    .code,
                "WEB_URL_BLOCKED"
            );
        }
    }

    #[tokio::test]
    async fn relay_rejects_unexpected_target_and_stops_on_cancellation() {
        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_address = upstream.local_addr().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let relay_task = tokio::spawn(relay_public_request(
            listener,
            Url::parse(&format!("http://{upstream_address}")).unwrap(),
            Url::parse("https://example.org/").unwrap(),
            vec!["93.184.216.34:443".parse().unwrap()],
            "Basic fixture".to_owned(),
        ));
        let mut connection = TcpStream::connect(address).await.unwrap();
        connection
            .write_all(
                b"CONNECT localhost:80 HTTP/1.1\r\nProxy-Authorization: Basic fixture\r\n\r\n",
            )
            .await
            .unwrap();
        assert!(relay_task.await.unwrap().is_err());
        assert!(
            tokio::time::timeout(Duration::from_millis(20), upstream.accept())
                .await
                .is_err()
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let relay_task = tokio::spawn(async move {
            let _ = listener.accept().await;
        });
        let transport = PublicClient {
            client: reqwest::Client::new(),
            route: "fixture",
            resolution: "fixture",
            relay: Some(relay_task),
        };
        drop(transport);
        tokio::task::yield_now().await;
        assert!(TcpStream::connect(address).await.is_err());
    }

    #[tokio::test]
    async fn upstream_rejected_connect_is_not_retried_or_bypassed() {
        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_address = upstream.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let _ = read_http_header(&mut stream).await.unwrap();
            stream
                .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let relay_task = tokio::spawn(relay_public_request(
            listener,
            Url::parse(&format!("http://{upstream_address}")).unwrap(),
            Url::parse("https://example.org/").unwrap(),
            vec!["93.184.216.34:443".parse().unwrap()],
            "Basic fixture".to_owned(),
        ));
        let mut connection = TcpStream::connect(address).await.unwrap();
        connection
            .write_all(
                b"CONNECT example.org:443 HTTP/1.1\r\nProxy-Authorization: Basic fixture\r\n\r\n",
            )
            .await
            .unwrap();
        let response = read_http_header(&mut connection).await.unwrap();
        assert!(response.starts_with(b"HTTP/1.1 502"));
        assert!(relay_task.await.unwrap().is_err());
        upstream_task.await.unwrap();
    }
}

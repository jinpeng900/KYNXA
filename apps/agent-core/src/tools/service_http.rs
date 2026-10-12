use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use serde_json::{Value, json};
use url::Url;

use super::intent::{matches, utf16_length};
use super::service_error::{ServiceError, ServiceResult};
use crate::models::protocol::{array, string, truthy};

const RESPONSE_BYTES: u64 = 2 * 1024 * 1024;
fn blocked() -> ServiceError {
    ServiceError::new(
        "WEB_URL_BLOCKED",
        "网页工具仅支持公共 HTTP(S) 地址，不能访问本机、内网或带凭据的地址。",
        403,
    )
}
fn unsupported() -> ServiceError {
    ServiceError::new(
        "WEB_UNSUPPORTED_CONTENT",
        "网页响应类型或压缩方式不受支持。",
        415,
    )
}
fn too_large() -> ServiceError {
    ServiceError::new("WEB_RESPONSE_TOO_LARGE", "网页响应超过 2 MiB 限制。", 413)
}

pub fn public_address(address: &str) -> bool {
    match address.parse::<IpAddr>() {
        Ok(IpAddr::V4(address)) => {
            let number = u32::from(address) as u64;
            ![
                ("0.0.0.0", 8),
                ("10.0.0.0", 8),
                ("100.64.0.0", 10),
                ("127.0.0.0", 8),
                ("169.254.0.0", 16),
                ("172.16.0.0", 12),
                ("192.0.0.0", 24),
                ("192.0.2.0", 24),
                ("192.88.99.0", 24),
                ("192.168.0.0", 16),
                ("198.18.0.0", 15),
                ("198.51.100.0", 24),
                ("203.0.113.0", 24),
                ("224.0.0.0", 4),
                ("240.0.0.0", 4),
            ]
            .iter()
            .any(|(base, prefix)| {
                let base = u32::from(base.parse::<Ipv4Addr>().unwrap()) as u64;
                number >= base && number < base + (1_u64 << (32 - prefix))
            })
        }
        Ok(IpAddr::V6(address_value)) if !address.contains('.') => {
            let value = u128::from(address_value);
            let prefix = |base: &str, length: u32| {
                value >> (128 - length)
                    == u128::from(base.parse::<Ipv6Addr>().unwrap()) >> (128 - length)
            };
            prefix("2000::", 3)
                && ![
                    ("2001::", 23),
                    ("2001:db8::", 32),
                    ("2002::", 16),
                    ("3fff::", 20),
                ]
                .iter()
                .any(|(base, length)| prefix(base, *length))
        }
        _ => false,
    }
}

fn family(address: &str) -> u8 {
    match address.parse::<IpAddr>() {
        Ok(IpAddr::V4(_)) => 4,
        Ok(IpAddr::V6(_)) => 6,
        _ => 0,
    }
}

pub fn public_url(value: &Value) -> ServiceResult<Url> {
    let value = value
        .as_str()
        .filter(|value| {
            !value.is_empty()
                && utf16_length(value) <= 8192
                && !matches(r"[\x00-\x20\x7f\\]|(?i)%(?:0[0-9a-f]|1[0-9a-f]|7f)", value)
                && matches(r"(?i)^https?://", value)
        })
        .ok_or_else(blocked)?;
    let mut url = Url::parse(value).map_err(|_| blocked())?;
    let authority = value
        .split_once("://")
        .map(|(_, suffix)| suffix.split(['/', '?', '#']).next().unwrap_or(""))
        .unwrap_or("");
    let hostname = url
        .host_str()
        .unwrap_or("")
        .trim_matches(['[', ']'])
        .trim_end_matches('.');
    if hostname.is_empty()
        || authority.contains('@')
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some_and(|port| ![80, 443].contains(&port))
        || matches(
            r"(?i)(?:^|\.)(?:localhost|local|internal|home|lan|onion|arpa)$",
            hostname,
        )
        || family(hostname) != 0 && !public_address(hostname)
    {
        return Err(blocked());
    }
    url.set_fragment(None);
    Ok(url)
}

fn validated_addresses(arguments: &Value) -> ServiceResult<Value> {
    let addresses = &arguments["addresses"];
    let values = array(addresses);
    let valid = addresses.is_array()
        && !values.is_empty()
        && values.len() <= 64
        && values.iter().all(|value| {
            public_address(string(&value["address"]))
                && value["family"] == family(string(&value["address"]))
        });
    if !valid {
        let proxy = |value: &Value| {
            value["family"] == 4
                && family(string(&value["address"])) == 4
                && matches(r"^198\.(?:18|19)\.", string(&value["address"]))
        };
        let hint = addresses.is_array()
            && !values.is_empty()
            && values.len() <= 64
            && values.iter().any(proxy)
            && values.iter().all(|value| {
                value["family"] == family(string(&value["address"]))
                    && (public_address(string(&value["address"])) || proxy(value))
            });
        return Err(ServiceError::new("WEB_URL_BLOCKED","当前 DNS 答案包含无效或非公共地址，可能来自代理的虚拟 IP；本次 HTTP 读取已拒绝，不会自动打开本机浏览器。",403)
            .with_details(json!({"webFailureReason":"dns-non-public","allowBackgroundReadHint":hint})));
    }
    Ok(json!(
        values
            .iter()
            .map(|value| json!({"address":value["address"],"family":value["family"]}))
            .collect::<Vec<_>>()
    ))
}

#[derive(Default)]
pub struct HttpState {
    redirects: Vec<String>,
    raw_bytes: u64,
    decoded_bytes: u64,
}

impl HttpState {
    pub fn apply(&mut self, operation: &str, arguments: &Value) -> ServiceResult<Value> {
        match operation {
            "http.url" => Ok(json!(public_url(&arguments["url"])?.to_string())),
            "http.init" => {
                let timeout = arguments["timeoutMs"]
                    .as_u64()
                    .filter(|timeout| (1..=120000).contains(timeout))
                    .ok_or_else(|| {
                        ServiceError::new(
                            "INVALID_TOOL_ARGUMENTS",
                            "网页超时须在 1–120000 毫秒之间。",
                            400,
                        )
                    })?;
                *self = Self::default();
                Ok(json!(timeout))
            }
            "http.host" => {
                let url = public_url(&arguments["url"])?;
                let hostname = url.host_str().unwrap_or("").trim_matches(['[', ']']);
                Ok(json!({"hostname":hostname,"family":family(hostname)}))
            }
            "http.addresses" => validated_addresses(arguments),
            "http.pin" => {
                let choices = array(&arguments["addresses"])
                    .iter()
                    .filter(|address| {
                        !truthy(&arguments["family"]) || address["family"] == arguments["family"]
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                if choices.is_empty() {
                    return Err(blocked());
                }
                Ok(if truthy(&arguments["all"]) {
                    json!({"all":true,"addresses":choices})
                } else {
                    json!({"all":false,"address":choices[0]["address"],"family":choices[0]["family"]})
                })
            }
            "http.response" => {
                let status = arguments["status"].as_i64().unwrap_or(0);
                let http_error = |message: &str| {
                    ServiceError::new("WEB_HTTP_ERROR", message, 502)
                        .with_details(json!({"status":status,"httpStatus":status}))
                };
                if [301, 302, 303, 307, 308].contains(&status) {
                    let location = arguments["headers"]["location"]
                        .as_str()
                        .ok_or_else(|| http_error("公共网页重定向未完成。"))?;
                    if self.redirects.len() >= 5 {
                        return Err(http_error("公共网页重定向未完成。"));
                    }
                    if matches(r"[\x00-\x20\x7f\\]", location) {
                        return Err(blocked());
                    }
                    let target = public_url(&arguments["url"])?
                        .join(location)
                        .map_err(|_| blocked())?;
                    let target = public_url(&json!(target.to_string()))?;
                    return Ok(json!({"kind":"redirect","url":target.to_string()}));
                }
                if !(200..300).contains(&status) {
                    return Err(http_error(&format!("公共网页返回 HTTP {status}。")));
                }
                let content_type = arguments["headers"]["content-type"]
                    .as_str()
                    .filter(|value| utf16_length(value) <= 1024)
                    .ok_or_else(unsupported)?;
                let encoding = arguments["headers"]
                    .get("content-encoding")
                    .filter(|value| !value.is_null())
                    .map(|value| value.as_str().ok_or_else(unsupported))
                    .transpose()?
                    .unwrap_or("identity")
                    .trim()
                    .to_lowercase();
                let media = content_type
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_lowercase();
                if !matches(
                    r"(?i)^text/[a-z0-9!#$&^_.+-]+$|^application/(?:json|xml|[a-z0-9!#$&^_.+-]+\+(?:json|xml))$",
                    &media,
                ) || !["identity", "gzip", "deflate", "br"].contains(&encoding.as_str())
                {
                    return Err(unsupported());
                }
                if arguments["headers"]["content-length"]
                    .as_str()
                    .is_some_and(|value| {
                        matches(r"^\d+$", value)
                            && value
                                .parse::<u64>()
                                .map(|length| length > RESPONSE_BYTES)
                                .unwrap_or(true)
                    })
                {
                    return Err(too_large());
                }
                self.raw_bytes = 0;
                self.decoded_bytes = 0;
                Ok(
                    json!({"kind":"content","headers":{"contentType":content_type,"contentEncoding":encoding}}),
                )
            }
            "http.redirectCommitted" => {
                self.redirects.push(string(&arguments["url"]).to_owned());
                Ok(Value::Null)
            }
            "http.redirects" => Ok(json!(self.redirects)),
            "http.count" => {
                let count = arguments["bytes"].as_u64().unwrap_or(u64::MAX);
                let total = if arguments["decoded"] == true {
                    &mut self.decoded_bytes
                } else {
                    &mut self.raw_bytes
                };
                *total = total.saturating_add(count);
                if *total > RESPONSE_BYTES {
                    return Err(too_large());
                }
                Ok(Value::Null)
            }
            _ => Err(ServiceError::new(
                "AGENT_CORE_UNKNOWN_OPERATION",
                "Unknown HTTP policy operation.",
                400,
            )),
        }
    }
}

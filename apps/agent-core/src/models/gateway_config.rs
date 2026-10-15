use std::{
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
    time::Duration,
};

use futures_util::StreamExt;
use reqwest::Method;
use serde_json::{Value, json};
use tokio::sync::{Mutex, watch};

use super::{
    capabilities, connections,
    discovery::ModelDiscoveryState,
    gateway::{self, ModelError},
    gateway_protocol,
};

const MAXIMUM_CONFIGURATION_BYTES: u64 = 32 * 1024 * 1024;
static SAVE_GATE: OnceLock<Arc<Mutex<()>>> = OnceLock::new();
static LOCAL_OBSERVER: OnceLock<
    std::sync::Mutex<crate::resources::external::ExternalObserverState>,
> = OnceLock::new();

fn observer_state(operation: &str, args: &Value) -> Result<Value, ModelError> {
    let state = LOCAL_OBSERVER.get_or_init(|| {
        let mut state = crate::resources::external::ExternalObserverState::default();
        let _ = state.apply("resources.external.init", &json!({"cacheTtlMs":2000}));
        std::sync::Mutex::new(state)
    });
    state
        .lock()
        .map_err(|_| {
            ModelError::new(
                "LOCAL_MODEL_OBSERVER_LOCK_FAILED",
                "本地模型观测状态不可用。",
                500,
            )
        })?
        .apply(operation, args)
        .map_err(ModelError::native)
}

fn observation_target(connection: &Value, model: &str) -> Result<(Value, String), ModelError> {
    use sha2::{Digest, Sha256};
    let plan=crate::resources::calculate("resources.externalEndpoint",&json!({"baseUrl":connection["baseUrl"],"providerId":connection["providerId"],"protocol":connection["protocol"],"model":model})).map_err(ModelError::native)?;
    let origin = endpoint_fields(connection)["origin"]
        .as_str()
        .unwrap_or("")
        .to_owned();
    let key = format!(
        "{:x}",
        Sha256::digest(format!("{origin}\0{model}").as_bytes())
    );
    Ok((plan, key))
}

pub struct LocalGeneration {
    key: Option<String>,
}
impl Drop for LocalGeneration {
    fn drop(&mut self) {
        if let Some(key) = self.key.take() {
            let _ = observer_state("resources.external.end", &json!({"key":key}));
        }
    }
}
pub fn begin_local_generation(
    connection: &Value,
    model: &str,
) -> Result<LocalGeneration, ModelError> {
    let (target, key) = observation_target(connection, model)?;
    if target["local"] != true {
        return Ok(LocalGeneration { key: None });
    }
    let begun = observer_state("resources.external.begin", &json!({"key":key}))?;
    Ok(LocalGeneration {
        key: (begun == true).then_some(key),
    })
}

async fn observation_read(
    connection: &Value,
    url: url::Url,
    body: Option<Value>,
    mut cancellation: watch::Receiver<bool>,
) -> Result<Value, ModelError> {
    let client = gateway::client()?;
    tokio::time::timeout(Duration::from_millis(1200),async {
        let method=if body.is_some(){Method::POST}else{Method::GET};
        let mut request=gateway::authenticated(&client,connection,method,url.as_str());
        if let Some(body)=body{request=request.json(&body);}
        let response=tokio::select!{_ = gateway::cancelled(&mut cancellation)=>return Err(ModelError::cancelled()),response=request.send()=>response.map_err(|_|ModelError::new("LOCAL_MODEL_OBSERVATION_UNAVAILABLE","本地模型观测接口不可用。",502))?};
        if !response.status().is_success(){return Err(ModelError::new("LOCAL_MODEL_OBSERVATION_HTTP","本地模型观测接口未成功响应。",502));}
        gateway::bounded_json(response,1024*1024,&mut cancellation).await
    }).await.unwrap_or_else(|_|Err(ModelError::new("LOCAL_MODEL_OBSERVATION_TIMEOUT","本地模型观测超时。",504)))
}

/// Read only bounded localhost telemetry; all trust, resource interpretation and cache decisions stay native.
/// 只读取有界 localhost 遥测；信任、资源解释及缓存决策均沿原有原生规则。
pub async fn observe_local(
    connection: &Value,
    model: &str,
    context_tokens: Option<u64>,
    cancellation: watch::Receiver<bool>,
) -> Result<Value, ModelError> {
    observe_local_with_refresh(connection, model, context_tokens, cancellation, false).await
}

/// Preparation completion requires a new server observation, not the pre-load cache entry.
/// 准备完成必须重新观察服务，不能用预载前的缓存冒充模型已经驻留。
pub async fn observe_local_fresh(
    connection: &Value,
    model: &str,
    context_tokens: Option<u64>,
    cancellation: watch::Receiver<bool>,
) -> Result<Value, ModelError> {
    observe_local_with_refresh(connection, model, context_tokens, cancellation, true).await
}

async fn observe_local_with_refresh(
    connection: &Value,
    model: &str,
    context_tokens: Option<u64>,
    mut cancellation: watch::Receiver<bool>,
    refresh: bool,
) -> Result<Value, ModelError> {
    if *cancellation.borrow() {
        return Err(ModelError::cancelled());
    }
    let (target, key) = observation_target(connection, model)?;
    let mut unknown = crate::resources::calculate("resources.externalUnknown", &json!({}))
        .map_err(ModelError::native)?;
    if target["local"] != true {
        unknown["diagnostic"] = json!({"code":"LOCAL_MODEL_OBSERVATION_NOT_LOCAL"});
        return Ok(unknown);
    }
    if target["model"].is_null() {
        unknown["diagnostic"] = json!({"code":"LOCAL_MODEL_OBSERVATION_MODEL_UNSPECIFIED"});
        return Ok(unknown);
    }
    let activity = observer_state("resources.external.activity", &json!({"key":key}))?;
    let cache_key = format!(
        "{key}:{}",
        context_tokens
            .map(|n| n.to_string())
            .unwrap_or_else(|| "unknown".into())
    );
    let cached = observer_state(
        "resources.external.get",
        &json!({"cacheKey":cache_key,"now":chrono::Utc::now().timestamp_millis(),"refresh":refresh}),
    )?;
    if !refresh && cached.is_object() {
        let mut snapshot = cached;
        snapshot
            .as_object_mut()
            .unwrap()
            .extend(activity.as_object().cloned().unwrap_or_default());
        snapshot["cached"] = json!(true);
        return Ok(snapshot);
    }
    let base = url::Url::parse(connection["baseUrl"].as_str().unwrap_or(""))
        .map_err(|_| ModelError::new("INVALID_MODEL_CONNECTION", "本地模型地址无效。", 400))?;
    let mut snapshot = if target["ollama"] == true {
        let running_url = base
            .join("/api/ps")
            .map_err(|_| ModelError::new("INVALID_MODEL_CONNECTION", "本地模型地址无效。", 400))?;
        let show_url = base
            .join("/api/show")
            .map_err(|_| ModelError::new("INVALID_MODEL_CONNECTION", "本地模型地址无效。", 400))?;
        let (running, show) = tokio::join!(
            observation_read(connection, running_url, None, cancellation.clone()),
            observation_read(
                connection,
                show_url,
                Some(json!({"model":model,"verbose":false})),
                cancellation.clone()
            )
        );
        if *cancellation.borrow() {
            return Err(ModelError::cancelled());
        }
        let mut tags = Value::Null;
        if crate::resources::calculate(
            "resources.externalNeedsTags",
            &json!({"running":running.as_ref().ok(),"model":model}),
        )
        .map_err(ModelError::native)?
            == true
        {
            let tags_url = base.join("/api/tags").map_err(|_| {
                ModelError::new("INVALID_MODEL_CONNECTION", "本地模型地址无效。", 400)
            })?;
            tags = observation_read(connection, tags_url, None, cancellation.clone())
                .await
                .unwrap_or(Value::Null);
        }
        crate::resources::calculate("resources.externalOllama",&json!({"model":model,"contextTokens":context_tokens,"running":running.as_ref().ok(),"show":show.as_ref().ok(),"tags":tags,
            "runningRejected":running.is_err(),"detailsRejected":show.is_err(),"runningErrorCode":running.as_ref().err().map(|e|&e.code),"detailsErrorCode":show.as_ref().err().map(|e|&e.code),"now":chrono::Utc::now().timestamp_millis()})).map_err(ModelError::native)?
    } else {
        let mut props_url = base
            .join("/props")
            .map_err(|_| ModelError::new("INVALID_MODEL_CONNECTION", "本地模型地址无效。", 400))?;
        props_url
            .query_pairs_mut()
            .append_pair("model", model)
            .append_pair("autoload", "false");
        let props = observation_read(connection, props_url, None, cancellation.clone())
            .await
            .unwrap_or(Value::Null);
        let observed=crate::resources::calculate("resources.externalLlama",&json!({"props":props,"origin":base.origin().ascii_serialization(),"now":chrono::Utc::now().timestamp_millis()})).map_err(ModelError::native)?;
        if observed.is_null() {
            unknown["diagnostic"] = json!({"code":"LOCAL_MODEL_RESOURCE_API_UNAVAILABLE"});
            unknown
        } else {
            observed
        }
    };
    if *cancellation.borrow_and_update() || cancellation.has_changed().is_err() {
        return Err(ModelError::cancelled());
    }
    snapshot
        .as_object_mut()
        .unwrap()
        .extend(activity.as_object().cloned().unwrap_or_default());
    observer_state(
        "resources.external.store",
        &json!({"cacheKey":cache_key,"snapshot":snapshot}),
    )?;
    snapshot["cached"] = json!(false);
    Ok(snapshot)
}

pub fn endpoint_fields(connection: &Value) -> Value {
    let Ok(url) = url::Url::parse(connection["baseUrl"].as_str().unwrap_or("")) else {
        return Value::Null;
    };
    json!({"protocol":format!("{}:",url.scheme()),"hostname":url.host_str().unwrap_or(""),
        "port":url.port().map(|p|p.to_string()).unwrap_or_default(),"username":url.username(),"password":url.password().unwrap_or(""),
        "search":url.query().map(|q|format!("?{q}")).unwrap_or_default(),"hash":url.fragment().map(|q|format!("#{q}")).unwrap_or_default(),
        "pathname":url.path(),"origin":url.origin().ascii_serialization()})
}
fn connection_contract(operation: &str, args: Value) -> Result<Value, ModelError> {
    let envelope = connections::calculate(operation, &args).map_err(|code| {
        if code == "INVALID_OUTPUT_BUDGET" {
            ModelError::new(&code, "模型输出额度无效。", 400)
        } else {
            ModelError::native(code)
        }
    })?;
    let value = if envelope.get("error").is_some() {
        envelope
    } else {
        envelope["value"].clone()
    };
    if value.get("error").is_some() {
        return Err(ModelError::new(
            "INVALID_MODEL_CONNECTION",
            value["error"]["message"]
                .as_str()
                .unwrap_or("模型连接配置无效。"),
            400,
        ));
    }
    Ok(value)
}

/// Read and atomically replace the same configured connections document through formal storage services.
/// 经正式存储服务读取并原子替换同一配置连接文档，不再通过 JavaScript 保存密钥或配置。
pub struct ModelConfigStore {
    home: PathBuf,
}
impl ModelConfigStore {
    pub fn new(data_home: &Path) -> Self {
        Self {
            home: data_home.to_path_buf(),
        }
    }
    pub async fn document(&self) -> Result<Value, ModelError> {
        let home = self.home.clone();
        let value = tokio::task::spawn_blocking(move || {
            let bytes = crate::storage::managed_files::read(
                &home,
                &home.join("connections.json"),
                MAXIMUM_CONFIGURATION_BYTES,
            )
            .map_err(|_| {
                ModelError::new(
                    "MODEL_CONFIGURATION_READ_FAILED",
                    "无法读取模型配置；原文件已保留。",
                    500,
                )
            })?;
            match bytes {
                Some(bytes) => serde_json::from_slice(&bytes).map_err(|_| {
                    ModelError::new(
                        "CORRUPT_MODEL_CONFIGURATION",
                        "模型配置 JSON 无效；原文件已保留。",
                        500,
                    )
                }),
                None => Ok(json!({"version":1,"providers":[]})),
            }
        })
        .await
        .map_err(|_| {
            ModelError::new(
                "MODEL_CONFIGURATION_READ_FAILED",
                "模型配置读取任务未完成。",
                500,
            )
        })??;
        connection_contract("models.connections.document", json!({"document":value}))
    }
    pub async fn list(&self) -> Result<Value, ModelError> {
        connection_contract(
            "models.connections.list",
            json!({"document":self.document().await?}),
        )
    }
    pub async fn connection_for(&self, provider_id: &str) -> Result<Option<Value>, ModelError> {
        let result = connection_contract(
            "models.connections.find",
            json!({"document":self.document().await?,"providerId":provider_id}),
        )?;
        Ok((result["found"] == true).then(|| result["value"].clone()))
    }
    pub async fn saved_key(
        &self,
        provider_id: &str,
        base_url: &str,
    ) -> Result<Option<String>, ModelError> {
        let result = connection_contract(
            "models.connections.savedKey",
            json!({"document":self.document().await?,"providerId":provider_id,"baseUrl":base_url}),
        )?;
        Ok(result["value"]
            .as_str()
            .filter(|_| result["found"] == true)
            .map(str::to_owned))
    }
    pub async fn save(&self, input: Value) -> Result<Value, ModelError> {
        let _save = SAVE_GATE
            .get_or_init(|| Arc::new(Mutex::new(())))
            .lock()
            .await;
        let connection = validate_connection(input, true)?;
        let endpoint = endpoint_fields(&connection);
        let plan = connection_contract(
            "models.connections.savePlan",
            json!({"document":self.document().await?,"connection":connection,
            "localEndpoint":connections::local_host(endpoint["hostname"].as_str().unwrap_or(""))}),
        )?;
        let bytes = serde_json::to_vec_pretty(&plan["document"]).map_err(|_| {
            ModelError::new(
                "MODEL_CONFIGURATION_WRITE_FAILED",
                "模型配置无法编码。",
                500,
            )
        })?;
        let home = self.home.clone();
        tokio::task::spawn_blocking(move || {
            crate::storage::managed_files::atomic_write(
                &home,
                &home.join("connections.json"),
                &bytes,
                MAXIMUM_CONFIGURATION_BYTES,
            )
        })
        .await
        .map_err(|_| {
            ModelError::new(
                "MODEL_CONFIGURATION_WRITE_FAILED",
                "模型配置保存任务未完成。",
                500,
            )
        })?
        .map_err(|_| {
            ModelError::new(
                "MODEL_CONFIGURATION_WRITE_FAILED",
                "模型配置保存失败；原记录已保留。",
                500,
            )
        })?;
        Ok(plan["provider"].clone())
    }
}

pub fn validate_connection(mut input: Value, require_models: bool) -> Result<Value, ModelError> {
    if !input.is_object() {
        return Err(ModelError::new(
            "INVALID_MODEL_CONNECTION",
            "连接信息不能为空。",
            400,
        ));
    }
    for key in ["providerId", "displayName", "baseUrl"] {
        let text = input[key].as_str().unwrap_or("").trim();
        input[key] = json!(if key == "baseUrl" {
            text.trim_end_matches('/')
        } else {
            text
        });
    }
    if input["protocol"].is_null() {
        input["protocol"] = json!("openai-completions");
    }
    if let Some(models) = input["models"].as_array_mut() {
        for model in models {
            if let Some(text) = model.as_str() {
                *model = json!(text.trim());
            }
        }
    } else {
        input["models"] = json!([]);
    }
    connection_contract(
        "models.connections.validate",
        json!({"endpoint":endpoint_fields(&input),"input":input,"requireModels":require_models}),
    )
}

/// Consume a small unsupported-discovery response so the shared transport can reuse its connection.
/// 有界消费不支持目录发现的响应，复用已建立连接；不解析错误正文、不发生成请求。
async fn drain_discovery_response(
    response: reqwest::Response,
    cancellation: &mut watch::Receiver<bool>,
) -> Result<bool, ModelError> {
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut stream = response.bytes_stream();
        let mut consumed_bytes = 0usize;
        loop {
            let chunk = tokio::select! {
                biased;
                _ = gateway::cancelled(cancellation) => return Err(ModelError::cancelled()),
                chunk = stream.next() => chunk,
            };
            let Some(chunk) = chunk else { return Ok(true); };
            let Ok(chunk) = chunk else { return Ok(false); };
            consumed_bytes = consumed_bytes.saturating_add(chunk.len());
            if consumed_bytes > 64 * 1024 {
                return Ok(false);
            }
        }
    }).await.unwrap_or(Ok(false))
}

pub async fn discover_models(
    connection: &Value,
    mut cancellation: watch::Receiver<bool>,
) -> Result<Value, ModelError> {
    let client = gateway::client()?;
    let ollama = gateway_protocol::protocol(connection) == "ollama";
    let mut url = if ollama {
        url::Url::parse(connection["baseUrl"].as_str().unwrap_or(""))
            .ok()
            .and_then(|url| url.join("/api/tags").ok())
    } else {
        url::Url::parse(&format!(
            "{}/models",
            connection["baseUrl"]
                .as_str()
                .unwrap_or("")
                .trim_end_matches('/')
        ))
        .ok()
    }
    .ok_or_else(|| ModelError::new("INVALID_MODEL_CONNECTION", "模型列表地址无效。", 400))?;
    let mut discovery = ModelDiscoveryState::default();
    let policy = discovery
        .apply(
            "models.lifecycle.discovery.init",
            &json!({"protocol":connection["protocol"]}),
        )
        .map_err(ModelError::native)?;
    if policy["anthropic"] == true {
        url.query_pairs_mut().append_pair("limit", "1000");
    }
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let response = tokio::select! { _ = gateway::cancelled(&mut cancellation) => return Err(ModelError::cancelled()),
                response = gateway::authenticated(&client, connection, Method::GET, url.as_str()).send() =>
                    response.map_err(|_| ModelError::new("MODEL_DISCOVERY_NETWORK_FAILED", "无法连接模型列表接口。", 502))? };
            if !response.status().is_success() {
                let mut error=ModelError::new("MODEL_DISCOVERY_HTTP_ERROR", "模型列表接口未成功返回。", 502);
                let status = response.status().as_u16();
                error.details["upstreamStatus"]=json!(status);
                if matches!(status, 404 | 405) {
                    error.details["connectionWarmed"] =
                        json!(drain_discovery_response(response, &mut cancellation).await?);
                }
                return Err(error);
            }
            let value = gateway::bounded_json(response, policy["maximumPageBytes"].as_u64().unwrap_or(8*1024*1024) as usize, &mut cancellation).await?;
            let result = discovery.apply("models.lifecycle.discovery.page", &json!({"result":value})).map_err(ModelError::native)?;
            if let Some(message) = result["error"].as_str() { return Err(ModelError::new("MODEL_DISCOVERY_INVALID", message, 502)); }
            if result["done"] == true { return Ok(result["models"].clone()); }
            url.query_pairs_mut().clear().append_pair("limit", "1000").append_pair("after_id", result["cursor"].as_str().unwrap_or(""));
        }
    }).await.unwrap_or_else(|_| Err(ModelError::new("MODEL_DISCOVERY_TIMEOUT", "模型列表读取超时。", 504)))
}

pub fn automatic_context(
    connection: &Value,
    model: &str,
    local_model: &Value,
) -> Result<Value, ModelError> {
    let endpoint = endpoint_fields(connection);
    let known = capabilities::calculate(
        "models.capabilities.lookup",
        &json!({"endpoint":endpoint,"model":model}),
    )
    .map_err(ModelError::native)?;
    capabilities::calculate(
        "models.capabilities.automaticContext",
        &json!({"connection":connection,"endpoint":endpoint,
        "localModel":local_model,"capabilities":known}),
    )
    .map_err(ModelError::native)
}

pub async fn handle_models(
    method: &str,
    path: &str,
    body: Value,
    data_home: &Path,
) -> Option<Result<(u16, Value), ModelError>> {
    let store = ModelConfigStore::new(data_home);
    match (method, path) {
        ("GET", "/api/models") => Some(
            store
                .list()
                .await
                .map(|providers| (200, json!({"providers":providers}))),
        ),
        ("POST", "/api/models") => Some(
            store
                .save(body)
                .await
                .map(|provider| (200, json!({"provider":provider}))),
        ),
        ("POST", "/api/models/test") => Some(
            async {
                let mut connection = validate_connection(body, false)?;
                if connection["apiKey"].as_str().unwrap_or("").is_empty() {
                    if let Some(key) = store
                        .saved_key(
                            connection["providerId"].as_str().unwrap_or(""),
                            connection["baseUrl"].as_str().unwrap_or(""),
                        )
                        .await?
                    {
                        connection["apiKey"] = json!(key);
                    }
                }
                let (_sender, cancellation) = watch::channel(false);
                let started = std::time::Instant::now();
                let models = discover_models(&connection, cancellation).await?;
                Ok((
                    200,
                    json!({"ok":true,"latencyMs":started.elapsed().as_millis(),"models":models}),
                ))
            }
            .await,
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validation_keeps_native_errors_and_supports_ollama_explicitly() {
        let input = json!({"providerId":"fixture","displayName":"Temporary","baseUrl":"http://127.0.0.1:18080","protocol":"ollama","models":["fixture-model"]});
        assert_eq!(
            validate_connection(input.clone(), true).unwrap()["protocol"],
            "ollama"
        );
        let mut invalid = input.clone();
        invalid["baseUrl"] = json!("file:///private");
        assert_eq!(
            validate_connection(invalid, true).unwrap_err().code,
            "INVALID_MODEL_CONNECTION"
        );
        let mut invalid = input;
        invalid["models"] = json!([]);
        assert!(validate_connection(invalid, true).is_err());
    }
    #[tokio::test]
    async fn legacy_public_model_limits_keep_desktop_contract_and_original_config() {
        let root = std::env::temp_dir().join(format!(
            "kynxa-legacy-model-contract-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&root).unwrap();
        let providers = json!([
            {"providerId":"fixture-missing","displayName":"Missing legacy window","baseUrl":"http://127.0.0.1:65501/v1","protocol":"openai-completions","models":["fixture-qwen"],"apiKey":"synthetic-private-key"},
            {"providerId":"fixture-null","displayName":"Null legacy window","baseUrl":"http://127.0.0.1:65501/v1","protocol":"openai-completions","models":["fixture-qwen"],"contextWindowTokens":null,"maxOutputTokens":null},
            {"providerId":"fixture-explicit","displayName":"Explicit legacy window","baseUrl":"http://127.0.0.1:65501/v1","protocol":"openai-completions","models":["fixture-qwen","fixture-qwen-two"],"contextWindowTokens":1000000,"maxOutputTokens":2048}
        ]);
        let original = serde_json::to_vec(&json!({"version":1,"providers":providers})).unwrap();
        let path = root.join("connections.json");
        std::fs::write(&path, &original).unwrap();
        // Exercise the actual public handler, not a manually reconstructed desktop fixture.
        // 调用真实公共处理器，避免手写桌面样例掩盖原生列表投影缺陷。
        let (status, response) = handle_models("GET", "/api/models", Value::Null, &root)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(status, 200);
        let listed = response["providers"].as_array().unwrap();
        assert_eq!(listed.len(), 3);
        for provider in &listed[..2] {
            assert_eq!(provider["contextWindowTokens"].as_u64(), Some(32768));
            assert_eq!(provider["maxOutputTokens"].as_u64(), Some(262144));
        }
        assert_eq!(listed[2]["contextWindowTokens"].as_u64(), Some(1000000));
        assert_eq!(listed[2]["maxOutputTokens"].as_u64(), Some(2048));
        assert!(
            listed
                .iter()
                .all(|provider| provider["contextWindowMode"] == "automatic"
                    && provider.get("apiKey").is_none())
        );
        assert_eq!(listed[0]["hasApiKey"], true);
        assert!(!response.to_string().contains("synthetic-private-key"));
        let store = ModelConfigStore::new(&root);
        let private = store
            .connection_for("fixture-missing")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(private["apiKey"], "synthetic-private-key");
        let explicit = store
            .connection_for("fixture-explicit")
            .await
            .unwrap()
            .unwrap();
        let capacity = automatic_context(&explicit, "fixture-qwen", &Value::Null).unwrap();
        assert_eq!(capacity["mode"], "automatic");
        assert_eq!(capacity["verified"], false);
        assert!(private.get("contextWindowTokens").is_none());
        assert!(
            store.connection_for("fixture-null").await.unwrap().unwrap()["contextWindowTokens"]
                .is_null()
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
        if let Some(fixture) = std::env::var_os("KYNXA_NATIVE_MODELS_FIXTURE") {
            let fixture = PathBuf::from(fixture);
            std::fs::create_dir_all(fixture.parent().unwrap()).unwrap();
            std::fs::write(fixture, serde_json::to_vec(&response).unwrap()).unwrap();
        }
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn temporary_config_roundtrip_never_returns_api_keys_in_public_list() {
        let root =
            std::env::temp_dir().join(format!("kynxa-model-config-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let store = ModelConfigStore::new(&root);
        store.save(json!({"providerId":"fixture","displayName":"Temporary","baseUrl":"https://api.example.invalid/v1","protocol":"openai-completions","models":["temporary"],"apiKey":"fixture-secret"})).await.unwrap();
        assert!(
            !store
                .list()
                .await
                .unwrap()
                .to_string()
                .contains("fixture-secret")
        );
        assert_eq!(
            store.connection_for("fixture").await.unwrap().unwrap()["apiKey"],
            "fixture-secret"
        );
        assert_eq!(
            store
                .saved_key("fixture", "https://api.example.invalid/v1")
                .await
                .unwrap(),
            Some("fixture-secret".into())
        );
        assert_eq!(
            store
                .saved_key("fixture", "https://other.example.invalid/v1")
                .await
                .unwrap(),
            None
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

//! Optional E5 capability ranking shares native inference and hardware admission, never tool execution.
//! 可选E5能力排序复用原生推理与硬件准入，不连接MCP或执行工具。

use std::sync::{Arc, Mutex, OnceLock, atomic::{AtomicBool, AtomicU64, Ordering}};
use std::time::Duration;

use serde_json::{Value, json};
use crate::{gateway_contract::GatewayConfig, models::gateway_inference::EmbedKind,
    resources::gateway::{self as hardware, ResourceLease}, retrieval::{gateway as retrieval, gateway_embedding}};
use super::gateway::GatewayToolContext;

// A shared optional metadata cache limit; exhaustion leaves every lexical capability available.
// 仅限制进程共享的可重建元信息缓存；额度不足仍保留全部词法能力，不限制模型任务。
const MAXIMUM_METADATA_BYTES: usize = 16 * 1024 * 1024;
const HOT_QUERY_WAIT: Duration = Duration::from_millis(60);

struct CatalogVectors {
    signature: String,
    indexes: Vec<usize>,
    vectors: Vec<Vec<f32>>,
    space: String,
    dimensions: usize,
    _memory: ResourceLease,
}

#[derive(Default)]
pub(crate) struct SemanticRuntime {
    config: OnceLock<GatewayConfig>,
    vectors: Mutex<Option<Arc<CatalogVectors>>>,
    prepare: tokio::sync::Mutex<()>,
    epoch: AtomicU64,
    prewarming: AtomicBool,
}

impl SemanticRuntime {
    pub(crate) fn install(&self, config: &GatewayConfig) { let _ = self.config.set(config.clone()); }
    pub(crate) fn clear(&self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
        if let Ok(mut vectors) = self.vectors.lock() { *vectors = None; }
    }
}

fn trusted_descriptors(context: &GatewayToolContext, descriptors: &[Value]) -> Vec<(usize,String)> {
    let servers = super::gateway_mcp::servers(context);
    descriptors.iter().enumerate().filter_map(|(index, tool)| {
        let name = tool["name"].as_str()?;
        if name.starts_with("mcp.") && !super::gateway_mcp::builtin_task(name) {
            let server = servers.iter().find(|server| {
                let group = format!("mcp.{}",server["id"].as_str().unwrap_or(""));
                name == group || name.starts_with(&(group + "."))
            })?;
            if server["trusted"] != true || server["enabled"] == false { return None; }
            if name != format!("mcp.{}",server["id"].as_str().unwrap_or(""))
                && super::gateway_mcp::authorized_server(name,context).is_err() { return None; }
        }
        Some((index,super::tool_semantic::descriptor_text(tool)?))
    }).collect()
}

fn signature(context: &GatewayToolContext, cards: &[(usize,String)], epoch: u64) -> String {
    super::local_files::sha256(&serde_json::to_vec(&json!({"root":context.data_home,
        "extensions":context.extension_root,"epoch":epoch,"cards":cards})).expect("tool metadata"))
}

struct Cancellation {
    signal: Arc<AtomicBool>,
    monitor: tokio::task::JoinHandle<()>,
}
impl Cancellation {
    fn new(context: &GatewayToolContext, epoch: u64) -> Self {
        let signal = Arc::new(AtomicBool::new(false));
        let token = signal.clone();
        let context = context.clone();
        let monitor = tokio::spawn(async move {
            loop {
                if context.runtime.semantic.epoch.load(Ordering::Acquire) != epoch
                    || super::gateway::check_configuration_generation(&context).is_err() {
                    token.store(true, Ordering::Release); break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
        Self { signal, monitor }
    }
}
impl Drop for Cancellation {
    fn drop(&mut self) { self.signal.store(true,Ordering::Release); self.monitor.abort(); }
}

async fn local_settings(config: &GatewayConfig, project: Option<String>) -> Option<Value> {
    let config = config.clone();
    tokio::task::spawn_blocking(move || retrieval::effective(&config,project.as_deref()).ok()
        .or_else(||retrieval::effective(&config,None).ok()).map(|settings|settings["local"].clone()))
        .await.ok().flatten()
        .filter(|local|local["enabled"] != false && local["semantic"] != "off")
}

async fn prepare(
    context: &GatewayToolContext, cards: Vec<(usize,String)>, key: &str, epoch: u64,
    cancel: &Arc<AtomicBool>,
) -> Option<(Arc<CatalogVectors>,Value)> {
    let config = context.runtime.semantic.config.get()?.clone();
    let Some(local) = local_settings(&config,context.project_id.clone()).await else {
        if let Ok(mut cache)=context.runtime.semantic.vectors.lock() {
            if cache.as_ref().is_some_and(|cache|cache.signature==key) { *cache=None; }
        }
        return None;
    };
    let observed = {
        let config = config.clone(); let local = local.clone();
        tokio::task::spawn_blocking(move || gateway_embedding::status(&config,&local)).await.ok()?
    };
    if observed["state"] != "ready" || cancel.load(Ordering::Acquire) {
        if let Ok(mut cache)=context.runtime.semantic.vectors.lock() {
            if cache.as_ref().is_some_and(|cache|cache.signature==key) { *cache=None; }
        }
        return None;
    }
    let _singleflight = context.runtime.semantic.prepare.lock().await;
    if let Some(cached) = context.runtime.semantic.vectors.lock().ok()?.as_ref()
        .filter(|cached|cached.signature == key).cloned() { return Some((cached,local)); }
    let bytes = cards.iter().map(|(_,text)|text.capacity()+384*std::mem::size_of::<f32>()+256)
        .sum::<usize>().saturating_add(128 * 1024);
    if cards.is_empty() || bytes > MAXIMUM_METADATA_BYTES { return None; }
    let service = hardware::global()?;
    let (stop,receiver) = tokio::sync::watch::channel(false);
    let cancellation = cancel.clone();
    let watch = tokio::spawn(async move {
        while !cancellation.load(Ordering::Acquire) { tokio::time::sleep(Duration::from_millis(10)).await; }
        let _ = stop.send(true);
    });
    let lease = service.acquire(json!({"taskId":uuid::Uuid::new_v4().to_string(),
        "workspaceId":"tool-semantic-metadata","kind":"background","workload":"cache",
        "cpuThreads":0,"memoryBytes":MAXIMUM_METADATA_BYTES,"gpuMemoryBytes":0,"ttlMs":30000,"waitMs":1000}),receiver).await;
    watch.abort();
    let lease = lease.ok()?;
    if cancel.load(Ordering::Acquire) { return None; }
    let owned_config=config.clone(); let owned_local=local.clone(); let token=cancel.clone();
    let vectors = tokio::task::spawn_blocking(move || {
        // Existing E5 passage slices yield to query work; no private model or inference thread is created.
        // 既有E5段落队列按片让出给查询，不建立私有模型或推理线程。
        let mut vectors = Vec::with_capacity(cards.len());
        let mut indexes = Vec::with_capacity(cards.len());
        let mut identity = None;
        for group in cards.chunks(8) {
            if token.load(Ordering::Acquire) { return None; }
            let documents=group.iter().map(|(_,text)|json!({"text":text})).collect::<Vec<_>>();
            let fitted=gateway_embedding::fit(&owned_config,&documents,&owned_local,token.clone()).ok()?;
            for ((index,text),fit) in group.iter().zip(&fitted) {
                let units=text.encode_utf16().collect::<Vec<_>>();
                let segments=fit["segments"].as_array()?;
                for window in segments.chunks(8) {
                    let texts=window.iter().map(|segment| {
                        let start=segment["start"].as_u64()? as usize;
                        let end=segment["end"].as_u64()? as usize;
                        (start<=end && end<=units.len()).then(||String::from_utf16_lossy(&units[start..end]))
                    }).collect::<Option<Vec<_>>>()?;
                    let batch=gateway_embedding::embed(&owned_config,&texts,EmbedKind::Passage,&owned_local,token.clone(),false).ok()??;
                    if batch.vectors.len()!=window.len() || batch.dimensions!=384 { return None; }
                    let current=(batch.space_id.clone(),batch.dimensions);
                    if identity.as_ref().is_some_and(|previous|previous!=&current) { return None; }
                    identity=Some(current);
                    if (vectors.len()+batch.vectors.len()).saturating_mul(384*std::mem::size_of::<f32>()+256)>MAXIMUM_METADATA_BYTES/2 { return None; }
                    indexes.extend(window.iter().map(|_|*index)); vectors.extend(batch.vectors);
                }
            }
        }
        let (space,dimensions)=identity?;
        Some((indexes,vectors,space,dimensions,lease))
    }).await.ok().flatten()?;
    if cancel.load(Ordering::Acquire) || context.runtime.semantic.epoch.load(Ordering::Acquire) != epoch { return None; }
    let cached = Arc::new(CatalogVectors { signature:key.to_owned(),indexes:vectors.0,vectors:vectors.1,
        space:vectors.2,dimensions:vectors.3,_memory:vectors.4 });
    *context.runtime.semantic.vectors.lock().ok()? = Some(cached.clone());
    Some((cached,local))
}

async fn ranked(context: GatewayToolContext, cards: Vec<(usize,String)>, key: String, query: String,
    epoch: u64) -> Option<Vec<(usize,f64)>> {
    let cancellation = Cancellation::new(&context,epoch);
    let (vectors,local) = prepare(&context,cards,&key,epoch,&cancellation.signal).await?;
    let config = context.runtime.semantic.config.get()?.clone();
    let token = cancellation.signal.clone();
    let scope = json!({"scopeKeys":["tool-capabilities"],"conversationId":context.conversation_id,
        "projectId":context.project_id,"permissionMode":context.permission_mode,
        "permissionRevision":context.config["configurationRevision"],"workspaceRoot":context.workdir,
        "workspaceBindingRevision":context.config["workspaceBindingRevision"]});
    let previous=vectors.clone();
    let mismatched=Arc::new(AtomicBool::new(false));
    let mismatch=mismatched.clone();
    let result = tokio::task::spawn_blocking(move || {
        let query = gateway_embedding::embed_query_cached(&config,&[query],&scope,&local,token.clone(),false).ok()??;
        if query.space_id != vectors.space || query.dimensions != vectors.dimensions {
            mismatch.store(true,Ordering::Release); return None;
        }
        if query.vectors.len()!=1 { return None; }
        let mut scores=std::collections::HashMap::<usize,f64>::new();
        for (index,vector) in vectors.indexes.iter().zip(&vectors.vectors) {
            if let Some(score)=super::tool_semantic::cosine(&query.vectors[0],vector) {
                scores.entry(*index).and_modify(|existing|*existing=existing.max(score)).or_insert(score);
            }
        }
        Some(scores.into_iter().collect())
    }).await.ok().flatten();
    // CPU/DML spaces cannot share vectors; retire only the exact stale generation, not a newer build.
    // CPU/DML空间不可混用；只回收这一代失效缓存，不删除并发建立的新缓存。
    if mismatched.load(Ordering::Acquire) {
        if let Ok(mut cache)=context.runtime.semantic.vectors.lock() {
            if cache.as_ref().is_some_and(|cache|Arc::ptr_eq(cache,&previous)) { *cache=None; }
        }
    }
    if cancellation.signal.load(Ordering::Acquire) { return None; }
    result
}

/// Never await a cold model or a busy descriptor build on the foreground tool-search path.
/// 前台工具搜索不等待冷模型或忙碌的描述构建；热查询只有一个很短的可选等待窗口。
pub(crate) async fn supplement(context: &GatewayToolContext, descriptors: &[Value], query: &str,
    lexical: &mut Vec<Value>) -> Value {
    if context.runtime.semantic.config.get().is_none() { return json!({"state":"lexical","reason":"runtime-not-configured"}); }
    let Some(active)=super::tool_semantic::query_text(query) else { return json!({"state":"lexical","reason":"explicit-or-empty-query"}); };
    let cards=trusted_descriptors(context,descriptors);
    let epoch=context.runtime.semantic.epoch.load(Ordering::Acquire);
    let key=signature(context,&cards,epoch);
    let hot=context.runtime.semantic.vectors.lock().ok().is_some_and(|cache|cache.as_ref().is_some_and(|cache|cache.signature==key));
    let worker=tokio::spawn(ranked(context.clone(),cards,key,active,epoch));
    if !hot { return json!({"state":"lexical","semanticPending":true}); }
    match tokio::time::timeout(HOT_QUERY_WAIT,worker).await {
        Ok(Ok(Some(scores))) if context.runtime.semantic.epoch.load(Ordering::Acquire)==epoch => {
            *lexical=super::tool_semantic::combine(query,descriptors,lexical,&scores);
            json!({"state":"hybrid","source":"local-e5","candidates":scores.len()})
        },
        Ok(_) => json!({"state":"lexical","semanticPending":false}),
        Err(_) => json!({"state":"lexical","semanticPending":true}),
    }
}

/// Warm only trusted metadata after the existing E5 owner is ready; never launch an MCP service here.
/// 只在既有E5所有者就绪后预热受信任元信息；此处不启动任何MCP服务。
pub(crate) fn prewarm(context: &GatewayToolContext) {
    if context.runtime.semantic.config.get().is_none()
        || context.runtime.semantic.prewarming.swap(true,Ordering::AcqRel) { return; }
    let mut context=context.clone(); context.events=None; context.external_execute=None; context.approved=false;
    context.authorized_roots.clear();
    tokio::spawn(async move {
        let epoch=context.runtime.semantic.epoch.load(Ordering::Acquire);
        let cancellation=Cancellation::new(&context,epoch);
        for _ in 0..30 {
            if cancellation.signal.load(Ordering::Acquire) { break; }
            let mut descriptors=super::gateway::builtin_catalog();
            descriptors.extend(super::gateway_mcp::reusable_catalog(&context));
            descriptors.extend(super::gateway_mcp::servers(&context).iter().map(|server|json!({
                "name":format!("mcp.{}",server["id"].as_str().unwrap_or("")),
                "description":format!("MCP service {}. Load this group to discover its actual enabled tools; configured does not imply ready.",server["name"].as_str().unwrap_or("")),
                "serverId":server["id"]})));
            let cards=trusted_descriptors(&context,&descriptors);
            let key=signature(&context,&cards,epoch);
            if prepare(&context,cards,&key,epoch,&cancellation.signal).await.is_some() { break; }
            let Some(config)=context.runtime.semantic.config.get().cloned() else {break;};
            let Some(local)=local_settings(&config,context.project_id.clone()).await else {break;};
            let observed=tokio::task::spawn_blocking(move ||gateway_embedding::status(&config,&local)).await;
            // A ready-model build failure retries on later discovery, not in a repeated inference loop.
            // 模型已就绪后的构建失败留给后续发现重试，不在预热中反复进行相同推理。
            if !observed.ok().is_some_and(|value|matches!(value["state"].as_str(),Some("unloaded"|"loading"|"waiting"))) { break; }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        context.runtime.semantic.prewarming.store(false,Ordering::Release);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn semantic_tool_discovery_cold_runtime_returns_lexical_without_connecting_mcp() {
        let (context,_keep)=super::super::gateway::test_context();
        let mut hits=vec![json!({"index":0,"score":100})];
        let original=hits.clone();
        let status=supplement(&context,&[json!({"name":"filesystem.read"})],"阅读本地文档",&mut hits).await;
        assert_eq!(hits,original); assert_eq!(status["state"],"lexical");
        assert!(context.runtime.mcp.lock().await.is_empty());
        let _=std::fs::remove_dir_all(context.workdir.parent().unwrap());
    }

    #[test]
    fn semantic_tool_discovery_trust_and_disabled_tools_filter_before_embedding() {
        let (mut context,_keep)=super::super::gateway::test_context();
        context.config["mcpServers"]=json!([
            {"id":"one","enabled":true,"trusted":true,"disabledTools":["write"]},
            {"id":"two","enabled":true,"trusted":false},
            {"id":"gone","enabled":false,"trusted":true}]);
        let descriptors=vec![json!({"name":"filesystem.read"}),json!({"name":"mcp.one"}),
            json!({"name":"mcp.one.read"}),json!({"name":"mcp.one.write"}),json!({"name":"mcp.two.read"}),
            json!({"name":"mcp.gone.read"})];
        assert_eq!(trusted_descriptors(&context,&descriptors).iter().map(|entry|entry.0).collect::<Vec<_>>(),vec![0,1,2]);
        let before=signature(&context,&trusted_descriptors(&context,&descriptors),0);
        context.runtime.semantic.clear();
        assert_ne!(signature(&context,&trusted_descriptors(&context,&descriptors),1),before);
        let _=std::fs::remove_dir_all(context.workdir.parent().unwrap());
    }
}

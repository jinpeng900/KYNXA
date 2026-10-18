//! Owned GGUF generation uses a fixed private llama.cpp runtime, never Ollama or the user's PATH.
//! 自管 GGUF 生成使用固定私有 llama.cpp；不依赖 Ollama 或用户 PATH，进程及资源随网关释放。

use super::gateway::{self, ModelError};
use crate::resources::gateway::ResourceLease;
use crate::tools::gateway_process::OwnedProcessJob;
use futures_util::StreamExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, io::{Read, Write}, path::{Path, PathBuf}, process::Stdio,
    sync::{Arc, Mutex as StateMutex, OnceLock, atomic::{AtomicBool, Ordering}}, time::Duration};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, process::{Child, Command}, sync::{Mutex, watch}};

const RELEASE: &str = "b11669";
const MIB: u64 = 1024 * 1024;
// Qwen2.5-VL's text/vision template can omit function fields; retain its media tokens with Hermes tool turns.
// Qwen2.5-VL 的普通视觉模板可能未处理工具字段；专用模板保留视觉标记并补齐 Hermes 调用与回执。
const QWEN25_VL_TOOL_TEMPLATE: &str = r#"{%- macro render(content) -%}
{%- if content is string -%}{{- content -}}
{%- elif content is not none -%}
{%- for part in content -%}
{%- if part.type == 'image' or 'image' in part or 'image_url' in part -%}
{{- '<|vision_start|><|image_pad|><|vision_end|>' -}}
{%- elif part.type == 'video' or 'video' in part -%}
{{- '<|vision_start|><|video_pad|><|vision_end|>' -}}
{%- elif part.text is defined -%}{{- part.text -}}{%- endif -%}
{%- endfor -%}{%- endif -%}{%- endmacro -%}
{{- '<|im_start|>system\n' -}}
{%- if messages and messages[0].role == 'system' -%}{{- render(messages[0].content) -}}
{%- else -%}{{- 'You are a helpful assistant.' -}}{%- endif -%}
{%- if tools -%}
{{- '\n\n# Tools\nAvailable functions are described in <tools></tools>.\n<tools>' -}}
{%- for tool in tools -%}{{- '\n' -}}{{- tool | tojson -}}{%- endfor -%}
{{- '\n</tools>\nCall a function with <tool_call>{"name": "function_name", "arguments": {}}</tool_call>. Use its real tool_response before reporting execution results.' -}}
{%- endif -%}{{- '<|im_end|>\n' -}}
{%- for message in messages -%}
{%- if not (loop.first and message.role == 'system') -%}
{%- if message.role == 'tool' -%}
{{- '<|im_start|>user\n<tool_response>\n' -}}{{- render(message.content) -}}{{- '\n</tool_response><|im_end|>\n' -}}
{%- else -%}
{{- '<|im_start|>' + message.role + '\n' -}}{{- render(message.content) -}}
{%- if message.role == 'assistant' and message.tool_calls -%}
{%- for call in message.tool_calls -%}
{{- '\n<tool_call>\n{"name": ' -}}{{- call.function.name | tojson -}}{{- ', "arguments": ' -}}
{%- if call.function.arguments is string -%}{{- call.function.arguments -}}
{%- else -%}{{- call.function.arguments | tojson -}}{%- endif -%}{{- '}\n</tool_call>' -}}
{%- endfor -%}{%- endif -%}{{- '<|im_end|>\n' -}}
{%- endif -%}{%- endif -%}{%- endfor -%}
{%- if add_generation_prompt -%}{{- '<|im_start|>assistant\n' -}}{%- endif -%}"#;

fn tool_template(metadata: &Value) -> Option<&'static str> {
    (metadata["toolTemplate"] == "qwen25-vl-tools-v1").then_some(QWEN25_VL_TOOL_TEMPLATE)
}
struct Asset { backend: &'static str, bytes: u64, sha256: &'static str }
const CPU: Asset = Asset { backend:"cpu", bytes:19_537_886,
    sha256:"37b3efcf962847ca869745fd4ccf7f5e7e8dacfeff9fa3523b4ca8cdc66d632b" };
const VULKAN: Asset = Asset { backend:"vulkan", bytes:32_182_350,
    sha256:"2c07574e7bde71321cb785282b20cc821e951132149eeceeea32ddebdbf0598e" };
#[derive(Clone, Debug, PartialEq)]
struct FileIdentity { path: PathBuf, bytes: u64, modified: Option<std::time::SystemTime>, volume: u64, file_id: u64 }

#[cfg(windows)]
#[link(name = "iphlpapi")]
unsafe extern "system" {
    fn GetExtendedTcpTable(table: *mut std::ffi::c_void, bytes: *mut u32, ordered: i32,
        family: u32, table_class: i32, reserved: u32) -> u32;
}

fn file_identity(path: &Path) -> Result<FileIdentity, ModelError> {
    let path = std::fs::canonicalize(path).map_err(io_error)?;
    let file = std::fs::File::open(&path).map_err(io_error)?;
    let metadata = file.metadata().map_err(io_error)?;
    // The shared handle identity detects replacements without reading large weights or duplicating Win32 declarations.
    // 复用统一句柄身份识别文件替换，不读取大权重，也不重复声明 Windows 接口。
    let identity = crate::file_identity::handle_identity(&file).map_err(io_error)?;
    Ok(FileIdentity { path, bytes: metadata.len(), modified: metadata.modified().ok(),
        volume: identity.volume, file_id: identity.identity })
}

async fn selected_files(metadata: &Value) -> Result<Vec<FileIdentity>, ModelError> {
    let mut paths = metadata["modelFiles"].as_array().map(|files| files.iter().filter_map(Value::as_str)
        .map(PathBuf::from).collect::<Vec<_>>()).unwrap_or_default();
    if paths.is_empty() { paths.push(PathBuf::from(metadata["path"].as_str().unwrap_or(""))); }
    if let Some(path) = metadata["mmprojPath"].as_str().filter(|path| !path.is_empty()) { paths.push(PathBuf::from(path)); }
    tokio::task::spawn_blocking(move || paths.iter().map(|path| file_identity(path)).collect())
        .await.map_err(|_| error("LOCAL_MODEL_IDENTITY_UNCONFIRMED", "本地模型文件身份读取未完成。", 503))?
}

fn listener_owned(base: &str, process_id: u32) -> Result<bool, ModelError> {
    #[cfg(windows)]
    {
        let port = url::Url::parse(base).ok().and_then(|url| url.port()).unwrap_or(0);
        if port == 0 { return Ok(false); }
        let mut size = 0u32;
        // AF_INET + TCP_TABLE_OWNER_PID_LISTENER returns fixed DWORD rows, including the real listener's PID.
        // 原生查询 IPv4 监听表并取得真实PID；不启动netstat/PowerShell，也不把HTTP别名作为进程所有权证据。
        let first = unsafe { GetExtendedTcpTable(std::ptr::null_mut(), &mut size, 0, 2, 3, 0) };
        if first != 0 && first != 122 { return Err(io_error(std::io::Error::from_raw_os_error(first as i32))); }
        let mut table = Vec::new();
        let mut result = 122u32;
        // Other applications can add listeners between sizing and reading; retry only the resized native buffer.
        // 其他应用可能在两次查询间新增监听，缓冲不足时只有限扩容重读，不误报模型身份变化。
        for _ in 0..3 {
            table.resize(size as usize, 0);
            result = unsafe { GetExtendedTcpTable(table.as_mut_ptr().cast(), &mut size, 0, 2, 3, 0) };
            if result != 122 { break; }
        }
        if result != 0 { return Err(io_error(std::io::Error::from_raw_os_error(result as i32))); }
        table.truncate(size as usize);
        if table.len() < 4 { return Ok(false); }
        let count = u32::from_ne_bytes(table[..4].try_into().unwrap()) as usize;
        for row in table[4..].chunks_exact(24).take(count) {
            let address = u32::from_ne_bytes(row[4..8].try_into().unwrap());
            let candidate_port = u32::from_ne_bytes(row[8..12].try_into().unwrap()) as u16;
            let owner = u32::from_ne_bytes(row[20..24].try_into().unwrap());
            if address == u32::from_ne_bytes([127, 0, 0, 1]) && u16::from_be(candidate_port) == port && owner == process_id {
                return Ok(true);
            }
        }
        Ok(false)
    }
    #[cfg(not(windows))]
    { let _ = (base, process_id); Ok(false) }
}

struct Process { child: Child, owned: OwnedProcessJob, lease: Option<ResourceLease>, base: String, files: Vec<FileIdentity>,
    diagnostic: tokio::task::JoinHandle<()> }
impl Drop for Process {
    fn drop(&mut self) { self.diagnostic.abort(); }
}
struct Engine {
    gate: Mutex<()>, process: Mutex<Option<Process>>, state: StateMutex<Value>,
    monitor: StateMutex<Option<tokio::task::JoinHandle<()>>>, closing: AtomicBool, suspended: AtomicBool,
}
impl Default for Engine {
    fn default() -> Self { Self { gate:Mutex::new(()), process:Mutex::new(None),
        state:StateMutex::new(json!({"state":"stopped","activeRequests":0,"runtimeVersion":RELEASE,
            "modelIdentityVerified":false,"supportsTools":null,"toolCallingMode":"unknown","toolCapabilityState":"unknown"})),
        monitor:StateMutex::new(None),closing:AtomicBool::new(false),suspended:AtomicBool::new(false) } }
}
static ENGINES: OnceLock<StateMutex<HashMap<(PathBuf, String), Arc<Engine>>>> = OnceLock::new();
static INSTALLS: OnceLock<Mutex<()>> = OnceLock::new();
static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);

fn error(code: &str, text: &str, status: u16) -> ModelError { ModelError::new(code, text, status) }
fn io_error(source: std::io::Error) -> ModelError {
    let mut failure = error("LOCAL_MODEL_IO_FAILED", "本地模型运行时读写失败，请检查目录权限与磁盘空间。", 500);
    failure.details["reason"] = json!(source.to_string()); failure
}
fn map_error(value: crate::gateway_contract::GatewayError) -> ModelError {
    error(&value.code, &value.message, value.status)
}
pub fn is_managed(connection: &Value) -> bool {
    connection["providerId"].as_str().is_some_and(|id| id.starts_with("kynxa-local-"))
}
fn identity<'a>(connection: &'a Value, model: &'a str) -> Result<&'a str, ModelError> {
    let id = connection["providerId"].as_str().and_then(|id| id.strip_prefix("kynxa-local-"))
        .filter(|id| !id.is_empty() && id.len() <= 128 && id.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte)))
        .ok_or_else(|| error("LOCAL_MODEL_CONFIG_INVALID", "本地模型身份无效。", 400))?;
    if id != model { return Err(error("LOCAL_MODEL_CONFIG_CHANGED", "本地模型选择已变化，请重新选择。", 409)); }
    Ok(id)
}
fn engine(root: &Path, id: &str) -> Arc<Engine> {
    ENGINES.get_or_init(Default::default).lock().unwrap().entry((root.to_owned(), id.to_owned())).or_default().clone()
}
fn patch(engine: &Engine, values: Value) {
    let mut state = engine.state.lock().unwrap();
    for (key, value) in values.as_object().expect("engine status fields") { state[key] = value.clone(); }
}
fn cancelled(token: &watch::Receiver<bool>, engine: &Engine) -> Result<(), ModelError> {
    if *token.borrow() || engine.closing.load(Ordering::Acquire) { Err(ModelError::cancelled()) } else { Ok(()) }
}
pub fn status(root: &Path, id: &str) -> Value {
    let value = ENGINES.get().and_then(|engines| engines.lock().ok())
        .and_then(|engines| engines.get(&(root.to_owned(), id.to_owned())).cloned());
    value.map(|engine| {
        if let Ok(mut process) = engine.process.try_lock() {
            if let Some(running) = process.as_mut() {
                match running.child.try_wait() {
                    Ok(Some(_)) => patch(&engine, json!({"state":"stopped","processId":null,"modelIdentityVerified":false,
                        "supportsTools":null,"toolCallingMode":"unknown","toolCapabilityState":"unknown"})),
                    Err(source) => binding_failure(&engine, &io_error(source)),
                    _ => {},
                }
            }
        }
        engine.state.lock().unwrap().clone()
    })
        .unwrap_or_else(|| json!({"state":"stopped","activeRequests":0,"runtimeVersion":RELEASE,
            "modelIdentityVerified":false,"supportsTools":null,"toolCallingMode":"unknown","toolCapabilityState":"unknown"}))
}

/// The guard spans the real HTTP request, so a settings change cannot kill an active generation.
/// 保护跨越真实 HTTP 请求；设置页切换、禁用或删除不能切断正在生成的模型。
pub struct GenerationGuard(Arc<Engine>);
impl Drop for GenerationGuard {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap();
        state["activeRequests"] = json!(state["activeRequests"].as_u64().unwrap_or(0).saturating_sub(1));
    }
}
pub fn request_guard(root: &Path, connection: &Value, model: &str) -> Result<Option<GenerationGuard>, ModelError> {
    if !is_managed(connection) { return Ok(None); }
    let owner = engine(root, identity(connection, model)?);
    { let mut state = owner.state.lock().unwrap();
      if state["state"] != "ready" || state["modelIdentityVerified"] != true
          || state["baseUrl"].as_str().unwrap_or("").trim_end_matches('/') != connection["baseUrl"].as_str().unwrap_or("").trim_end_matches('/')
          || owner.closing.load(Ordering::Acquire) || owner.suspended.load(Ordering::Acquire) {
          return Err(error("LOCAL_MODEL_TRANSITION", "本地模型状态正在变化，请稍后继续。", 409));
      }
      state["activeRequests"] = json!(state["activeRequests"].as_u64().unwrap_or(0) + 1);
    }
    Ok(Some(GenerationGuard(owner)))
}

fn application_root() -> Option<PathBuf> {
    std::env::var_os("KYNXA_APPLICATION_ROOT").map(PathBuf::from).or_else(||
        std::env::current_exe().ok()?.parent()?.parent()?.parent().map(Path::to_owned))
}
fn executable(root: &Path, depth: usize) -> Option<PathBuf> {
    let direct = root.join("llama-server.exe");
    if direct.is_file() { return Some(direct); }
    if depth == 0 { return None; }
    for child in std::fs::read_dir(root).ok()?.flatten() {
        if child.file_type().ok()?.is_dir() {
            if let Some(found) = executable(&child.path(), depth - 1) { return Some(found); }
        }
    }
    None
}
async fn install_runtime(root: &Path, asset: &'static Asset, owner: &Arc<Engine>, token: &watch::Receiver<bool>) -> Result<PathBuf, ModelError> {
    if let Some(path) = application_root().and_then(|app| executable(&app.join("runtime/local-llm/llama.cpp").join(RELEASE).join(asset.backend), 2)) {
        return Ok(path);
    }
    let directory = root.join("LocalRuntime/llama.cpp").join(RELEASE).join(asset.backend);
    if directory.join(".ready").is_file() { if let Some(path) = executable(&directory, 2) { return Ok(path); } }
    let _install = INSTALLS.get_or_init(Default::default).lock().await;
    if directory.join(".ready").is_file() { if let Some(path) = executable(&directory, 2) { return Ok(path); } }
    cancelled(token, owner)?;
    tokio::fs::create_dir_all(&directory).await.map_err(io_error)?;
    let archive = directory.with_extension("zip.part");
    let url = format!("https://github.com/ggml-org/llama.cpp/releases/download/{RELEASE}/llama-{RELEASE}-bin-win-{}-x64.zip", asset.backend);
    let mut failure = None;
    for attempt in 0..3 {
        cancelled(token, owner)?;
        let offset = tokio::fs::metadata(&archive).await.map(|value| value.len()).unwrap_or(0);
        if offset == asset.bytes { break; }
        let mut request = download_client()?.get(&url).timeout(Duration::from_secs(180));
        if offset > 0 && offset < asset.bytes { request = request.header(reqwest::header::RANGE, format!("bytes={offset}-")); }
        let transfer = async {
            let response = request.send().await.map_err(|_| error("LOCAL_RUNTIME_DOWNLOAD_FAILED", "本地推理运行时下载失败，可重试续传。", 502))?;
            if !response.status().is_success() { return Err(error("LOCAL_RUNTIME_DOWNLOAD_FAILED", "官方推理运行时下载服务暂不可用，可重试。", 502)); }
            let append = response.status() == reqwest::StatusCode::PARTIAL_CONTENT;
            if append && !response.headers().get(reqwest::header::CONTENT_RANGE).and_then(|value| value.to_str().ok())
                .is_some_and(|range| range.starts_with(&format!("bytes {offset}-")) && range.ends_with(&format!("/{}", asset.bytes))) {
                return Err(error("LOCAL_RUNTIME_DOWNLOAD_RANGE", "运行时续传范围无效，保留已有下载。", 502));
            }
            let mut file = tokio::fs::OpenOptions::new().create(true).write(true).append(append).truncate(!append)
                .open(&archive).await.map_err(io_error)?;
            let mut bytes = if append { offset } else { 0 };
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                cancelled(token, owner)?;
                let chunk = chunk.map_err(|_| error("LOCAL_RUNTIME_DOWNLOAD_INTERRUPTED", "运行时下载中断，下次将继续。", 502))?;
                bytes += chunk.len() as u64;
                if bytes > asset.bytes { return Err(error("LOCAL_RUNTIME_DOWNLOAD_SIZE", "运行时下载大小与固定版本不符。", 502)); }
                file.write_all(&chunk).await.map_err(io_error)?;
                patch(owner, json!({"state":"downloading-runtime","downloadedBytes":bytes,"downloadBytes":asset.bytes,"runtimeBackend":asset.backend}));
            }
            file.flush().await.map_err(io_error)?;
            if bytes != asset.bytes { return Err(error("LOCAL_RUNTIME_DOWNLOAD_INTERRUPTED", "运行时下载尚未完整，下次将继续。", 502)); }
            Ok(())
        };
        tokio::pin!(transfer);
        let mut cancellation = token.clone();
        let transfer = tokio::select! {
            result = &mut transfer => result,
            _ = gateway::cancelled(&mut cancellation) => return Err(ModelError::cancelled()),
        };
        match transfer { Ok(()) => { failure = None; break; }, Err(error) => { failure = Some(error); if attempt < 2 { tokio::time::sleep(Duration::from_secs(1 << attempt)).await; } } }
    }
    if let Some(error) = failure { return Err(error); }
    let archive_path = archive.clone();
    let destination = directory.clone();
    tokio::task::spawn_blocking(move || {
        let mut input = std::fs::File::open(&archive_path).map_err(io_error)?;
        let mut digest = Sha256::new(); let mut buffer = [0u8; 65536];
        loop { let read = input.read(&mut buffer).map_err(io_error)?; if read == 0 { break; } digest.update(&buffer[..read]); }
        if format!("{:x}", digest.finalize()) != asset.sha256 {
            std::fs::remove_file(&archive_path).map_err(io_error)?;
            return Err(error("LOCAL_RUNTIME_CHECKSUM_FAILED", "运行时校验失败，请重新下载固定官方版本。", 502));
        }
        let mut zip = zip::ZipArchive::new(std::fs::File::open(&archive_path).map_err(io_error)?)
            .map_err(|_| error("LOCAL_RUNTIME_ARCHIVE_INVALID", "运行时压缩包无效。", 502))?;
        for index in 0..zip.len() {
            let mut entry = zip.by_index(index).map_err(|_| error("LOCAL_RUNTIME_ARCHIVE_INVALID", "运行时压缩包无法读取。", 502))?;
            let relative = entry.enclosed_name().ok_or_else(|| error("LOCAL_RUNTIME_ARCHIVE_INVALID", "运行时压缩包路径无效。", 502))?;
            if entry.unix_mode().is_some_and(|mode| mode & 0o170000 == 0o120000) {
                return Err(error("LOCAL_RUNTIME_ARCHIVE_INVALID", "运行时压缩包包含不支持的链接。", 502));
            }
            let path = destination.join(relative);
            if entry.is_dir() { std::fs::create_dir_all(&path).map_err(io_error)?; continue; }
            if let Some(parent) = path.parent() { std::fs::create_dir_all(parent).map_err(io_error)?; }
            let mut output = std::fs::File::create(&path).map_err(io_error)?;
            std::io::copy(&mut entry, &mut output).map_err(io_error)?;
            output.flush().map_err(io_error)?;
        }
        let server = executable(&destination, 2).ok_or_else(|| error("LOCAL_RUNTIME_ENTRY_MISSING", "官方运行时未包含 llama-server.exe。", 502))?;
        std::fs::write(destination.join(".ready"), asset.sha256).map_err(io_error)?;
        std::fs::remove_file(archive_path).map_err(io_error)?;
        Ok(server)
    }).await.map_err(|_| error("LOCAL_RUNTIME_INSTALL_FAILED", "运行时解压任务未完成。", 500))?
}

fn hardware_plan(model: &Value, snapshot: &Value) -> (u64, usize, bool, u64, u64) {
    let cores = snapshot["cpu"]["logicalCores"].as_u64().unwrap_or_else(||
        std::thread::available_parallelism().map(usize::from).unwrap_or(4) as u64);
    let available = snapshot["accounting"]["availableMemoryBytes"].as_u64()
        .or_else(|| snapshot["memory"]["availableBytes"].as_u64()).unwrap_or(4 * 1024 * MIB);
    let requested = model["contextTokens"].as_u64().unwrap_or(16384).clamp(512, 131072);
    let bytes = model["bytes"].as_u64().unwrap_or(0);
    let gpu_available = snapshot["accounting"]["availableGpuMemoryBytes"].as_u64().unwrap_or(0);
    let vulkan = gpu_available > 1024 * MIB
        && std::env::var_os("SystemRoot").is_some_and(|root| PathBuf::from(root).join("System32/vulkan-1.dll").is_file());
    let gpu_capacity = if vulkan { gpu_available.saturating_sub(1024 * MIB) } else { 0 };
    let (context, host_memory, gpu_memory) = residency_plan(bytes, requested, available, gpu_capacity);
    let threads = if snapshot["cpu"]["usagePercent"].as_f64().unwrap_or(0.0) > 80.0 { cores / 2 } else { cores.saturating_sub(1) }.clamp(1, 64) as usize;
    (context, threads, vulkan, host_memory, gpu_memory)
}
fn residency_plan(bytes: u64, requested: u64, host_available: u64, gpu_capacity: u64) -> (u64, u64, u64) {
    // Charge offloaded weights/KV to real VRAM, retaining host buffers instead of double charging all weights.
    // 已卸载到显卡的权重和KV计入实际显存，主存保留缓冲，不将全部权重在两处重复计量。
    let host_buffers = 512 * MIB;
    let fit = host_available.saturating_add(gpu_capacity)
        .saturating_sub(bytes.saturating_add(host_buffers)) / 65536;
    let context = requested.min(fit.max(512));
    let total = bytes.saturating_add(context * 65536);
    let gpu = total.min(gpu_capacity);
    (context, total.saturating_sub(gpu).saturating_add(host_buffers), gpu)
}
async fn admit(bytes: u64, gpu_bytes: u64, token: watch::Receiver<bool>) -> Result<ResourceLease, ModelError> {
    let service = crate::resources::gateway::global().ok_or_else(|| error("RESOURCE_RUNTIME_UNAVAILABLE", "本地模型资源服务尚未就绪。", 503))?;
    service.acquire_waiting(json!({"taskId":format!("local-model-{}", uuid::Uuid::new_v4()),"workspaceId":"managed-local-models",
        "kind":"foreground","workload":"compute","cpuThreads":0,"memoryBytes":bytes,"gpuMemoryBytes":gpu_bytes,"waitMs":30000,"ttlMs":30000}), token)
        .await.map_err(map_error)
}
fn local_client() -> Result<reqwest::Client, ModelError> {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    let client = if let Some(client) = CLIENT.get() { client.clone() } else {
        let client = reqwest::Client::builder().no_proxy().build()
            .map_err(|_| error("LOCAL_MODEL_HTTP_FAILED", "本地模型连接初始化失败。", 500))?;
        let _ = CLIENT.set(client.clone()); client
    };
    Ok(client)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalRequestTokens {
    pub input_tokens: u64,
    pub context_window_tokens: u64,
}

async fn count_input_tokens(base: &str, body: &Value, cancellation: &mut watch::Receiver<bool>) -> Result<u64, ModelError> {
    let request = local_client()?.post(format!("{base}/v1/chat/completions/input_tokens"))
        .json(body).timeout(Duration::from_secs(3)).send();
    let response = tokio::select! {
        biased;
        _ = gateway::cancelled(cancellation) => return Err(ModelError::cancelled()),
        response = request => response.map_err(|_| error("LOCAL_MODEL_TOKEN_COUNT_FAILED", "本地模型上下文预检尚未完成，请稍后重试。", 503))?,
    };
    let status = response.status();
    let counted = gateway::bounded_json(response, 64 * 1024, cancellation).await?;
    if !status.is_success() {
        return Err(gateway::upstream_rejection(status.as_u16(), &counted));
    }
    counted["input_tokens"].as_u64().filter(|count| *count > 0)
        .ok_or_else(|| error("LOCAL_MODEL_TOKEN_COUNT_INVALID", "本地模型未返回有效的上下文计数。", 502))
}

/// Count the exact final request using the owned model's template/tokenizer, without generating or storing prompts.
/// 使用自有模型的完整模板和分词器统计最终请求，不生成回复、不保存提示词；普通 API 请求不增加调用。
pub async fn count_request_tokens(
    connection: &Value,
    model: &str,
    body: &Value,
    mut cancellation: watch::Receiver<bool>,
) -> Result<Option<LocalRequestTokens>, ModelError> {
    if !is_managed(connection) { return Ok(None); }
    let id = identity(connection, model)?;
    let expected = connection["baseUrl"].as_str().unwrap_or("").trim_end_matches('/');
    // Match the verified engine endpoint as well as the alias; another data root can contain the same model name.
    // 同时匹配已核验的服务地址和模型标识；其他数据根目录允许存在相同模型名，不能只按别名计数。
    let owners = ENGINES.get().and_then(|engines| engines.lock().ok()).map(|engines| {
        engines.iter().filter(|((_, model_id), owner)| {
            if model_id != id { return false; }
            let state = owner.state.lock().unwrap();
            state["modelIdentityVerified"] == true && state["state"] == "ready"
                && state["baseUrl"].as_str().unwrap_or("").trim_end_matches('/') == expected
        }).map(|(_, owner)| owner.clone()).collect::<Vec<_>>()
    }).unwrap_or_default();
    if owners.len() != 1 {
        return Err(error("LOCAL_MODEL_IDENTITY_UNCONFIRMED", "本地模型服务身份尚未核验，请重新加载。", 503));
    }
    let owner = &owners[0];
    let checked = async {
        let mut process = tokio::select! {
            biased;
            _ = gateway::cancelled(&mut cancellation) => return Err(ModelError::cancelled()),
            process = owner.process.lock() => process,
        };
        cancelled(&cancellation, owner)?;
        let running = process.as_mut().ok_or_else(|| error("LOCAL_MODEL_IDENTITY_UNCONFIRMED", "本地模型服务尚未就绪。", 503))?;
        let process_id = running.child.id().unwrap_or(0);
        if binding_checked(owner, running.child.try_wait().map_err(io_error))?.is_some()
            || format!("{}/v1", running.base) != expected
            || !binding_checked(owner, listener_owned(&running.base, process_id))? {
            let failure = error("LOCAL_MODEL_IDENTITY_MISMATCH", "本地模型服务已变化，请重新加载后继续。", 409);
            binding_failure(owner, &failure);
            return Err(failure);
        }
        let context_window_tokens = owner.state.lock().unwrap()["runtimeContextTokens"].as_u64()
            .filter(|tokens| *tokens > 0)
            .ok_or_else(|| error("LOCAL_MODEL_CONTEXT_UNCONFIRMED", "本地模型实际上下文窗口尚未核验。", 503))?;
        let input_tokens = count_input_tokens(&running.base, body, &mut cancellation).await?;
        cancelled(&cancellation, owner)?;
        Ok(Some(LocalRequestTokens { input_tokens, context_window_tokens }))
    };
    // The same process lock fences replacement during this bounded preflight; the caller keeps its generation guard.
    // 有界预检期间持有同一进程锁，防止服务被替换；沿用调用方的生成保护，不重复获取保护或占用推理槽。
    tokio::time::timeout(Duration::from_secs(3), checked).await
        .map_err(|_| error("LOCAL_MODEL_TOKEN_COUNT_TIMEOUT", "本地模型上下文预检稍慢，请稍后继续。", 503))?
}

async fn response(base: &str, endpoint: &str) -> Result<Value, ModelError> {
    let result = local_client()?.get(format!("{base}{endpoint}")).timeout(Duration::from_secs(3)).send().await
        .map_err(|_| error("LOCAL_MODEL_CONNECTING", "本地模型服务正在准备。", 503))?;
    if !result.status().is_success() { return Err(error("LOCAL_MODEL_CONNECTING", "本地模型尚未就绪。", 503)); }
    result.json().await.map_err(|_| error("LOCAL_MODEL_RESPONSE_INVALID", "本地模型返回了无效状态。", 502))
}
fn template_status(props: &Value, adapted: bool) -> Value {
    let capabilities = &props["chat_template_caps"];
    let supported = match (capabilities["supports_tools"].as_bool(), capabilities["supports_tool_calls"].as_bool()) {
        (Some(true), Some(true)) => Some(true),
        (Some(false), _) | (_, Some(false)) => Some(false),
        _ => None,
    };
    json!({"supportsTools":supported,
        "toolCallingMode":match supported { Some(true) if adapted => "adapted", Some(true) => "native", Some(false) => "unsupported", None => "unknown" },
        "toolCapabilityState":match supported { Some(true) => "available", Some(false) => "unsupported", None => "unknown" },
        "chatTemplateCapabilities":capabilities})
}

async fn verify_reported_file(props: &Value, selected: &[FileIdentity]) -> Result<(), ModelError> {
    let reported = props["model_path"].as_str().filter(|path| !path.is_empty())
        .ok_or_else(|| error("LOCAL_MODEL_IDENTITY_UNCONFIRMED", "本地服务未报告实际模型路径，尚不能确认选中模型已装载。", 503))?;
    let path = PathBuf::from(reported);
    let observed = tokio::task::spawn_blocking(move || file_identity(&path)).await
        .map_err(|_| error("LOCAL_MODEL_IDENTITY_UNCONFIRMED", "本地模型文件身份读取未完成。", 503))??;
    if selected.first() != Some(&observed) {
        return Err(error("LOCAL_MODEL_IDENTITY_MISMATCH", "本地服务实际模型与选中 GGUF 不一致，请重新加载。", 409));
    }
    Ok(())
}

async fn observe_model(owner: &Arc<Engine>, base: &str, process_id: u32, selected: &[FileIdentity]) -> Result<(), ModelError> {
    let props = response(base, "/props").await?;
    if !listener_owned(base, process_id)? {
        return Err(error("LOCAL_MODEL_IDENTITY_MISMATCH", "本地模型端口不属于当前受管进程，请重新加载。", 409));
    }
    verify_reported_file(&props, selected).await?;
    let paths = selected.iter().map(|file| file.path.clone()).collect::<Vec<_>>();
    let current = tokio::task::spawn_blocking(move || paths.iter().map(|path| file_identity(path))
        .collect::<Result<Vec<_>, ModelError>>()).await
        .map_err(|_| error("LOCAL_MODEL_IDENTITY_UNCONFIRMED", "本地模型文件身份读取未完成。", 503))??;
    if current != selected {
        return Err(error("LOCAL_MODEL_IDENTITY_MISMATCH", "本地模型分片或视觉投影文件在装载期间发生变化，请重新加载。", 409));
    }
    let adapted = !owner.state.lock().unwrap()["toolTemplate"].is_null();
    let context = props["default_generation_settings"]["n_ctx"].as_u64()
        .filter(|tokens| *tokens > 0)
        .ok_or_else(|| error("LOCAL_MODEL_CONTEXT_UNCONFIRMED", "本地服务未报告实际上下文窗口，尚不能开始生成。", 503))?;
    patch(owner, template_status(&props, adapted));
    patch(owner, json!({"modelIdentityVerified":true,"loadedModelPath":props["model_path"],
        "baseUrl":format!("{base}/v1"),"processId":process_id,"runtimeContextTokens":context,"contextTokens":context}));
    Ok(())
}

fn binding_failure(owner: &Engine, failure: &ModelError) {
    patch(owner, json!({"state":"failed","modelIdentityVerified":false,"supportsTools":null,
        "toolCallingMode":"unknown","toolCapabilityState":"unknown","code":failure.code}));
}

fn binding_checked<T>(owner: &Engine, checked: Result<T, ModelError>) -> Result<T, ModelError> {
    checked.inspect_err(|failure| binding_failure(owner, failure))
}

/// Reuse only capabilities observed from the owned process; catalog availability is not runtime tool support.
/// 仅使用已观测的受管进程能力；目录可安装、HTTP连通或alias相同都不能代替运行时工具支持。
pub fn request_capabilities(metadata: &Value, observed: &Value) -> Value {
    let mut capabilities = metadata["capabilities"].as_array().cloned().unwrap_or_default();
    capabilities.retain(|capability| capability != "tools");
    if observed["modelIdentityVerified"] == true && observed["supportsTools"] == true { capabilities.push(json!("tools")); }
    json!(capabilities)
}

/// Cached readiness is reusable only while the same files, endpoint and owned child still exist.
/// 仅当文件身份、端口与自有子进程仍一致时复用就绪观测；暖路径不重复HTTP、不扫描目录或读取大权重。
pub async fn refresh_status(root: &Path, connection: &Value, model: &str) -> Result<Value, ModelError> {
    let id = identity(connection, model)?;
    let owner = engine(root, id);
    let Ok(mut process) = owner.process.try_lock() else { return Ok(status(root, id)); };
    let Some(running) = process.as_mut() else { return Ok(status(root, id)); };
    if binding_checked(&owner, running.child.try_wait().map_err(io_error))?.is_some() {
        patch(&owner, json!({"state":"stopped","processId":null,"modelIdentityVerified":false,
            "supportsTools":null,"toolCallingMode":"unknown","toolCapabilityState":"unknown"}));
        return Ok(status(root, id));
    }
    let metadata = binding_checked(&owner, super::gateway_local_models::enabled_model(root, id).await)?;
    let files = binding_checked(&owner, selected_files(&metadata).await)?;
    let expected = format!("{}/v1", running.base);
    if connection["baseUrl"].as_str().unwrap_or("").trim_end_matches('/') != expected
        || files != running.files || !binding_checked(&owner, listener_owned(&running.base, running.child.id().unwrap_or(0)))? {
        let failure = error("LOCAL_MODEL_IDENTITY_MISMATCH", "本地模型文件、端口或受管进程已变化，请重新加载。", 409);
        binding_failure(&owner, &failure);
        return Err(failure);
    }
    Ok(status(root, id))
}

pub fn preparation_status(observed: &Value) -> Value {
    let ready = observed["state"] == "ready" && observed["modelIdentityVerified"] == true;
    let mut prepared = json!({"state":if ready {"ready"}else if observed["state"]=="sleeping" {"sleeping"}else{"unloaded"},
        "connectionReady":ready,"modelReady":ready,"available":true,
        "preparationKind":"managed-local-model","inferenceVerified":false,"code":observed["code"]});
    for field in ["modelIdentityVerified", "loadedModelPath", "processId", "baseUrl", "supportsTools", "toolCallingMode", "toolCapabilityState"] {
        prepared[field] = observed[field].clone();
    }
    prepared
}
async fn wake(base: &str, mut token: watch::Receiver<bool>) -> Result<(), ModelError> {
    // A tokenization task wakes sleeping weights without generating or injecting a business prompt.
    // 空文本分词任务唤醒休眠权重，不生成内容，也不注入任何业务提示词。
    let request = local_client()?.post(format!("{base}/tokenize"))
        .json(&json!({"content":"","add_special":false})).timeout(Duration::from_secs(300)).send();
    let response = tokio::select! {
        result = request => result.map_err(|_| error("LOCAL_MODEL_WAKE_FAILED", "本地模型唤醒失败，请重试。", 503))?,
        _ = gateway::cancelled(&mut token) => return Err(ModelError::cancelled()),
    };
    if !response.status().is_success() { return Err(error("LOCAL_MODEL_WAKE_FAILED", "本地模型唤醒未完成，请重试。", 503)); }
    Ok(())
}
fn download_client() -> Result<reqwest::Client, ModelError> {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    if let Some(client) = CLIENT.get() { return Ok(client.clone()); }
    let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::limited(8))
        .connect_timeout(Duration::from_secs(30)).build()
        .map_err(|_| error("LOCAL_RUNTIME_HTTP_FAILED", "运行时下载连接初始化失败。", 500))?;
    let _ = CLIENT.set(client.clone()); Ok(client)
}

pub async fn ensure(root: &Path, connection: &Value, model: &str, token: watch::Receiver<bool>) -> Result<(), ModelError> {
    if !is_managed(connection) { return Ok(()); }
    let id = identity(connection, model)?.to_owned();
    let owner = engine(root, &id);
    if SHUTTING_DOWN.load(Ordering::Acquire) { return Err(ModelError::cancelled()); }
    let _singleflight = owner.gate.lock().await;
    if SHUTTING_DOWN.load(Ordering::Acquire) { return Err(ModelError::cancelled()); }
    if owner.suspended.load(Ordering::Acquire) {
        return Err(error("LOCAL_MODEL_BUSY", "本地模型配置正在更新，请稍后继续。", 409));
    }
    owner.closing.store(false, Ordering::Release);
    cancelled(&token, &owner)?;
    let metadata = binding_checked(&owner, super::gateway_local_models::enabled_model(root, &id).await)?;
    let files = binding_checked(&owner, selected_files(&metadata).await)?;
    let port = binding_checked(&owner, metadata["port"].as_u64().filter(|port| *port > 0 && *port <= 65535)
        .ok_or_else(|| error("LOCAL_MODEL_CONFIG_INVALID", "本地模型端口无效。", 400)))?;
    let base = format!("http://127.0.0.1:{port}");
    if connection["baseUrl"].as_str().unwrap_or("").trim_end_matches('/') != format!("{base}/v1") {
        return binding_checked(&owner, Err(error("LOCAL_MODEL_CONFIG_CHANGED", "本地模型地址与已启用配置不一致。", 409)));
    }
    let mut process = owner.process.lock().await;
    if let Some(running) = process.as_mut() {
        if binding_checked(&owner, running.child.try_wait().map_err(io_error))?.is_none() {
            let process_id = running.child.id().unwrap_or(0);
            if running.base != base || running.files != files || !binding_checked(&owner, listener_owned(&base, process_id))? {
                let failure = error("LOCAL_MODEL_IDENTITY_MISMATCH", "本地模型文件、端口或受管进程已变化，请重新加载。", 409);
                binding_failure(&owner, &failure);
                return Err(failure);
            }
            let needs_wake = running.lease.is_none() || owner.state.lock().unwrap()["state"] != "ready";
            if running.lease.is_none() {
                let requested = owner.state.lock().unwrap()["memoryBytes"].as_u64().unwrap_or(512 * MIB);
                let gpu = owner.state.lock().unwrap()["gpuMemoryBytes"].as_u64().unwrap_or(0);
                running.lease = Some(admit(requested, gpu, token.clone()).await?);
            }
            if needs_wake {
                patch(&owner, json!({"state":"loading"}));
                wake(&base, token.clone()).await?;
            }
            if needs_wake {
                if let Err(failure) = observe_model(&owner, &base, process_id, &files).await {
                    binding_failure(&owner, &failure);
                    return Err(failure);
                }
            }
            patch(&owner, json!({"state":"ready"}));
            return Ok(());
        }
    }
    *process = None;
    let service = crate::resources::gateway::global().ok_or_else(|| error("RESOURCE_RUNTIME_UNAVAILABLE", "本地模型资源服务尚未就绪。", 503))?;
    let snapshot = service.request("snapshot", json!({}), Duration::from_secs(5)).await.map_err(map_error)?;
    let (context, threads, vulkan, memory, gpu_memory) = hardware_plan(&metadata, &snapshot);
    patch(&owner, json!({"state":"preparing-runtime","modelId":id,"contextTokens":context,"cpuThreads":threads,
        "memoryBytes":memory,"gpuMemoryBytes":gpu_memory,"supportsTools":Value::Null,
        "chatTemplateCapabilities":Value::Null,"toolCallingMode":"unknown","toolCapabilityState":"unknown",
        "modelIdentityVerified":false,"loadedModelPath":Value::Null,"baseUrl":format!("{base}/v1"),"toolTemplate":metadata["toolTemplate"]}));
    let asset = if vulkan { &VULKAN } else { &CPU };
    let path = install_runtime(root, asset, &owner, &token).await?;
    cancelled(&token, &owner)?;
    let lease = admit(memory, gpu_memory, token.clone()).await?;
    let mut command = Command::new(&path);
    command.current_dir(path.parent().unwrap()).env_clear();
    for key in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(key) { command.env(key, value); }
    }
    command.args(["--model", metadata["path"].as_str().unwrap_or(""), "--alias", &id, "--host", "127.0.0.1", "--port", &port.to_string(),
        "--ctx-size", &context.to_string(), "--threads", &threads.to_string(), "--parallel", "1",
        "--fit", "on", "--fit-target", "1024", "--gpu-layers", if vulkan { "auto" } else { "0" }, "--sleep-idle-seconds", "300", "--jinja"]);
    if let Some(template) = tool_template(&metadata) { command.args(["--chat-template", template]); }
    if let Some(mmproj) = metadata["mmprojPath"].as_str().filter(|path| !path.is_empty()) { command.args(["--mmproj", mmproj]); }
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).kill_on_drop(true);
    #[cfg(windows)] command.creation_flags(0x08000000);
    let mut child = command.spawn().map_err(|source| {
        let mut failure = error("LOCAL_MODEL_START_FAILED", "私有本地推理服务启动失败，请修复运行时。", 503);
        failure.details["reason"] = json!(source.to_string()); failure
    })?;
    let owned = match OwnedProcessJob::attach(&child) { Ok(owned) => owned, Err(error) => { let _ = child.kill().await; return Err(map_error(error)); } };
    patch(&owner, json!({"state":"loading","processId":child.id(),"runtimeBackend":asset.backend,"backend":asset.backend,"runtimeDiagnostic":""}));
    let diagnostic_owner = owner.clone();
    let stderr = child.stderr.take();
    // Drain stderr continuously but retain only a small tail for actionable load failures.
    // 持续消费标准错误，仅保留小段末尾诊断；失败时说明真实原因，不累积日志占用磁盘。
    let mut diagnostic = tokio::spawn(async move {
        let Some(mut stderr) = stderr else { return; };
        let mut tail = Vec::new(); let mut chunk = [0u8; 4096];
        while let Ok(bytes) = stderr.read(&mut chunk).await {
            if bytes == 0 { break; }
            tail.extend_from_slice(&chunk[..bytes]);
            if tail.len() > 8192 { tail.drain(..tail.len() - 8192); }
            patch(&diagnostic_owner, json!({"runtimeDiagnostic":String::from_utf8_lossy(&tail)}));
        }
    });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(300);
    let loaded = loop {
        if let Err(error) = cancelled(&token, &owner) { break Err(error); }
        if child.try_wait().map_err(io_error)?.is_some() { break Err(error("LOCAL_MODEL_START_FAILED", "本地推理服务退出，请检查模型与可用内存。", 503)); }
        if response(&base, "/health").await.is_ok() {
            break observe_model(&owner, &base, child.id().unwrap_or(0), &files).await;
        }
        if tokio::time::Instant::now() >= deadline { break Err(error("LOCAL_MODEL_LOAD_TIMEOUT", "本地模型装载尚未完成，请重试或调整模型大小。", 504)); }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    if let Err(mut error) = loaded {
        let _ = owned.terminate_and_wait().await; let _ = child.wait().await; lease.release().await;
        let _ = tokio::time::timeout(Duration::from_millis(500), &mut diagnostic).await;
        diagnostic.abort();
        error.details["runtimeDiagnostic"] = owner.state.lock().unwrap()["runtimeDiagnostic"].clone();
        binding_failure(&owner, &error);
        return Err(error);
    }
    *process = Some(Process { child, owned, lease:Some(lease), base, files, diagnostic });
    patch(&owner, json!({"state":"ready","code":Value::Null}));
    drop(process);
    if let Some(previous) = owner.monitor.lock().unwrap().take() { previous.abort(); }
    let monitored = owner.clone();
    *owner.monitor.lock().unwrap() = Some(tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            if monitored.closing.load(Ordering::Acquire) { break; }
            if monitored.state.lock().unwrap()["activeRequests"].as_u64().unwrap_or(0) > 0 { continue; }
            let Ok(mut process) = monitored.process.try_lock() else { continue; };
            let Some(process) = process.as_mut() else { break; };
            if response(&process.base, "/props").await.is_ok_and(|props| props["is_sleeping"] == true) {
                if let Some(lease) = process.lease.take() { lease.release().await; }
                patch(&monitored, json!({"state":"sleeping"}));
            }
        }
    }));
    Ok(())
}

pub async fn stop(root: &Path, id: &str) -> Result<(), ModelError> {
    let owner = engine(root, id);
    let _gate = owner.gate.try_lock().map_err(|_| error("LOCAL_MODEL_BUSY", "本地模型正在准备，请稍后停止或切换。", 409))?;
    {
        let mut state = owner.state.lock().unwrap();
        if state["activeRequests"].as_u64().unwrap_or(0) > 0 {
            return Err(error("LOCAL_MODEL_BUSY", "本地模型正在生成，请结束当前回复后再切换或删除。", 409));
        }
        state["state"] = json!("stopping");
        owner.closing.store(true, Ordering::Release);
    }
    terminate(&owner).await
}
pub struct MutationGuard(Arc<Engine>);
impl Drop for MutationGuard {
    fn drop(&mut self) { self.0.suspended.store(false, Ordering::Release); }
}
/// Keep restart admission closed until the model registry mutation has committed.
/// 注册表变更提交前保持启动准入关闭，不能在停止与删除之间重新装载旧权重。
pub async fn suspend(root: &Path, id: &str) -> Result<MutationGuard, ModelError> {
    let owner = engine(root, id);
    if owner.suspended.swap(true, Ordering::AcqRel) {
        return Err(error("LOCAL_MODEL_BUSY", "本地模型已有配置变更正在进行。", 409));
    }
    let guard = MutationGuard(owner);
    stop(root, id).await?;
    Ok(guard)
}
async fn terminate(owner: &Arc<Engine>) -> Result<(), ModelError> {
    if let Some(monitor) = owner.monitor.lock().unwrap().take() { monitor.abort(); }
    let mut process = owner.process.lock().await;
    if let Some(running) = process.as_mut() {
        running.owned.terminate_and_wait().await.map_err(map_error)?;
        let _ = running.child.wait().await;
        if let Some(lease) = running.lease.take() { lease.release().await; }
    }
    *process = None;
    patch(owner, json!({"state":"stopped","processId":Value::Null,"modelIdentityVerified":false,
        "supportsTools":Value::Null,"toolCallingMode":"unknown","toolCapabilityState":"unknown"}));
    Ok(())
}
pub async fn shutdown_for(root: &Path) {
    shutdown_matching(Some(root)).await;
}
pub async fn shutdown() {
    shutdown_matching(None).await;
}
async fn shutdown_matching(root: Option<&Path>) {
    SHUTTING_DOWN.store(true, Ordering::Release);
    let owners = ENGINES.get().and_then(|engines| engines.lock().ok())
        .map(|engines| engines.iter().filter(|((home, _), _)| root.is_none_or(|root| home == root))
            .map(|(_, engine)| engine.clone()).collect::<Vec<_>>()).unwrap_or_default();
    for owner in &owners { owner.closing.store(true, Ordering::Release); }
    for owner in owners { let _gate = owner.gate.lock().await; let _ = terminate(&owner).await; }
    // Storage migration creates a new runtime in the same process after the old lifecycle has drained.
    // 存储迁移排空旧生命周期后会在同进程创建新运行时；停机围栏不能永久阻止新根目录启动。
    SHUTTING_DOWN.store(false, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn exact_token_preflight_sends_complete_request_without_inference() {
        use axum::{Json, Router, extract::State, routing::post};
        let request = json!({"model":"fixture","messages":[
            {"role":"system","content":"Synthetic template fixture."},
            {"role":"user","content":"中文长任务 fixture"},
            {"role":"assistant","tool_calls":[{"id":"fixture-call","type":"function","function":{"name":"fixture_read","arguments":"{\"path\":\"a.rs\"}"}}]},
            {"role":"tool","tool_call_id":"fixture-call","content":"Synthetic receipt."}],
            "tools":[{"type":"function","function":{"name":"fixture_read","parameters":{"type":"object","properties":{"path":{"type":"string"}}}}}],
            "max_tokens":512,"stream":true,"chat_template_kwargs":{"enable_thinking":false}});
        let app = Router::new().route("/v1/chat/completions/input_tokens", post(
            |State(expected): State<Value>, Json(observed): Json<Value>| async move {
                assert_eq!(observed, expected);
                Json(json!({"input_tokens":12163,"object":"response.input_tokens"}))
            })).with_state(request.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
        let (_cancel, mut cancellation) = watch::channel(false);
        assert_eq!(count_input_tokens(&base, &request, &mut cancellation).await.unwrap(), 12163);
        assert!(count_request_tokens(&json!({"providerId":"ordinary-api","baseUrl":"http://127.0.0.1:1/v1"}),
            "fixture", &request, cancellation).await.unwrap().is_none());
        server.abort();
    }

    #[tokio::test]
    async fn exact_token_preflight_keeps_provider_rejection_recoverable_without_exposing_body() {
        use axum::{Json, Router, http::StatusCode, routing::post};
        let app = Router::new().route("/v1/chat/completions/input_tokens", post(|| async {
            (StatusCode::BAD_REQUEST, Json(json!({"error":{"code":400,
                "message":"request (12163 tokens) exceeds the available context size (8192 tokens), try increasing it"}})))
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
        let (_cancel, mut cancellation) = watch::channel(false);
        let rejected = count_input_tokens(&base, &json!({"messages":[]}), &mut cancellation).await.unwrap_err();
        assert_eq!(rejected.code, "MODEL_CONTEXT_LIMIT_REJECTED");
        assert_eq!(rejected.details["contextRejection"]["contextWindowTokens"], 8192);
        assert!(!rejected.details.to_string().contains("try increasing it"));
        server.abort();
    }

    #[tokio::test]
    async fn exact_token_preflight_cancels_a_pending_local_observation() {
        use axum::{Json, Router, extract::State, routing::post};
        let received = Arc::new(tokio::sync::Notify::new());
        let app = Router::new().route("/v1/chat/completions/input_tokens", post(
            |State(received): State<Arc<tokio::sync::Notify>>| async move {
                received.notify_one();
                std::future::pending::<()>().await;
                Json(json!({"input_tokens":1}))
            })).with_state(received.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
        let (cancel, mut cancellation) = watch::channel(false);
        let count = tokio::spawn(async move { count_input_tokens(&base, &json!({"messages":[]}), &mut cancellation).await });
        tokio::time::timeout(Duration::from_secs(1), received.notified()).await.unwrap();
        cancel.send(true).unwrap();
        let rejected = tokio::time::timeout(Duration::from_secs(1), count).await.unwrap().unwrap().unwrap_err();
        assert_eq!(rejected.code, "ABORT_ERR");
        server.abort();
    }

    #[test]
    fn managed_identity_and_context_adapt_without_affecting_api_models() {
        assert!(!is_managed(&json!({"providerId":"api"})));
        let connection = json!({"providerId":"kynxa-local-model-a"});
        assert_eq!(identity(&connection, "model-a").unwrap(), "model-a");
        assert!(identity(&connection, "other").is_err());
        let small = hardware_plan(&json!({"bytes":2*1024*MIB,"contextTokens":32768}),
            &json!({"cpu":{"logicalCores":8},"accounting":{"availableMemoryBytes":4*1024*MIB}}));
        let large = hardware_plan(&json!({"bytes":2*1024*MIB,"contextTokens":32768}),
            &json!({"cpu":{"logicalCores":16},"accounting":{"availableMemoryBytes":16*1024*MIB}}));
        assert!(small.0 < large.0); assert!(small.1 < large.1);
        let offloaded = residency_plan(18*1024*MIB, 16384, 8*1024*MIB, 23*1024*MIB);
        assert_eq!(offloaded.0, 16384);
        assert!(offloaded.1 < 8*1024*MIB);
        assert!(offloaded.2 >= 18*1024*MIB);
    }

    #[test]
    fn tool_and_preparation_states_require_verified_runtime_observations() {
        let native = template_status(&json!({"chat_template_caps":{"supports_tools":true,"supports_tool_calls":true}}), false);
        assert_eq!(native["supportsTools"], true);
        assert_eq!(native["toolCallingMode"], "native");
        let partial = template_status(&json!({"chat_template_caps":{"supports_tools":true}}), false);
        assert!(partial["supportsTools"].is_null());
        assert_eq!(partial["toolCallingMode"], "unknown");
        let unsupported = template_status(&json!({"chat_template_caps":{"supports_tools":true,"supports_tool_calls":false}}), true);
        assert_eq!(unsupported["supportsTools"], false);
        assert_eq!(unsupported["toolCapabilityState"], "unsupported");
        let metadata = json!({"capabilities":["text","tools","vision"]});
        assert_eq!(request_capabilities(&metadata, &partial), json!(["text","vision"]));
        let alias_only = json!({"state":"ready","model_alias":"selected","supportsTools":true});
        assert_eq!(preparation_status(&alias_only)["modelReady"], false);
        assert_eq!(request_capabilities(&metadata, &alias_only), json!(["text","vision"]));
        let verified = json!({"state":"ready","modelIdentityVerified":true,"supportsTools":true,"toolCallingMode":"native"});
        assert_eq!(preparation_status(&verified)["modelReady"], true);
        assert_eq!(preparation_status(&verified)["toolCallingMode"], "native");
        assert_eq!(request_capabilities(&metadata, &verified), json!(["text","vision","tools"]));
        let owner = Engine::default();
        patch(&owner, verified);
        let checked: Result<(), ModelError> = binding_checked(&owner, Err(error("LOCAL_MODEL_IO_FAILED", "fixture unreadable file", 500)));
        assert_eq!(checked.unwrap_err().code, "LOCAL_MODEL_IO_FAILED");
        let invalidated = owner.state.lock().unwrap().clone();
        assert_eq!(invalidated["modelIdentityVerified"], false);
        assert!(invalidated["supportsTools"].is_null());
        assert_eq!(invalidated["code"], "LOCAL_MODEL_IO_FAILED");
    }

    #[cfg(windows)]
    #[test]
    fn listener_binding_requires_the_actual_owning_process() {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        assert!(listener_owned(&base, std::process::id()).unwrap());
        assert!(!listener_owned(&base, std::process::id().wrapping_add(1)).unwrap());
    }

    #[tokio::test]
    async fn file_binding_rejects_wrong_paths_and_atomic_replacement_with_identical_size_and_time() {
        let root = std::env::temp_dir().join(format!("kynxa-model-binding-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let path = root.join("selected.gguf");
        let replacement = root.join("replacement.gguf");
        tokio::fs::write(&path, b"fixture-weights").await.unwrap();
        tokio::fs::write(&replacement, b"fixture-weights").await.unwrap();
        let selected = file_identity(&path).unwrap();
        let same_time = std::fs::FileTimes::new().set_modified(selected.modified.unwrap());
        std::fs::File::options().write(true).open(&replacement).unwrap().set_times(same_time).unwrap();
        verify_reported_file(&json!({"model_path":path}), &[selected.clone()]).await.unwrap();
        assert_eq!(verify_reported_file(&json!({"model_alias":"selected"}), &[selected.clone()]).await.unwrap_err().code,
            "LOCAL_MODEL_IDENTITY_UNCONFIRMED");
        assert_eq!(verify_reported_file(&json!({"model_path":replacement}), &[selected.clone()]).await.unwrap_err().code,
            "LOCAL_MODEL_IDENTITY_MISMATCH");
        tokio::fs::rename(&replacement, &path).await.unwrap();
        let changed = file_identity(&path).unwrap();
        assert_eq!(selected.bytes, changed.bytes);
        assert_eq!(selected.modified, changed.modified);
        assert_ne!(selected, changed);
        assert_eq!(verify_reported_file(&json!({"model_path":path}), &[selected]).await.unwrap_err().code,
            "LOCAL_MODEL_IDENTITY_MISMATCH");
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
    #[tokio::test]
    async fn active_generation_blocks_stop_until_its_guard_finishes() {
        let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let connection = json!({"providerId":"kynxa-local-busy"});
        let owner = engine(&root, "busy"); patch(&owner, json!({"state":"ready","modelIdentityVerified":true}));
        let guard = request_guard(&root, &connection, "busy").unwrap();
        assert_eq!(stop(&root, "busy").await.unwrap_err().code, "LOCAL_MODEL_BUSY");
        drop(guard); stop(&root, "busy").await.unwrap();
        assert_eq!(status(&root, "busy")["state"], "stopped");
    }
}

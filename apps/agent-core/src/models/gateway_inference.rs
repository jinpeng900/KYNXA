//! Native, offline inference preserves the pinned tokenizer, projection and vector-space contract.
//! 原生离线推理保留固定分词器、投影和向量空间合同，不启动 Node 或下载模型。

use std::{
    collections::HashMap,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use ndarray::{Array2, IxDyn};
use ort::{
    execution_providers::DirectMLExecutionProvider,
    session::{Session, SessionInputValue, run_options::RunOptions},
    value::Tensor,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokenizers::{Encoding, Tokenizer};

use super::{batches::BatchState, document_fit::DocumentFitState, gateway::ModelError, registry};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmbedKind {
    Query,
    Passage,
}

#[derive(Clone, Debug)]
pub struct InferenceOptions {
    pub device: String,
    pub device_id: Option<usize>,
    pub cpu_threads: usize,
    pub batch_size: usize,
    pub batch_token_budget: usize,
    pub activation_memory_bytes: u64,
    pub gpu_audit_memory_bytes: u64,
}
impl Default for InferenceOptions {
    fn default() -> Self {
        Self {
            device: "cpu".into(),
            device_id: None,
            cpu_threads: 1,
            batch_size: 1,
            batch_token_budget: 512,
            activation_memory_bytes: 0,
            gpu_audit_memory_bytes: 0,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingBatch {
    pub vectors: Vec<Vec<f32>>,
    pub profile_id: String,
    pub model_version: String,
    pub space_id: String,
    pub dimensions: usize,
    pub device: String,
    pub inference_status: Value,
    pub batch_measurements: Vec<Value>,
}

#[derive(Clone, Copy)]
struct Asset {
    path: &'static str,
    bytes: u64,
    sha256: &'static str,
}
const EMBEDDING_ASSETS: &[Asset] = &[
    Asset {
        path: "config.json",
        bytes: 658,
        sha256: "cb99455288675345e1a4f411438d5d0adbba5fbd3a67ea4fb03c015433b996c1",
    },
    Asset {
        path: "tokenizer_config.json",
        bytes: 443,
        sha256: "a1d6bc8734a6f635dc158508bef000f8e2e5a759c7d92f984b2c86e5ff53425b",
    },
    Asset {
        path: "special_tokens_map.json",
        bytes: 167,
        sha256: "d05497f1da52c5e09554c0cd874037a083e1dc1b9cfd48034d1c717f1afc07a7",
    },
    Asset {
        path: "tokenizer.json",
        bytes: 17082730,
        sha256: "0b44a9d7b51c3c62626640cda0e2c2f70fdacdc25bbbd68038369d14ebdf4c39",
    },
    Asset {
        path: "onnx/model_quantized.onnx",
        bytes: 118308185,
        sha256: "f80102d3f2a1229f387d3c81909990d8945513e347b0eab049f7de3c6f98c193",
    },
    Asset {
        path: "MODEL_CARD.md",
        bytes: 497538,
        sha256: "0038de97aee16258cecbad7ffda4b4febd6953e747a00e0ddbc8e6ed241e9c1c",
    },
    Asset {
        path: "CONVERSION_MODEL_CARD.md",
        bytes: 1077,
        sha256: "561a19594636657fe033f8b4427a7743b5f6f3a12f16cecc5f286feca0453245",
    },
    Asset {
        path: "LICENSE",
        bytes: 1104,
        sha256: "904dc4d8749877f1dba1cda48200d2462dccbeb7c134d5e4ef6fa75e0198c8fe",
    },
    Asset {
        path: "licenses/onnxruntime-MIT.txt",
        bytes: 1073,
        sha256: "2f07c72751aed99790b8a4869cf2311df85a860b22ded05fa22803587a48922c",
    },
    Asset {
        path: "licenses/onnxruntime-ThirdPartyNotices.txt",
        bytes: 316114,
        sha256: "8c06e8cff286a4a117b3b246a4c7da68428a144af757823db50e3d6520941ec6",
    },
    Asset {
        path: "licenses/sqlite-vec-MIT.txt",
        bytes: 1068,
        sha256: "6ce72bbe12d975bd5286e5ab0a064c069693300c47bccbc57bec18485f1621ea",
    },
];
static DLL: OnceLock<PathBuf> = OnceLock::new();
static INITIALIZE_GATE: Mutex<()> = Mutex::new(());
static SESSIONS: OnceLock<Mutex<HashMap<String, Arc<Mutex<LoadedModel>>>>> = OnceLock::new();
static TOKENIZERS: OnceLock<Mutex<HashMap<PathBuf, Arc<Tokenizer>>>> = OnceLock::new();

struct LoadedModel {
    tokenizer: Arc<Tokenizer>,
    session: Session,
    audit: Value,
    batch_state: super::worker::WorkerState,
}

/// The additional CPU audit graph uses the existing resource estimate, and must have its own granted RAM.
/// 额外 CPU 审计图复用现有资源估计，必须持有单独批准的 RAM 授额。
pub fn qualification_request(cpu_threads: usize) -> Result<Value, ModelError> {
    let assets = EMBEDDING_ASSETS;
    crate::resources::calculate("resources.inferenceRequest",&json!({"profile":{"dtype":"q8","dimensions":384,"hiddenSize":384,"attentionHeads":12,"maxInputTokens":512,
        "files":assets.iter().map(|asset|json!({"path":asset.path,"bytes":asset.bytes})).collect::<Vec<_>>()},"backend":"cpu","sequenceTokens":512,"batchSize":1,"requestedCpuThreads":cpu_threads}))
        .map_err(ModelError::native)
}

fn error(code: &str) -> ModelError {
    ModelError::new(code, "本地模型推理未能完成。", 503)
}
fn check_cancelled(cancel: &AtomicBool) -> Result<(), ModelError> {
    if cancel.load(Ordering::Acquire) {
        Err(ModelError::cancelled())
    } else {
        Ok(())
    }
}

/// Initialize once from the installed DLL; an incompatible or absent runtime never silently falls back.
/// 从随包 DLL 初始化一次；运行库不兼容或缺失时不能静默回退到另一运行路径。
pub fn initialize_runtime(dll_path: &Path) -> Result<(), ModelError> {
    let _gate = INITIALIZE_GATE
        .lock()
        .map_err(|_| error("INFERENCE_RUNTIME_LOCK_FAILED"))?;
    let path = dll_path
        .canonicalize()
        .map_err(|_| error("INFERENCE_RUNTIME_UNAVAILABLE"))?;
    if let Some(existing) = DLL.get() {
        return if existing == &path {
            Ok(())
        } else {
            Err(error("INFERENCE_RUNTIME_CHANGED"))
        };
    }
    if !path.is_file() {
        return Err(error("INFERENCE_RUNTIME_UNAVAILABLE"));
    }
    let spelling = path
        .to_str()
        .ok_or_else(|| error("INFERENCE_RUNTIME_INVALID_PATH"))?;
    ort::init_from(spelling)
        .with_name("KYNXA")
        .commit()
        .map_err(|_| error("INFERENCE_RUNTIME_INCOMPATIBLE"))?;
    DLL.set(path)
        .map_err(|_| error("INFERENCE_RUNTIME_CHANGED"))
}

fn verify_assets(root: &Path, assets: &[Asset], cancel: &AtomicBool) -> Result<(), ModelError> {
    for asset in assets {
        check_cancelled(cancel)?;
        let path = root.join(asset.path);
        let metadata =
            std::fs::symlink_metadata(&path).map_err(|_| error("EMBEDDING_ASSET_MISSING"))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() != asset.bytes
        {
            return Err(error("EMBEDDING_ASSET_INVALID"));
        }
        let mut file = File::open(path).map_err(|_| error("EMBEDDING_ASSET_INVALID"))?;
        let mut digest = Sha256::new();
        let mut buffer = [0u8; 65536];
        loop {
            check_cancelled(cancel)?;
            let count = file
                .read(&mut buffer)
                .map_err(|_| error("EMBEDDING_ASSET_INVALID"))?;
            if count == 0 {
                break;
            }
            digest.update(&buffer[..count]);
        }
        if format!("{:x}", digest.finalize()) != asset.sha256 {
            return Err(error("EMBEDDING_ASSET_INVALID"));
        }
    }
    Ok(())
}

fn validate_options(options: &InferenceOptions) -> Result<(), ModelError> {
    if !matches!(options.device.as_str(), "cpu" | "dml")
        || options.device == "dml" && options.device_id.is_none()
        || options.device_id.is_some_and(|id| id > i32::MAX as usize)
        || options.cpu_threads == 0
        || options.batch_size == 0
        || options.batch_token_budget < 512
    {
        return Err(error("INFERENCE_INVALID_BUDGET"));
    }
    Ok(())
}

fn loaded(
    root: &Path,
    options: &InferenceOptions,
    cancel: &AtomicBool,
) -> Result<Arc<Mutex<LoadedModel>>, ModelError> {
    check_cancelled(cancel)?;
    validate_options(options)?;
    if DLL.get().is_none() {
        return Err(error("INFERENCE_RUNTIME_NOT_INITIALIZED"));
    }
    let root = root
        .canonicalize()
        .map_err(|_| error("EMBEDDING_ASSET_MISSING"))?;
    let key = format!(
        "{}:{}:{}:{}:{:?}",
        root.display(),
        false,
        options.device,
        options.cpu_threads,
        options.device_id
    );
    let mut cache = SESSIONS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| error("INFERENCE_RUNTIME_LOCK_FAILED"))?;
    if let Some(model) = cache.get(&key) {
        return Ok(model.clone());
    }
    // At most one session per model/device is retained; a thread-budget change retires the old cache entry.
    // 每种模型/设备最多保留一个会话；线程授额变更时淘汰旧缓存，活动请求仍持有自身句柄。
    let prefix = format!("{}:{}:{}:", root.display(), false, options.device);
    if cache
        .iter()
        .any(|(name, model)| name.starts_with(&prefix) && Arc::strong_count(model) > 1)
    {
        return Err(error("INFERENCE_RESOURCE_BUSY"));
    }
    cache.retain(|name, _| !name.starts_with(&prefix));
    if cache.len() >= 4 {
        return Err(error("INFERENCE_SESSION_BUDGET_FULL"));
    }
    verify_assets(
        &root,
        EMBEDDING_ASSETS,
        cancel,
    )?;
    let tokenizer = load_tokenizer(&root, cancel)?;
    let mut session = if options.device == "dml" {
        let requested = qualification_request(options.cpu_threads)?;
        if options.gpu_audit_memory_bytes
            < requested["memoryBytes"]
                .as_f64()
                .unwrap_or(f64::INFINITY)
                .ceil() as u64
        {
            return Err(error("INFERENCE_GPU_AUDIT_RESOURCE_REQUIRED"));
        }
        qualified_session(&root, tokenizer.clone(), options, cancel)?
    } else {
        LoadedModel {
            tokenizer: tokenizer.clone(),
            session: session(&root, options, None)?,
            audit: json!({"device":"cpu","gpuValidated":false}),
            batch_state: Default::default(),
        }
    };
    session.batch_state.apply("models.lifecycle.worker.init",&json!({"kind":"embedding","profile":{"maxInputTokens":512,"embeddingSpaceId":embedding_metadata(&options.device)?["spaceId"]},"budget":options_budget(options)})).map_err(ModelError::native)?;
    session
        .batch_state
        .apply(
            "models.lifecycle.worker.loaded",
            &json!({"backend":session.audit}),
        )
        .map_err(ModelError::native)?;
    check_cancelled(cancel)?;
    let model = Arc::new(Mutex::new(session));
    cache.insert(key, model.clone());
    Ok(model)
}

fn session(
    root: &Path,
    options: &InferenceOptions,
    profiling: Option<&Path>,
) -> Result<Session, ModelError> {
    let mut builder = Session::builder()
        .map_err(|_| error("INFERENCE_RUNTIME_UNAVAILABLE"))?
        .with_intra_threads(options.cpu_threads)
        .map_err(|_| error("INFERENCE_INVALID_BUDGET"))?
        .with_inter_threads(1)
        .map_err(|_| error("INFERENCE_INVALID_BUDGET"))?
        .with_parallel_execution(false)
        .map_err(|_| error("INFERENCE_RUNTIME_UNAVAILABLE"))?;
    if options.device == "dml" {
        builder = builder
            .with_memory_pattern(false)
            .map_err(|_| error("INFERENCE_RUNTIME_UNAVAILABLE"))?
            .with_execution_providers([DirectMLExecutionProvider::default()
                .with_device_id(options.device_id.unwrap() as i32)
                .build()
                .error_on_failure()])
            .map_err(|_| error("INFERENCE_DEVICE_UNAVAILABLE"))?;
    }
    if let Some(path) = profiling {
        builder = builder
            .with_profiling(path)
            .map_err(|_| error("INFERENCE_GPU_AUDIT_FAILED"))?;
    }
    builder
        .commit_from_file(root.join("onnx/model_quantized.onnx"))
        .map_err(|_| error("INFERENCE_MODEL_LOAD_FAILED"))
}

struct AuditDirectory(PathBuf);
impl Drop for AuditDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn qualified_session(
    root: &Path,
    tokenizer: Arc<Tokenizer>,
    options: &InferenceOptions,
    cancel: &AtomicBool,
) -> Result<LoadedModel, ModelError> {
    let directory =
        std::env::temp_dir().join(format!("kynxa-inference-audit-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).map_err(|_| error("INFERENCE_GPU_AUDIT_FAILED"))?;
    let directory = AuditDirectory(directory);
    let inputs = vec![
        tokenizer.encode("query: 如何安全读取临时文件？", true),
        tokenizer.encode("passage: This is a fixed multilingual model qualification probe.", true),
    ];
    let encodings = inputs
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| error("EMBEDDING_TOKENIZER_INVALID"))?;
    let cpu_options = InferenceOptions {
        device: "cpu".into(),
        device_id: None,
        batch_size: 1,
        ..options.clone()
    };
    let cpu = LoadedModel {
        tokenizer: tokenizer.clone(),
        session: session(root, &cpu_options, None)?,
        audit: Value::Null,
        batch_state: Default::default(),
    };
    let probe = |model: &LoadedModel| -> Result<Vec<f32>, ModelError> {
        let mut values = Vec::new();
        for encoding in &encodings {
            values
                .extend(execute(model, std::slice::from_ref(encoding), cancel)?.remove(0));
        }
        Ok(values)
    };
    let reference = probe(&cpu)?;
    check_cancelled(cancel)?;
    let audited = LoadedModel {
        tokenizer: tokenizer.clone(),
        session: session(root, options, Some(&directory.0.join("partition")))?,
        audit: Value::Null,
        batch_state: Default::default(),
    };
    let actual = probe(&audited)?;
    let compatibility = super::inference::calculate(
        "models.inference.qualifiedOutputs",
        &json!({"reference":reference,"actual":actual,"dimensions":384}),
    ).map_err(ModelError::native)?;
    if compatibility.is_null() || compatibility == false {
        return Err(error("INFERENCE_GPU_OUTPUT_INCOMPATIBLE"));
    }
    let repeated = probe(&audited)?;
    if super::inference::calculate(
        "models.inference.compatibleOutputs",
        &json!({"reference":actual,"actual":repeated}),
    )
    .map_err(ModelError::native)?
        != true
    {
        return Err(error("INFERENCE_GPU_OUTPUT_INCOMPATIBLE"));
    }
    let trace_path = PathBuf::from(
        audited
            .session
            .end_profiling()
            .map_err(|_| error("INFERENCE_GPU_AUDIT_FAILED"))?,
    );
    drop(audited);
    if !trace_path.starts_with(&directory.0)
        || std::fs::metadata(&trace_path)
            .map_err(|_| error("INFERENCE_GPU_AUDIT_FAILED"))?
            .len()
            > 16 * 1024 * 1024
    {
        return Err(error("INFERENCE_GPU_AUDIT_FAILED"));
    }
    let events: Value = serde_json::from_slice(
        &std::fs::read(trace_path).map_err(|_| error("INFERENCE_GPU_AUDIT_FAILED"))?,
    )
    .map_err(|_| error("INFERENCE_GPU_AUDIT_FAILED"))?;
    let partition = super::inference::calculate(
        "models.inference.auditPartition",
        &json!({"device":"dml","events":events}),
    )
    .map_err(ModelError::native)?;
    if partition["usefulGpuCompute"] != true {
        return Err(error("INFERENCE_GPU_AUDIT_FAILED"));
    }
    drop(cpu);
    check_cancelled(cancel)?;
    let mut model = LoadedModel {
        tokenizer,
        session: session(root, options, None)?,
        audit: Value::Null,
        batch_state: Default::default(),
    };
    let final_probe = probe(&model)?;
    if super::inference::calculate(
        "models.inference.compatibleOutputs",
        &json!({"reference":actual,"actual":final_probe}),
    )
    .map_err(ModelError::native)?
        != true
    {
        return Err(error("INFERENCE_GPU_OUTPUT_INCOMPATIBLE"));
    }
    model.audit = json!({"device":"dml","deviceId":options.device_id,"gpuValidated":true,"executionMode":"hybrid","cpuOperatorFallback":true,"operatorAudit":partition,"compatibility":compatibility});
    Ok(model)
}

fn load_tokenizer(
    root: &Path,
    cancel: &AtomicBool,
) -> Result<Arc<Tokenizer>, ModelError> {
    check_cancelled(cancel)?;
    let root = root
        .canonicalize()
        .map_err(|_| error("EMBEDDING_ASSET_MISSING"))?;
    let mut cache = TOKENIZERS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| error("INFERENCE_RUNTIME_LOCK_FAILED"))?;
    if let Some(tokenizer) = cache.get(&root) {
        return Ok(tokenizer.clone());
    }
    if cache.len() >= 2 {
        return Err(error("INFERENCE_TOKENIZER_BUDGET_FULL"));
    }
    verify_assets(
        &root,
        &(EMBEDDING_ASSETS)[..4],
        cancel,
    )?;
    let mut tokenizer = Tokenizer::from_file(root.join("tokenizer.json"))
        .map_err(|_| error("EMBEDDING_TOKENIZER_INVALID"))?;
    tokenizer.with_padding(None);
    tokenizer
        .with_truncation(None)
        .map_err(|_| error("EMBEDDING_TOKENIZER_INVALID"))?;
    let tokenizer = Arc::new(tokenizer);
    cache.insert(root, tokenizer.clone());
    Ok(tokenizer)
}

/// Report success only after the last native session owner is gone; resident leases can then be released.
/// 最后一个原生会话拥有者退役后才报告成功，此时才能归还驻留内存租约。
pub fn retire_idle(root: &Path, device: &str) -> Result<bool, ModelError> {
    let root = root
        .canonicalize()
        .map_err(|_| error("EMBEDDING_ASSET_MISSING"))?;
    let mut cache = SESSIONS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| error("INFERENCE_RUNTIME_LOCK_FAILED"))?;
    let prefix = format!("{}:", root.display());
    let names: Vec<String> = cache
        .keys()
        .filter(|name| name.starts_with(&prefix) && name.rsplitn(4, ':').nth(2) == Some(device))
        .cloned()
        .collect();
    let mut retired = true;
    for name in names {
        if Arc::strong_count(cache.get(&name).unwrap()) == 1 {
            drop(cache.remove(&name));
        } else {
            retired = false;
        }
    }
    if let Some(tokenizers) = TOKENIZERS.get() {
        let mut tokenizers = tokenizers
            .lock()
            .map_err(|_| error("INFERENCE_RUNTIME_LOCK_FAILED"))?;
        if tokenizers
            .get(&root)
            .is_some_and(|tokenizer| Arc::strong_count(tokenizer) == 1)
        {
            tokenizers.remove(&root);
        } else if tokenizers.contains_key(&root) {
            retired = false;
        }
    }
    Ok(retired)
}

pub fn shutdown_idle() -> Result<bool, ModelError> {
    let mut cache = SESSIONS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| error("INFERENCE_RUNTIME_LOCK_FAILED"))?;
    cache.retain(|_, model| Arc::strong_count(model) > 1);
    if let Some(tokenizers) = TOKENIZERS.get() {
        let mut tokenizers = tokenizers
            .lock()
            .map_err(|_| error("INFERENCE_RUNTIME_LOCK_FAILED"))?;
        tokenizers.retain(|_, tokenizer| Arc::strong_count(tokenizer) > 1);
        return Ok(cache.is_empty() && tokenizers.is_empty());
    }
    Ok(cache.is_empty())
}

pub fn embedding_metadata(device: &str) -> Result<Value, ModelError> {
    let gpu = device == "dml";
    if !gpu && device != "cpu" {
        return Err(error("INFERENCE_DEVICE_UNAVAILABLE"));
    }
    let version = format!(
        "761b726dd34fb83930e26aab4e9ac3899aa1fa78:q8{}",
        if gpu { ":dml-hybrid-v1" } else { "" }
    );
    let profile =
        json!({"modelId":"Xenova/multilingual-e5-small","modelVersion":version,"dimensions":384});
    let manifest: Vec<String> = EMBEDDING_ASSETS
        .iter()
        .map(|a| {
            format!(
                "{{\"path\":{},\"bytes\":{},\"sha256\":{}}}",
                json!(a.path),
                a.bytes,
                json!(a.sha256)
            )
        })
        .collect();
    let descriptor = registry::calculate("models.registry.register", &json!({"kind":"embedding","profile":profile,
        "implementationId":if gpu {"transformers-onnx-dml-hybrid-q8-v1"} else {"transformers-onnx-feature-extraction-v1"},
        "inputProjectionJson":"{\"version\":\"e5-prefixed-mean-l2-v1\",\"queryPrefix\":\"query: \",\"documentPrefix\":\"passage: \",\"pooling\":\"mean\",\"normalize\":true}",
        "manifestEntriesJson":manifest})).map_err(ModelError::native)?;
    Ok(
        json!({"profileId":if gpu {"builtin-multilingual-dml-q8"} else {"builtin-multilingual"},"modelVersion":version,
        "spaceId":descriptor["embeddingSpaceId"],"embeddingSpaceId":descriptor["embeddingSpaceId"],"dimensions":384,"device":device,
        "modelId":profile["modelId"],"implementationId":descriptor["implementationId"],"inputProjectionVersion":descriptor["inputProjectionVersion"],
        "modelAssetSignature":descriptor["modelAssetSignature"],"assetSignature":descriptor["assetSignature"]}),
    )
}

fn tensors(
    encodings: &[Encoding],
    session: &Session,
) -> Result<
    (
        Vec<(&'static str, SessionInputValue<'static>)>,
        Vec<i64>,
        usize,
    ),
    ModelError,
> {
    let width = encodings.iter().map(Encoding::len).max().unwrap_or(0);
    if width == 0 || width > 512 {
        return Err(error("EMBEDDING_CONTEXT_TOO_LONG"));
    }
    let mut ids = vec![1i64; encodings.len() * width];
    let mut mask = vec![0i64; ids.len()];
    let mut types = vec![0i64; ids.len()];
    for (row, encoding) in encodings.iter().enumerate() {
        for (column, id) in encoding.get_ids().iter().enumerate() {
            ids[row * width + column] = i64::from(*id);
            mask[row * width + column] = i64::from(encoding.get_attention_mask()[column]);
            types[row * width + column] = i64::from(encoding.get_type_ids()[column]);
        }
    }
    let make = |data: Vec<i64>| {
        Tensor::from_array(
            Array2::from_shape_vec((encodings.len(), width), data)
                .map_err(|_| error("INFERENCE_INVALID_SHAPE"))?,
        )
        .map_err(|_| error("INFERENCE_INVALID_SHAPE"))
    };
    let mut inputs: Vec<(&'static str, SessionInputValue<'static>)> = vec![
        ("input_ids", make(ids)?.into()),
        ("attention_mask", make(mask.clone())?.into()),
    ];
    if session
        .inputs
        .iter()
        .any(|input| input.name == "token_type_ids")
    {
        inputs.push(("token_type_ids", make(types)?.into()));
    }
    Ok((inputs, mask, width))
}

/// Stop the native graph on cancellation and join the watcher before dropping RunOptions.
/// 取消时停止原生图；释放 RunOptions 前等待观察线程退出。
fn execute(
    model: &LoadedModel,
    encodings: &[Encoding],
    cancel: &AtomicBool,
) -> Result<Vec<Vec<f32>>, ModelError> {
    check_cancelled(cancel)?;
    let (inputs, mask, width) = tensors(encodings, &model.session)?;
    let options = RunOptions::new().map_err(|_| error("INFERENCE_RUNTIME_UNAVAILABLE"))?;
    let done = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let watcher = scope.spawn(|| {
            while !done.load(Ordering::Acquire) {
                if cancel.load(Ordering::Acquire) {
                    let _ = options.terminate();
                    break;
                }
                std::thread::park_timeout(Duration::from_millis(5));
            }
        });
        let result = (|| {
            let output = model
                .session
                .run_with_options(inputs, &options)
                .map_err(|failure| {
                    if cancel.load(Ordering::Acquire) {
                        ModelError::cancelled()
                    } else if super::inference::calculate(
                        "models.inference.memoryPressure",
                        &json!({"message":failure.to_string()}),
                    )
                    .unwrap_or(Value::Null)
                        == true
                    {
                        error("INFERENCE_ALLOCATION_FAILED")
                    } else {
                        error("INFERENCE_FAILED")
                    }
                })?;
            let tensor = output["last_hidden_state"]
                .try_extract_tensor::<f32>()
                .map_err(|_| error("INFERENCE_INVALID_OUTPUT"))?;
            if tensor.shape() != [encodings.len(), width, 384] {
                return Err(error("EMBEDDING_INVALID_DIMENSIONS"));
            }
            let mut vectors = Vec::with_capacity(encodings.len());
            for row in 0..encodings.len() {
                let count = mask[row * width..(row + 1) * width].iter().sum::<i64>() as f64;
                if count <= 0.0 {
                    return Err(error("EMBEDDING_INVALID_OUTPUT"));
                }
                let mut vector = vec![0f32; 384];
                for dimension in 0..384 {
                    let mut sum = 0f64;
                    for column in 0..width {
                        sum += f64::from(tensor[IxDyn(&[row, column, dimension])])
                            * mask[row * width + column] as f64;
                    }
                    vector[dimension] = (sum / count) as f32;
                }
                normalize(&mut vector)?;
                vectors.push(vector);
            }
            Ok(vectors)
        })();
        done.store(true, Ordering::Release);
        watcher.thread().unpark();
        watcher
            .join()
            .map_err(|_| error("INFERENCE_CANCELLATION_FAILED"))?;
        check_cancelled(cancel)?;
        result
    })
}

fn normalize(vector: &mut [f32]) -> Result<(), ModelError> {
    if vector.iter().any(|value| !value.is_finite()) {
        return Err(error("EMBEDDING_INVALID_OUTPUT"));
    }
    // Match the former Float32 tensor: round the pooled values and the norm before division.
    // 对齐原 Float32 张量：池化值与范数先取 Float32，再执行归一化除法。
    let mut squared = 0f32;
    for value in vector.iter() {
        squared = (f64::from(squared) + f64::from(*value).powi(2)) as f32;
    }
    let norm = f64::from(squared).sqrt() as f32;
    if !norm.is_finite() || norm == 0.0 {
        return Err(error("EMBEDDING_INVALID_OUTPUT"));
    }
    for value in vector {
        *value = (f64::from(*value) / f64::from(norm)) as f32;
    }
    Ok(())
}

fn execute_batches(
    model: &LoadedModel,
    encodings: &[Encoding],
    options: &InferenceOptions,
    cancel: &AtomicBool,
) -> Result<(Vec<Vec<f32>>, Vec<Value>), ModelError> {
    let mut state = BatchState::default();
    state.apply("models.lifecycle.batchInit",&json!({"count":encodings.len(),"tokenLengths":encodings.iter().map(Encoding::len).collect::<Vec<_>>(),"batchSize":options.batch_size,
        "batchTokenBudget":options.batch_token_budget,"activationMemoryBytes":options.activation_memory_bytes,"hiddenSize":384,"attentionHeads":12,"cpuThreads":options.cpu_threads})).map_err(ModelError::native)?;
    let mut output = vec![Vec::new(); encodings.len()];
    let mut measurements = Vec::new();
    loop {
        check_cancelled(cancel)?;
        let indexes = state
            .apply("models.lifecycle.batchNext", &json!({}))
            .map_err(ModelError::native)?;
        let Some(indexes) = indexes.as_array() else {
            return Ok((output, measurements));
        };
        let subset: Vec<Encoding> = indexes
            .iter()
            .map(|index| encodings[index.as_u64().unwrap() as usize].clone())
            .collect();
        let started = std::time::Instant::now();
        match execute(model, &subset, cancel) {
            Ok(values) => {
                for (index, value) in indexes.iter().zip(values) {
                    output[index.as_u64().unwrap() as usize] = value;
                }
                measurements.push(
                    state
                        .apply(
                            "models.lifecycle.batchSuccess",
                            &json!({"latencyMs":started.elapsed().as_secs_f64()*1000.0}),
                        )
                        .map_err(ModelError::native)?,
                );
            }
            Err(failure) if failure.code == "INFERENCE_ALLOCATION_FAILED" => {
                let pressure = state
                    .apply("models.lifecycle.batchPressure", &json!({}))
                    .map_err(ModelError::native)?;
                if pressure["terminal"] == true {
                    return Err(failure);
                }
                measurements.push(pressure);
            }
            Err(failure) => return Err(failure),
        }
    }
}

fn options_budget(options: &InferenceOptions) -> Value {
    json!({"device":options.device,"deviceId":options.device_id,"cpuThreads":options.cpu_threads,"batchSize":options.batch_size,
        "batchTokenBudget":options.batch_token_budget,"activationMemoryBytes":options.activation_memory_bytes,"hiddenSize":384,"attentionHeads":12})
}

fn audit_batch_budget(
    model: &mut LoadedModel,
    options: &InferenceOptions,
    cancel: &AtomicBool,
) -> Result<InferenceOptions, ModelError> {
    model
        .batch_state
        .apply(
            "models.lifecycle.worker.begin",
            &json!({"budget":options_budget(options),"sessionLoaded":true}),
        )
        .map_err(ModelError::native)?;
    let plan = model
        .batch_state
        .apply("models.lifecycle.worker.auditBegin", &json!({}))
        .map_err(ModelError::native)?;
    let mut applied = options.clone();
    applied.batch_size = plan["budget"]["batchSize"].as_u64().unwrap_or(1) as usize;
    if plan["cached"] == true {
        return Ok(applied);
    }
    let probes = [
        "passage: How do I reset an account password?",
        "passage: 如何重置账户密码？",
        "passage: function resolveEvidence(sourceId) { return sources.get(sourceId); }",
    ];
    let encodings = probes
        .iter()
        .map(|text| model.tokenizer.encode(*text, true))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| error("EMBEDDING_TOKENIZER_INVALID"))?;
    let reference = if plan["reference"].is_array() {
        serde_json::from_value::<Vec<Vec<f32>>>(plan["reference"].clone())
            .map_err(|_| error("EMBEDDING_SPACE_INCOMPATIBLE"))?
    } else {
        execute_batches(
            model,
            &encodings,
            &InferenceOptions {
                batch_size: 1,
                ..applied.clone()
            },
            cancel,
        )?
        .0
    };
    let mut first = true;
    loop {
        let (actual, measurements) = execute_batches(model, &encodings, &applied, cancel)?;
        let expected: Vec<f32> = reference.iter().flatten().copied().collect();
        let actual: Vec<f32> = actual.into_iter().flatten().collect();
        let mut status = super::inference::calculate(
            "models.inference.qualifiedOutputs",
            &json!({"reference":expected,"actual":actual,"dimensions":384}),
        )
        .map_err(ModelError::native)?;
        let qualified = status.is_object();
        if status.is_null() {
            status = json!({});
        }
        status["version"] = json!("normalized-batch-qualification-v1");
        status["qualified"] = json!(qualified);
        status["requestedBatchSize"] = json!(options.batch_size);
        status["strictlyEquivalent"] = super::inference::calculate(
            "models.inference.compatibleOutputs",
            &json!({"reference":expected,"actual":actual}),
        )
        .map_err(ModelError::native)?;
        status["measurements"] = json!(measurements);
        status["retrievalQualityValidated"] = json!(false);
        let adjustment = model
            .batch_state
            .apply(
                if first {
                    "models.lifecycle.worker.auditResult"
                } else {
                    "models.lifecycle.worker.auditRetry"
                },
                &json!({"reference":reference,"qualified":qualified}),
            )
            .map_err(ModelError::native)?;
        if adjustment.is_null() {
            model.audit = model
                .batch_state
                .apply(
                    "models.lifecycle.worker.auditFinish",
                    &json!({"status":status}),
                )
                .map_err(ModelError::native)?;
            return Ok(applied);
        }
        applied.batch_size = adjustment["batchSize"].as_u64().unwrap_or(1) as usize;
        first = false;
    }
}

pub fn embed_blocking(
    root: &Path,
    texts: &[String],
    kind: EmbedKind,
    cancel: Arc<AtomicBool>,
) -> Result<EmbeddingBatch, ModelError> {
    embed_with_options(root, texts, kind, &InferenceOptions::default(), cancel)
}

/// Use the caller's admitted hardware budget without a second scheduler.
/// 使用调用方已准入的硬件额度，不另建调度器。
pub fn embed_with_options(
    root: &Path,
    texts: &[String],
    kind: EmbedKind,
    options: &InferenceOptions,
    cancel: Arc<AtomicBool>,
) -> Result<EmbeddingBatch, ModelError> {
    let metadata = embedding_metadata(&options.device)?;
    let model = loaded(root, options, &cancel)?;
    let mut model = model
        .lock()
        .map_err(|_| error("INFERENCE_RUNTIME_LOCK_FAILED"))?;
    let prefix = if kind == EmbedKind::Query {
        "query: "
    } else {
        "passage: "
    };
    let mut encodings = Vec::with_capacity(texts.len());
    for (index, text) in texts.iter().enumerate() {
        check_cancelled(&cancel)?;
        let encoding = model
            .tokenizer
            .encode(format!("{prefix}{text}"), true)
            .map_err(|_| error("EMBEDDING_TOKENIZER_INVALID"))?;
        if encoding.len() > 512 {
            let mut failure = error("EMBEDDING_CONTEXT_TOO_LONG");
            failure.details =
                json!({"index":index,"tokenCount":encoding.len(),"maxInputTokens":512});
            return Err(failure);
        }
        encodings.push(encoding);
    }
    let applied = audit_batch_budget(&mut model, options, &cancel)?;
    let (vectors, batch_measurements) =
        execute_batches(&model, &encodings, &applied, &cancel)?;
    Ok(EmbeddingBatch {
        vectors,
        profile_id: metadata["profileId"].as_str().unwrap().into(),
        model_version: metadata["modelVersion"].as_str().unwrap().into(),
        space_id: metadata["spaceId"].as_str().unwrap().into(),
        dimensions: 384,
        device: options.device.clone(),
        inference_status: model.audit.clone(),
        batch_measurements,
    })
}

/// Use the existing native fitting state with the real pinned tokenizer; offsets remain UTF-16 offsets.
/// 现有原生分块状态使用真实固定分词器计量；偏移继续采用 UTF-16 单位。
pub fn fit_documents(
    root: &Path,
    documents: &[Value],
    cancel: Arc<AtomicBool>,
) -> Result<Vec<Value>, ModelError> {
    let tokenizer = load_tokenizer(root, &cancel)?;
    let mut output = Vec::new();
    for (index, document) in documents.iter().enumerate() {
        let text = document["text"]
            .as_str()
            .ok_or_else(|| error("EMBEDDING_INVALID_INPUT"))?;
        let context = document["context"].as_str().unwrap_or("");
        let units: Vec<u16> = text.encode_utf16().collect();
        let mut fit = DocumentFitState::default();
        fit.apply("models.lifecycle.fit.init",&json!({"textUtf16":units,"index":index,"maxInputTokens":512,"contextCharacters":context.encode_utf16().count()})).map_err(ModelError::native)?;
        loop {
            check_cancelled(&cancel)?;
            let plan = fit
                .apply("models.lifecycle.fit.next", &json!({}))
                .map_err(ModelError::native)?;
            if let Some(code) = plan["errorCode"].as_str() {
                let mut failure = error(code);
                failure.details = plan["details"].clone();
                return Err(failure);
            }
            if plan["done"] == true {
                output.push(plan["result"].clone());
                break;
            }
            let start = plan["start"].as_u64().unwrap_or(0) as usize;
            let end = plan["end"].as_u64().unwrap_or(0) as usize;
            let body = String::from_utf16_lossy(&units[start..end]);
            let input = format!("passage: {context}{body}");
            let tokens = tokenizer
                .encode(input, true)
                .map_err(|_| error("EMBEDDING_TOKENIZER_INVALID"))?
                .len();
            let result = fit
                .apply("models.lifecycle.fit.measured", &json!({"tokens":tokens}))
                .map_err(ModelError::native)?;
            if let Some(code) = result["errorCode"].as_str() {
                let mut failure = error(code);
                failure.details = result["details"].clone();
                return Err(failure);
            }
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn spaces_are_fixed_and_devices_cannot_share_vectors() {
        let cpu = embedding_metadata("cpu").unwrap();
        let gpu = embedding_metadata("dml").unwrap();
        assert_eq!(cpu["dimensions"], 384);
        assert_ne!(cpu["spaceId"], gpu["spaceId"]);
        assert_eq!(cpu["profileId"], "builtin-multilingual");
        assert!(embedding_metadata("cuda").is_err());
        assert!(cpu.get("gpuValidated").is_none());
    }
    #[test]
    fn mean_normalization_rejects_invalid_output() {
        let mut vector = vec![3.0, 4.0];
        normalize(&mut vector).unwrap();
        assert_eq!(vector, vec![0.6, 0.8]);
        assert!(normalize(&mut [f32::NAN]).is_err());
        assert!(normalize(&mut [0.0]).is_err());
    }

    /// This explicitly selected test uses six fixed offline inputs, never the user's data or a model download.
    /// 显式执行此测试仅使用六条固定离线输入，不读取用户数据或下载模型。
    #[test]
    #[ignore = "requires the pinned offline ORT DLL and model assets"]
    fn native_cpu_parity_against_frozen_sdk() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let fixture = std::env::var_os("KYNXA_EMBEDDING_BASELINE")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                repository
                    .join("artifacts/development/rust-gateway-20261012/embedding-cpu-baseline.json")
            });
        let baseline: Value = serde_json::from_slice(&std::fs::read(&fixture).unwrap()).unwrap();
        let dll = std::env::var_os("KYNXA_ORT_DLL")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(baseline["dll"].as_str().unwrap()));
        assert_eq!(
            format!("{:x}", Sha256::digest(std::fs::read(&dll).unwrap())),
            baseline["dllSha256"].as_str().unwrap()
        );
        initialize_runtime(&dll).unwrap();
        let root = PathBuf::from(baseline["modelRoot"].as_str().unwrap());
        let cancel = Arc::new(AtomicBool::new(false));
        let tokenizer = load_tokenizer(&root, &cancel).unwrap();
        let mut reports = Vec::new();
        for sample in baseline["samples"].as_array().unwrap() {
            let encoding = tokenizer
                .encode(sample["input"].as_str().unwrap(), true)
                .unwrap();
            assert_eq!(json!(encoding.get_ids()), sample["tokenIds"]);
            assert_eq!(
                json!(encoding.get_attention_mask()),
                sample["attentionMask"]
            );
            let kind = if sample["kind"] == "query" {
                EmbedKind::Query
            } else {
                EmbedKind::Passage
            };
            let result = embed_blocking(
                &root,
                &[sample["text"].as_str().unwrap().into()],
                kind,
                cancel.clone(),
            )
            .unwrap();
            let actual = &result.vectors[0];
            let reference = sample["vector"].as_array().unwrap();
            assert_eq!(actual.len(), 384);
            assert_eq!(reference.len(), 384);
            let (mut difference, mut dot, mut a_norm, mut r_norm) =
                (0.0_f64, 0.0_f64, 0.0_f64, 0.0_f64);
            for (a, r) in actual.iter().zip(reference) {
                let a = f64::from(*a);
                let r = r.as_f64().unwrap();
                difference = difference.max((a - r).abs());
                dot += a * r;
                a_norm += a * a;
                r_norm += r * r;
            }
            let cosine = dot / (a_norm * r_norm).sqrt();
            reports.push(json!({"kind":sample["kind"],"tokenCount":encoding.len(),"maxAbs":difference,"cosine":cosine,"nativeNorm":a_norm.sqrt(),"baselineNorm":r_norm.sqrt()}));
            assert!(
                difference <= 1e-5,
                "native Float32 projection differs: {difference}"
            );
            assert!(
                cosine >= 0.999999,
                "native vector direction differs: {cosine}"
            );
            assert!((a_norm.sqrt() - 1.0).abs() <= 1e-5);
        }
        drop(tokenizer);
        assert!(retire_idle(&root, "cpu").unwrap());
        let output = fixture.with_file_name("embedding-cpu-parity.json");
        std::fs::write(output,serde_json::to_vec_pretty(&json!({"schemaVersion":1,"dllSha256":baseline["dllSha256"],"samples":reports,"passed":true})).unwrap()).unwrap();
    }
}

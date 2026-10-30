//! Local GGUF library metadata owns paths and downloads; engine execution remains a separate owner.
//! 本地 GGUF 库负责路径、目录和下载；推理引擎执行仍由独立所有者管理。

use super::{gateway::ModelError, gateway_config::ModelConfigStore};
use crate::storage::managed_files;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::{BTreeMap, HashMap}, path::{Path, PathBuf}, sync::{Arc, OnceLock}};
use tokio::sync::{Mutex, watch};

#[path = "gateway_local_download.rs"]
mod download;

const MANIFEST_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelFile {
    name: String,
    url: String,
    bytes: u64,
    sha256: String,
    role: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelSpec {
    id: String,
    name: String,
    family: String,
    category: String,
    parameters: String,
    quantization: String,
    download_bytes: u64,
    recommended_ram_gb: u64,
    recommended_vram_gb: u64,
    capabilities: Vec<String>,
    context_tokens: u64,
    license: String,
    source_url: String,
    #[serde(default)]
    revision: String,
    files: Vec<ModelFile>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct InstalledModel {
    spec: ModelSpec,
    directory: String,
    path: String,
    mmproj_path: Option<String>,
    owned: bool,
    enabled: bool,
    status: String,
    downloaded_bytes: u64,
    error: Option<String>,
    port: u16,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    schema_version: u32,
    enabled: bool,
    install_directory: String,
    load_policy: String,
    models: BTreeMap<String, InstalledModel>,
}

impl Default for Manifest {
    fn default() -> Self {
        Self { schema_version: 1, enabled: true, install_directory: "Local".into(),
            load_policy: "auto".into(), models: BTreeMap::new() }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum DownloadAction { Running, Paused, Cancelled }

#[derive(Clone)]
struct DownloadJob {
    command: watch::Sender<DownloadAction>,
    done: watch::Receiver<bool>,
}

struct LibraryState { manifest: Manifest, jobs: HashMap<String, DownloadJob>, downloads: HashMap<String, download::DownloadStatus> }
struct Library { state: Mutex<LibraryState>, operations: Mutex<()> }
static LIBRARIES: OnceLock<Mutex<HashMap<PathBuf, Arc<Library>>>> = OnceLock::new();

fn error(code: &str, message: impl Into<String>, status: u16) -> ModelError {
    ModelError::new(code, &message.into(), status)
}

fn storage_error(error: crate::storage::ContractError) -> ModelError {
    ModelError::new(error.code(), error.message(), error.status_code())
}

fn catalog() -> &'static Vec<ModelSpec> {
    static CATALOG: OnceLock<Vec<ModelSpec>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        #[derive(Deserialize)] struct Catalog { models: Vec<ModelSpec> }
        serde_json::from_str::<Catalog>(include_str!("local_model_catalog.json"))
            .expect("checked built-in local model catalog").models
    })
}

fn catalog_model(spec: &ModelSpec) -> Option<&'static ModelSpec> {
    catalog().iter().find(|current| current.id == spec.id && current.files.len() == spec.files.len()
        && current.files.iter().all(|file| spec.files.iter().any(|installed| {
            file.name == installed.name && file.role == installed.role && file.bytes == installed.bytes
                && !file.sha256.is_empty() && file.sha256 == installed.sha256
        })))
}

/// Refresh corrected capability metadata only when the catalog still names the same installed files.
/// 仅对相同文件身份同步已修正的能力；不替换用户路径、启用状态、下载来源或上下文设置。
fn refresh_catalog_capabilities(manifest: &mut Manifest) -> bool {
    let mut changed = false;
    for model in manifest.models.values_mut() {
        if let Some(current) = catalog_model(&model.spec) {
            if model.spec.capabilities != current.capabilities {
                model.spec.capabilities = current.capabilities.clone();
                changed = true;
            }
        }
    }
    changed
}

fn tool_template(spec: &ModelSpec) -> Option<&'static str> {
    let current = catalog_model(spec)?;
    (current.family == "Qwen2.5-VL" && current.capabilities.iter().any(|value| value == "tools"))
        .then_some("qwen25-vl-tools-v1")
}

fn identifier(value: &Value) -> Result<&str, ModelError> {
    value.as_str().filter(|id| !id.is_empty() && id.len() <= 28
        && id.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'))
        .ok_or_else(|| error("INVALID_LOCAL_MODEL", "本地模型标识无效。", 400))
}

fn resolve(home: &Path, stored: &str) -> PathBuf {
    let path = Path::new(stored);
    if path.is_absolute() { path.to_path_buf() } else { home.join(path) }
}

fn stored(home: &Path, path: &Path) -> String {
    path.strip_prefix(home).unwrap_or(path).to_string_lossy().into_owned()
}

fn provider_id(id: &str) -> String { format!("kynxa-local-{id}") }

async fn library(home: &Path) -> Result<Arc<Library>, ModelError> {
    let libraries = LIBRARIES.get_or_init(Default::default);
    if let Some(library) = libraries.lock().await.get(home).cloned() { return Ok(library); }
    let root = home.to_path_buf();
    let manifest = tokio::task::spawn_blocking(move || {
        let bytes = managed_files::read(&root, &root.join("local-models.json"), MANIFEST_BYTES)
            .map_err(storage_error)?;
        let mut manifest: Manifest = match bytes {
            Some(bytes) => serde_json::from_slice(&bytes).map_err(|_| error("CORRUPT_LOCAL_MODELS", "本地模型清单损坏，原文件已保留。", 409))?,
            None => Manifest::default(),
        };
        if manifest.schema_version != 1 { return Err(error("LOCAL_MODELS_VERSION_UNSUPPORTED", "本地模型清单版本不受支持。", 409)); }
        for model in manifest.models.values_mut() {
            if model.status == "downloading" || model.status == "queued" {
                model.status = "paused".into();
                model.error = Some("上次下载已中断，可从已有断点继续。".into());
            }
        }
        if refresh_catalog_capabilities(&mut manifest) {
            let bytes = serde_json::to_vec_pretty(&manifest)
                .map_err(|_| error("LOCAL_MODEL_STORAGE_FAILED", "本地模型清单无法编码。", 500))?;
            managed_files::atomic_write(&root, &root.join("local-models.json"), &bytes, MANIFEST_BYTES)
                .map_err(storage_error)?;
        }
        Ok::<_, ModelError>(manifest)
    }).await.map_err(|_| error("LOCAL_MODEL_STORAGE_FAILED", "本地模型清单读取任务未完成。", 500))??;
    let instance = Arc::new(Library { state: Mutex::new(LibraryState { manifest, jobs: HashMap::new(), downloads: HashMap::new() }), operations: Mutex::new(()) });
    Ok(libraries.lock().await.entry(home.to_path_buf()).or_insert(instance).clone())
}

async fn persist(home: &Path, manifest: &Manifest) -> Result<(), ModelError> {
    let bytes = serde_json::to_vec_pretty(manifest).map_err(|_| error("LOCAL_MODEL_STORAGE_FAILED", "本地模型清单无法编码。", 500))?;
    let root = home.to_path_buf();
    tokio::task::spawn_blocking(move || managed_files::atomic_write(&root, &root.join("local-models.json"), &bytes, MANIFEST_BYTES))
        .await.map_err(|_| error("LOCAL_MODEL_STORAGE_FAILED", "本地模型清单保存任务未完成。", 500))?
        .map_err(storage_error)
}

async fn update(home: &Path, library: &Library, change: impl FnOnce(&mut Manifest)) -> Result<(), ModelError> {
    let mut state = library.state.lock().await;
    let mut next = state.manifest.clone();
    change(&mut next);
    persist(home, &next).await?;
    state.manifest = next;
    Ok(())
}

fn safe_leaf(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', '\0', ':'])
}

async fn prepare_directory(path: &Path) -> Result<(), ModelError> {
    if !path.is_absolute() || path.components().any(|part| matches!(part, std::path::Component::ParentDir)) {
        return Err(error("INVALID_LOCAL_MODEL_PATH", "模型目录必须是绝对路径，且不能含上级跳转。", 400));
    }
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let root = path.ancestors().find(|ancestor| ancestor.is_dir())
            .ok_or_else(|| error("INVALID_LOCAL_MODEL_PATH", "模型目录的磁盘不可用。", 400))?;
        managed_files::inspect(root, &path, true, false, u64::MAX).map_err(storage_error)?;
        Ok::<_, ModelError>(())
    }).await.map_err(|_| error("LOCAL_MODEL_STORAGE_FAILED", "模型目录准备任务未完成。", 500))?
}

async fn own_directory(home: &Path, model: &InstalledModel) -> Result<PathBuf, ModelError> {
    let directory = resolve(home, &model.directory);
    prepare_directory(&directory).await?;
    let marker = directory.join(".kynxa-local-model.json");
    let root = directory.clone();
    let id = model.spec.id.clone();
    tokio::task::spawn_blocking(move || {
        if let Some(bytes) = managed_files::read(&root, &marker, 4096).map_err(storage_error)? {
            let value: Value = serde_json::from_slice(&bytes).map_err(|_| error("LOCAL_MODEL_OWNERSHIP_CONFLICT", "模型目录归属记录无效。", 409))?;
            if value["id"] != id { return Err(error("LOCAL_MODEL_OWNERSHIP_CONFLICT", "目标目录属于其他模型。", 409)); }
        } else {
            let bytes = serde_json::to_vec(&json!({"schemaVersion":1,"id":id})).unwrap();
            managed_files::atomic_write(&root, &marker, &bytes, 4096).map_err(storage_error)?;
        }
        Ok::<_, ModelError>(())
    }).await.map_err(|_| error("LOCAL_MODEL_STORAGE_FAILED", "模型目录登记任务未完成。", 500))??;
    Ok(directory)
}

async fn validate_gguf(path: &Path) -> Result<u64, ModelError> {
    use tokio::io::AsyncReadExt;
    let metadata = tokio::fs::metadata(path).await.map_err(|_| error("LOCAL_MODEL_FILE_MISSING", "模型文件不存在。", 404))?;
    if !metadata.is_file() || metadata.len() < 24 { return Err(error("INVALID_GGUF", "请选择完整的 GGUF 模型文件。", 400)); }
    let mut file = tokio::fs::File::open(path).await.map_err(|_| error("LOCAL_MODEL_READ_FAILED", "模型文件无法读取。", 500))?;
    let mut header = [0u8; 8];
    file.read_exact(&mut header).await.map_err(|_| error("INVALID_GGUF", "GGUF 文件头不完整。", 400))?;
    if &header[..4] != b"GGUF" || ![2, 3].contains(&u32::from_le_bytes(header[4..].try_into().unwrap())) {
        return Err(error("INVALID_GGUF", "文件不是受支持的 GGUF v2/v3 模型。", 400));
    }
    Ok(metadata.len())
}

async fn snapshot(home: &Path, library: &Library) -> Value {
    let (manifest, downloads) = { let state = library.state.lock().await; (state.manifest.clone(), state.downloads.clone()) };
    let mut models = catalog().iter().map(|spec| (spec.id.clone(), spec.clone())).collect::<BTreeMap<_, _>>();
    models.extend(manifest.models.iter().map(|(id, model)| (id.clone(), model.spec.clone())));
    let entries = models.into_iter().map(|(id, spec)| {
        let record = manifest.models.get(&id);
        let exists = record.is_some_and(|model| resolve(home, &model.path).is_file());
        let installed = record.is_some_and(|model| model.status == "ready") && exists;
        json!({"id":id,"name":spec.name,"family":spec.family,"category":spec.category,
            "parameters":spec.parameters,"quantization":spec.quantization,"downloadBytes":spec.download_bytes,
            "recommendedRamGb":spec.recommended_ram_gb,"recommendedVramGb":spec.recommended_vram_gb,
            "capabilities":spec.capabilities,"license":spec.license,"sourceUrl":spec.source_url,
            "installed":installed,"enabled":manifest.enabled && record.is_some_and(|model| model.enabled),
            "status":record.map(|model| if model.status=="ready" && !exists {"missing"}else{model.status.as_str()}).unwrap_or("available"),
            "path":record.map(|model|resolve(home,&model.path)),
            "downloadedBytes":record.map_or(0,|model|model.downloaded_bytes),"download":downloads.get(&id),
            "error":record.and_then(|model|model.error.clone())})
    }).collect::<Vec<_>>();
    json!({"enabled":manifest.enabled,"installDirectory":resolve(home,&manifest.install_directory),
        "loadPolicy":manifest.load_policy,"models":entries})
}

async fn register_provider(home: &Path, model: &InstalledModel, enabled: bool) -> Result<(), ModelError> {
    let store = ModelConfigStore::new(home);
    let id = provider_id(&model.spec.id);
    if enabled {
        let title=model.spec.name.chars().scan(0usize,|units,character|{
            *units+=character.len_utf16();(*units<=72).then_some(character)
        }).collect::<String>();
        store.save(json!({"providerId":id,"displayName":format!("本地 · {title}"),
            "baseUrl":format!("http://127.0.0.1:{}/v1",model.port),"apiKey":"","protocol":"openai-completions",
            "models":[model.spec.id],"contextWindowTokens":model.spec.context_tokens})).await?;
    } else { store.remove(&id).await?; }
    Ok(())
}

async fn stop_download(home: &Path, library: &Library, id: &str, action: DownloadAction) -> Result<(), ModelError> {
    let job = library.state.lock().await.jobs.get(id).cloned();
    if let Some(mut job) = job {
        let _ = job.command.send(action);
        while !*job.done.borrow() {
            if job.done.changed().await.is_err() { break; }
        }
    } else {
        update(home, library, |manifest| {
            if let Some(model) = manifest.models.get_mut(id) {
                if model.status != "ready" { model.status = if action==DownloadAction::Cancelled {"cancelled"}else{"paused"}.into(); }
            }
        }).await?;
    }
    Ok(())
}

async fn cleanup_owned(home: &Path, model: &InstalledModel, remove_complete: bool) -> Result<(), ModelError> {
    if !model.owned { return Ok(()); }
    let directory = resolve(home, &model.directory);
    let root = directory.clone();
    let model = model.clone();
    tokio::task::spawn_blocking(move || {
        let Some(bytes) = managed_files::read(&root,&root.join(".kynxa-local-model.json"),4096).map_err(storage_error)? else {return Ok(());};
        let marker: Value = serde_json::from_slice(&bytes).map_err(|_|error("LOCAL_MODEL_OWNERSHIP_CONFLICT","模型目录归属记录无效。",409))?;
        if marker["id"] != model.spec.id {return Err(error("LOCAL_MODEL_OWNERSHIP_CONFLICT","模型目录归属已改变，未删除文件。",409));}
        for file in &model.spec.files {
            if !safe_leaf(&file.name) {return Err(error("INVALID_LOCAL_MODEL_PATH","模型文件名无效。",400));}
            let mut names = if remove_complete {vec![file.name.clone()]}else{vec![]};
            for entry in std::fs::read_dir(&root).map_err(|_|error("LOCAL_MODEL_DELETE_FAILED","模型断点目录无法读取。",500))? {
                let entry = entry.map_err(|_|error("LOCAL_MODEL_DELETE_FAILED","模型断点条目无法读取。",500))?;
                if let Some(name) = entry.file_name().to_str().filter(|name|download::owns_temporary_name(&file.name,name)) {
                    names.push(name.to_owned());
                }
            }
            for name in names {
                let path=root.join(name);
                if managed_files::inspect(&root,&path,false,true,u64::MAX).map_err(storage_error)? {
                    std::fs::remove_file(path).map_err(|error|self::error("LOCAL_MODEL_DELETE_FAILED",format!("模型文件删除失败：{error}"),500))?;
                }
            }
        }
        if remove_complete {
            std::fs::remove_file(root.join(".kynxa-local-model.json")).map_err(|_|error("LOCAL_MODEL_DELETE_FAILED","模型归属记录删除失败。",500))?;
            match std::fs::remove_dir(&root) {Ok(())=>{},Err(error) if error.kind()==std::io::ErrorKind::DirectoryNotEmpty=>{},Err(error)=>return Err(self::error("LOCAL_MODEL_DELETE_FAILED",format!("模型目录删除失败：{error}"),500))}
        }
        Ok::<_,ModelError>(())
    }).await.map_err(|_|error("LOCAL_MODEL_DELETE_FAILED","模型清理线程结束。",500))?
}

fn new_record(home:&Path, directory:&Path, spec:ModelSpec)->InstalledModel {
    let path=directory.join(&spec.files.iter().find(|file|file.role=="model").unwrap().name);
    let mmproj_path=spec.files.iter().find(|file|file.role=="mmproj").map(|file|stored(home,&directory.join(&file.name)));
    InstalledModel {spec,directory:stored(home,directory),path:stored(home,&path),mmproj_path,owned:true,
        enabled:false,status:"queued".into(),downloaded_bytes:0,error:None,port:0}
}

async fn start_download(home:&Path, library:&Arc<Library>, id:&str)->Result<(),ModelError> {
    let mut state=library.state.lock().await;
    if state.jobs.contains_key(id) {return Ok(());}
    let record=state.manifest.models.get_mut(id).ok_or_else(||error("LOCAL_MODEL_NOT_FOUND","本地模型不存在。",404))?;
    if record.status=="ready" {return Ok(());}
    record.status="queued".into();record.error=None;
    let record=record.clone();
    persist(home,&state.manifest).await?;
    let (command,receive)=watch::channel(DownloadAction::Running);
    let (done,completion)=watch::channel(false);
    state.jobs.insert(id.into(),DownloadJob{command,done:completion});
    let root=home.to_path_buf();let library=library.clone();
    tokio::spawn(async move {
        let result=download::run(&root,&library,&record,receive).await;
        let mut state=library.state.lock().await;
        if let Some(model)=state.manifest.models.get_mut(&record.spec.id) {
            match result {
                Ok(DownloadAction::Running)=>{model.status="ready".into();model.downloaded_bytes=model.spec.download_bytes;model.error=None;},
                Ok(DownloadAction::Paused)=>{model.status="paused".into();model.error=None;},
                Ok(DownloadAction::Cancelled)=>{model.status="cancelled".into();model.downloaded_bytes=0;model.error=None;},
                Err(error)=>{model.status="failed".into();model.error=Some(format!("{}: {}",error.code,error.message));}
            }
        }
        if let Err(error)=persist(&root,&state.manifest).await {eprintln!("local model download state: {}",error.code);}
        state.jobs.remove(&record.spec.id);
        state.downloads.remove(&record.spec.id);
        let _=done.send(true);
    });
    Ok(())
}

/// Routes stop affected engines before mutation; the manager does not repeat that work.
/// 路由在变更前停止受影响引擎；管理器不重复停止。
pub async fn affected_models(home:&Path,path:&str,body:&Value)->Result<Vec<String>,ModelError> {
    let library=library(home).await?;let state=library.state.lock().await;
    if path=="/api/local-models/delete" || path=="/api/local-models/enable" && body["enabled"]==false {
        return Ok(vec![identifier(&body["id"])?.into()]);
    }
    if path=="/api/local-models/settings" {
        let moving=body["moveExisting"]==true && body["installDirectory"].as_str().is_some_and(|path|
            resolve(home,path)!=resolve(home,&state.manifest.install_directory));
        return Ok(state.manifest.models.iter().filter(|(_,model)|body["enabled"]==false || moving && model.owned)
            .map(|(id,_)|id.clone()).collect());
    }
    Ok(vec![])
}

pub async fn enabled_model(home:&Path,id:&str)->Result<Value,ModelError> {
    let library=library(home).await?;let state=library.state.lock().await;
    let model=state.manifest.models.get(id).filter(|model|state.manifest.enabled && model.enabled && model.status=="ready")
        .ok_or_else(||error("LOCAL_MODEL_DISABLED","请先安装并启用此本地模型。",409))?;
    let path=resolve(home,&model.path);
    if !path.is_file() {return Err(error("LOCAL_MODEL_FILE_MISSING","本地模型文件已移动或不存在。",404));}
    if model.mmproj_path.as_ref().is_some_and(|path|!resolve(home,path).is_file()) {
        return Err(error("LOCAL_MODEL_PROJECTOR_MISSING","视觉模型的 mmproj 文件不存在。",404));
    }
    Ok(json!({"id":id,"name":model.spec.name,"path":path,
        "modelFiles":model.spec.files.iter().filter(|file|file.role=="model")
            .map(|file|resolve(home,&model.directory).join(&file.name)).collect::<Vec<_>>(),
        "mmprojPath":model.mmproj_path.as_ref().map(|path|resolve(home,path)),"contextTokens":model.spec.context_tokens,
        "port":model.port,"loadPolicy":state.manifest.load_policy,"bytes":model.spec.download_bytes,
        "capabilities":model.spec.capabilities,"toolTemplate":tool_template(&model.spec)}))
}

/// Pause and settle all owned downloads before storage migration or gateway exit.
/// 存储迁移或网关退出前暂停并等候所有自有下载实际退场，再撤出此根目录缓存。
pub async fn shutdown(data_home:&Path)->Result<(),ModelError> {
    let Some(libraries)=LIBRARIES.get() else {return Ok(());};
    let Some(library)=libraries.lock().await.get(data_home).cloned() else {return Ok(());};
    let _operation=library.operations.lock().await;
    let jobs=library.state.lock().await.jobs.keys().cloned().collect::<Vec<_>>();
    for id in jobs {stop_download(data_home,&library,&id,DownloadAction::Paused).await?;}
    libraries.lock().await.remove(data_home);
    Ok(())
}

async fn import_model(home:&Path,library:&Library,body:&Value)->Result<(),ModelError> {
    let path=PathBuf::from(body["path"].as_str().unwrap_or(""));
    if !path.is_absolute() {return Err(error("INVALID_LOCAL_MODEL_PATH","请选择模型文件的绝对路径。",400));}
    let mut selected=vec![(path.clone(),"model")];
    if let Some(projector)=body["mmprojPath"].as_str().filter(|path|!path.is_empty()) {
        if !Path::new(projector).is_absolute() {return Err(error("INVALID_LOCAL_MODEL_PATH","请选择视觉投影文件的绝对路径。",400));}
        selected.push((PathBuf::from(projector),"mmproj"));
    }
    let split=regex::Regex::new(r"^(.*)-00001-of-(\d{5})\.gguf$").unwrap();
    if let Some(captures)=path.file_name().and_then(|name|name.to_str()).and_then(|name|split.captures(name)) {
        let count=captures[2].parse::<usize>().unwrap_or(0);
        if !(1..=256).contains(&count) {return Err(error("INVALID_GGUF_SPLIT","GGUF 分片数量无效。",400));}
        for index in 2..=count {selected.push((path.with_file_name(format!("{}-{index:05}-of-{count:05}.gguf",&captures[1])),"model"));}
    }
    let mut files=Vec::new();
    for (file,role) in &selected {
        let bytes=validate_gguf(file).await?;
        let name=file.file_name().and_then(|name|name.to_str()).filter(|name|safe_leaf(name))
            .ok_or_else(||error("INVALID_LOCAL_MODEL_PATH","模型文件名无效。",400))?;
        if files.iter().any(|entry:&ModelFile|entry.name==name) {return Err(error("LOCAL_MODEL_FILE_CONFLICT","模型与投影文件名重复。",400));}
        files.push(ModelFile{name:name.into(),url:String::new(),bytes,sha256:String::new(),role:(*role).into()});
    }
    let id=format!("import-{}",&uuid::Uuid::new_v4().simple().to_string()[..16]);
    let spec=ModelSpec{id:id.clone(),name:body["name"].as_str().filter(|name|!name.trim().is_empty()).unwrap_or_else(||path.file_stem().and_then(|name|name.to_str()).unwrap_or("GGUF")).chars().take(256).collect(),
        family:"Imported".into(),category:if selected.iter().any(|(_,role)|*role=="mmproj"){"vision"}else{"general"}.into(),parameters:"自定义".into(),quantization:"GGUF".into(),
        download_bytes:files.iter().map(|file|file.bytes).sum(),recommended_ram_gb:0,recommended_vram_gb:0,
        capabilities:if selected.iter().any(|(_,role)|*role=="mmproj"){vec!["text".into(),"vision".into()]}else{vec!["text".into()]},
        context_tokens:8192,license:"user-supplied".into(),source_url:String::new(),revision:String::new(),files};
    let directory=resolve(home,&library.state.lock().await.manifest.install_directory).join(&id);
    let mut record=new_record(home,&directory,spec);
    if body["copyToLibrary"]!=false {
        own_directory(home,&record).await?;
        record.status="importing".into();
        update(home,library,|manifest|{manifest.models.insert(id.clone(),record.clone());}).await?;
        for ((source,_),file) in selected.iter().zip(&record.spec.files) {
            if let Err(failure)=tokio::fs::copy(source,directory.join(&file.name)).await {
                let detail=format!("模型复制失败，已复制文件可在此条目删除：{failure}");
                update(home,library,|manifest|{if let Some(model)=manifest.models.get_mut(&id){model.status="failed".into();model.error=Some(detail.clone());}}).await?;
                return Err(error("LOCAL_MODEL_IMPORT_FAILED",detail,500));
            }
        }
    } else {
        record.owned=false;record.path=stored(home,&path);
        record.mmproj_path=selected.iter().find(|(_,role)|*role=="mmproj").map(|(path,_)|stored(home,path));
        record.directory=stored(home,path.parent().unwrap());
    }
    record.status="ready".into();record.downloaded_bytes=record.spec.download_bytes;
    update(home,library,|manifest|{manifest.models.insert(id,record);}).await
}

async fn settings(home:&Path,library:&Library,body:&Value)->Result<(),ModelError> {
    let current=library.state.lock().await.manifest.clone();
    let new_directory=body["installDirectory"].as_str().map(|path|resolve(home,path)).unwrap_or_else(||resolve(home,&current.install_directory));
    prepare_directory(&new_directory).await?;
    let policy=body["loadPolicy"].as_str().unwrap_or(&current.load_policy);
    if !["auto","on-demand","preload"].contains(&policy) {return Err(error("INVALID_LOCAL_MODEL_POLICY","加载策略应为 auto、on-demand 或 preload。",400));}
    let moving=body["moveExisting"]==true && new_directory!=resolve(home,&current.install_directory);
    if moving {
        for (id,model) in &current.models {
            if !model.owned {continue;}
            stop_download(home,library,id,DownloadAction::Paused).await?;
            let model=library.state.lock().await.manifest.models.get(id).cloned().unwrap();
            let old=resolve(home,&model.directory);
            let target=new_directory.join(id);
            if old==target {continue;}
            let mut next=model.clone();next.directory=stored(home,&target);
            next.path=stored(home,&target.join(model.spec.files.iter().find(|file|file.role=="model").map(|file|file.name.as_str()).unwrap_or("model.gguf")));
            next.mmproj_path=model.spec.files.iter().find(|file|file.role=="mmproj").map(|file|stored(home,&target.join(&file.name)));
            own_directory(home,&next).await?;
            for file in &model.spec.files {
                for name in [file.name.clone(),format!("{}.part",file.name)] {
                    let source=old.join(&name);
                    if source.is_file() {
                        if let Err(failure)=tokio::fs::copy(&source,target.join(name)).await {
                            cleanup_owned(home,&next,true).await?;
                            return Err(error("LOCAL_MODEL_MOVE_FAILED",format!("模型迁移失败，原文件仍保留：{failure}"),500));
                        }
                    }
                }
            }
            update(home,library,|manifest|{manifest.models.insert(id.clone(),next);}).await?;
            cleanup_owned(home,&model,true).await?;
        }
    }
    update(home,library,|manifest|{
        manifest.install_directory=stored(home,&new_directory);manifest.load_policy=policy.into();
        if let Some(enabled)=body["enabled"].as_bool(){manifest.enabled=enabled;}
    }).await?;
    let current=library.state.lock().await.manifest.clone();
    for model in current.models.values() {register_provider(home,model,current.enabled && model.enabled).await?;}
    Ok(())
}

pub async fn handle(method:&str,path:&str,body:Value,data_home:&Path)->Option<Result<(u16,Value),ModelError>> {
    if path!="/api/local-models" && !path.starts_with("/api/local-models/") {return None;}
    Some(async {
        let library=library(data_home).await?;
        if method=="GET" && path=="/api/local-models" {return Ok((200,snapshot(data_home,&library).await));}
        if method!="POST" {return Err(error("METHOD_NOT_ALLOWED","本地模型管理不支持此请求方法。",405));}
        let _operation=library.operations.lock().await;
        let status=match path {
            "/api/local-models/settings"=>{settings(data_home,&library,&body).await?;200},
            "/api/local-models/import"=>{import_model(data_home,&library,&body).await?;200},
            "/api/local-models/install"=>{
                let id=identifier(&body["id"])?;
                let spec=catalog().iter().find(|model|model.id==id).cloned().ok_or_else(||error("LOCAL_MODEL_NOT_FOUND","推荐目录中没有此模型。",404))?;
                if !library.state.lock().await.manifest.models.contains_key(id) {
                    let directory=resolve(data_home,&library.state.lock().await.manifest.install_directory).join(id);
                    let record=new_record(data_home,&directory,spec);own_directory(data_home,&record).await?;
                    update(data_home,&library,|manifest|{manifest.models.insert(id.into(),record);}).await?;
                }
                start_download(data_home,&library,id).await?;202
            },
            "/api/local-models/enable"=>{
                let id=identifier(&body["id"])?;let enabled=body["enabled"].as_bool().ok_or_else(||error("INVALID_LOCAL_MODEL","缺少启用状态。",400))?;
                let mut record=library.state.lock().await.manifest.models.get(id).cloned().ok_or_else(||error("LOCAL_MODEL_NOT_FOUND","请先安装或导入模型。",404))?;
                if enabled {validate_gguf(&resolve(data_home,&record.path)).await?;
                    if let Some(projector)=&record.mmproj_path {validate_gguf(&resolve(data_home,projector)).await?;}
                    if record.status!="ready" {return Err(error("LOCAL_MODEL_NOT_READY","模型尚未下载完成。",409));}
                    if record.port==0 {record.port=std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST,0)).map_err(|_|error("LOCAL_MODEL_PORT_UNAVAILABLE","无法分配本地模型端口。",503))?.local_addr().unwrap().port();}}
                record.enabled=enabled;register_provider(data_home,&record,enabled).await?;
                update(data_home,&library,|manifest|{if enabled {manifest.enabled=true;}manifest.models.insert(id.into(),record);}).await?;200
            },
            "/api/local-models/delete"=>{
                let id=identifier(&body["id"])?;stop_download(data_home,&library,id,DownloadAction::Paused).await?;
                let record=library.state.lock().await.manifest.models.get(id).cloned().ok_or_else(||error("LOCAL_MODEL_NOT_FOUND","本地模型不存在。",404))?;
                register_provider(data_home,&record,false).await?;cleanup_owned(data_home,&record,true).await?;
                update(data_home,&library,|manifest|{manifest.models.remove(id);}).await?;200
            },
            "/api/local-models/download"=>{
                let id=identifier(&body["id"])?;
                match body["action"].as_str().unwrap_or("") {
                    "pause"=>stop_download(data_home,&library,id,DownloadAction::Paused).await?,
                    "cancel"=>{stop_download(data_home,&library,id,DownloadAction::Cancelled).await?;
                        let record=library.state.lock().await.manifest.models.get(id).cloned().ok_or_else(||error("LOCAL_MODEL_NOT_FOUND","本地模型不存在。",404))?;
                        if record.status!="ready" {cleanup_owned(data_home,&record,true).await?;}},
                    "resume"=>start_download(data_home,&library,id).await?,
                    _=>return Err(error("INVALID_LOCAL_MODEL_ACTION","请选择 pause、resume 或 cancel。",400)),
                }202
            },
            _=>return Err(error("NOT_FOUND","本地模型接口不存在。",404)),
        };
        Ok((status,snapshot(data_home,&library).await))
    }.await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, extract::State, http::{HeaderMap, StatusCode}, response::IntoResponse, routing::get};
    use sha2::{Digest, Sha256};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    async fn call(home:&Path,path:&str,body:Value)->Value {
        handle("POST",path,body,home).await.unwrap().unwrap().1
    }

    #[tokio::test]
    async fn catalog_upgrade_persists_capabilities_without_replacing_installed_settings() {
        let home = std::env::temp_dir().join(format!("kynxa-local-capabilities-{}", uuid::Uuid::new_v4()));
        let current = catalog().iter().find(|model| model.id == "q25vl-7b-q4km").unwrap();
        let directory = home.join("User Models").join(&current.id);
        tokio::fs::create_dir_all(&directory).await.unwrap();
        let mut installed = new_record(&home, &directory, current.clone());
        installed.spec.capabilities = vec!["text".into(), "vision".into()];
        installed.spec.context_tokens = 24576;
        installed.spec.files[0].url = "https://mirror.example.invalid/model.gguf".into();
        installed.enabled = true;
        installed.status = "ready".into();
        installed.port = 43123;
        installed.downloaded_bytes = installed.spec.download_bytes;
        tokio::fs::write(resolve(&home, &installed.path), b"GGUF fixture weights").await.unwrap();
        tokio::fs::write(resolve(&home, installed.mmproj_path.as_ref().unwrap()), b"GGUF fixture projector").await.unwrap();
        let mut manifest = Manifest { install_directory:"User Models".into(), load_policy:"on-demand".into(), ..Manifest::default() };
        manifest.models.insert(current.id.clone(), installed);
        let mut different = new_record(&home, &home.join("Other"), catalog()[0].clone());
        different.status = "ready".into();
        different.spec.capabilities = vec!["text".into()];
        different.spec.files[0].sha256 = "changed-artifact".into();
        manifest.models.insert(different.spec.id.clone(), different);
        let mut expected = serde_json::to_value(&manifest).unwrap();
        expected["models"][&current.id]["spec"]["capabilities"] = json!(current.capabilities);
        persist(&home, &manifest).await.unwrap();

        let metadata = enabled_model(&home, &current.id).await.unwrap();
        assert_eq!(metadata["toolTemplate"], "qwen25-vl-tools-v1");
        assert_eq!(metadata["capabilities"], json!(current.capabilities));
        assert_eq!(metadata["contextTokens"], 24576);
        let persisted: Value = serde_json::from_slice(&tokio::fs::read(home.join("local-models.json")).await.unwrap()).unwrap();
        assert_eq!(persisted, expected);
        shutdown(&home).await.unwrap();
        let restored = enabled_model(&home, &current.id).await.unwrap();
        assert_eq!(restored["capabilities"], metadata["capabilities"]);
        shutdown(&home).await.unwrap();
        tokio::fs::remove_dir_all(home).await.unwrap();
    }

    #[tokio::test]
    async fn local_model_library_persists_import_enable_move_delete_and_resumes_download() {
        assert!(catalog().len()>=25);
        assert!(catalog().iter().all(|model|model.id.len()<=28 && model.files.iter().all(|file|file.sha256.len()==64)));
        let root=std::env::temp_dir().join(format!("kynxa-local-models-{}",uuid::Uuid::new_v4()));
        let home=root.join("Models");tokio::fs::create_dir_all(&home).await.unwrap();
        let mut bytes=b"GGUF".to_vec();bytes.extend_from_slice(&3u32.to_le_bytes());bytes.resize(1048,0);
        let source=root.join("tiny.gguf");tokio::fs::write(&source,&bytes).await.unwrap();
        ModelConfigStore::new(&home).save(json!({"providerId":"fixture-api","displayName":"API fixture",
            "baseUrl":"https://example.invalid/v1","apiKey":"fixture-not-a-real-key","protocol":"openai-completions","models":["fixture"]})).await.unwrap();
        let imported=call(&home,"/api/local-models/import",json!({"path":source,"name":"Tiny fixture"})).await;
        let selected=imported["models"].as_array().unwrap().iter().find(|model|model["family"]=="Imported").unwrap();
        let id=selected["id"].as_str().unwrap().to_owned();
        assert_eq!(selected["installed"],true);
        assert!(!selected["capabilities"].as_array().unwrap().contains(&json!("tools")));
        call(&home,"/api/local-models/enable",json!({"id":id,"enabled":true})).await;
        assert!(ModelConfigStore::new(&home).connection_for("fixture-api").await.unwrap().is_some());
        let configured=enabled_model(&home,&id).await.unwrap();
        assert!(configured["port"].as_u64().unwrap()>0);
        let changed=home.join("Relocated");
        assert_eq!(affected_models(&home,"/api/local-models/settings",&json!({"installDirectory":changed,"moveExisting":true})).await.unwrap(),vec![id.clone()]);
        call(&home,"/api/local-models/settings",json!({"installDirectory":changed,"moveExisting":true})).await;
        let manifest:Value=serde_json::from_slice(&tokio::fs::read(home.join("local-models.json")).await.unwrap()).unwrap();
        assert_eq!(manifest["installDirectory"],"Relocated");
        assert!(!Path::new(manifest["models"][&id]["path"].as_str().unwrap()).is_absolute());
        shutdown(&home).await.unwrap();
        let configured=enabled_model(&home,&id).await.unwrap();
        assert!(configured["path"].as_str().unwrap().contains("Relocated"));
        call(&home,"/api/local-models/enable",json!({"id":id,"enabled":false})).await;
        assert!(enabled_model(&home,&id).await.is_err());
        call(&home,"/api/local-models/delete",json!({"id":id})).await;
        assert!(source.is_file());
        let external=call(&home,"/api/local-models/import",json!({"path":source,"copyToLibrary":false})).await;
        let id=external["models"].as_array().unwrap().iter().find(|model|model["family"]=="Imported").unwrap()["id"].clone();
        call(&home,"/api/local-models/delete",json!({"id":id})).await;
        assert!(source.is_file());

        // Resume a tiny real HTTP response from a persisted partial file, without downloading public weights.
        // 从已落盘断点续接微型真实 HTTP 响应，不下载任何公开大模型权重。
        let observed=Arc::new(AtomicBool::new(false));
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address=listener.local_addr().unwrap();
        async fn payload(State((bytes,observed)):State<(Arc<Vec<u8>>,Arc<AtomicBool>)>,headers:HeaderMap)->impl IntoResponse {
            let offset=headers.get("range").and_then(|value|value.to_str().ok()).and_then(|value|value.strip_prefix("bytes="))
                .and_then(|value|value.strip_suffix('-')).and_then(|value|value.parse::<usize>().ok()).unwrap_or(0);
            observed.store(offset==64,Ordering::SeqCst);
            (if offset>0 {StatusCode::PARTIAL_CONTENT}else{StatusCode::OK},
                [("content-range",format!("bytes {offset}-{}/{}",bytes.len()-1,bytes.len()))],bytes[offset..].to_vec())
        }
        let app=Router::new().route("/tiny.gguf",get(payload)).with_state((Arc::new(bytes.clone()),observed.clone()));
        let server=tokio::spawn(async move {axum::serve(listener,app).await.unwrap();});
        let mut spec=catalog()[0].clone();spec.id="fixture-download".into();spec.name="Download fixture".into();
        spec.download_bytes=bytes.len() as u64;spec.files=vec![ModelFile{name:"tiny.gguf".into(),url:format!("http://{address}/tiny.gguf"),
            bytes:bytes.len() as u64,sha256:format!("{:x}",Sha256::digest(&bytes)),role:"model".into()}];
        let instance=library(&home).await.unwrap();let directory=changed.join(&spec.id);
        let mut record=new_record(&home,&directory,spec);record.status="paused".into();
        own_directory(&home,&record).await.unwrap();tokio::fs::write(directory.join("tiny.gguf.part"),&bytes[..64]).await.unwrap();
        update(&home,&instance,|manifest|{manifest.models.insert(record.spec.id.clone(),record);}).await.unwrap();
        shutdown(&home).await.unwrap();
        call(&home,"/api/local-models/download",json!({"id":"fixture-download","action":"resume"})).await;
        tokio::time::timeout(Duration::from_secs(5),async {
            loop {
                let snapshot=handle("GET","/api/local-models",Value::Null,&home).await.unwrap().unwrap().1;
                let model=snapshot["models"].as_array().unwrap().iter().find(|model|model["id"]=="fixture-download").unwrap();
                if model["status"]=="ready" {break;}
                assert_ne!(model["status"],"failed","{model}");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.unwrap();
        assert!(observed.load(Ordering::SeqCst));
        assert_eq!(tokio::fs::read(directory.join("tiny.gguf")).await.unwrap(),bytes);
        shutdown(&home).await.unwrap();server.abort();let _=server.await;
        tokio::fs::remove_dir_all(&root).await.unwrap();
    }
}

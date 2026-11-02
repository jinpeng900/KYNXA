//! 系统选择器只授权所选资料副本或模型，不扩展项目工作目录。
use super::{invalid, store};
use crate::{
    gateway_contract::GatewayError,
    storage::managed_files,
    tools::gateway::{self, GatewayToolContext},
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const MAX_SOURCE: u64 = 128 * 1024 * 1024;
fn supported(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "pdf"
            | "doc"
            | "docx"
            | "ppt"
            | "pptx"
            | "xls"
            | "xlsx"
            | "txt"
            | "md"
            | "csv"
            | "tsv"
            | "json"
            | "jsonl"
            | "png"
            | "jpg"
            | "jpeg"
            | "webp"
            | "bmp"
            | "tif"
            | "tiff"
            | "gif"
    )
}
fn selected_path(ctx: &GatewayToolContext, path: &str) -> Result<PathBuf, GatewayError> {
    let (path, _) = gateway::canonical_authorized(ctx, path, false)?;
    if gateway::protected_path(ctx, &path) {
        return Err(GatewayError::new(
            "PROTECTED_APP_DATA",
            "不能将软件私有数据或扩展运行目录作为训练资料。",
            403,
        ));
    }
    Ok(path)
}
pub(crate) fn resolve(
    id: &str,
    ctx: &GatewayToolContext,
) -> Result<(PathBuf, Value), GatewayError> {
    let id = uuid::Uuid::parse_str(id)
        .map_err(|_| invalid("训练资料标识无效。"))?
        .to_string();
    let directory = store::scope_dir(ctx).join("imports").join(id);
    let item = store::read(ctx, &directory.join("source.json"), 65536)?;
    let name = item["name"]
        .as_str()
        .ok_or_else(|| invalid("训练资料索引无效。"))?;
    if Path::new(name).components().count() != 1 {
        return Err(invalid("训练资料路径无效。"));
    }
    let path = directory.join(name);
    let bytes = managed_files::read(&ctx.data_home, &path, MAX_SOURCE)?
        .ok_or_else(|| invalid("训练资料副本已丢失。"))?;
    if item["sha256"] != managed_files::digest(&bytes) {
        return Err(GatewayError::new(
            "TRAINING_SOURCE_CHANGED",
            "训练资料副本已变化，请重新导入。",
            409,
        ));
    }
    Ok((path, item))
}
// Only the trusted workbench picker calls this; it is absent from model tool actions.
pub(super) async fn import(input: &Value, ctx: &GatewayToolContext) -> Result<Value, GatewayError> {
    let mut paths = Vec::new();
    if let Some(directory) = input["directory"].as_str() {
        let path = selected_path(ctx, directory)?;
        for entry in ignore::WalkBuilder::new(path).follow_links(false).build() {
            let entry = entry.map_err(|_| invalid("训练资料目录无法遍历。"))?;
            if entry.file_type().is_some_and(|t| t.is_file()) && supported(entry.path()) {
                paths.push(entry.into_path());
            }
            if paths.len() > 10000 {
                return Err(invalid("单次导入文件过多，请缩小资料目录。"));
            }
        }
    } else {
        for item in input["sources"]
            .as_array()
            .ok_or_else(|| invalid("请选择训练资料。"))?
        {
            let path = selected_path(
                ctx,
                item["path"]
                    .as_str()
                    .ok_or_else(|| invalid("资料路径无效。"))?,
            )?;
            if !path.is_file() || !supported(&path) {
                return Err(invalid("请选择支持的资料文件。"));
            }
            paths.push(path);
        }
    }
    let scope = ctx.clone();
    tokio::task::spawn_blocking(move || {
        let mut sources=Vec::new();
        for path in paths {
            gateway::check_cancelled(&scope)?;
            let size=std::fs::metadata(&path)?.len();
            if size>MAX_SOURCE {return Err(invalid("单个训练资料超过128 MiB，请分开导入。"));}
            // Shared stable-read checks reject links and changed file identities.
            let bytes=crate::tools::local_files::read_checked(&path, MAX_SOURCE, true, "TRAINING_SOURCE_CHANGED")
                .map_err(|e| GatewayError::new(e.code,e.message,e.status_code))?;
            let name=path.file_name().and_then(|n|n.to_str()).ok_or_else(||invalid("资料名称无效。"))?;
            let id=uuid::Uuid::new_v4().to_string(); let dir=store::scope_dir(&scope).join("imports").join(&id);
            managed_files::atomic_write(&scope.data_home,&dir.join(name),&bytes,MAX_SOURCE)?;
            let item=json!({"importId":id,"name":name,"path":path,"sha256":managed_files::digest(&bytes),"sizeBytes":bytes.len()});
            store::write(&scope,&dir.join("source.json"),&item)?; sources.push(item);
        }
        Ok(json!({"sources":sources}))
    }).await.map_err(|_| GatewayError::new("TRAINING_IMPORT_FAILED","训练资料导入未完成。",500))?
}
pub(super) fn register_model(
    input: &Value,
    ctx: &GatewayToolContext,
) -> Result<Value, GatewayError> {
    let path = selected_path(
        ctx,
        input["path"]
            .as_str()
            .ok_or_else(|| invalid("请选择模型目录。"))?,
    )?;
    if !path.is_dir() || !path.join("config.json").is_file() {
        return Err(invalid("请选择含config.json的可训练模型目录。"));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let bytes = crate::tools::local_files::read_checked(
        &path.join("config.json"),
        2 * 1024 * 1024,
        true,
        "TRAINING_MODEL_CHANGED",
    )
    .map_err(|e| GatewayError::new(e.code, e.message, e.status_code))?;
    let config: Value =
        serde_json::from_slice(&bytes).map_err(|_| invalid("模型config.json格式无效。"))?;
    let text = config.get("text_config").unwrap_or(&config);
    let structure = json!({"modelType":config["model_type"],"hiddenLayers":text["num_hidden_layers"],"hiddenSize":text["hidden_size"],"vocabularySize":text["vocab_size"]});
    let model = json!({"id":id,"name":path.file_name().unwrap_or_default().to_string_lossy(),"path":path,"format":"transformers","source":"user-selected","trainable":true,"structure":structure});
    store::write(
        ctx,
        &store::scope_dir(ctx)
            .join("models")
            .join(&id)
            .join("model.json"),
        &model,
    )?;
    Ok(json!({"model":model,"modelPath":path,"path":path}))
}

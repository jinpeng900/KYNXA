//! Explicit workspace UI actions reuse filesystem, task ledger and history ownership services.
use super::gateway::{GatewayToolContext, check_cancelled, failure};
use crate::{gateway_contract::{GatewayConfig, GatewayError}, storage::managed_files};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{path::{Path, PathBuf}, sync::Mutex};

const LIMIT: u64 = 64 * 1024 * 1024;
static CACHE_TRANSACTION: Mutex<()> = Mutex::new(());

fn invalid(message: &str) -> GatewayError { failure("INVALID_TOOL_ARGUMENTS", message, 400) }
fn digest(value: &str) -> bool { value.len() == 64 && value.bytes().all(|value| value.is_ascii_hexdigit()) }
fn requested(input: &Value) -> Result<&str, GatewayError> { input["path"].as_str().filter(|path| !path.is_empty()).ok_or_else(|| invalid("请提供文件路径。")) }

pub(super) async fn bound(input: &Value, context: &GatewayToolContext, missing: bool) -> Result<(PathBuf, Value), GatewayError> {
    let (path, outside) = super::gateway::canonical_authorized(context, requested(input)?, missing)?;
    if outside { return Err(failure("WORKSPACE_PREVIEW_FORBIDDEN", "此操作只适用于当前已授权工作目录。", 403)); }
    // The ordinary service owns protected-data/private-resource checks. No UI flag grants another root.
    let mut scoped = context.clone(); scoped.approved = false;
    if !missing || path.exists() {
        let receipt = super::gateway::filesystem("filesystem.stat", json!({"path":path}), &scoped).await?;
        if receipt["requiresApproval"] == true { return Err(failure("WORKSPACE_PREVIEW_FORBIDDEN", "文件尚未获得本次操作授权。", 403)); }
    }
    Ok((path.clone(), json!({"requestedPath":path,"path":path,"allowMissing":missing,"allowHardLinks":!missing})))
}

fn history_directory(context: &GatewayToolContext, path: &Path) -> PathBuf {
    let owner = managed_files::digest(context.conversation_id.as_bytes());
    let path_id = managed_files::digest(super::local_files::path_text(path).as_bytes());
    context.data_home.join("Cache/user-workbench/versions").join(owner).join(path_id)
}

fn cache_index(context: &GatewayToolContext, directory: &Path) -> Result<Value, GatewayError> {
    managed_files::inspect(&context.data_home, directory, true, false, LIMIT)?;
    let bytes = managed_files::read(&context.data_home, &directory.join("index.json"), 1024 * 1024)?;
    match bytes { Some(bytes) => serde_json::from_slice(&bytes).map_err(|_| failure("WORKBENCH_CACHE_INVALID", "版本索引无法读取，原件已保留。", 409)), None => Ok(json!([])) }
}

fn snapshot(path: &Path, binding: &Value, context: &GatewayToolContext, label: &str) -> Result<Value, GatewayError> {
    let bytes = super::gateway_workspace_preview::checked_bytes(path, binding)?;
    if bytes.len() as u64 > LIMIT { return Err(failure("WORKBENCH_VERSION_LIMIT", "文件超过 64 MiB 快照上限。", 413)); }
    let version = managed_files::digest(&bytes);
    let directory = history_directory(context, path);
    let _guard = CACHE_TRANSACTION.lock().map_err(|_| failure("WORKBENCH_STATE_FAILED", "版本缓存不可用。", 500))?;
    let mut records = cache_index(context, &directory)?;
    let records = records.as_array_mut().ok_or_else(|| failure("WORKBENCH_CACHE_INVALID", "版本索引格式无效。", 409))?;
    if let Some(record) = records.iter().find(|record| record["version"] == version) { return Ok(record.clone()); }
    if records.len() >= 32 || records.iter().filter_map(|record| record["sizeBytes"].as_u64()).sum::<u64>() + bytes.len() as u64 > 256 * 1024 * 1024 {
        return Err(failure("WORKBENCH_VERSION_LIMIT", "此文件已达到 32 个版本或 256 MiB 快照容量，请导出后清理缓存。", 409));
    }
    managed_files::atomic_write(&context.data_home, &directory.join(format!("{version}.bin")), &bytes, LIMIT)?;
    let record = json!({"version":version,"sha256":version,"sizeBytes":bytes.len(),"createdAt":chrono::Utc::now().to_rfc3339(),"label":label.chars().take(160).collect::<String>()});
    records.insert(0, record.clone());
    managed_files::atomic_write(&context.data_home, &directory.join("index.json"), &serde_json::to_vec(records).unwrap(), 1024 * 1024)?;
    Ok(record)
}

fn load_version(path: &Path, version: &str, context: &GatewayToolContext) -> Result<Vec<u8>, GatewayError> {
    if !digest(version) { return Err(invalid("版本必须是完整 SHA-256。")); }
    let directory = history_directory(context, path);
    let records = cache_index(context, &directory)?;
    if !records.as_array().into_iter().flatten().any(|record| record["version"] == version) { return Err(failure("WORKBENCH_VERSION_MISSING", "此文件没有所选快照。", 404)); }
    let bytes = managed_files::read(&context.data_home, &directory.join(format!("{version}.bin")), LIMIT)?
        .ok_or_else(|| failure("WORKBENCH_VERSION_MISSING", "快照缓存已清理。", 404))?;
    if managed_files::digest(&bytes) != version { return Err(failure("WORKBENCH_VERSION_CHANGED", "快照校验失败，不能恢复。", 409)); }
    Ok(bytes)
}

async fn versions(action: &str, input: &Value, context: &GatewayToolContext) -> Result<Value, GatewayError> {
    let (path, binding) = bound(input, context, false).await?;
    let source = path.clone(); let scope = context.clone(); let input = input.clone(); let operation = action.to_owned();
    let value = tokio::task::spawn_blocking(move || {
        if operation == "versions.create" {
            return snapshot(&source, &binding, &scope, input["label"].as_str().unwrap_or("手动快照"));
        }
        if operation == "versions.list" {
            let bytes = super::gateway_workspace_preview::checked_bytes(&source, &binding)?;
            let _guard = CACHE_TRANSACTION.lock().map_err(|_| failure("WORKBENCH_STATE_FAILED", "版本缓存不可用。", 500))?;
            return Ok(json!({"path":source,"currentHash":managed_files::digest(&bytes),"versions":cache_index(&scope, &history_directory(&scope, &source))?,"coverage":"explicit-and-workbench-write-snapshots"}));
        }
        let version = input["version"].as_str().ok_or_else(|| invalid("请选择快照版本。"))?;
        let saved = load_version(&source, version, &scope)?;
        let current = super::gateway_workspace_preview::checked_bytes(&source, &binding)?;
        let actual = managed_files::digest(&current);
        if operation == "versions.restore" {
            let expected = input["expectedHash"].as_str().filter(|hash| digest(hash)).ok_or_else(|| invalid("恢复前必须提供当前 expectedHash，防止覆盖后来修改。"))?;
            if actual != expected { return Err(failure("TOOL_FILE_CONFLICT", "文件已变化，请重新比较后恢复。", 409)); }
            snapshot(&source, &binding, &scope, "恢复前自动保存")?;
            return Ok(json!({"restoreBytes":STANDARD.encode(saved),"path":source,"expectedHash":expected}));
        }
        let before = std::str::from_utf8(&saved).ok().filter(|text| !text.contains('\0'));
        let after = std::str::from_utf8(&current).ok().filter(|text| !text.contains('\0'));
        Ok(json!({"path":source,"version":version,"currentHash":actual,"identical":current==saved,"beforeSize":saved.len(),"afterSize":current.len(),
            "binary":before.is_none()||after.is_none(),"beforeText":before.map(|text|text.chars().take(64000).collect::<String>()),"afterText":after.map(|text|text.chars().take(64000).collect::<String>()),
            "truncated":before.is_some_and(|text|text.chars().count()>64000)||after.is_some_and(|text|text.chars().count()>64000)}))
    }).await.map_err(|_| failure("WORKBENCH_WORKER_FAILED", "版本操作未完成。", 500))??;
    if action == "versions.restore" {
        let bytes = STANDARD.decode(value["restoreBytes"].as_str().unwrap()).map_err(|_| invalid("快照编码无效。"))?;
        let mut scoped = context.clone(); scoped.approved = true;
        return super::gateway::filesystem_binary_write(json!({"path":path,"expectedHash":value["expectedHash"]}), bytes, &scoped).await;
    }
    Ok(value)
}

fn collection(action: &str, input: &Value, context: &GatewayToolContext) -> Result<Value, GatewayError> {
    let _guard = CACHE_TRANSACTION.lock().map_err(|_| failure("WORKBENCH_STATE_FAILED", "工作台缓存不可用。", 500))?;
    let family = action.split('.').next().unwrap();
    let directory = context.data_home.join("Workspace/Collections");
    managed_files::inspect(&context.data_home, &directory, true, false, 8 * 1024 * 1024)?;
    let path = directory.join(format!("{family}.json"));
    let original = managed_files::read(&context.data_home, &path, 8 * 1024 * 1024)?;
    let original = match original { Some(bytes) => Some(bytes), None => managed_files::read(&context.data_home, &context.data_home.join("Cache/user-workbench").join(format!("{family}.json")), 8 * 1024 * 1024)? };
    let mut items: Vec<Value> = original.map(|bytes| serde_json::from_slice(&bytes)).transpose()
        .map_err(|_| failure("WORKBENCH_CACHE_INVALID", "工作台资料无法读取，原件已保留。", 409))?.unwrap_or_default();
    if action.ends_with(".list") { return Ok(json!({"items":items})); }
    let id = input["id"].as_str().or_else(|| input["item"]["id"].as_str()).map(str::to_owned).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    if id.is_empty() || id.len() > 128 || !id.chars().all(|value| value.is_ascii_alphanumeric() || matches!(value, '-' | '_')) { return Err(invalid("资料标识无效。")); }
    if action.ends_with(".delete") { items.retain(|item|item["id"]!=id); }
    else {
        let mut item = input["item"].clone();
        let maximum = if family == "projects" { 1024 * 1024 } else { 64 * 1024 };
        if !item.is_object() || serde_json::to_vec(&item).unwrap().len() > maximum { return Err(invalid("资料内容超过保存容量。")); }
        item["id"] = json!(id); item["updatedAt"] = json!(chrono::Utc::now().to_rfc3339());
        if let Some(index) = items.iter().position(|item|item["id"]==id) { items[index]=item; }
        else { if items.len()>=512 { return Err(invalid("最多保存 512 条工作台资料。")); } items.insert(0,item); }
    }
    managed_files::atomic_write(&context.data_home,&path,&serde_json::to_vec(&items).unwrap(),8*1024*1024)?;
    Ok(json!({"id":id,"items":items}))
}

pub(crate) async fn handle(arguments: &Value, context: &GatewayToolContext, config: &GatewayConfig) -> Result<Value, GatewayError> {
    check_cancelled(context)?;
    let action = arguments["action"].as_str().unwrap_or("");
    let input = if arguments["payload"].is_object() { arguments["payload"].clone() } else { json!({}) };
    match action {
        "project.read" | "project.save" => {
            let scope=context.clone(); let identity=context.project_id.clone().unwrap_or_else(||context.conversation_id.clone());
            let action=action.to_owned(); let mut input=input; input["id"]=json!(identity);
            tokio::task::spawn_blocking(move||{
                let result=collection(if action=="project.read" {"projects.list"} else {"projects.save"},&input,&scope)?;
                Ok(result["items"].as_array().and_then(|items|items.iter().find(|item|item["id"]==input["id"])).cloned().unwrap_or_else(||json!({})))
            }).await.map_err(|_|failure("WORKBENCH_WORKER_FAILED","项目记录保存未完成。",500))?
        }
        "media.transcribe" => super::gateway_workspace_transcription::transcribe(&input,context,config).await,
        "media.cancel" => super::gateway_workspace_transcription::cancel(&input,context),
        "files.list" => {
            let input=json!({"path":input["path"].as_str().unwrap_or("."),"limit":input["limit"].as_u64().unwrap_or(500).clamp(1,500)});
            let (directory,_)=bound(&input,context,false).await?;
            let mut result=super::gateway::filesystem("filesystem.list",input,context).await?;
            if let Some(entries)=result["entries"].as_array_mut(){for entry in entries{
                if let Some(name)=entry["name"].as_str(){entry["path"]=json!(directory.join(name));}
            }}
            Ok(result)
        }
        "files.read" => {
            let (path,binding)=bound(&input,context,false).await?;
            if input["encoding"]=="base64" {
                return tokio::task::spawn_blocking(move||{
                    let bytes=super::gateway_workspace_preview::checked_bytes(&path,&binding)?;
                    if bytes.len() as u64>LIMIT {return Err(invalid("文件超过 64 MiB。"));}
                    Ok(json!({"path":path,"content":STANDARD.encode(&bytes),"encoding":"base64","sha256":managed_files::digest(&bytes),"sizeBytes":bytes.len()}))
                }).await.map_err(|_|failure("WORKBENCH_WORKER_FAILED","文件读取未完成。",500))?;
            }
            let mut input=input;
            if let Some(maximum)=input["maxChars"].as_u64(){input["maxChars"]=json!(maximum.clamp(1,64000));}
            super::gateway::filesystem("filesystem.read",input,context).await
        }
        "files.write" => {
            let (path,binding) = bound(&input,context,true).await?;
            if !input.get("expectedHash").is_some_and(|hash| hash.is_null() || hash.as_str().is_some_and(digest)) { return Err(invalid("保存必须明确 expectedHash；新文件为 null，覆盖为当前 SHA-256。")); }
            let content = input["content"].as_str().ok_or_else(||invalid("缺少文件内容。"))?;
            let bytes = match input["encoding"].as_str().unwrap_or("utf8") {
                "utf8" => content.as_bytes().to_vec(),
                "base64" => { if content.len()>LIMIT as usize*4/3+4 {return Err(invalid("文件超过 64 MiB。"));} STANDARD.decode(content).map_err(|_|invalid("Base64 文件编码无效。"))? }
                _ => return Err(invalid("encoding 只接受 utf8 或 base64。")),
            };
            if bytes.len() as u64>LIMIT { return Err(invalid("文件超过 64 MiB。")); }
            if path.is_file() {
                let source=path.clone(); let scope=context.clone(); let mut existing=binding; existing["allowMissing"]=json!(false);
                tokio::task::spawn_blocking(move||snapshot(&source,&existing,&scope,"工作台保存前")).await.map_err(|_|failure("WORKBENCH_WORKER_FAILED","快照保存未完成。",500))??;
            }
            let mut scoped=context.clone(); scoped.approved=true;
            super::gateway::filesystem_binary_write(input,bytes,&scoped).await
        }
        "versions.list" | "versions.create" | "versions.compare" | "versions.restore" => versions(action,&input,context).await,
        "templates.list" | "templates.save" | "templates.delete" | "bookmarks.list" | "bookmarks.save" | "bookmarks.delete" => {
            let scope=context.clone(); let action=action.to_owned();
            tokio::task::spawn_blocking(move||collection(&action,&input,&scope)).await.map_err(|_|failure("WORKBENCH_WORKER_FAILED","资料保存未完成。",500))?
        }
        "search.conversations" => {
            let config=config.clone(); let identity=json!({"conversationId":context.conversation_id,"projectId":context.project_id,"expectedProjectId":context.project_id});
            let mut input=input; if input["scope"].is_null(){input["scope"]=json!(if context.project_id.is_some(){"work"}else{"chat"});}
            if let Some(limit)=input["limit"].as_u64(){input["limit"]=json!(limit.clamp(1,20));}
            tokio::task::spawn_blocking(move||crate::retrieval::gateway_history::handle(&config,"conversation.history.search",&input,&identity).map_err(GatewayError::from))
                .await.map_err(|_|failure("WORKBENCH_WORKER_FAILED","历史搜索未完成。",500))?
        }
        "deliverables.list" | "diagnostics" => {
            let files = if action=="deliverables.list" {
                let listing=super::gateway::filesystem("filesystem.list",json!({"path":".","limit":500}),context).await?;
                listing["entries"].as_array().into_iter().flatten().filter(|entry|entry["type"]=="file").map(|entry|{
                    let path=context.workdir.join(entry["name"].as_str().unwrap_or(""));
                    json!({"path":path,"title":entry["name"],"kind":"workspace-file","source":"authorized-workspace"})
                }).collect::<Vec<_>>()
            }else{Vec::new()};
            let root=config.data_root.clone(); let conversation=context.conversation_id.clone(); let project=context.project_id.clone(); let action=action.to_owned();
            let extensions=super::gateway_mcp::diagnostics(context).await.into_iter().map(|state|json!({"id":state["serverId"],"name":state["name"],"enabled":state["enabled"],"transport":state["transport"],"state":state["state"],"ready":state["ready"],"code":state["code"],"toolCount":state["toolCount"]})).collect::<Vec<_>>();
            tokio::task::spawn_blocking(move||{
                let repository=crate::storage::task_repository::TaskRepository::open(&root)?;
                let tasks=repository.list_for_scope(if project.is_some(){None}else{Some(conversation.as_str())},project.as_deref(),None,128)?;
                if action=="diagnostics" {
                    let usage=repository.usage_summary(Some(&conversation),project.as_deref())?;
                    let tasks=tasks.into_iter().map(|task|json!({"taskId":task.task_id,"title":task.title,"state":task.state,"updatedAtMs":task.updated_at_ms,"stopReason":task.stop_reason,"usage":task.usage})).collect::<Vec<_>>();
                    return Ok(json!({"tasks":tasks,"usage":usage,"extensions":extensions,"scope":"current-chat-and-project","ledger":"existing-task-repository"}));
                }
                let mut items=Vec::new();
                for task in &tasks { for artifact in repository.artifacts(&task.task_id)? { items.push(json!({"taskId":task.task_id,"title":task.title,"conversationId":task.scope.conversation_id,"artifactId":artifact.artifact_id,"kind":artifact.kind,"reference":artifact.reference,"sha256":artifact.sha256,"sizeBytes":artifact.size_bytes,"sourceVersion":artifact.source_version})); } }
                Ok::<_,GatewayError>(json!({"items":files,"artifacts":items,"coverage":"existing-task-artifacts-and-current-workspace-files","tasksExamined":tasks.len(),"hasMore":tasks.len()==128}))
            }).await.map_err(|_|failure("WORKBENCH_WORKER_FAILED","任务资料读取未完成。",500))?
        }
        _ => Err(failure("WORKBENCH_ACTION_UNSUPPORTED","工作台操作不在允许列表中。",400)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn restoring_a_snapshot_requires_the_current_hash() {
        let (context,_cancel)=super::super::gateway::test_context();
        let path=context.workdir.join("report.txt");
        std::fs::write(&path,b"first").unwrap();
        let created=versions("versions.create",&json!({"path":path}),&context).await.unwrap();
        std::fs::write(&path,b"second").unwrap();
        let stale=versions("versions.restore",&json!({"path":path,"version":created["version"],"expectedHash":created["version"]}),&context).await.unwrap_err();
        assert_eq!(stale.code,"TOOL_FILE_CONFLICT"); assert_eq!(std::fs::read(&path).unwrap(),b"second");
        let hash=managed_files::digest(b"second");
        let restored=versions("versions.restore",&json!({"path":path,"version":created["version"],"expectedHash":hash}),&context).await.unwrap();
        assert_eq!(restored["sha256"],created["version"]); assert_eq!(std::fs::read(&path).unwrap(),b"first");
        let listed=versions("versions.list",&json!({"path":path}),&context).await.unwrap();
        assert_eq!(listed["versions"].as_array().unwrap().len(),2);
        let external=context.workdir.parent().unwrap().join("secret.txt"); std::fs::write(&external,b"secret").unwrap();
        assert_eq!(versions("versions.create",&json!({"path":external}),&context).await.unwrap_err().code,"WORKSPACE_PREVIEW_FORBIDDEN");
        std::fs::remove_dir_all(context.workdir.parent().unwrap()).unwrap();
    }
}

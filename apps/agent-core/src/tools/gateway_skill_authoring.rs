//! Author application Skills through the existing package roots, receipts and approval owner.
//! 沿用现有技能目录、工具回执与调用审批，实现项目编写、校验和经确认的全局复用。

use super::{gateway::{self, GatewayToolContext, WorkerCancellation}, local_files, service_skills};
use crate::gateway_contract::GatewayError;
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{collections::HashSet, fs::{self, OpenOptions}, io::Write, path::{Path, PathBuf}, sync::{Arc, Mutex, OnceLock, atomic::{AtomicBool, Ordering}}};

const MAX_FILE_BYTES: usize = 2 * 1024 * 1024;
const MAX_PACKAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_FILES: usize = 128;
static AUTHORING_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

pub(crate) fn supports(name: &str) -> bool { matches!(name, "skill.save" | "skill.validate" | "skill.promote") }

pub(crate) fn descriptors() -> Vec<Value> {
    let name=json!({"type":"string","minLength":1,"maxLength":64});
    let expected=json!({"type":["string","null"],"description":"Use null only when creating a new package; updates require the current complete packageHash."});
    vec![
        json!({"name":"skill.save","source":"builtin","description":"Create or update a reusable project Skill with its complete SKILL.md and optional resources. Save a focused method, then validate and really use it. Updates replace the complete package; omitted resources are removed. Project writes follow current permissions. Global storage requires skill.promote after successful verification.","inputSchema":{"type":"object","additionalProperties":false,
            "properties":{"name":name,"scope":{"type":"string","enum":["project"],"default":"project"},"content":{"type":"string","description":"Full UTF-8 SKILL.md, including valid YAML name/description frontmatter and practical instructions."},"expectedHash":expected,
                "files":{"type":"array","maxItems":127,"items":{"type":"object","additionalProperties":false,"properties":{"path":{"type":"string"},"content":{"type":"string"},"contentBase64":{"type":"string"}},"required":["path"],"oneOf":[{"required":["content"],"not":{"required":["contentBase64"]}},{"required":["contentBase64"],"not":{"required":["content"]}}]}}},"required":["name","content","expectedHash"]},"readOnly":false}),
        json!({"name":"skill.validate","source":"builtin","description":"Inspect the current project or global Skill package, validate its header, resources, tool availability and hashes, and report executable files still requiring real execution. Structural validity alone does not prove runtime reliability. Returns packageHash for safe updates and verification.","inputSchema":{"type":"object","additionalProperties":false,"properties":{"name":name,"scope":{"type":"string","enum":["project","global"],"default":"project"}},"required":["name"]},"readOnly":true}),
        json!({"name":"skill.promote","source":"builtin","description":"Promote a current project Skill to global reuse only after original successful tool receipts prove use of this version. Include the archived skill.read or skill.validate receipt, subsequent successful real operations, and matching isolated skill.run proofs for scripts. Explain general applicability; global reliability requires user confirmation. Unknown, pending or failed results never count. Existing global versions require their own package hash.","inputSchema":{"type":"object","additionalProperties":false,
            "properties":{"name":name,"scope":{"type":"string","enum":["project"],"default":"project"},"expectedHash":{"type":"string","description":"Current project packageHash from skill.validate."},"targetExpectedHash":expected,"generalization":{"type":"string","minLength":1,"maxLength":2000},
                "verificationRefs":{"type":"array","minItems":1,"maxItems":32,"items":{"type":"object","additionalProperties":false,"properties":{"id":{"type":"string"},"sha256":{"type":"string"},"bytes":{"type":"integer","minimum":0}},"required":["id","sha256","bytes"]}}},"required":["name","expectedHash","targetExpectedHash","generalization","verificationRefs"]},"readOnly":false}),
    ]
}

fn invalid(message: &str) -> GatewayError {
    GatewayError::new("INVALID_SKILL_CONTENT", message, 400)
        .with_details(json!({"executed":false,"requestDispatched":false,"recoverable":true}))
}

fn io(error: impl std::fmt::Display) -> GatewayError {
    GatewayError::new("APP_SKILL_STORAGE_FAILED", format!("技能文件操作失败，已有版本保留：{error}"), 503)
}

fn service(error: super::service_error::ServiceError) -> GatewayError {
    GatewayError::new(error.code, error.message, error.status_code)
}

fn stopped(signal: &Arc<AtomicBool>) -> Result<(), GatewayError> {
    if signal.load(Ordering::Acquire) { Err(GatewayError::new("TOOL_CANCELLED", "技能操作已取消，已有版本保留。", 499)) } else { Ok(()) }
}

fn package_root(ctx: &GatewayToolContext, scope: &str) -> Result<PathBuf, GatewayError> {
    match scope {
        "project" => Ok(ctx.workdir.join(".kynxa/skills")),
        "global" => Ok(ctx.extension_root.join("Skills")),
        _ => Err(invalid("scope须为project或global。")),
    }
}

fn selected_target(ctx: &GatewayToolContext, args: &Value) -> Result<(PathBuf, String), GatewayError> {
    let name = args["name"].as_str().filter(|name| !name.is_empty() && name.chars().count() <= 64)
        .ok_or_else(||invalid("请提供技能名称。"))?;
    if name.contains(['/', '\\', ':', '\0']) || name.starts_with('.') { return Err(invalid("技能名称不能包含路径。")); }
    let scope = args["scope"].as_str().unwrap_or("project");
    let base = package_root(ctx, scope)?;
    let (target, relative) = service_skills::resolve_resource(&base, &json!(name)).map_err(service)?;
    if relative.contains('/') { return Err(invalid("技能名称不能包含路径。")); }
    Ok((target, scope.to_owned()))
}

fn inventory(root: &Path, signal: &Arc<AtomicBool>) -> Result<Value, GatewayError> {
    let mut inventory = service_skills::SkillInventory::default();
    let cancellation = Some(signal.clone());
    let mut phase = inventory.apply("skill.inventoryInit", &json!({"root":root,"hashes":true}), &cancellation).map_err(service)?;
    while phase["authorization"].is_object() {
        phase = inventory.apply("skill.inventoryNext", &json!({"allowed":true}), &cancellation).map_err(service)?;
    }
    let value = phase["inventory"].clone();
    if value["valid"] != true || value["truncated"] == true { return Err(invalid("技能文件清单未完整通过校验。")); }
    Ok(value)
}

fn package_hash(files: &[Value]) -> String {
    let mut manifest = files.iter().map(|file|json!({"path":file["relativePath"],"bytes":file["size"],"sha256":file["sha256"]})).collect::<Vec<_>>();
    manifest.sort_by(|left,right| left["path"].as_str().cmp(&right["path"].as_str()));
    local_files::sha256(json!(manifest).to_string().as_bytes())
}

fn validate_body(body: &str, name: &str, files: &[Value], available: &[String]) -> Result<Value, GatewayError> {
    let normalized = body.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    let (header, instructions) = normalized.strip_prefix("---\n")
        .and_then(|value|value.split_once("\n---\n")).ok_or_else(||invalid("SKILL.md需要YAML头和操作正文。"))?;
    if instructions.trim().is_empty() { return Err(invalid("技能操作正文不能为空。")); }
    let values: Value = serde_yaml::from_str(header).map_err(|_|invalid("技能YAML头无效。"))?;
    let mut validation = super::configuration::calculate("tools.validateSkillHeader", &json!({"values":values,"directoryName":name}))
        .map_err(|_|invalid("技能头校验失败。"))?;
    let declared = match &values["allowed-tools"] {
        Value::Array(items) => items.iter().filter_map(Value::as_str).map(str::to_owned).collect::<Vec<_>>(),
        Value::String(value) => value.split([',',' ']).filter(|value|!value.is_empty()).map(str::to_owned).collect(),
        _ => Vec::new(),
    };
    let unavailable = declared.iter().filter(|tool| !available.contains(tool)).cloned().collect::<Vec<_>>();
    validation["unavailableTools"] = json!(unavailable);
    validation["executionVerified"] = json!(false);
    validation["scripts"] = json!(files.iter().filter(|file| file["relativePath"].as_str().is_some_and(|path|
        matches!(Path::new(path).extension().and_then(|value|value.to_str()), Some("js"|"mjs"|"cjs"|"py"|"ps1"|"sh"|"bat"|"cmd"))))
        .map(|file|json!({"path":file["relativePath"],"sha256":file["sha256"],"status":"requires-real-execution"})).collect::<Vec<_>>());
    Ok(validation)
}

fn read_package(target: &Path, available: &[String], signal: &Arc<AtomicBool>) -> Result<Value, GatewayError> {
    stopped(signal)?;
    let inventory = inventory(target, signal)?;
    let bytes = local_files::read_checked(&target.join("SKILL.md"), MAX_FILE_BYTES as u64, false, "APP_SKILL_CHANGED").map_err(service)?;
    let body = std::str::from_utf8(&bytes).map_err(|_|invalid("技能正文须使用UTF-8。"))?;
    let files = inventory["files"].as_array().cloned().unwrap_or_default();
    let name = target.file_name().and_then(|value|value.to_str()).unwrap_or("");
    let validation = validate_body(body, name, &files, available)?;
    Ok(json!({"name":name,"path":target,"packageHash":package_hash(&files),"sha256":local_files::sha256(&bytes),
        "files":files,"validation":validation,"valid":validation["valid"]==true}))
}

struct AuthoredFile { relative: String, bytes: Vec<u8> }

fn proposed_files(args: &Value, target: &Path) -> Result<Vec<AuthoredFile>, GatewayError> {
    let body = args["content"].as_str().ok_or_else(||invalid("请提供完整SKILL.md正文。"))?;
    let mut files = vec![AuthoredFile { relative:"SKILL.md".into(), bytes:body.as_bytes().to_vec() }];
    let mut seen = HashSet::from(["skill.md".to_owned()]);
    if !args["files"].is_null() && !args["files"].is_array() { return Err(invalid("files须为资源文件数组。")); }
    for resource in args["files"].as_array().into_iter().flatten() {
        let (_, relative) = service_skills::resolve_resource(target, &resource["path"]).map_err(service)?;
        if !seen.insert(relative.to_lowercase()) { return Err(invalid("技能包中存在重复文件路径。")); }
        let bytes = match (resource["content"].as_str(),resource["contentBase64"].as_str()) {
            (Some(content), None) => content.as_bytes().to_vec(),
            (None, Some(content)) => STANDARD.decode(content).map_err(|_|invalid("资源Base64无效。"))?,
            _ => return Err(invalid("每个资源须提供content或contentBase64中的一项。")),
        };
        files.push(AuthoredFile {relative,bytes});
    }
    if files.len()>MAX_FILES || files.iter().any(|file|file.bytes.len()>MAX_FILE_BYTES)
        || files.iter().map(|file|file.bytes.len()).sum::<usize>()>MAX_PACKAGE_BYTES {
        return Err(invalid("技能包超过既有128个文件、单文件2MiB或总计8MiB容量。"));
    }
    Ok(files)
}

fn proposed_manifest(files: &[AuthoredFile]) -> Vec<Value> {
    files.iter().map(|file|json!({"relativePath":file.relative,"size":file.bytes.len(),"sha256":local_files::sha256(&file.bytes)})).collect()
}

fn commit_package(target: &Path, files: &[AuthoredFile], expected: &Value, signal: &Arc<AtomicBool>) -> Result<Value, GatewayError> {
    let _lock = AUTHORING_LOCK.get_or_init(||Mutex::new(())).lock().map_err(|_|io("技能保存锁不可用"))?;
    stopped(signal)?;
    let base = target.parent().ok_or_else(||invalid("技能目录无效。"))?;
    local_files::ensure_directory(base).map_err(service)?;
    let current = local_files::inspect(target, true, false).map_err(service)?;
    let previous = if current.is_some() {
        let existing = inventory(target, signal)?;
        Some(package_hash(existing["files"].as_array().unwrap_or(&Vec::new())))
    } else { None };
    if previous.as_deref() != expected.as_str() { return Err(GatewayError::new("APP_SKILL_CONFLICT", "技能已有版本与expectedHash不符；先读取当前包，已有版本未修改。",409)); }
    let staging_parent = base.join(".kynxa-authoring");
    local_files::ensure_directory(&staging_parent).map_err(service)?;
    let identity = uuid::Uuid::new_v4().to_string();
    let stage = staging_parent.join(format!("stage-{identity}"));
    let backup = staging_parent.join(format!("previous-{identity}"));
    fs::create_dir(&stage).map_err(io)?;
    let mut moved_previous = false;
    let mut result = (|| {
        for file in files {
            stopped(signal)?;
            let (destination,_) = service_skills::resolve_resource(&stage,&json!(file.relative)).map_err(service)?;
            local_files::ensure_directory(destination.parent().unwrap()).map_err(service)?;
            let mut output = OpenOptions::new().write(true).create_new(true).open(destination).map_err(io)?;
            output.write_all(&file.bytes).map_err(io)?;
            output.sync_all().map_err(io)?;
        }
        let proposed = package_hash(&proposed_manifest(files));
        let staged = inventory(&stage,signal)?;
        if proposed != package_hash(staged["files"].as_array().unwrap_or(&Vec::new())) { return Err(invalid("暂存技能包与提交内容不一致。")); }
        stopped(signal)?;
        if previous.is_some() {
            let checked = inventory(target,signal)?;
            if previous.as_deref() != Some(package_hash(checked["files"].as_array().unwrap_or(&Vec::new())).as_str()) {
                return Err(GatewayError::new("APP_SKILL_CHANGED", "保存前技能已被修改，已有版本保留。",409));
            }
            fs::rename(target,&backup).map_err(io)?;
            moved_previous = true;
        }
        match fs::rename(&stage,target) {
            Ok(()) => Ok(json!({"saved":true,"updated":previous.is_some(),"path":target.join("SKILL.md"),"packageHash":proposed,
                "sha256":local_files::sha256(&files[0].bytes),"bytes":files.iter().map(|file|file.bytes.len()).sum::<usize>()})),
            Err(error) => {
                if moved_previous { fs::rename(&backup,target).map_err(io)?; moved_previous=false; }
                Err(io(error))
            }
        }
    })();
    // Clean only the verified staging owner; never delete a live target or a failed rollback's old version.
    // 只清理已核验的自有暂存目录；不删除正式目标或回滚失败时保存的旧版目录。
    let mut cleanup_warnings = Vec::new();
    for path in [&stage, &backup] {
        if *path==backup && result.is_err() && moved_previous { continue; }
        let cleaned = (|| {
            if local_files::inspect(path,true,false).map_err(service)?.is_some() {
                let canonical_parent=fs::canonicalize(&staging_parent).map_err(io)?;
                let canonical=fs::canonicalize(path).map_err(io)?;
                if !local_files::within(&canonical_parent,&canonical) || canonical==canonical_parent { return Err(invalid("技能暂存清理目录越界。")); }
                fs::remove_dir_all(path).map_err(io)?;
            }
            Ok::<_, GatewayError>(())
        })();
        if let Err(error) = cleaned { cleanup_warnings.push(json!({"path":path,"code":error.code,"message":error.message})); }
    }
    // Cleanup failure after the atomic rename must not erase a committed result or encourage a duplicate write.
    // 原子替换后的清理问题不能抹去已提交回执，否则会诱发重复执行。
    if let Ok(receipt) = &mut result { if !cleanup_warnings.is_empty() { receipt["cleanupWarnings"] = json!(cleanup_warnings); } }
    result
}

fn successful(value: &Value) -> bool {
    if !value.is_object() { return false; }
    let mut pending = vec![(value, 0)];
    while let Some((item, depth)) = pending.pop() {
        if item["isError"]==true || item["success"]==false || item["completed"]==false || item["executed"]==false
            || item["requiresApproval"]==true || !item["error"].is_null()
            || item["code"].as_str().is_some_and(|code|code.to_ascii_uppercase().contains("UNKNOWN"))
            || item["exitCode"].as_i64().is_some_and(|code|code!=0)
            || ["state","status","outcome"].iter().any(|field|matches!(item[*field].as_str(),Some("unknown"|"running"|"queued"|"pending"|"not-dispatched"|"partial"|"failed"|"error"|"cancelled"|"interrupted"))) {
            return false;
        }
        for key in ["command", "structuredContent", "receipt"] {
            if item[key].is_object() {
                if depth >= 8 { return false; }
                pending.push((&item[key], depth+1));
            }
        }
    }
    true
}

/// Verify original receipts before global promotion; model claims and editable metadata are not proof.
/// 提升到全局前核验原始回执；模型自报和可编辑元数据不算证据。
fn promotion_evidence(ctx: &GatewayToolContext, args: &Value, package: &Value) -> Result<Value, GatewayError> {
    let generalization=args["generalization"].as_str().filter(|text|!text.trim().is_empty() && text.chars().count()<=2000)
        .ok_or_else(||invalid("请说明此流程如何去掉项目专用路径、数据和约定，适用于其他项目。"))?;
    let references=args["verificationRefs"].as_array().filter(|refs|!refs.is_empty() && refs.len()<=32)
        .ok_or_else(||invalid("全局技能需要原始验证回执引用；先在项目中读取、实际使用并验证。"))?;
    let mut archives=Vec::new();
    for reference in references {
        let id=reference["id"].as_str().ok_or_else(||invalid("验证回执需要id、sha256和bytes。"))?;
        let archive=crate::storage::gateway::load_private_tool_result(&ctx.data_home,&ctx.conversation_id,id)
            .map_err(|_|invalid("验证回执无法核验，请使用当前会话的真实结果引用。"))?;
        if archive["conversationId"]!=ctx.conversation_id || archive["sha256"]!=reference["sha256"] || archive["bytes"]!=reference["bytes"]
            || !successful(&archive["canonical"]) { return Err(invalid("验证引用不是当前会话中身份匹配的成功回执。")); }
        archives.push(archive);
    }
    let source=archives.iter().filter(|archive| {
        let content=&archive["canonical"];
        archive["toolName"]=="skill.read" && content["sha256"]==package["sha256"]
            || archive["toolName"]=="skill.validate" && content["packageHash"]==package["packageHash"] && content["valid"]==true
    }).min_by(|left,right|left["createdAt"].as_str().cmp(&right["createdAt"].as_str()))
        .ok_or_else(||invalid("请先读取或校验这一版项目技能；旧版技能回执不能证明当前版本。"))?;
    let actual=archives.iter().filter(|archive|!archive["toolName"].as_str().unwrap_or("").starts_with("skill.")
        && !matches!(archive["toolName"].as_str(),Some("tool.search"|"tool.load"|"model.continuation"))
        && archive["createdAt"].as_str()>=source["createdAt"].as_str()).collect::<Vec<_>>();
    let scripts=package["validation"]["scripts"].as_array().cloned().unwrap_or_default();
    for script in &scripts {
        let verified=archives.iter().any(|archive|archive["toolName"]=="skill.run"
            && archive["canonical"]["skillExecution"]["hashChecked"]==true
            && archive["canonical"]["skillExecution"]["readOnlyPackage"]==true
            && archive["canonical"]["skillExecution"]["scriptSha256"]==script["sha256"]
            && archive["canonical"]["skillExecution"]["script"]==script["path"]);
        if !verified { return Err(invalid("包含脚本的全局技能须先有匹配当前脚本哈希的真实隔离执行回执；未验证版本仍保留在项目。")); }
    }
    if actual.is_empty() && scripts.is_empty() { return Err(invalid("仅阅读技能不代表方法已验证；请附使用后真实操作的成功回执。")); }
    Ok(json!({"generalization":generalization,"verifiedReferences":references,"executionVerified":true,
        "generalReliability":"requires-user-confirmation","packageHash":package["packageHash"]}))
}

fn authoring_approval(ctx: &GatewayToolContext, tool: &str, args: &Value, identity: &Value, global: bool) -> Result<Option<Value>, GatewayError> {
    let needs=super::policy::calculate("tools.needsApproval",&json!({"name":"filesystem.write","permissionMode":ctx.permission_mode,
        "options":{"outsideWorkspace":global}})).map_err(|_|invalid("技能写入授权不可用。"))?;
    if (global || needs==true) && !ctx.approved {
        let mut pending=gateway::approval(tool,args,ctx);
        pending["invocation"]["skillAuthoringIdentity"]=identity.clone();
        pending["invocation"]["summary"]=json!(if global {"将已在项目验证的通用技能保存到全局；不会提升普通聊天或临时结果。"} else {"保存当前项目技能，后续可按需读取和复用。"});
        return Ok(Some(pending));
    }
    if ctx.approved && ctx.config["approvedInvocation"].is_object()
        && ctx.config["approvedInvocation"]["skillAuthoringIdentity"]!=*identity {
        return Err(GatewayError::new("APP_SKILL_CHANGED","技能版本在审批期间变化，请重新核对当前包。",409));
    }
    Ok(None)
}

pub(crate) async fn execute(name: &str, args: Value, ctx: &GatewayToolContext) -> Result<Value, GatewayError> {
    gateway::check_cancelled(ctx)?;
    let (target,scope)=selected_target(ctx,&args)?;
    let mut available=gateway::builtin_catalog().iter().filter_map(|tool|tool["name"].as_str().map(str::to_owned)).collect::<Vec<_>>();
    available.extend(ctx.runtime.schemas.read().await.iter().filter_map(|tool|tool["name"].as_str().map(str::to_owned)));
    let cancellation=WorkerCancellation::new(ctx);
    let signal=cancellation.signal.clone();
    if name=="skill.validate" {
        return tokio::task::spawn_blocking(move||read_package(&target,&available,&signal)).await.map_err(io)?;
    }
    if name=="skill.save" {
        if scope!="project" { return Err(invalid("请先保存为项目技能，实际验证后使用skill.promote提升全局。")); }
        let files=proposed_files(&args,&target)?;
        let manifest=proposed_manifest(&files);
        let body=std::str::from_utf8(&files[0].bytes).map_err(|_|invalid("SKILL.md须为UTF-8。"))?;
        let validation=validate_body(body,args["name"].as_str().unwrap_or(""),&manifest,&available)?;
        if validation["valid"]!=true { return Ok(json!({"saved":false,"executed":false,"isError":true,"code":"INVALID_SKILL_CONTENT","validation":validation,"previousVersionPreserved":true})); }
        let identity=json!({"scope":scope,"name":args["name"],"packageHash":package_hash(&manifest),"expectedHash":args["expectedHash"]});
        if let Some(pending)=authoring_approval(ctx,name,&args,&identity,false)? { return Ok(pending); }
        let expected=args["expectedHash"].clone();
        let mut receipt=tokio::task::spawn_blocking(move||commit_package(&target,&files,&expected,&signal)).await.map_err(io)??;
        gateway::invalidate_skill_metadata(ctx);
        receipt["scope"]=json!("project");
        receipt["validation"]=validation;
        receipt["next"]=json!("Use skill.list/skill.read to discover and reuse this version. Run executable scripts with skill.run; only verified general methods qualify for skill.promote.");
        return Ok(receipt);
    }
    if name=="skill.promote" {
        if scope!="project" { return Err(invalid("全局提升的来源必须为当前项目技能。")); }
        let source=target.clone();
        let context=ctx.clone();
        let proposal=args.clone();
        let prepared=tokio::task::spawn_blocking(move|| {
            let package=read_package(&source,&available,&signal)?;
            if package["valid"]!=true || package["packageHash"]!=proposal["expectedHash"] { return Err(invalid("项目技能校验未通过，或expectedHash不是当前完整包版本。")); }
            if package["validation"]["unavailableTools"].as_array().is_some_and(|tools|!tools.is_empty()) { return Err(invalid("请先连接或加载技能声明的工具并重新验证，再提升全局。")); }
            let evidence=promotion_evidence(&context,&proposal,&package)?;
            let files=package["files"].as_array().into_iter().flatten().map(|file| {
                let (path,relative)=service_skills::resolve_resource(&source,&file["relativePath"]).map_err(service)?;
                let bytes=local_files::read_checked(&path,MAX_FILE_BYTES as u64,false,"APP_SKILL_CHANGED").map_err(service)?;
                if local_files::sha256(&bytes)!=file["sha256"] { return Err(invalid("项目技能资源在提升期间变化。")); }
                Ok(AuthoredFile{relative,bytes})
            }).collect::<Result<Vec<_>,GatewayError>>()?;
            Ok::<_,GatewayError>((package,evidence,files))
        }).await.map_err(io)??;
        let (package,evidence,mut files)=prepared;
        // Put SKILL.md first so the ordinary commit receipt keeps its original content hash.
        // 统一正文为第一个文件，使提交回执保留正确的SKILL.md原文哈希。
        files.sort_by_key(|file|file.relative!="SKILL.md");
        let identity=json!({"scope":"global","name":args["name"],"packageHash":package["packageHash"],"targetExpectedHash":args["targetExpectedHash"],"verification":evidence});
        if let Some(pending)=authoring_approval(ctx,name,&args,&identity,true)? { return Ok(pending); }
        let target=package_root(ctx,"global")?.join(args["name"].as_str().unwrap());
        let expected=args["targetExpectedHash"].clone();
        let signal=cancellation.signal.clone();
        let mut receipt=tokio::task::spawn_blocking(move||commit_package(&target,&files,&expected,&signal)).await.map_err(io)??;
        gateway::invalidate_skill_metadata(ctx);
        receipt["scope"]=json!("global");
        receipt["verification"]=evidence;
        receipt["verification"]["generalReliability"]=json!("user-confirmed");
        return Ok(receipt);
    }
    Err(invalid("不支持的技能编写操作。"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{gateway as data, sqlite_connections};

    struct Workspace { ctx: GatewayToolContext, _owner: tokio::sync::watch::Sender<bool> }
    impl Workspace {
        fn new() -> Self {
            let (ctx, owner)=gateway::test_context();
            Self {ctx,_owner:owner}
        }
        fn archive(&self, name: &str, canonical: Value) -> Value {
            data::ensure_conversation(&self.ctx.data_home,&self.ctx.conversation_id,"Synthetic Skill authoring verification").unwrap();
            data::save_tool_result(&self.ctx.data_home,&self.ctx.conversation_id,&uuid::Uuid::new_v4().to_string(),
                &json!({"id":uuid::Uuid::new_v4().to_string(),"name":name}),&canonical,None).unwrap()
        }
    }
    impl Drop for Workspace {
        fn drop(&mut self) {
            let root=self.ctx.workdir.parent().unwrap();
            let _=sqlite_connections::close(&self.ctx.data_home,&self.ctx.data_home.join("Conversations/index.sqlite"));
            if root.starts_with(std::env::temp_dir()) { let _=fs::remove_dir_all(root); }
        }
    }
    fn body(description: &str) -> String {
        format!("---\nname: fixture-skill\ndescription: {description}\n---\nRead the chosen source, check its result, and report the useful outcome.\n")
    }
    fn save(content: String, expected: Value) -> Value {
        json!({"name":"fixture-skill","content":content,"expectedHash":expected})
    }
    fn promotion(package: &Value, references: Vec<Value>) -> Value {
        json!({"name":"fixture-skill","expectedHash":package["packageHash"],"targetExpectedHash":null,
            "generalization":"Read user-selected sources without project-specific paths or private data; verify actual tool results.","verificationRefs":references})
    }

    #[tokio::test]
    async fn project_skill_create_validate_reuse_and_update_refresh_the_existing_discovery_cache() {
        let workspace=Workspace::new();
        let ctx=&workspace.ctx;
        assert!(descriptors().iter().all(gateway::executable_descriptor));
        let first=gateway::execute("skill.save",save(body("Original useful method"),Value::Null),ctx).await.unwrap();
        assert_eq!(first["saved"],true);
        let checked=gateway::execute("skill.validate",json!({"name":"fixture-skill"}),ctx).await.unwrap();
        assert_eq!(checked["valid"],true);
        assert_eq!(checked["packageHash"],first["packageHash"]);
        assert_eq!(checked["validation"]["executionVerified"],false);
        let listed=gateway::execute("skill.list",json!({}),ctx).await.unwrap();
        let skill=listed["skills"].as_array().unwrap().iter().find(|skill|skill["name"]=="fixture-skill").unwrap();
        let id=skill["id"].clone();
        assert_eq!(gateway::execute("skill.read",json!({"id":id}),ctx).await.unwrap()["description"],"Original useful method");
        let second=execute("skill.save",save(body("Updated useful method"),first["packageHash"].clone()),ctx).await.unwrap();
        assert_eq!(second["updated"],true);
        assert_ne!(second["packageHash"],first["packageHash"]);
        assert_eq!(gateway::execute("skill.read",json!({"id":id}),ctx).await.unwrap()["description"],"Updated useful method");
        let stale=execute("skill.save",save(body("Must not overwrite"),first["packageHash"].clone()),ctx).await.unwrap_err();
        assert_eq!(stale.code,"APP_SKILL_CONFLICT");
        let invalid=execute("skill.save",save(body(""),second["packageHash"].clone()),ctx).await.unwrap();
        assert_eq!(invalid["saved"],false);
        let mut traversal=save(body("Resource path check"),second["packageHash"].clone());
        traversal["files"]=json!([{"path":"../escape.txt","content":"Must not escape"}]);
        assert!(execute("skill.save",traversal,ctx).await.is_err());
        assert!(!ctx.workdir.join(".kynxa/skills/escape.txt").exists());
        assert_eq!(execute("skill.validate",json!({"name":"fixture-skill"}),ctx).await.unwrap()["packageHash"],second["packageHash"]);
    }

    #[tokio::test]
    async fn global_skill_requires_current_real_receipts_and_explicit_general_reliability_confirmation() {
        let mut workspace=Workspace::new();
        workspace.ctx.permission_mode="full".into();
        let package=execute("skill.save",save(body("A reusable verified procedure"),Value::Null),&workspace.ctx).await.unwrap();
        assert!(execute("skill.promote",promotion(&package,Vec::new()),&workspace.ctx).await.is_err());
        let checked=execute("skill.validate",json!({"name":"fixture-skill"}),&workspace.ctx).await.unwrap();
        let source=workspace.archive("skill.validate",checked);
        let pending=workspace.archive("filesystem.read",json!({"requiresApproval":true,"status":"not-dispatched"}));
        assert!(execute("skill.promote",promotion(&package,vec![source.clone(),pending]),&workspace.ctx).await.is_err());
        let actual=workspace.archive("filesystem.read",json!({"content":"Synthetic verified source","sha256":"a".repeat(64)}));
        let arguments=promotion(&package,vec![source,actual]);
        let pending=execute("skill.promote",arguments.clone(),&workspace.ctx).await.unwrap();
        assert_eq!(pending["requiresApproval"],true);
        assert!(!workspace.ctx.extension_root.join("Skills/fixture-skill").exists());
        let mut approved=workspace.ctx.clone();
        approved.approved=true;
        approved.config["approvedInvocation"]=pending["invocation"].clone();
        let receipt=execute("skill.promote",arguments,&approved).await.unwrap();
        assert_eq!(receipt["saved"],true);
        assert_eq!(receipt["verification"]["generalReliability"],"user-confirmed");
        assert_eq!(execute("skill.validate",json!({"name":"fixture-skill","scope":"global"}),&workspace.ctx).await.unwrap()["packageHash"],package["packageHash"]);
        assert_eq!(fs::read_to_string(workspace.ctx.extension_root.join("Skills/fixture-skill/SKILL.md")).unwrap(),body("A reusable verified procedure"));
    }

    #[tokio::test]
    async fn executable_skill_stays_project_scoped_without_a_matching_isolated_execution_proof() {
        let workspace=Workspace::new();
        let mut proposal=save(body("Executable draft"),Value::Null);
        proposal["files"]=json!([{"path":"scripts/check.mjs","content":"// Synthetic draft; never executed by this test.\n"}]);
        let package=execute("skill.save",proposal,&workspace.ctx).await.unwrap();
        let checked=execute("skill.validate",json!({"name":"fixture-skill"}),&workspace.ctx).await.unwrap();
        assert_eq!(checked["validation"]["scripts"][0]["status"],"requires-real-execution");
        let source=workspace.archive("skill.validate",checked);
        let unrelated=workspace.archive("filesystem.read",json!({"content":"Another successful read"}));
        let error=execute("skill.promote",promotion(&package,vec![source,unrelated]),&workspace.ctx).await.unwrap_err();
        assert_eq!(error.code,"INVALID_SKILL_CONTENT");
        assert!(workspace.ctx.workdir.join(".kynxa/skills/fixture-skill/scripts/check.mjs").exists());
        assert!(!workspace.ctx.extension_root.join("Skills/fixture-skill").exists());
    }

    #[test]
    fn global_verification_never_treats_pending_nested_failure_or_unknown_as_success() {
        for receipt in [Value::Null,json!({"requiresApproval":true}),json!({"code":"MODEL_DISPATCH_UNKNOWN"}),
            json!({"receipt":{"command":{"completed":false}}}),json!({"structuredContent":{"error":"tool failed"}}),
            json!({"status":"not-dispatched"}),json!({"receipt":{"exitCode":1}})] {
            assert!(!successful(&receipt),"{receipt}");
        }
        assert!(successful(&json!({"command":{"completed":true,"exitCode":0},"receipt":{"status":"completed"}})));
    }
}

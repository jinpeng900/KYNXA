//! Stable workspace mounts share the formal catalog; they are not another path or permission ledger.
//! 稳定工作挂载复用正式目录，不建立另一套路径或权限账本。

use super::{ContractError, ContractResult};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::HashSet, path::Path};

fn invalid(message: &str) -> ContractError {
    ContractError::new(message, "INVALID_WORKSPACE_MOUNTS", 400)
}
fn path_key(path: &str) -> String {
    if cfg!(windows) { path.replace('/', "\\").trim_end_matches('\\').to_lowercase() }
    else { path.trim_end_matches('/').to_owned() }
}
fn legacy_id(project: &Value) -> String {
    let hash = Sha256::digest(format!("kynxa-workspace-mount-v1:{}", project["Id"].as_str().unwrap_or("")));
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&hash[..16]);
    bytes[6] = (bytes[6] & 15) | 80;
    bytes[8] = (bytes[8] & 63) | 128;
    uuid::Uuid::from_bytes(bytes).to_string()
}

/// Normalize old single-root and current multi-root projects without reading or creating any directory.
/// 规范化旧单根与多根项目，不读取或创建实体目录；首次正式保存会固化派生标识。
pub fn project(value: &Value) -> ContractResult {
    let mut project = value.clone();
    let explicit = value.get("Mounts").filter(|mounts| !mounts.is_null());
    let mounts = match explicit {
        Some(mounts) => mounts.as_array().ok_or_else(|| invalid("工作挂载列表无效。"))?.clone(),
        None => value["FolderPath"].as_str().filter(|path| !path.trim().is_empty()).map(|path|
            vec![json!({"Id":legacy_id(value),"Name":Path::new(path).file_name().and_then(|name| name.to_str()).unwrap_or("工作目录"),"Path":path,"BindingRevision":1})]).unwrap_or_default(),
    };
    let mut normalized = Vec::new();
    let mut ids = HashSet::new();
    let mut paths = HashSet::new();
    for mount in mounts {
        let id = mount["Id"].as_str().and_then(|id| uuid::Uuid::parse_str(id).ok())
            .ok_or_else(|| invalid("工作挂载标识无效。"))?.to_string();
        let path = mount["Path"].as_str().filter(|path| !path.trim().is_empty()
            && Path::new(path).is_absolute() && !path.contains(['\0','\r','\n']))
            .ok_or_else(|| invalid("挂载目录必须使用有效绝对路径。"))?;
        let revision = mount.get("BindingRevision").map_or(Some(1), Value::as_u64)
            .filter(|revision| *revision > 0 && *revision <= 9_007_199_254_740_991)
            .ok_or_else(|| invalid("工作挂载版本无效。"))?;
        let name = mount["Name"].as_str().filter(|name| !name.trim().is_empty()).unwrap_or("工作目录");
        if !ids.insert(id.clone()) || !paths.insert(path_key(path)) {
            return Err(invalid("工作挂载标识或目录重复。"));
        }
        normalized.push(json!({"Id":id,"Name":name,"Path":path,"BindingRevision":revision}));
    }
    let default = value["DefaultMountId"].as_str().filter(|id| !id.is_empty())
        .map(|id| uuid::Uuid::parse_str(id).map(|id|id.to_string()).map_err(|_|invalid("默认挂载标识无效。")))
        .transpose()?.or_else(|| normalized.first().and_then(|mount| mount["Id"].as_str()).map(str::to_owned));
    let default_mount = default.as_ref().and_then(|id| normalized.iter().find(|mount| mount["Id"] == *id));
    if default.is_some() && default_mount.is_none() { return Err(invalid("默认挂载不在当前工作目录列表中。")); }
    project["FolderPath"] = default_mount.map(|mount| mount["Path"].clone()).unwrap_or(Value::Null);
    project["DefaultMountId"] = default.map(Value::String).unwrap_or(Value::Null);
    project["Mounts"] = json!(normalized);
    Ok(project)
}

pub fn relationship(value: &Value) -> ContractResult {
    if !value.is_object() { return Ok(json!({"mounts":[],"defaultMountId":null,"folderPath":null})); }
    let project = project(value)?;
    Ok(json!({"mounts":project["Mounts"].as_array().unwrap().iter().map(|mount|
        json!({"id":mount["Id"],"name":mount["Name"],"path":mount["Path"],"bindingRevision":mount["BindingRevision"]})).collect::<Vec<_>>(),
        "defaultMountId":project["DefaultMountId"],"folderPath":project["FolderPath"]}))
}

pub fn bind_default(value: &Value, path: &str) -> ContractResult {
    let mut project = project(value)?;
    let id = project["DefaultMountId"].as_str().map(str::to_owned).unwrap_or_else(||legacy_id(value));
    let mounts = project["Mounts"].as_array_mut().unwrap();
    if let Some(mount) = mounts.iter_mut().find(|mount| mount["Id"] == id) {
        if mount["Path"] != path { mount["BindingRevision"] = json!(mount["BindingRevision"].as_u64().unwrap_or(1).saturating_add(1)); }
        mount["Path"] = json!(path);
    } else {
        mounts.push(json!({"Id":id,"Name":Path::new(path).file_name().and_then(|name|name.to_str()).unwrap_or("工作目录"),"Path":path,"BindingRevision":1}));
    }
    project["DefaultMountId"] = json!(id);
    super::workspace_mounts::project(&project)
}

pub fn preserve_versions(next: &Value, previous: &Value) -> ContractResult {
    let mut next = project(next)?;
    let previous = project(previous)?;
    for mount in next["Mounts"].as_array_mut().unwrap() {
        if let Some(old) = previous["Mounts"].as_array().unwrap().iter().find(|old|old["Id"] == mount["Id"]) {
            let old_revision = old["BindingRevision"].as_u64().unwrap_or(1);
            let changed = old["Path"].as_str().zip(mount["Path"].as_str()).is_none_or(|(a,b)|path_key(a)!=path_key(b));
            let minimum = old_revision.saturating_add(u64::from(changed));
            mount["BindingRevision"] = json!(mount["BindingRevision"].as_u64().unwrap_or(1).max(minimum));
        }
    }
    project(&next)
}

pub fn snapshot(relationship: &Value) -> Value {
    json!({"projectId":relationship["projectId"],"folderPath":relationship["folderPath"],
        "defaultMountId":relationship["defaultMountId"],"mounts":relationship["mounts"]})
}

pub fn same_mount(previous: &Value, current: &Value) -> bool {
    previous["id"] == current["id"] && previous["bindingRevision"] == current["bindingRevision"]
        && previous["path"].as_str().zip(current["path"].as_str()).is_some_and(|(a,b)|path_key(a)==path_key(b))
}

/// A new unrelated mount does not revoke an old task; only its selected/default binding must remain valid.
/// 新增无关挂载不撤销旧任务；只复核该任务选定或默认的既有绑定，实际工具仍逐次检查所用根。
pub fn validate_snapshot(previous: &Value, current: &Value) -> bool {
    if previous["projectId"] != current["projectId"] { return false; }
    if let Some(id) = previous["defaultMountId"].as_str() {
        let old = previous["mounts"].as_array().into_iter().flatten().find(|mount|mount["id"] == id);
        let new = current["mounts"].as_array().into_iter().flatten().find(|mount|mount["id"] == id);
        return old.zip(new).is_some_and(|(old,new)|same_mount(old,new));
    }
    previous["folderPath"] == current["folderPath"]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_mount_identity_is_stable_and_unrelated_changes_do_not_revoke_it() {
        let old = json!({"Id":"58b59200-e290-4610-93c8-daf7d9da2199","FolderPath":std::env::temp_dir().join("work-a")});
        let project = project(&old).unwrap();
        assert_eq!(project["Mounts"][0]["Id"],super::project(&old).unwrap()["Mounts"][0]["Id"]);
        let mut relation = relationship(&project).unwrap();
        relation["projectId"] = old["Id"].clone();
        let saved = snapshot(&relation);
        relation["mounts"].as_array_mut().unwrap().push(json!({"id":uuid::Uuid::new_v4().to_string(),"name":"B","path":std::env::temp_dir().join("work-b"),"bindingRevision":1}));
        assert!(validate_snapshot(&saved,&relation));
        relation["mounts"][0]["path"] = json!(std::env::temp_dir().join("replaced"));
        assert!(!validate_snapshot(&saved,&relation));
    }
}

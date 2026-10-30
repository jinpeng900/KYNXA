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

/// Keep one work folder while retaining its existing permission identity.
/// 每个工作只保留一个关联目录，沿用原权限标识；不读取或创建实体目录。
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
        .transpose()?.or_else(|| value["FolderPath"].as_str()
            .and_then(|folder| normalized.iter().find(|mount|mount["Path"].as_str().is_some_and(|path|path_key(path)==path_key(folder))))
            .or_else(||normalized.first()).and_then(|mount|mount["Id"].as_str()).map(str::to_owned));
    let default_mount = default.as_ref().and_then(|id| normalized.iter().find(|mount| mount["Id"] == *id));
    if default.is_some() && default_mount.is_none() { return Err(invalid("默认挂载不在当前工作目录列表中。")); }
    project["FolderPath"] = default_mount.map(|mount| mount["Path"].clone()).unwrap_or(Value::Null);
    project["DefaultMountId"] = default.map(Value::String).unwrap_or(Value::Null);
    normalized.retain(|mount|mount["Id"] == project["DefaultMountId"]);
    project["Mounts"] = json!(normalized);
    Ok(project)
}

/// Reject new multi-folder submissions; legacy reads still select their original default.
/// 新提交拒绝多目录，旧数据读取仍兼容并保留原默认目录。
pub fn validate_submission(value: &Value) -> Result<(), ContractError> {
    if value["Mounts"].as_array().is_some_and(|mounts|mounts.len()>1) {
        return Err(invalid("一个工作只能关联一个文件夹；请选择更换当前文件夹。"));
    }
    Ok(())
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
        if !mount["Path"].as_str().is_some_and(|old|path_key(old)==path_key(path)) { mount["BindingRevision"] = json!(mount["BindingRevision"].as_u64().unwrap_or(1).saturating_add(1)); }
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
    fn old_multi_folder() -> Value {
        json!({"Id":"58b59200-e290-4610-93c8-daf7d9da2199",
            "DefaultMountId":"f228652d-5733-4c5c-b246-f971886444ed",
            "Mounts":[
                {"Id":"67bb9c9f-aedd-4cdd-a858-4c7be90beab9","Name":"Other","Path":std::env::temp_dir().join("other"),"BindingRevision":2},
                {"Id":"f228652d-5733-4c5c-b246-f971886444ed","Name":"Current","Path":std::env::temp_dir().join("current"),"BindingRevision":7}]})
    }
    #[test]
    fn old_multi_folder_keeps_only_the_default_with_its_identity_and_revision() {
        let old=old_multi_folder(); let single=project(&old).unwrap();
        assert_eq!(single["Mounts"],json!([old["Mounts"][1]]));
        assert_eq!(single["DefaultMountId"],old["DefaultMountId"]);
        assert_eq!(single["FolderPath"],old["Mounts"][1]["Path"]);
        assert_eq!(project(&single).unwrap(),single);
        let mut fallback=old.clone(); fallback["DefaultMountId"]=Value::Null; fallback["FolderPath"]=old["Mounts"][1]["Path"].clone();
        assert_eq!(project(&fallback).unwrap()["Mounts"],single["Mounts"]);
    }
    #[test]
    fn multi_folder_submission_is_rejected_and_replacement_keeps_one_versioned_binding() {
        let old=old_multi_folder(); assert!(validate_submission(&old).is_err());
        let single=project(&old).unwrap(); assert!(validate_submission(&single).is_ok());
        let same=bind_default(&single,single["FolderPath"].as_str().unwrap()).unwrap();
        assert_eq!(same["Mounts"][0]["BindingRevision"],7);
        let replacement=std::env::temp_dir().join("replacement");
        let next=bind_default(&single,replacement.to_str().unwrap()).unwrap();
        assert_eq!(next["Mounts"].as_array().unwrap().len(),1);
        assert_eq!(next["Mounts"][0]["Id"],single["Mounts"][0]["Id"]);
        assert_eq!(next["Mounts"][0]["BindingRevision"],8);
        assert!(!validate_snapshot(&snapshot(&relationship(&single).unwrap()),&relationship(&next).unwrap()));
    }
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

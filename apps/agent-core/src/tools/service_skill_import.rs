use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{
    Arc, Mutex, OnceLock, Weak,
    atomic::{AtomicBool, Ordering},
};

use serde_json::{Value, json};

use super::local_files::{
    absolute_path, ensure_directory, inspect, path_key, path_text, random_identity, read_checked,
    sha256, within,
};
use super::service_error::{ServiceError, ServiceResult};
use super::service_skills::{SkillInventory, resolve_resource};
use crate::models::protocol::{array, string};

static IMPORT_LOCKS: OnceLock<Mutex<HashMap<String, Weak<Mutex<()>>>>> = OnceLock::new();

fn cancelled(cancellation: &Option<Arc<AtomicBool>>) -> ServiceResult<()> {
    if cancellation
        .as_ref()
        .is_some_and(|cancellation| cancellation.load(Ordering::Acquire))
    {
        return Err(ServiceError::new("TOOL_CANCELLED", "技能导入已取消。", 409));
    }
    Ok(())
}

fn inventory(path: &Path, cancellation: &Option<Arc<AtomicBool>>) -> ServiceResult<Value> {
    let mut inventory = SkillInventory::default();
    let mut result = inventory.apply(
        "skill.inventoryInit",
        &json!({"root":path_text(path),"hashes":true}),
        cancellation,
    )?;
    while result.get("authorization").is_some() {
        result = inventory.apply(
            "skill.inventoryNext",
            &json!({"allowed":true}),
            cancellation,
        )?;
    }
    Ok(result["inventory"].clone())
}

fn fingerprint(files: &Value) -> Vec<Value> {
    array(files).iter().map(|file| json!({"relativePath":file["relativePath"],"size":file["size"],"sha256":file["sha256"]})).collect()
}

fn equal(
    target: &Path,
    files: &Value,
    cancellation: &Option<Arc<AtomicBool>>,
) -> ServiceResult<bool> {
    let Some(metadata) = inspect(target, true, false)? else {
        return Ok(false);
    };
    if !metadata.is_dir() {
        return Err(ServiceError::new(
            "APP_SKILL_CONFLICT",
            "A different skill package already uses this name.",
            409,
        ));
    }
    let existing = inventory(target, cancellation)?;
    if existing["valid"] != true || fingerprint(&existing["files"]) != fingerprint(files) {
        return Err(ServiceError::new(
            "APP_SKILL_CONFLICT",
            "A different skill package already uses this name; existing files were preserved.",
            409,
        ));
    }
    Ok(true)
}

pub fn import_package(
    arguments: &Value,
    cancellation: &Option<Arc<AtomicBool>>,
) -> ServiceResult<Value> {
    cancelled(cancellation)?;
    let root = absolute_path(string(&arguments["root"]));
    let target = absolute_path(string(&arguments["target"]));
    let files = &arguments["files"];
    if !within(&root.join("Skills"), &target) || target == root.join("Skills") {
        return Err(ServiceError::new(
            "UNSAFE_SKILL_RESOURCE_PATH",
            "Invalid skill import target.",
            403,
        ));
    }
    let key = path_key(&target);
    let lock = {
        let mut locks = IMPORT_LOCKS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap();
        locks.retain(|_, lock| lock.strong_count() > 0);
        let lock = locks
            .get(&key)
            .and_then(Weak::upgrade)
            .unwrap_or_else(|| Arc::new(Mutex::new(())));
        locks.insert(key, Arc::downgrade(&lock));
        lock
    };
    let _guard = lock.lock().unwrap();
    cancelled(cancellation)?;
    if let Some(bundled) = arguments["bundledDirectory"].as_str() {
        let bundled = absolute_path(bundled).join(target.file_name().unwrap_or_default());
        if inspect(&bundled, true, false)?.is_some_and(|metadata| metadata.is_dir()) {
            let existing = inventory(&bundled, cancellation)?;
            if existing["valid"] == true && fingerprint(&existing["files"]) == fingerprint(files) {
                return Ok(
                    json!({"imported":false,"reused":true,"directory":path_text(&bundled),"origin":"builtin"}),
                );
            }
        }
    }
    if equal(&target, files, cancellation)? {
        return Ok(
            json!({"imported":false,"reused":true,"directory":path_text(&target),"origin":"data"}),
        );
    }
    ensure_directory(&root.join("Skills"))?;
    let staging_parent = root.join("Agent/skill-imports");
    ensure_directory(&staging_parent)?;
    let stage = staging_parent.join(format!("package-{}", random_identity()?));
    fs::create_dir(&stage).map_err(ServiceError::io)?;
    let result = (|| {
        for file in array(files) {
            cancelled(cancellation)?;
            let source = absolute_path(string(&file["path"]));
            let bytes = read_checked(&source, 2 * 1024 * 1024, false, "APP_SKILL_CHANGED")?;
            if json!(bytes.len()) != file["size"] || sha256(&bytes) != string(&file["sha256"]) {
                return Err(ServiceError::new(
                    "APP_SKILL_CHANGED",
                    "Skill package changed after validation; import was cancelled.",
                    409,
                ));
            }
            let (destination, _) = resolve_resource(&stage, &file["relativePath"])?;
            ensure_directory(destination.parent().unwrap())?;
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut output = options.open(destination).map_err(ServiceError::io)?;
            output.write_all(&bytes).map_err(ServiceError::io)?;
            output.sync_all().map_err(ServiceError::io)?;
        }
        cancelled(cancellation)?;
        if equal(&target, files, cancellation)? {
            return Ok(
                json!({"imported":false,"reused":true,"directory":path_text(&target),"origin":"data"}),
            );
        }
        // Only the validated complete staging package can be committed; later cancellation preserves the commit receipt.
        // 只有完整且经过校验的暂存包才能提交；随后取消仍保留真实提交回执。
        match fs::rename(&stage, &target) {
            Ok(()) => Ok(
                json!({"imported":true,"reused":false,"directory":path_text(&target),"origin":"data"}),
            ),
            Err(error)
                if [
                    std::io::ErrorKind::AlreadyExists,
                    std::io::ErrorKind::PermissionDenied,
                    std::io::ErrorKind::DirectoryNotEmpty,
                ]
                .contains(&error.kind())
                    && equal(&target, files, cancellation)? =>
            {
                Ok(
                    json!({"imported":false,"reused":true,"directory":path_text(&target),"origin":"data"}),
                )
            }
            Err(error) => Err(ServiceError::io(error)),
        }
    })();
    // Verify the canonical staging boundary before any recursive cleanup, including replacement/link races.
    // 递归清理前核验真实暂存边界，同时拒绝目录替换和链接竞态。
    if !within(&staging_parent, &stage) || stage == staging_parent {
        return Err(ServiceError::new(
            "UNSAFE_SKILL_RESOURCE_PATH",
            "Invalid skill staging cleanup path.",
            403,
        ));
    }
    if inspect(&stage, true, false)?.is_some() {
        let canonical_parent = fs::canonicalize(&staging_parent).map_err(ServiceError::io)?;
        let canonical_stage = fs::canonicalize(&stage).map_err(ServiceError::io)?;
        if !within(&canonical_parent, &canonical_stage) || canonical_parent == canonical_stage {
            return Err(ServiceError::new(
                "UNSAFE_SKILL_RESOURCE_PATH",
                "Invalid skill staging cleanup path.",
                403,
            ));
        }
        fs::remove_dir_all(&stage).map_err(ServiceError::io)?;
    }
    result
}

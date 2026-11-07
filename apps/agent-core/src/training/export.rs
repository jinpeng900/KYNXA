//! 已登记模型按流导出；不覆盖目标，不把大权重一次装进内存。
use super::{invalid, jobs, store};
use crate::{
    gateway_contract::GatewayError,
    storage::managed_files,
    tools::{gateway, local_files},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

fn conflict(message: &str) -> GatewayError {
    GatewayError::new("TRAINING_EXPORT_CONFLICT", message, 409)
}

pub(super) async fn export(
    input: &Value,
    ctx: &gateway::GatewayToolContext,
) -> Result<Value, GatewayError> {
    let result = jobs::read(input, ctx)?;
    let Some(destination) = input["destination"].as_str() else {
        return Ok(result);
    };
    let artifact = input["artifactPath"]
        .as_str()
        .ok_or_else(|| invalid("请选择要导出的训练产物。"))?;
    let id = store::id(input, "jobId")?;
    let owned_jobdir = store::scope_dir(ctx).join("jobs").join(id);
    let source = local_files::absolute_path(artifact);
    let metadata = local_files::inspect(&source, false, false)
        .map_err(|e| GatewayError::new(e.code, e.message, e.status_code))?
        .ok_or_else(|| invalid("训练产物不存在。"))?;
    let source = local_files::absolute_path(&local_files::path_text(&fs::canonicalize(&source)?));
    let jobdir =
        local_files::absolute_path(&local_files::path_text(&fs::canonicalize(&owned_jobdir)?));
    if !local_files::within(&jobdir, &source)
        || !result["artifacts"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|item| {
                item["path"]
                    .as_str()
                    .and_then(|p| fs::canonicalize(p).ok())
                    .is_some_and(|p| local_files::path_key(&p) == local_files::path_key(&source))
            })
    {
        return Err(invalid("产物不属于当前训练作业。"));
    }
    // 真实路径只核对归属；内部读取沿用正式根的拼写，兼容Windows长名与8.3别名。
    let relative = source
        .strip_prefix(&jobdir)
        .map_err(|_| invalid("产物归属无法绑定到当前作业。"))?;
    let source = owned_jobdir.join(relative);
    if local_files::absolute_path(&local_files::path_text(&fs::canonicalize(&source)?))
        != jobdir.join(relative)
    {
        return Err(conflict("产物路径在绑定期间已变化。"));
    }
    if !metadata.is_file() && !metadata.is_dir() {
        return Err(invalid("训练产物不是普通文件或目录。"));
    }
    let target = authorized_target(ctx, destination)?;
    if target.exists() {
        return Err(GatewayError::new(
            "TRAINING_EXPORT_EXISTS",
            "目标已存在，请选择新的名称，原文件未覆盖。",
            409,
        ));
    }
    let expected = input["sourceExpectedHash"].as_str().map(str::to_owned);
    let scope = ctx.clone();
    let requested = destination.to_owned();
    tokio::task::spawn_blocking(move || {
        copy_registered(&scope, &source, &target, &requested, expected.as_deref())
    })
    .await
    .map_err(|_| {
        GatewayError::new(
            "TRAINING_EXPORT_FAILED",
            "训练产物导出未完成，原产物保留。",
            500,
        )
    })?
}

fn authorized_target(
    ctx: &gateway::GatewayToolContext,
    requested: &str,
) -> Result<PathBuf, GatewayError> {
    gateway::check_cancelled(ctx)?;
    let (target, outside) = gateway::canonical_authorized(ctx, requested, true)?;
    if gateway::protected_path(ctx, &target)
        || outside && !ctx.approved && ctx.permission_mode != "full"
    {
        return Err(GatewayError::new(
            "TRAINING_EXPORT_FORBIDDEN",
            "请导出到当前授权工作目录，或先授权目标目录。",
            403,
        ));
    }
    local_files::inspect(&target, true, false)
        .map_err(|e| GatewayError::new(e.code, e.message, e.status_code))?;
    Ok(target)
}

struct DirectoryStamp {
    path: PathBuf,
    volume: u64,
    identity: u64,
    modified: Option<std::time::SystemTime>,
}
fn directory_file(path: &Path) -> Result<File, GatewayError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0200_0000 | 0x0020_0000);
    }
    Ok(options.open(path)?)
}
impl DirectoryStamp {
    fn capture(path: &Path) -> Result<Self, GatewayError> {
        if !local_files::inspect(path, false, false)
            .map_err(|e| GatewayError::new(e.code, e.message, e.status_code))?
            .is_some_and(|m| m.is_dir())
        {
            return Err(conflict("目录目标已变化。"));
        }
        let handle = directory_file(path)?;
        let identity = crate::file_identity::handle_identity(&handle)?;
        if identity.is_link {
            return Err(conflict("目录包含不允许的链接。"));
        }
        Ok(Self {
            path: path.to_owned(),
            volume: identity.volume,
            identity: identity.identity,
            modified: handle.metadata()?.modified().ok(),
        })
    }
    fn verify(&self, contents: bool) -> Result<(), GatewayError> {
        let current = Self::capture(&self.path)?;
        if (self.volume, self.identity) != (current.volume, current.identity)
            || contents && self.modified != current.modified
        {
            return Err(conflict("导出期间目录身份或内容已变化，原产物保留。"));
        }
        Ok(())
    }
}

struct SourceFile {
    relative: PathBuf,
    version: managed_files::StableFileVersion,
    token: Option<String>,
    bytes: u64,
}
struct Plan {
    files: Vec<SourceFile>,
    directories: Vec<DirectoryStamp>,
    is_directory: bool,
}
impl Plan {
    fn collect(ctx: &gateway::GatewayToolContext, source: &Path) -> Result<Self, GatewayError> {
        let mut plan = Self {
            files: Vec::new(),
            directories: Vec::new(),
            is_directory: source.is_dir(),
        };
        plan.visit(ctx, source, Path::new(""))?;
        Ok(plan)
    }
    fn visit(
        &mut self,
        ctx: &gateway::GatewayToolContext,
        path: &Path,
        relative: &Path,
    ) -> Result<(), GatewayError> {
        gateway::check_cancelled(ctx)?;
        let metadata = local_files::inspect(path, false, false)
            .map_err(|e| GatewayError::new(e.code, e.message, e.status_code))?
            .ok_or_else(|| conflict("源产物在导出前已变化。"))?;
        if metadata.is_dir() {
            managed_files::inspect(&ctx.data_home,path,false,false,u64::MAX).map_err(|error|GatewayError::from(error).with_details(json!({"stage":"training-export-source-directory","withinOwnedRoot":path.starts_with(&ctx.data_home)})))?;
            self.directories.push(DirectoryStamp::capture(path)?);
            let mut entries = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                self.visit(ctx, &entry.path(), &relative.join(entry.file_name()))?;
            }
        } else if metadata.is_file() {
            let reader=managed_files::open_reader(&ctx.data_home,path,u64::MAX).map_err(|error|GatewayError::from(error).with_details(json!({"stage":"training-export-source-file","withinOwnedRoot":path.starts_with(&ctx.data_home)})))?.ok_or_else(||conflict("源产物已不存在。"))?;
            let token = reader.version_token()?;
            let bytes = reader.file.metadata()?.len();
            self.files.push(SourceFile {
                relative: relative.to_owned(),
                version: reader.into_version(),
                token,
                bytes,
            });
        } else {
            return Err(invalid("导出目录中包含不支持的对象。"));
        }
        Ok(())
    }
}

/// 只清理本次独占创建的暂存项，不递归删除用户目录。
struct Stage {
    files: Vec<(PathBuf, u64, u64)>,
    versions: Vec<(PathBuf, local_files::FileVersion, Option<i64>)>,
    directories: Vec<DirectoryStamp>,
    published: bool,
}
impl Stage {
    fn verify(&self) -> Result<(), GatewayError> {
        let expected = self
            .files
            .iter()
            .map(|(path, _, _)| path)
            .chain(self.directories.iter().map(|dir| &dir.path))
            .collect::<std::collections::HashSet<_>>();
        for dir in &self.directories {
            dir.verify(false)?;
            for entry in fs::read_dir(&dir.path)? {
                if !expected.contains(&entry?.path()) {
                    return Err(conflict("导出暂存目录中出现未登记对象。"));
                }
            }
        }
        for (path, version, stamp) in &self.versions {
            local_files::inspect(path, false, false)
                .map_err(|e| GatewayError::new(e.code, e.message, e.status_code))?;
            let handle = local_files::open_read(path)
                .map_err(|e| GatewayError::new(e.code, e.message, e.status_code))?;
            let current = local_files::handle_version(&handle, &handle.metadata()?)
                .map_err(|e| GatewayError::new(e.code, e.message, e.status_code))?;
            if current != *version || crate::file_identity::change_stamp(&handle)? != *stamp {
                return Err(conflict("导出暂存文件被修改，未发布导出。"));
            }
        }
        Ok(())
    }
}
impl Drop for Stage {
    fn drop(&mut self) {
        if self.published {
            return;
        }
        for (path, volume, identity) in self.files.iter().rev() {
            if local_files::inspect(path, true, false)
                .ok()
                .flatten()
                .is_some_and(|m| m.is_file())
                && local_files::open_read(path)
                    .ok()
                    .and_then(|f| crate::file_identity::handle_identity(&f).ok())
                    .is_some_and(|i| (i.volume, i.identity) == (*volume, *identity))
            {
                let _ = fs::remove_file(path);
            }
        }
        for directory in self.directories.iter().rev() {
            if directory.verify(false).is_ok() {
                let _ = fs::remove_dir(&directory.path);
            }
        }
    }
}

fn copy_registered(
    ctx: &gateway::GatewayToolContext,
    source: &Path,
    target: &Path,
    requested: &str,
    expected: Option<&str>,
) -> Result<Value, GatewayError> {
    let parent = target
        .parent()
        .ok_or_else(|| invalid("导出目标缺少父目录。"))?;
    let parent_stamp = DirectoryStamp::capture(parent)?;
    let plan = Plan::collect(ctx, source)?;
    let root = parent.join(format!(
        ".kynxa-training-export-{}.tmp",
        uuid::Uuid::new_v4()
    ));
    let mut stage = Stage {
        files: Vec::new(),
        versions: Vec::new(),
        directories: Vec::new(),
        published: false,
    };
    if plan.is_directory {
        fs::create_dir(&root)?;
        stage.directories.push(DirectoryStamp::capture(&root)?);
        for dir in plan.directories.iter().skip(1) {
            let relative = dir
                .path
                .strip_prefix(source)
                .map_err(|_| invalid("源目录结构无效。"))?;
            let path = root.join(relative);
            fs::create_dir(&path)?;
            stage.directories.push(DirectoryStamp::capture(&path)?);
        }
    }
    let mut receipts = Vec::new();
    let mut aggregate = Sha256::new();
    let mut total = 0u64;
    for file in &plan.files {
        gateway::check_cancelled(ctx)?;
        let output = if plan.is_directory {
            root.join(&file.relative)
        } else {
            root.clone()
        };
        let (bytes, hash) = copy_file(ctx, file, &output, &mut stage)?;
        total = total
            .checked_add(bytes)
            .ok_or_else(|| invalid("导出总大小无法表示。"))?;
        let relative = file.relative.to_string_lossy().replace('\\', "/");
        let record = json!({"path":relative,"bytes":bytes,"sha256":hash});
        aggregate.update(serde_json::to_vec(&record).unwrap());
        aggregate.update(b"\n");
        if receipts.len() < 128 {
            receipts.push(record);
        }
    }
    for dir in &plan.directories {
        dir.verify(true)?;
    }
    for source in &plan.files {
        let current = source
            .version
            .reopen()?
            .ok_or_else(|| conflict("源文件在导出提交前已变化。"))?;
        current.verify()?;
        if current.version_token()? != source.token {
            return Err(conflict("源文件在导出提交前已变化。"));
        }
    }
    stage.verify()?;
    let digest = if plan.is_directory {
        format!("{:x}", aggregate.finalize())
    } else {
        receipts
            .first()
            .and_then(|r| r["sha256"].as_str())
            .unwrap_or("")
            .to_owned()
    };
    if expected.is_some_and(|value| value != digest) {
        return Err(conflict("源产物版本与预期摘要不一致，未发布导出。"));
    }
    parent_stamp.verify(false)?;
    if authorized_target(ctx, requested)? != target {
        return Err(conflict("导出目标的授权绑定已变化。"));
    }
    if target.exists() {
        return Err(GatewayError::new(
            "TRAINING_EXPORT_EXISTS",
            "目标在导出期间出现，原文件未覆盖。",
            409,
        ));
    }
    gateway::check_cancelled(ctx)?;
    publish_new(&root, target)?;
    stage.published = true;
    // The commit has happened. A later cancellation cannot turn its receipt into a failed write.
    Ok(
        json!({"path":local_files::path_text(target),"kind":if plan.is_directory{"directory"}else{"file"},"bytes":total,
        "sha256":digest,"fileCount":plan.files.len(),"files":receipts,"filesTruncated":plan.files.len()>128,
        "created":true,"executed":true,"reused":false,"overwrite":false}),
    )
}

fn copy_file(
    ctx: &gateway::GatewayToolContext,
    source: &SourceFile,
    target: &Path,
    stage: &mut Stage,
) -> Result<(u64, String), GatewayError> {
    let mut reader = source
        .version
        .reopen()?
        .ok_or_else(|| conflict("源文件已不存在。"))?;
    if reader.version_token()? != source.token {
        return Err(conflict("源文件版本在导出前已变化。"));
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options.open(target)?;
    let owned = crate::file_identity::handle_identity(&output)?;
    stage
        .files
        .push((target.to_owned(), owned.volume, owned.identity));
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut total = 0u64;
    loop {
        gateway::check_cancelled(ctx)?;
        let count = reader.file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        output.write_all(&buffer[..count])?;
        hasher.update(&buffer[..count]);
        total += count as u64;
    }
    output.sync_all()?;
    let version = local_files::handle_version(&output, &output.metadata()?)
        .map_err(|e| GatewayError::new(e.code, e.message, e.status_code))?;
    let stamp = crate::file_identity::change_stamp(&output)?;
    stage.versions.push((target.to_owned(), version, stamp));
    drop(output);
    reader.verify()?;
    if total != source.bytes || reader.version_token()? != source.token {
        return Err(conflict("源文件在导出期间被修改，未发布导出。"));
    }
    Ok((total, format!("{:x}", hasher.finalize())))
}

fn publish_new(source: &Path, target: &Path) -> Result<(), GatewayError> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn MoveFileExW(source: *const u16, target: *const u16, flags: u32) -> i32;
        }
        let wide = |path: &Path| {
            let text = path.to_string_lossy();
            let value = if text.starts_with(r"\\?\") {
                text.into_owned()
            } else if let Some(unc) = text.strip_prefix(r"\\") {
                format!(r"\\?\UNC\{unc}")
            } else {
                format!(r"\\?\{text}")
            };
            std::ffi::OsStr::new(&value)
                .encode_wide()
                .chain(Some(0))
                .collect::<Vec<_>>()
        };
        let a = wide(source);
        let b = wide(target);
        // WRITE_THROUGH, without REPLACE_EXISTING: both files and directories remain no-clobber.
        if unsafe { MoveFileExW(a.as_ptr(), b.as_ptr(), 8) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        unsafe extern "C" {
            fn renameat2(
                oldfd: i32,
                old: *const std::ffi::c_char,
                newfd: i32,
                new: *const std::ffi::c_char,
                flags: u32,
            ) -> i32;
        }
        let a = std::ffi::CString::new(source.as_os_str().as_bytes())
            .map_err(|_| invalid("导出路径无效。"))?;
        let b = std::ffi::CString::new(target.as_os_str().as_bytes())
            .map_err(|_| invalid("导出路径无效。"))?;
        if unsafe { renameat2(-100, a.as_ptr(), -100, b.as_ptr(), 1) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        return Ok(());
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = (source, target);
        Err(GatewayError::new(
            "TRAINING_EXPORT_PLATFORM_UNSUPPORTED",
            "此平台未提供安全的不覆盖发布。",
            501,
        ))
    }
}

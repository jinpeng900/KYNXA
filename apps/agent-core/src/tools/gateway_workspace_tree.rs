//! Workspace tree mirrors one conversation's work directory for the right pane.
//! 工作区树把一个会话的工作目录镜像给右栏：只读、不复制字节，也不暴露受保护路径或链接。
//!
//! The mirror lists whatever the work directory holds, whether the user uploaded it or the model produced it.
//! Attachment originals stay in the formal attachment ledger and appear here as a virtual read-only folder.
//! 镜像列出工作目录里的全部内容，不区分用户上传还是模型生成；附件原件仍保存在正式附件账本中，
//! 在这里以只读的虚拟目录呈现，避免同一文件出现两份副本。

use super::gateway::GatewayToolContext;
use crate::gateway_contract::GatewayError;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// Large generated trees are truncated instead of blocking the right pane.
/// 生成文件过多时截断结果，避免阻塞右栏。
const MAXIMUM_ENTRIES: usize = 2000;
const MAXIMUM_DEPTH: usize = 4;
const MAXIMUM_SNAPSHOT_BYTES: u64 = 8 * 1024 * 1024;
const SNAPSHOT_VERSION: u64 = 1;
/// Files outside these names are listed normally; the virtual folder is reserved for attachment originals.
/// 这些名字之外的文件正常列出；虚拟目录名专供附件原件，避免与真实目录冲突。
const VIRTUAL_ATTACHMENT_FOLDER: &str = "_attachments";

pub(crate) async fn tree(
    arguments: &Value,
    context: &GatewayToolContext,
) -> Result<Value, GatewayError> {
    let since = arguments["since"].as_str().unwrap_or("").to_owned();
    let maximum_entries = limit(arguments, "maximumEntries", MAXIMUM_ENTRIES);
    let maximum_depth = limit(arguments, "maximumDepth", MAXIMUM_DEPTH);
    let root = context.data_home.clone();
    let workdir = context.workdir.clone();
    let conversation = context.conversation_id.clone();
    tokio::task::spawn_blocking(move || {
        build(
            &root,
            &workdir,
            &conversation,
            &since,
            maximum_entries,
            maximum_depth,
        )
    })
    .await
    .map_err(|_| GatewayError::new("WORKSPACE_TREE_FAILED", "工作区目录读取线程结束。", 500))?
}

fn limit(arguments: &Value, name: &str, fallback: usize) -> usize {
    arguments[name]
        .as_u64()
        .map(|value| value as usize)
        .filter(|value| *value > 0)
        .unwrap_or(fallback)
}

/// An isolated work directory is the per-chat sandbox; anything else is a linked work folder.
/// 隔离工作目录是按聊天分配的沙盒；其余情况是已挂载的工作文件夹。
fn is_isolated(root: &Path, workdir: &Path, conversation: &str) -> bool {
    workdir == root.join("Sandboxes").join(conversation)
}

fn build(
    root: &Path,
    workdir: &Path,
    conversation: &str,
    since: &str,
    maximum_entries: usize,
    maximum_depth: usize,
) -> Result<Value, GatewayError> {
    let mut entries = Vec::new();
    let mut truncated = false;
    if workdir.is_dir() {
        walk(
            workdir,
            workdir,
            0,
            maximum_depth,
            maximum_entries,
            &mut entries,
            &mut truncated,
        );
    }
    entries.sort_by(|left, right| left["path"].as_str().cmp(&right["path"].as_str()));
    let version = fingerprint(&entries);
    let changed = since != version;
    if changed {
        let previous = read_snapshot(root, conversation).unwrap_or_default();
        mark_status(&mut entries, &previous);
        // A snapshot is a derived annotation cache; failing to write it must not fail the mirror.
        // 快照是派生的标注缓存；写入失败不应让镜像请求失败。
        let _ = write_snapshot(root, conversation, &entries);
    }
    let attachments = crate::storage::message_attachments::conversation_list(root, conversation)
        .map(|value| value["attachments"].clone())
        .unwrap_or_else(|_| json!([]));
    Ok(json!({
        "version":version,
        "changed":changed,
        "workspaceKind":if is_isolated(root, workdir, conversation) {"isolated"} else {"linked"},
        "root":workdir,
        "truncated":truncated,
        "virtualAttachmentFolder":VIRTUAL_ATTACHMENT_FOLDER,
        "entries":entries,
        "attachments":attachments,
    }))
}

/// Depth-limited traversal never follows links, because a link can point outside the work directory.
/// 有深度上限的遍历不跟随链接，链接可能指向工作目录之外。
fn walk(
    root: &Path,
    directory: &Path,
    depth: usize,
    maximum_depth: usize,
    maximum_entries: usize,
    entries: &mut Vec<Value>,
    truncated: &mut bool,
) {
    let Ok(read) = std::fs::read_dir(directory) else {
        return;
    };
    let mut children = read.filter_map(Result::ok).collect::<Vec<_>>();
    children.sort_by_key(|entry| entry.file_name());
    for child in children {
        if entries.len() >= maximum_entries {
            *truncated = true;
            return;
        }
        let name = child.file_name().to_string_lossy().into_owned();
        // The virtual folder name is reserved; a real directory with that name would shadow attachments.
        // 虚拟目录名已被占用；真实目录使用该名会遮蔽附件视图。
        if name == VIRTUAL_ATTACHMENT_FOLDER {
            continue;
        }
        let path = child.path();
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.file_type().is_symlink() || is_reparse_point(&metadata) {
            continue;
        }
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        let relative = relative.to_string_lossy().replace('\\', "/");
        if relative.is_empty() || relative.split('/').any(|part| part.is_empty() || part == "..") {
            continue;
        }
        if metadata.is_dir() {
            entries.push(json!({
                "path":relative, "name":name, "kind":"directory",
                "modifiedAt":modified_millis(&metadata),
            }));
            if depth + 1 < maximum_depth {
                walk(
                    root,
                    &path,
                    depth + 1,
                    maximum_depth,
                    maximum_entries,
                    entries,
                    truncated,
                );
            } else if has_children(&path) {
                *truncated = true;
            }
        } else if metadata.is_file() {
            entries.push(json!({
                "path":relative, "name":name, "kind":"file",
                "sizeBytes":metadata.len(), "modifiedAt":modified_millis(&metadata),
            }));
        }
    }
}

fn has_children(directory: &Path) -> bool {
    std::fs::read_dir(directory)
        .map(|mut read| read.next().is_some())
        .unwrap_or(false)
}

#[cfg(windows)]
fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_metadata: &std::fs::Metadata) -> bool {
    false
}

fn modified_millis(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// The version is a stable hash of paths, sizes and timestamps, so an unchanged directory keeps its value.
/// 版本是路径、大小与修改时间的稳定哈希；目录未变化时版本保持不变。
fn fingerprint(entries: &[Value]) -> String {
    let identity = json!(
        entries
            .iter()
            .map(|entry| {
                json!([
                    entry["path"],
                    entry["kind"],
                    entry["sizeBytes"],
                    entry["modifiedAt"]
                ])
            })
            .collect::<Vec<_>>()
    );
    super::local_files::sha256(identity.to_string().as_bytes())
}

/// The first observation only records a baseline; later additions and edits carry a visible status.
/// 第一次观察只记录基线；之后的增删改才带可见状态。
fn mark_status(entries: &mut [Value], previous: &BTreeMap<String, Value>) {
    for entry in entries.iter_mut() {
        if entry["kind"] != "file" {
            continue;
        }
        let path = entry["path"].as_str().unwrap_or("").to_owned();
        let status = match previous.get(&path) {
            None if previous.is_empty() => "unchanged",
            None => "added",
            Some(previous) => {
                if previous["sizeBytes"] != entry["sizeBytes"]
                    || previous["modifiedAt"] != entry["modifiedAt"]
                {
                    "modified"
                } else {
                    "unchanged"
                }
            }
        };
        entry["status"] = json!(status);
    }
}

fn snapshot_path(root: &Path, conversation: &str) -> PathBuf {
    root.join("Agent")
        .join("workspace-index")
        .join(format!("{}.json", conversation.to_ascii_lowercase()))
}

fn read_snapshot(root: &Path, conversation: &str) -> Option<BTreeMap<String, Value>> {
    let bytes = std::fs::read(snapshot_path(root, conversation)).ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    let entries = value["entries"].as_object()?;
    let mut result = BTreeMap::new();
    for (path, item) in entries {
        result.insert(
            path.clone(),
            json!({"sizeBytes":item["sizeBytes"],"modifiedAt":item["modifiedAt"]}),
        );
    }
    Some(result)
}

fn write_snapshot(
    root: &Path,
    conversation: &str,
    entries: &[Value],
) -> Result<(), crate::storage::ContractError> {
    let files = entries
        .iter()
        .filter(|entry| entry["kind"] == "file")
        .map(|entry| {
            (
                entry["path"].as_str().unwrap_or("").to_owned(),
                json!({"sizeBytes":entry["sizeBytes"],"modifiedAt":entry["modifiedAt"]}),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let payload = json!({
        "version":SNAPSHOT_VERSION,
        "conversationId":conversation,
        "entries":files,
    });
    crate::storage::managed_files::atomic_write(
        root,
        &snapshot_path(root, conversation),
        payload.to_string().as_bytes(),
        MAXIMUM_SNAPSHOT_BYTES,
    )
}

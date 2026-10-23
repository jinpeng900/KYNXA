//! Transactional task tree and recovery references owned by the Rust gateway.
//! Rust 网关拥有事务化任务树；执行者更换不改变任务身份，派发未知不能被续接当作未执行。

#[path = "task_repository_cleanup.rs"]
mod cleanup;
#[path = "task_repository_contracts.rs"]
mod contracts;
#[path = "task_repository_experiences.rs"]
mod verified_experiences;
#[path = "task_repository_schema.rs"]
mod schema;
#[path = "task_event_codec.rs"]
mod event_codec;
#[path = "task_stream_projection.rs"]
mod stream_projection;
pub use cleanup::{TaskCleanupFile, TaskCleanupPlan};
pub use contracts::*;

use super::{ContractError, managed_files, sqlite_connections};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Duration;

type Result<T> = std::result::Result<T, ContractError>;
const MAX_RECORD_BYTES: usize = 65_536;
const MAX_TREE_DEPTH: usize = 64;

fn failure() -> ContractError {
    ContractError::new(
        "Task storage failed; original records are preserved. / 任务存储失败，原记录已保留。",
        "TASK_STORAGE_FAILED",
        500,
    )
}
fn invalid(message: &str) -> ContractError {
    ContractError::new(message, "INVALID_TASK", 400)
}
fn conflict(message: &str) -> ContractError {
    ContractError::new(message, "TASK_CONFLICT", 409)
}
fn blocked(message: &str) -> ContractError {
    ContractError::new(message, "TASK_BLOCKED", 409)
}
fn stale() -> ContractError {
    ContractError::new(
        "Execution ownership changed. / 执行权已改变，旧执行者不能写入。",
        "STALE_TASK_ATTEMPT",
        409,
    )
}
fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
fn identifier(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 160 || value.chars().any(char::is_control) {
        return Err(invalid("Task identity is invalid. / 任务身份无效。"));
    }
    Ok(())
}
fn sha256(value: &str) -> Result<()> {
    if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid("SHA-256 identity is invalid. / SHA-256 标识无效。"));
    }
    Ok(())
}
fn raw<T: Serialize>(value: &T) -> Result<String> {
    let raw = serde_json::to_string(value).map_err(|_| failure())?;
    if raw.len() > MAX_RECORD_BYTES {
        return Err(invalid(
            "Use an original-content reference for large task records. / 大任务正文须使用原文引用。",
        ));
    }
    Ok(raw)
}
fn decode<T: DeserializeOwned>(raw: String) -> Result<T> {
    let original = event_codec::expand(&raw, MAX_RECORD_BYTES).ok_or_else(failure)?;
    serde_json::from_str(&original).map_err(|_| failure())
}
fn canonical(value: Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut entries: Vec<_> = object.into_iter().collect();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key, canonical(value)))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.into_iter().map(canonical).collect()),
        value => value,
    }
}
fn fingerprint<T: Serialize>(value: &T) -> Result<String> {
    let value = serde_json::to_value(value).map_err(|_| failure())?;
    Ok(format!(
        "{:x}",
        Sha256::digest(raw(&canonical(value))?.as_bytes())
    ))
}
fn state_text<T: Serialize>(state: &T) -> String {
    serde_json::to_value(state)
        .expect("serializable task enum")
        .as_str()
        .expect("string task enum")
        .to_owned()
}
fn row_task(connection: &Connection, task_id: &str) -> Result<TaskRecord> {
    let document: Option<String> = connection
        .query_row(
            "SELECT document FROM tasks WHERE task_id=?",
            [task_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| failure())?;
    decode(document.ok_or_else(|| {
        ContractError::new("Task was not found. / 任务不存在。", "TASK_NOT_FOUND", 404)
    })?)
}
fn steering_roots(connection: &Connection, conversation_id: &str, message_id: &str) -> Result<Vec<String>> {
    let mut query = connection.prepare_cached(
        "SELECT t.task_id FROM tasks t JOIN task_events e ON e.task_id=t.task_id WHERE t.conversation_id=? AND t.parent_task_id IS NULL AND e.event_id=? ORDER BY t.task_id",
    ).map_err(|_| failure())?;
    let rows = query.query_map(params![conversation_id, format!("steer:{message_id}")], |row| row.get::<_, String>(0))
        .map_err(|_| failure())?;
    rows.collect::<std::result::Result<Vec<_>, _>>().map_err(|_| failure())
}
fn save_task(connection: &Connection, task: &mut TaskRecord) -> Result<()> {
    task.updated_at_ms = now_ms();
    connection.execute("UPDATE tasks SET state=?,version=?,execution_version=?,current_attempt_id=?,event_sequence=?,document=? WHERE task_id=?", params![state_text(&task.state),task.version,task.execution_version,task.current_attempt_id,task.event_sequence,raw(task)?,task.task_id]).map_err(|_| failure())?;
    Ok(())
}
fn ancestors(connection: &Connection, task: &TaskRecord) -> Result<Vec<TaskRecord>> {
    let mut records = vec![task.clone()];
    while let Some(parent) = records
        .last()
        .and_then(|task| task.parent_task_id.as_deref())
    {
        if records.len() >= MAX_TREE_DEPTH || records.iter().any(|task| task.task_id == parent) {
            return Err(failure());
        }
        records.push(row_task(connection, parent)?);
    }
    Ok(records)
}
fn pending_reservations(connection: &Connection, task_id: &str) -> Result<u64> {
    connection.query_row("WITH RECURSIVE tree(id) AS (SELECT ? UNION ALL SELECT t.task_id FROM tasks t JOIN tree p ON t.parent_task_id=p.id) SELECT COUNT(*) FROM task_model_reservations r JOIN tree t ON r.task_id=t.id WHERE r.state IN ('reserved','unknown')",[task_id],|row|row.get(0)).map_err(|_|failure())
}
fn blocking_model_reservations(connection: &Connection, task_id: &str) -> Result<u64> {
    connection.query_row("WITH RECURSIVE tree(id) AS (SELECT ?1 UNION ALL SELECT t.task_id FROM tasks t JOIN tree p ON t.parent_task_id=p.id) SELECT COUNT(*) FROM task_model_reservations r JOIN tree t ON r.task_id=t.id WHERE r.state='reserved' OR (r.state='unknown' AND COALESCE(json_extract(r.document,'$.verificationReference.kind'),'')<>?2)",params![task_id,UNKNOWN_MODEL_COST_ACKNOWLEDGED_KIND],|row|row.get(0)).map_err(|_|failure())
}
fn tree_has_unknown_mutation(connection: &Connection, task: &TaskRecord) -> Result<bool> {
    let lineage = ancestors(connection, task)?;
    let root = lineage.last().ok_or_else(failure)?;
    connection.query_row("WITH RECURSIVE tree(id) AS (SELECT ? UNION ALL SELECT t.task_id FROM tasks t JOIN tree p ON t.parent_task_id=p.id) SELECT EXISTS(SELECT 1 FROM task_operations o JOIN tree t ON o.task_id=t.id WHERE o.state='unknown' AND o.effect<>'observation' AND COALESCE(json_extract(o.document,'$.verificationReference.kind'),'')<>?2)",params![&root.task_id,UNKNOWN_ACKNOWLEDGED_KIND],|row|row.get(0)).map_err(|_|failure())
}
fn same_authorization(left: &TaskScope, right: &TaskScope) -> Result<bool> {
    Ok(left.project_id == right.project_id
        && fingerprint(&left.permission_snapshot)? == fingerprint(&right.permission_snapshot)?)
}
fn verify_fence(
    connection: &Connection,
    token: &AttemptToken,
    allow_controlled_receipt: bool,
) -> Result<TaskRecord> {
    let task = row_task(connection, &token.task_id)?;
    if task.current_attempt_id.as_deref() != Some(&token.attempt_id)
        || task.execution_version != token.execution_version
    {
        return Err(stale());
    }
    let lease: Option<(i64,i64)> = connection.query_row("SELECT ended,lease_expires_at_ms FROM task_attempts WHERE attempt_id=? AND task_id=? AND execution_version=?", params![token.attempt_id,token.task_id,token.execution_version], |row| Ok((row.get(0)?,row.get(1)?))).optional().map_err(|_| failure())?;
    if lease.is_none_or(|(ended, expires)| ended != 0 || expires <= now_ms()) {
        return Err(stale());
    }
    // Cancellation blocks new work while keeping an already dispatched receipt observable.
    // 取消阻止后续工作，但仍允许当前尝试记录已经派发操作的真实回执。
    if !allow_controlled_receipt && (task.state.is_terminal() || task.state == TaskState::Paused) {
        return Err(blocked("Task is paused or ended. / 任务已暂停或结束。"));
    }
    Ok(task)
}

/// Quiescence is tied to the current lease and latest state transition, not an old confirmation flag.
/// 静止状态绑定当前租约和最近状态转换；旧标记不能为新执行者或后续运行放行确认。
fn verification_is_quiescent(connection: &Connection, task: &TaskRecord) -> Result<bool> {
    if task.state != TaskState::WaitingVerification { return Ok(false); }
    let event: Option<String> = connection.query_row("SELECT document FROM task_events WHERE task_id=? AND json_extract(document,'$.kind') IN ('task.state','task.waiting','task.control','task.attempt.started','task.plan-superseded','task.child.review-return','task.verification.quiescent') ORDER BY sequence DESC LIMIT 1", [&task.task_id], |row|row.get(0)).optional().map_err(|_|failure())?;
    let Some(event) = event else { return Ok(false); };
    let event: TaskEvent = decode(event)?;
    Ok(event.kind == "task.verification.quiescent"
        && event.payload["attemptId"].as_str() == task.current_attempt_id.as_deref()
        && event.payload["executionVersion"].as_u64() == Some(task.execution_version))
}

fn verify_acknowledgement_executor(connection: &Connection, task: &TaskRecord) -> Result<()> {
    if let Some(id) = task.current_attempt_id.as_deref() {
        let active: bool = connection.query_row("SELECT ended=0 AND lease_expires_at_ms>? FROM task_attempts WHERE attempt_id=?", params![now_ms(),id], |row|row.get(0)).map_err(|_|failure())?;
        if active && !verification_is_quiescent(connection, task)? {
            return Err(blocked("A live executor has not reached verification quiescence. / 原执行者尚未进入核验静止状态。"));
        }
    }
    Ok(())
}

fn verify_new_work_fence(connection: &Connection, token: &AttemptToken) -> Result<TaskRecord> {
    let task = verify_fence(connection, token, false)?;
    if ancestors(connection, &task)?.iter().any(|member|member.state==TaskState::WaitingVerification) {
        return Err(blocked("Verification suspends new dispatch while original receipts remain writable. / 核验期间暂停新派发，仍可记录原始回执。"));
    }
    Ok(task)
}
fn append(
    connection: &Connection,
    task: &mut TaskRecord,
    event_id: &str,
    kind: &str,
    payload: &Value,
) -> Result<TaskEvent> {
    identifier(event_id)?;
    identifier(kind)?;
    let digest = fingerprint(&json!([kind, payload]))?;
    let existing: Option<(String, String)> = connection
        .query_row(
            "SELECT fingerprint,document FROM task_events WHERE task_id=? AND event_id=?",
            params![task.task_id, event_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|_| failure())?;
    if let Some((old, document)) = existing {
        if old != digest {
            return Err(conflict(
                "Event identity was reused for different content. / 事件身份不能复用于不同内容。",
            ));
        }
        return decode(document);
    }
    task.event_sequence += 1;
    let event = TaskEvent {
        task_id: task.task_id.clone(),
        sequence: task.event_sequence,
        event_id: event_id.to_owned(),
        kind: kind.to_owned(),
        payload: payload.clone(),
        created_at_ms: now_ms(),
    };
    let document = event_codec::store(&event, raw(&event)?);
    connection.execute("INSERT INTO task_events(task_id,sequence,event_id,fingerprint,document) VALUES(?,?,?,?,?)",params![task.task_id,event.sequence,event_id,digest,document]).map_err(|_| failure())?;
    save_task(connection, task)?;
    Ok(event)
}

/// Use GatewayConfig.data_root; do not hardcode paths or duplicate tool/body stores.
/// 使用 GatewayConfig.data_root，不写死路径或另建工具正文存储。
#[derive(Clone)]
pub struct TaskRepository {
    root: PathBuf,
    path: PathBuf,
}

impl TaskRepository {
    /// Validate ledgers without creating schemas or rewriting rows; preserve corrupt or future-version files.
    /// 核验账本不建表或改行，损坏及未来版本文件保留原样并报错。
    pub fn validate_existing(root: &Path) -> Result<()> {
        let path = root.join("Agent/tasks.sqlite");
        if !managed_files::inspect(root, &path, false, true, u64::MAX)? {
            return Ok(());
        }
        let wal = PathBuf::from(format!("{}-wal", path.to_string_lossy()));
        if managed_files::inspect(root, &wal, false, true, u64::MAX)?
            && std::fs::metadata(&wal).map_err(|_| failure())?.len() != 0
        {
            return Err(blocked(
                "Task WAL is not settled. / 任务 WAL 尚未结算，不能迁移。",
            ));
        }
        let connection =
            Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(|_| failure())?;
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|_| failure())?;
        if !(1..=2).contains(&version) {
            return Err(ContractError::new(
                "Task database version is unsupported. / 任务库版本不受支持，原库已保留。",
                "UNSUPPORTED_TASK_STORE_VERSION",
                409,
            ));
        }
        let integrity: String = connection
            .query_row("PRAGMA quick_check", [], |row| row.get(0))
            .map_err(|_| failure())?;
        let tables: i64 = connection.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN ('tasks','task_events','task_attempts','task_operations','task_checkpoints','task_artifacts')", [], |row| row.get(0)).map_err(|_| failure())?;
        if integrity != "ok" || tables != 6 {
            return Err(failure());
        }
        Ok(())
    }
    pub fn open(root: &Path) -> Result<Self> {
        let directory = root.join("Agent");
        let path = directory.join("tasks.sqlite");
        managed_files::inspect(root, &directory, true, false, u64::MAX)?;
        managed_files::inspect(root, &path, false, true, u64::MAX)?;
        for suffix in ["-wal", "-shm", "-journal"] {
            managed_files::inspect(
                root,
                Path::new(&format!("{}{suffix}", path.to_string_lossy())),
                false,
                true,
                u64::MAX,
            )?;
        }
        let connection = Connection::open(&path).map_err(|_| failure())?;
        connection
            .busy_timeout(Duration::from_secs(3))
            .map_err(|_| failure())?;
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|_| failure())?;
        if version > 2 {
            return Err(ContractError::new(
                "Task database uses a newer version. / 任务库来自更新版本，原库已保留。",
                "UNSUPPORTED_TASK_STORE_VERSION",
                409,
            ));
        }
        if version == 1 {
            super::sqlite_store::migrate_integrity(&connection, 2, &[
                ("tasks", ""), ("task_dependencies", ""),
                ("task_attempts", "UNIQUE(task_id,attempt_id)"),
                ("task_events", ""),
                ("task_operations", "FOREIGN KEY(task_id,attempt_id) REFERENCES task_attempts(task_id,attempt_id)"),
                ("task_checkpoints", ""), ("task_artifacts", ""), ("task_usage", ""),
                ("task_model_reservations", "FOREIGN KEY(task_id,attempt_id) REFERENCES task_attempts(task_id,attempt_id)"),
                ("task_cleanup_plans", ""),
            ], &[
                "SELECT EXISTS(SELECT 1 FROM task_operations o LEFT JOIN task_attempts a ON a.attempt_id=o.attempt_id AND a.task_id=o.task_id WHERE a.attempt_id IS NULL)",
                "SELECT EXISTS(SELECT 1 FROM task_model_reservations r LEFT JOIN task_attempts a ON a.attempt_id=r.attempt_id AND a.task_id=r.task_id WHERE a.attempt_id IS NULL)",
                "SELECT EXISTS(SELECT 1 FROM tasks WHERE CASE WHEN json_valid(document) THEN json_extract(document,'$.taskId') IS NOT task_id OR json_extract(document,'$.state') IS NOT state OR json_extract(document,'$.version') IS NOT version OR json_extract(document,'$.executionVersion') IS NOT execution_version OR json_extract(document,'$.currentAttemptId') IS NOT current_attempt_id ELSE 1 END)",
                "SELECT EXISTS(SELECT 1 FROM task_attempts WHERE CASE WHEN json_valid(document) THEN json_extract(document,'$.token.taskId') IS NOT task_id OR json_extract(document,'$.token.attemptId') IS NOT attempt_id OR json_extract(document,'$.token.executionVersion') IS NOT execution_version ELSE 1 END)",
                "SELECT EXISTS(SELECT 1 FROM task_operations WHERE CASE WHEN json_valid(document) THEN json_extract(document,'$.taskId') IS NOT task_id OR json_extract(document,'$.attemptId') IS NOT attempt_id OR json_extract(document,'$.state') IS NOT state OR json_extract(document,'$.intent.effect') IS NOT effect ELSE 1 END)",
                "SELECT EXISTS(SELECT 1 FROM task_model_reservations WHERE CASE WHEN json_valid(document) THEN json_extract(document,'$.taskId') IS NOT task_id OR json_extract(document,'$.attemptId') IS NOT attempt_id OR json_extract(document,'$.reservationId') IS NOT reservation_id OR json_extract(document,'$.state') IS NOT state ELSE 1 END)",
            ])?;
        }
        connection
            .execute_batch(schema::SCHEMA)
            .map_err(|_| failure())?;
        drop(connection);
        Ok(Self {
            root: root.to_owned(),
            path,
        })
    }
    fn write<T>(&self, action: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let connection = sqlite_connections::acquire_write(&self.root, &self.path, 30_000)?;
        connection
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(|_| failure())?;
        match action(&connection) {
            Ok(value) => {
                connection.execute_batch("COMMIT").map_err(|_| failure())?;
                Ok(value)
            }
            Err(error) => {
                let _ = connection.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }
    pub fn load(&self, task_id: &str) -> Result<TaskRecord> {
        identifier(task_id)?;
        let connection = sqlite_connections::acquire(&self.root, &self.path, 30_000)?;
        row_task(&connection, task_id)
    }

    /// Let the owner name and list its existing task without changing execution or usage.
    /// 执行模型可为已有任务命名和展示，不改执行事实或用量。
    pub fn describe_task(
        &self,
        token: &AttemptToken,
        title: &str,
        display_in_task_list: bool,
    ) -> Result<TaskRecord> {
        let title = title.trim();
        if title.is_empty() || title.chars().count() > 160 || title.chars().any(char::is_control) {
            return Err(invalid(
                "Task title must be a short nonempty label. / 任务名称须简短且非空。",
            ));
        }
        self.write(|connection| {
            let mut task = verify_fence(connection, token, false)?;
            if task.title.as_deref() == Some(title)
                && task.display_in_task_list == Some(display_in_task_list)
            {
                return Ok(task);
            }
            task.title = Some(title.to_owned());
            task.display_in_task_list = Some(display_in_task_list);
            task.version += 1;
            let payload = json!({"title":title,"displayInTaskList":display_in_task_list});
            append(
                connection,
                &mut task,
                &uuid::Uuid::new_v4().to_string(),
                "task.presentation",
                &payload,
            )?;
            Ok(task)
        })
    }
    pub fn check_attempt(&self, token: &AttemptToken) -> Result<TaskRecord> {
        let connection = sqlite_connections::acquire(&self.root, &self.path, 30_000)?;
        let task = verify_fence(&connection, token, false)?;
        if ancestors(&connection, &task)?
            .iter()
            .skip(1)
            .any(|parent| parent.state.is_terminal() || parent.state == TaskState::Paused)
        {
            return Err(blocked(
                "Parent task is paused or ended. / 父任务已暂停或结束。",
            ));
        }
        Ok(task)
    }
    pub fn check_receipt_attempt(&self, token: &AttemptToken) -> Result<TaskRecord> {
        let connection = sqlite_connections::acquire(&self.root, &self.path, 30_000)?;
        verify_fence(&connection, token, true)
    }
    pub fn list_unfinished(&self, limit: usize) -> Result<Vec<TaskRecord>> {
        self.list_unfinished_after(None, limit)
    }
    pub fn list_unfinished_after(
        &self,
        after_task_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<TaskRecord>> {
        let connection = sqlite_connections::acquire(&self.root, &self.path, 30_000)?;
        let mut query=connection.prepare_cached("SELECT document FROM tasks WHERE state IN ('queued','running','waiting-network','waiting-config','waiting-approval','waiting-verification','waiting-children','paused') AND task_id>? ORDER BY task_id LIMIT ?").map_err(|_|failure())?;
        let rows = query
            .query_map(
                params![after_task_id.unwrap_or(""), limit.clamp(1, 512) as i64],
                |row| row.get::<_, String>(0),
            )
            .map_err(|_| failure())?;
        rows.map(|row| decode(row.map_err(|_| failure())?))
            .collect()
    }
    pub fn list_for_scope(
        &self,
        conversation_id: Option<&str>,
        project_id: Option<&str>,
        after_task_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<TaskRecord>> {
        if conversation_id.is_none() && project_id.is_none() {
            return Err(invalid(
                "List tasks in an explicit chat or work scope. / 查询任务须指定聊天或工作范围。",
            ));
        }
        let connection = sqlite_connections::acquire(&self.root, &self.path, 30_000)?;
        let mut query=connection.prepare_cached("SELECT document FROM tasks WHERE (?1 IS NULL OR conversation_id=?1) AND (?2 IS NULL OR project_id=?2) AND task_id>?3 ORDER BY task_id LIMIT ?4").map_err(|_|failure())?;
        let rows = query
            .query_map(
                params![
                    conversation_id,
                    project_id,
                    after_task_id.unwrap_or(""),
                    limit.clamp(1, 512) as i64
                ],
                |row| row.get::<_, String>(0),
            )
            .map_err(|_| failure())?;
        rows.map(|row| decode(row.map_err(|_| failure())?))
            .collect()
    }
    pub fn list_roots_for_scope(
        &self,
        conversation_id: Option<&str>,
        project_id: Option<&str>,
        after_task_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<TaskRecord>> {
        if conversation_id.is_none() && project_id.is_none() {
            return Err(invalid(
                "List tasks in an explicit chat or work scope. / 查询任务须指定聊天或工作范围。",
            ));
        }
        let connection = sqlite_connections::acquire(&self.root, &self.path, 30_000)?;
        let mut query=connection.prepare_cached("SELECT document FROM tasks WHERE parent_task_id IS NULL AND (?1 IS NULL OR conversation_id=?1) AND (?2 IS NULL OR project_id=?2) AND task_id>?3 ORDER BY task_id LIMIT ?4").map_err(|_|failure())?;
        let rows = query
            .query_map(
                params![
                    conversation_id,
                    project_id,
                    after_task_id.unwrap_or(""),
                    limit.clamp(1, 512) as i64
                ],
                |row| row.get::<_, String>(0),
            )
            .map_err(|_| failure())?;
        rows.map(|row| decode(row.map_err(|_| failure())?))
            .collect()
    }
    /// Read at most two paused roots in the exact chat/work scope to detect ambiguity.
    /// 在当前聊天和工作范围内最多查两个暂停根任务，足以识别歧义。
    pub fn paused_roots_for_conversation(
        &self,
        conversation_id: &str,
        project_id: Option<&str>,
    ) -> Result<Vec<TaskRecord>> {
        identifier(conversation_id)?;
        if let Some(project) = project_id {
            identifier(project)?;
        }
        let connection = sqlite_connections::acquire(&self.root, &self.path, 30_000)?;
        let mut query = connection
            .prepare_cached("SELECT document FROM tasks WHERE conversation_id=?1 AND project_id IS ?2 AND parent_task_id IS NULL AND state='paused' ORDER BY task_id LIMIT 2")
            .map_err(|_| failure())?;
        let rows = query
            .query_map(params![conversation_id, project_id], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|_| failure())?;
        rows.map(|row| decode(row.map_err(|_| failure())?))
            .collect()
    }

    /// Read earlier roots newest-first, pinned to task creation time, to resolve continuation goals.
    /// 按新到旧读取先前根任务，固定于本任务创建时刻，用于恢复续作目标。
    pub fn previous_roots_for_conversation(
        &self,
        conversation_id: &str,
        before_created_ms: i64,
        limit: usize,
    ) -> Result<Vec<TaskRecord>> {
        identifier(conversation_id)?;
        let connection = sqlite_connections::acquire(&self.root, &self.path, 30_000)?;
        let mut query = connection
            .prepare_cached("SELECT document FROM tasks WHERE conversation_id=?1 AND parent_task_id IS NULL AND json_extract(document,'$.createdAtMs')<?2 ORDER BY json_extract(document,'$.createdAtMs') DESC LIMIT ?3")
            .map_err(|_| failure())?;
        let rows = query
            .query_map(params![conversation_id, before_created_ms, limit.min(16) as i64], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|_| failure())?;
        rows.map(|row| decode(row.map_err(|_| failure())?))
            .collect()
    }

    /// Recovery lists the exact chat's unfinished roots; display preferences do not hide recoverable work.
    /// 恢复列表限定当前聊天的未结束根任务，展示偏好不能隐藏可恢复工作。
    pub fn recoverable_roots_for_conversation(
        &self,
        conversation_id: &str,
        project_id: Option<&str>,
        after_task_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<TaskRecord>> {
        identifier(conversation_id)?;
        let connection = sqlite_connections::acquire(&self.root, &self.path, 30_000)?;
        let mut query = connection.prepare_cached("SELECT document FROM tasks WHERE conversation_id=?1 AND project_id IS ?2 AND parent_task_id IS NULL AND state IN ('partial','paused','waiting-network','waiting-config','waiting-approval','waiting-verification','waiting-children') AND task_id>?3 ORDER BY task_id LIMIT ?4").map_err(|_| failure())?;
        let rows = query.query_map(params![conversation_id, project_id, after_task_id.unwrap_or(""), limit.clamp(1, 256) as i64], |row| row.get::<_, String>(0)).map_err(|_| failure())?;
        rows.map(|row| decode(row.map_err(|_| failure())?)).collect()
    }
    pub fn find_by_request_id(
        &self,
        conversation_id: &str,
        request_id: &str,
    ) -> Result<Option<TaskRecord>> {
        identifier(conversation_id)?;
        identifier(request_id)?;
        let connection = sqlite_connections::acquire(&self.root, &self.path, 30_000)?;
        let document:Option<String>=connection.query_row("SELECT document FROM tasks WHERE conversation_id=? AND json_extract(document,'$.requestId')=? ORDER BY rowid DESC LIMIT 1",params![conversation_id,request_id],|row|row.get(0)).optional().map_err(|_|failure())?;
        document.map(decode).transpose()
    }
    pub fn current_attempt(&self, task_id: &str) -> Result<Option<TaskAttempt>> {
        let connection = sqlite_connections::acquire(&self.root, &self.path, 30_000)?;
        let task = row_task(&connection, task_id)?;
        let Some(id) = task.current_attempt_id else {
            return Ok(None);
        };
        let document: Option<String> = connection
            .query_row(
                "SELECT document FROM task_attempts WHERE attempt_id=?",
                [id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| failure())?;
        document.map(decode).transpose()
    }
    pub fn mark_unstarted_wait(
        &self,
        task_id: &str,
        expected_version: u64,
        state: TaskState,
        reason: &str,
    ) -> Result<TaskRecord> {
        identifier(reason)?;
        if !matches!(
            state,
            TaskState::WaitingNetwork
                | TaskState::WaitingConfig
                | TaskState::WaitingChildren
                | TaskState::WaitingVerification
                | TaskState::Failed
        ) {
            return Err(invalid(
                "Invalid unstarted task outcome. / 未启动任务状态无效。",
            ));
        }
        self.write(|connection| {
            let mut task=row_task(connection,task_id)?;
            if task.version!=expected_version {return Err(conflict("Task changed before waiting state. / 记录等待状态前任务已变化。"));}
            if task.state.is_terminal() || task.state==TaskState::Paused {return Err(blocked("Task is controlled or ended. / 任务已暂停、取消或结束。"));}
            if let Some(id)=task.current_attempt_id.as_deref() {
                let active:bool=connection.query_row("SELECT ended=0 AND lease_expires_at_ms>? FROM task_attempts WHERE attempt_id=?",params![now_ms(),id],|row|row.get(0)).map_err(|_|failure())?;
                if active {return Err(blocked("A live executor owns task transitions. / 有效执行者拥有任务状态提交权。"));}
            }
            task.state=state;task.version+=1;task.stop_reason=Some(reason.to_owned());
            let payload=json!({"state":task.state,"reason":task.stop_reason});
            append(connection,&mut task,&uuid::Uuid::new_v4().to_string(),"task.waiting",&payload)?;Ok(task)
        })
    }
    pub fn children(&self, task_id: &str) -> Result<Vec<TaskRecord>> {
        let connection = sqlite_connections::acquire(&self.root, &self.path, 30_000)?;
        let mut query = connection
            .prepare_cached("SELECT document FROM tasks WHERE parent_task_id=? ORDER BY task_id")
            .map_err(|_| failure())?;
        let rows = query
            .query_map([task_id], |row| row.get::<_, String>(0))
            .map_err(|_| failure())?;
        rows.map(|row| decode(row.map_err(|_| failure())?))
            .collect()
    }
    pub fn accept_task(&self, input: &NewTask) -> Result<TaskRecord> {
        identifier(&input.request_id)?;
        if input.goal.trim().is_empty()
            || input.goal.encode_utf16().count() > 16_384
            || input.acceptance.len() > 128
            || input.dependency_ids.len() > 128
            || input.budget.max_model_requests == 0
            || input.budget.max_known_tokens == Some(0)
            || input.budget.max_execution_ms == Some(0)
        {
            return Err(invalid(
                "Task goal, dependencies or budget are invalid. / 任务目标、依赖或预算无效。",
            ));
        }
        if uuid::Uuid::parse_str(&input.scope.conversation_id).is_err()
            || input
                .scope
                .project_id
                .as_deref()
                .is_some_and(|id| uuid::Uuid::parse_str(id).is_err())
            || !input.scope.permission_snapshot.is_object()
        {
            return Err(invalid(
                "Formal task scope is invalid. / 正式任务范围无效。",
            ));
        }
        raw(input)?;
        let request_key = fingerprint(&json!([
            input.scope.project_id,
            input.scope.conversation_id,
            input.request_id
        ]))?;
        let digest = fingerprint(input)?;
        self.write(|connection| {
            let cleaning:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM task_cleanup_plans WHERE conversation_id=?)",[&input.scope.conversation_id],|row|row.get(0)).map_err(|_|failure())?;
            if cleaning {return Err(ContractError::new("Conversation task cleanup is still pending. / 聊天任务清理尚未完成。","TASK_CLEANUP_PENDING",409));}
            let old:Option<(String,String)>=connection.query_row("SELECT fingerprint,document FROM tasks WHERE request_key=?",[&request_key],|row|Ok((row.get(0)?,row.get(1)?))).optional().map_err(|_|failure())?;
            if let Some((old,document))=old { if old!=digest {return Err(conflict("Request identity was reused for different work. / 请求身份不能复用于不同任务。"));} return decode(document); }
            if let Some(parent_id)=input.parent_task_id.as_deref() {
                let parent=row_task(connection,parent_id)?;
                if parent.state.is_terminal() || parent.state==TaskState::Paused || !same_authorization(&parent.scope,&input.scope)? { return Err(blocked("Parent task is inactive or outside the authorized scope. / 父任务不可运行或子任务超出授权范围。")); }
                if ancestors(connection,&parent)?.len()>=MAX_TREE_DEPTH { return Err(invalid("Task tree is too deep. / 任务树过深。")); }
            }
            let timestamp=now_ms();
            let mut task=TaskRecord { task_id:uuid::Uuid::new_v4().to_string(),request_id:input.request_id.clone(),goal:input.goal.clone(),title:None,display_in_task_list:None,acceptance:input.acceptance.clone(),scope:input.scope.clone(),parent_task_id:input.parent_task_id.clone(),dependency_ids:input.dependency_ids.clone(),budget:input.budget.clone(),usage:TaskUsage::default(),input_reference:input.input_reference.clone(),state:TaskState::Queued,version:1,execution_version:0,current_attempt_id:None,event_sequence:0,stop_reason:None,paused_by_task_id:None,created_at_ms:timestamp,updated_at_ms:timestamp };
            for dependency_id in &task.dependency_ids {
                let dependency=row_task(connection,dependency_id)?;
                if !same_authorization(&task.scope,&dependency.scope)? || ancestors(connection,&task)?.iter().any(|parent|parent.task_id==*dependency_id) { return Err(invalid("Dependency is outside scope or creates a parent cycle. / 依赖超出范围或形成父子循环。")); }
            }
            connection.execute("INSERT INTO tasks(task_id,request_key,fingerprint,parent_task_id,project_id,conversation_id,permission_hash,state,version,execution_version,current_attempt_id,event_sequence,document) VALUES(?,?,?,?,?,?,?,'queued',1,0,NULL,0,?)",params![task.task_id,request_key,digest,task.parent_task_id,task.scope.project_id,task.scope.conversation_id,fingerprint(&task.scope.permission_snapshot)?,raw(&task)?]).map_err(|_|failure())?;
            for dependency in &task.dependency_ids { connection.execute("INSERT INTO task_dependencies(task_id,dependency_id) VALUES(?,?)",params![task.task_id,dependency]).map_err(|_|failure())?; }
            let payload=json!({"parentTaskId":task.parent_task_id});
            append(connection,&mut task,"accepted","task.accepted",&payload)?;
            Ok(task)
        })
    }

    pub fn set_dependencies(
        &self,
        task_id: &str,
        expected_version: u64,
        dependencies: &[String],
    ) -> Result<TaskRecord> {
        if dependencies.len() > 128 {
            return Err(invalid("Too many dependencies. / 依赖数量过多。"));
        }
        self.write(|connection| {
            let mut task=row_task(connection,task_id)?;
            if task.version!=expected_version {return Err(conflict("Task changed before dependency update. / 修改依赖前任务已变化。"));}
            if task.current_attempt_id.is_some() || task.state!=TaskState::Queued {return Err(blocked("Dependencies cannot change after execution begins. / 开始执行后不能修改依赖。"));}
            for dependency_id in dependencies {
                let dependency=row_task(connection,dependency_id)?;
                if !same_authorization(&task.scope,&dependency.scope)? || ancestors(connection,&task)?.iter().any(|record|record.task_id==*dependency_id) {return Err(invalid("Dependency is outside scope or is an ancestor. / 依赖超出范围或指向祖先。"));}
                let reaches:bool=connection.query_row("WITH RECURSIVE links(id) AS (SELECT ? UNION SELECT d.dependency_id FROM task_dependencies d JOIN links l ON d.task_id=l.id) SELECT EXISTS(SELECT 1 FROM links WHERE id=?)",params![dependency_id,task_id],|row|row.get(0)).map_err(|_|failure())?;
                if reaches {return Err(invalid("Dependency cycle is not allowed. / 不允许依赖循环。"));}
            }
            connection.execute("DELETE FROM task_dependencies WHERE task_id=?",[task_id]).map_err(|_|failure())?;
            for dependency_id in dependencies {connection.execute("INSERT OR IGNORE INTO task_dependencies(task_id,dependency_id) VALUES(?,?)",params![task_id,dependency_id]).map_err(|_|failure())?;}
            task.dependency_ids=dependencies.to_vec();task.version+=1;save_task(connection,&mut task)?;Ok(task)
        })
    }

    /// Expired ownership blocks internal writes but cannot prove external work stopped.
    /// 执行权过期阻止内部写入，不证明外部操作已停止，接管仍保留未知。
    pub fn start_attempt(
        &self,
        task_id: &str,
        expected_version: u64,
        executor_id: &str,
        lease_ms: u64,
    ) -> Result<TaskAttempt> {
        identifier(executor_id)?;
        if !(1_000..=3_600_000).contains(&lease_ms) {
            return Err(invalid(
                "Execution lease must be between one second and one hour. / 执行租约须为一秒至一小时。",
            ));
        }
        self.write(|connection| {
            let mut task=row_task(connection,task_id)?;
            if task.version!=expected_version {return Err(conflict("Task changed before execution claim. / 接管前任务已变化。"));}
            if task.state.is_terminal() || task.state==TaskState::Paused {return Err(blocked("Task is paused or ended. / 任务已暂停或结束。"));}
            let reconciliation_needed=task.current_attempt_id.is_some()
                && (read_operations(connection,task_id)?.iter().any(|operation|matches!(operation.state,OperationState::Dispatched|OperationState::Unknown) && !operation.is_unknown_acknowledged())
                    || read_reservations(connection,task_id)?.iter().any(ModelRequestReservation::blocks_recovery));
            for record in ancestors(connection,&task)? {
                if record.state.is_terminal() || record.state==TaskState::Paused || (!reconciliation_needed && (record.usage.is_exhausted(&record.budget) || record.usage.model_requests.saturating_add(pending_reservations(connection,&record.task_id)?)>=record.budget.max_model_requests)) {return Err(blocked("Task tree is controlled or its budget is exhausted. / 任务树已暂停、取消或总预算耗尽。"));}
            }
            for dependency_id in &task.dependency_ids {if row_task(connection,dependency_id)?.state!=TaskState::Completed {return Err(blocked("Required dependency is not verified complete. / 必需依赖尚未核验完成。"));}}
            let timestamp=now_ms();
            if let Some(attempt_id)=task.current_attempt_id.as_deref() {
                let document:String=connection.query_row("SELECT document FROM task_attempts WHERE attempt_id=?",[attempt_id],|row|row.get(0)).map_err(|_|failure())?;
                let mut previous:TaskAttempt=decode(document)?;
                if previous.ended_at_ms.is_none() && previous.lease_expires_at_ms>timestamp {return Err(blocked("Task already has a live executor. / 任务已有有效执行者。"));}
                if previous.ended_at_ms.is_none() {
                    previous.ended_at_ms=Some(timestamp);previous.stop_reason=Some("ownership-expired".into());
                    connection.execute("UPDATE task_attempts SET ended=1,document=? WHERE attempt_id=?",params![raw(&previous)?,attempt_id]).map_err(|_|failure())?;
                    let unsettled=read_operations(connection,task_id)?;
                    for mut operation in unsettled.into_iter().filter(|operation|operation.state==OperationState::Dispatched) {
                        operation.state=OperationState::Unknown;operation.updated_at_ms=timestamp;save_operation(connection,&operation)?;
                    }
                    preserve_unsettled_reservations(connection,task_id,attempt_id)?;
                }
            }
            task.execution_version+=1;task.version+=1;
            let token=AttemptToken {task_id:task_id.to_owned(),attempt_id:uuid::Uuid::new_v4().to_string(),execution_version:task.execution_version};
            let attempt=TaskAttempt {token:token.clone(),executor_id:executor_id.to_owned(),lease_expires_at_ms:timestamp.saturating_add(lease_ms as i64),started_at_ms:timestamp,ended_at_ms:None,stop_reason:None,active_duration_ms:0};
            task.current_attempt_id=Some(token.attempt_id.clone());
            task.state=if read_operations(connection,task_id)?.iter().any(|operation|operation.state==OperationState::Unknown && operation.blocks_recovery())
                || read_reservations(connection,task_id)?.iter().any(|reservation| reservation.state==ModelReservationState::Unknown && reservation.blocks_recovery()) {TaskState::WaitingVerification} else {TaskState::Running};
            task.stop_reason=None;
            connection.execute("INSERT INTO task_attempts(attempt_id,task_id,execution_version,ended,lease_expires_at_ms,document) VALUES(?,?,?,0,?,?)",params![token.attempt_id,task_id,token.execution_version,attempt.lease_expires_at_ms,raw(&attempt)?]).map_err(|_|failure())?;
            let payload=json!({"attempt":attempt,"state":task.state});
            append(connection,&mut task,&format!("attempt:{}",token.attempt_id),"task.attempt.started",&payload)?;
            Ok(attempt)
        })
    }
    pub fn renew_attempt(&self, token: &AttemptToken, lease_ms: u64) -> Result<TaskAttempt> {
        if !(1_000..=3_600_000).contains(&lease_ms) {
            return Err(invalid("Invalid execution lease. / 执行租约无效。"));
        }
        self.write(|connection| {
            verify_fence(connection, token, false)?;
            let document: String = connection
                .query_row(
                    "SELECT document FROM task_attempts WHERE attempt_id=?",
                    [&token.attempt_id],
                    |row| row.get(0),
                )
                .map_err(|_| failure())?;
            let mut attempt: TaskAttempt = decode(document)?;
            if attempt.lease_expires_at_ms < now_ms() {
                return Err(stale());
            }
            attempt.lease_expires_at_ms = now_ms().saturating_add(lease_ms as i64);
            connection
                .execute(
                    "UPDATE task_attempts SET lease_expires_at_ms=?,document=? WHERE attempt_id=?",
                    params![
                        attempt.lease_expires_at_ms,
                        raw(&attempt)?,
                        token.attempt_id
                    ],
                )
                .map_err(|_| failure())?;
            Ok(attempt)
        })
    }
    pub fn release_attempt(&self, token: &AttemptToken, reason: &str) -> Result<()> {
        identifier(reason)?;
        self.write(|connection| {
            verify_fence(connection, token, true)?;
            let document: String = connection
                .query_row(
                    "SELECT document FROM task_attempts WHERE attempt_id=?",
                    [&token.attempt_id],
                    |row| row.get(0),
                )
                .map_err(|_| failure())?;
            let mut attempt: TaskAttempt = decode(document)?;
            attempt.ended_at_ms = Some(now_ms());
            attempt.stop_reason = Some(reason.to_owned());
            for mut operation in read_operations(connection, &token.task_id)?
                .into_iter()
                .filter(|operation| operation.state == OperationState::Dispatched)
            {
                operation.state = OperationState::Unknown;
                operation.updated_at_ms = now_ms();
                save_operation(connection, &operation)?;
            }
            preserve_unsettled_reservations(connection, &token.task_id, &token.attempt_id)?;
            connection
                .execute(
                    "UPDATE task_attempts SET ended=1,document=? WHERE attempt_id=?",
                    params![raw(&attempt)?, token.attempt_id],
                )
                .map_err(|_| failure())?;
            Ok(())
        })
    }

    pub fn transition(
        &self,
        token: &AttemptToken,
        expected_version: u64,
        state: TaskState,
        reason: Option<&str>,
    ) -> Result<TaskRecord> {
        if matches!(
            state,
            TaskState::Cancelled | TaskState::Paused | TaskState::Queued
        ) {
            return Err(invalid(
                "Use the control API for pause, resume and cancel. / 暂停、继续、取消须使用控制接口。",
            ));
        }
        self.write(|connection| {
            let mut task=verify_fence(connection,token,false)?;
            if task.version!=expected_version {return Err(conflict("Task changed before transition. / 状态提交前任务已变化。"));}
            if state==TaskState::Running && task.state==TaskState::WaitingVerification
                && (read_operations(connection,&task.task_id)?.iter().any(TaskOperation::blocks_recovery)
                    || blocking_model_reservations(connection,&task.task_id)?>0 || tree_has_unknown_mutation(connection,&task)?) {
                return Err(blocked("Verification must settle or explicitly acknowledge unknown outcomes before execution continues. / 须先结算或显式确认未知结果，才能继续执行。"));
            }
            if state==TaskState::Completed {
                // Keep interrupted observations Unknown without making unrelated reads a write barrier.
                // 中断观察保持 Unknown，不把无关读取当作写入屏障。
                // Acknowledged Unknown no longer blocks completion, but never becomes success.
                // 用户确认的 Unknown 不再阻止完成，但不改记成功。
                if read_operations(connection,&task.task_id)?.iter().any(TaskOperation::blocks_recovery) {return Err(blocked("Unknown effects prevent verified completion. / 结果未知的副作用阻止核验完成。"));}
                if blocking_model_reservations(connection,&task.task_id)?>0 {return Err(blocked("Unsettled model requests prevent verified completion. / 未结算模型请求阻止核验完成。"));}
                if tree_has_unknown_mutation(connection,&task)? { return Err(blocked("Unknown delegated effects still require verification after a plan update. / 更新方案后，子任务未知副作用仍须核验。")); }
                let incomplete:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM tasks WHERE parent_task_id=? AND state<>'completed' AND NOT(state='cancelled' AND json_extract(document,'$.stopReason')='superseded-by-user-steering'))",[&task.task_id],|row|row.get(0)).map_err(|_|failure())?;
                if incomplete {return Err(blocked("Child tasks remain incomplete; use a partial outcome. / 子任务未全部完成，应记录部分结果。"));}
            }
            task.state=state;task.version+=1;task.stop_reason=reason.map(str::to_owned);
            let payload=json!({"state":task.state,"reason":task.stop_reason});
            append(connection,&mut task,&uuid::Uuid::new_v4().to_string(),"task.state",&payload)?;Ok(task)
        })
    }

    /// Require settled I/O and dispatch records; retain leases and original Unknown outcomes.
    /// 须先结束 I/O 并结算在途记录，保留租约及原始 Unknown 结果。
    pub fn mark_verification_quiescent(&self, token: &AttemptToken, reason: &str) -> Result<TaskRecord> {
        identifier(reason)?;
        self.write(|connection| {
            let mut task = verify_fence(connection, token, false)?;
            let inflight: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM task_operations WHERE task_id=?1 AND state='dispatched') OR EXISTS(SELECT 1 FROM task_model_reservations WHERE task_id=?1 AND state='reserved')", [&task.task_id], |row|row.get(0)).map_err(|_|failure())?;
            if inflight { return Err(blocked("In-flight operations or model reservations have not settled. / 仍有进行中的操作或模型预约，不能宣告静止。")); }
            if verification_is_quiescent(connection, &task)? { return Ok(task); }
            task.state = TaskState::WaitingVerification;
            task.stop_reason = Some(reason.to_owned());
            task.version += 1;
            let state = json!({"state":task.state,"reason":reason});
            append(connection,&mut task,&uuid::Uuid::new_v4().to_string(),"task.state",&state)?;
            let payload = json!({"attemptId":token.attempt_id,"executionVersion":token.execution_version,"stateVersion":task.version,"reason":reason});
            append(connection,&mut task,&uuid::Uuid::new_v4().to_string(),"task.verification.quiescent",&payload)?;
            Ok(task)
        })
    }
    pub fn control(
        &self,
        task_id: &str,
        expected_version: u64,
        control: TaskControl,
    ) -> Result<TaskRecord> {
        let event_id = format!(
            "control:{task_id}:{expected_version}:{}",
            state_text(&control)
        );
        self.write(|connection| {
            let task=row_task(connection,task_id)?;
            let replayed:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM task_events WHERE task_id=? AND event_id=?)",params![task_id,event_id],|row|row.get(0)).map_err(|_|failure())?;
            if replayed {return Ok(task);}
            if task.version!=expected_version {return Err(conflict("Task changed before user control. / 用户控制前任务已变化。"));}
            let resume_partial = control==TaskControl::Resume && task.state==TaskState::Partial;
            // A user-requested revision reopens only this settled root; ordinary resume stays unchanged.
            // 用户明确要求修订时只重开已结算的原根任务，普通继续规则保持不变。
            let revise_root = control==TaskControl::Revise && task.parent_task_id.is_none()
                && matches!(task.state,TaskState::Completed|TaskState::Partial);
            if control==TaskControl::Revise && !revise_root { return Err(blocked("Only settled root work can be revised. / 仅已结算的根任务可按要求修订。")); }
            let resume_settled = resume_partial || revise_root;
            let continuing = control==TaskControl::Resume || revise_root;
            if resume_settled && task.current_attempt_id.as_ref().is_some_and(|id| {
                connection.query_row("SELECT ended=0 AND lease_expires_at_ms>? FROM task_attempts WHERE attempt_id=?",params![now_ms(),id],|row|row.get::<_,bool>(0)).unwrap_or(true)
            }) { return Err(blocked("The previous partial attempt is still settling. / 上次部分完成的尝试仍在结算。")); }
            if task.state.is_terminal() && !resume_settled {return if control==TaskControl::Cancel && task.state==TaskState::Cancelled {Ok(task)} else {Err(blocked("Ended tasks cannot be resumed. / 已结束的任务不能继续。"))};}
            if continuing && !revise_root && !matches!(task.state,TaskState::Partial|TaskState::Paused|TaskState::WaitingNetwork|TaskState::WaitingConfig|TaskState::WaitingApproval|TaskState::WaitingVerification|TaskState::WaitingChildren) {return Err(blocked("Task is not waiting or paused. / 任务并非等待或暂停状态。"));}
            if continuing && ancestors(connection,&task)?.iter().skip(1).any(|ancestor|ancestor.state.is_terminal() || ancestor.state==TaskState::Paused) {return Err(blocked("The parent remains paused or ended. / 父任务仍然暂停或已经结束。"));}
            let mut ids=vec![task_id.to_owned()];
            {
                let mut query=connection.prepare_cached("WITH RECURSIVE tree(id) AS (SELECT task_id FROM tasks WHERE parent_task_id=? UNION ALL SELECT t.task_id FROM tasks t JOIN tree p ON t.parent_task_id=p.id) SELECT id FROM tree").map_err(|_|failure())?;
                ids.extend(query.query_map([task_id],|row|row.get::<_,String>(0)).map_err(|_|failure())?.collect::<std::result::Result<Vec<_>,_>>().map_err(|_|failure())?);
            }
            for id in ids {
                let mut child=row_task(connection,&id)?;
                if child.state.is_terminal() && !(resume_settled && child.task_id==task_id) {continue;}
                if continuing && child.task_id!=task_id && (child.state!=TaskState::Paused || child.paused_by_task_id.as_deref()!=Some(task_id)) {continue;}
                if control==TaskControl::Pause && child.state==TaskState::Paused && child.task_id!=task_id {continue;}
                child.state=match control {TaskControl::Pause=>TaskState::Paused,TaskControl::Resume|TaskControl::Revise=>TaskState::Queued,TaskControl::Cancel=>TaskState::Cancelled};
                child.paused_by_task_id=if control==TaskControl::Pause {Some(task_id.to_owned())} else {None};
                child.version+=1;child.stop_reason=Some(match control{TaskControl::Pause=>"user-paused",TaskControl::Resume=>"user-resumed",TaskControl::Revise=>"user-requested-revision",TaskControl::Cancel=>"user-cancelled"}.into());
                let payload=json!({"state":child.state,"reason":child.stop_reason});
                append(connection,&mut child,&event_id,"task.control",&payload)?;
            }
            row_task(connection,task_id)
        })
    }

    /// Verify child settlement, version and outputs, then save parent acceptance in one transaction.
    /// 同一事务核对子任务结算、版本和产物，再保存父级验收。
    pub fn accept_child_output(&self, parent_token: &AttemptToken, child_id: &str, expected_version: u64,
        reason: &str, output_ref: &Value) -> Result<TaskRecord> {
        identifier(child_id)?;
        if reason.trim().is_empty() || reason.chars().count()>2000 || !output_ref.is_object() {
            return Err(invalid("Acceptance needs a reason and current output reference. / 接纳须提供原因与当前产物引用。"));
        }
        self.write(|connection| {
            let mut parent=verify_fence(connection,parent_token,false)?;
            let child=row_task(connection,child_id)?;
            if child.parent_task_id.as_deref()!=Some(parent.task_id.as_str()) { return Err(blocked("Only the direct parent may accept this child. / 仅直属父任务可接纳该子任务。")); }
            let payload=json!({"parentTaskId":parent.task_id,"childTaskId":child_id,"decision":"accept","reviewedVersion":expected_version,
                "reason":reason,"outputRef":output_ref,"correctnessCertified":false});
            let event_id=format!("parent-child-accept:{child_id}:{expected_version}");
            let previous:Option<String>=connection.query_row("SELECT fingerprint FROM task_events WHERE task_id=? AND event_id=?",params![parent.task_id,event_id],|row|row.get(0)).optional().map_err(|_|failure())?;
            if let Some(previous)=previous {
                return if previous==fingerprint(&json!(["task.child.review",payload]))? {Ok(child)}
                    else {Err(conflict("Parent acceptance identity was reused. / 父验收身份不能复用于不同结论。"))};
            }
            if child.version!=expected_version { return Err(conflict("Child changed before acceptance. / 父接纳前子任务已变化。")); }
            if child.state!=TaskState::Completed { return Err(blocked("Only a completed child may be accepted. / 仅可接纳已完成的子任务。")); }
            if let Some(attempt_id)=child.current_attempt_id.as_deref() {
                let active:bool=connection.query_row("SELECT ended=0 AND lease_expires_at_ms>? FROM task_attempts WHERE attempt_id=?",params![now_ms(),attempt_id],|row|row.get(0)).map_err(|_|failure())?;
                if active { return Err(blocked("Child execution is still settling. / 子执行者仍在结算。")); }
            }
            let artifact:Option<String>=connection.query_row("SELECT document FROM task_artifacts WHERE task_id=? AND json_extract(document,'$.kind')='task-output' ORDER BY rowid DESC LIMIT 1",[child_id],|row|row.get(0)).optional().map_err(|_|failure())?;
            let artifact:TaskArtifact=decode(artifact.ok_or_else(||blocked("Child output is unavailable. / 子任务产物不存在。"))?)?;
            if artifact.reference!=*output_ref { return Err(conflict("The accepted output is no longer current. / 被接纳的产物已经过期。")); }
            append(connection,&mut parent,&event_id,"task.child.review",&payload)?;
            Ok(child)
        })
    }

    /// Only the active direct parent may return current child output for revision; ordinary resume cannot.
    /// 仅活动直属父任务可退回当前子产物修订，普通恢复不具备此权限。
    pub fn request_child_revision(
        &self,
        parent_token: &AttemptToken,
        child_id: &str,
        expected_version: u64,
        reason: &str,
        output_ref: &Value,
    ) -> Result<TaskRecord> {
        identifier(child_id)?;
        if reason.trim().is_empty() || reason.chars().count()>2000 || !output_ref.is_object() {
            return Err(invalid("Revision needs a reason and the reviewed output reference. / 退回须提供原因与被核对的产物引用。"));
        }
        self.write(|connection| {
            let mut parent=verify_fence(connection,parent_token,false)?;
            let mut child=row_task(connection,child_id)?;
            if child.parent_task_id.as_deref()!=Some(parent.task_id.as_str()) {
                return Err(blocked("Only the direct parent may review this child. / 仅直属父任务可退回该子任务。"));
            }
            let payload=json!({"parentTaskId":parent.task_id,"childTaskId":child_id,"reviewedVersion":expected_version,
                "reason":reason,"outputRef":output_ref,"state":"queued"});
            let parent_review=json!({"parentTaskId":parent.task_id,"childTaskId":child_id,"decision":"return",
                "reviewedVersion":expected_version,"reason":reason,"outputRef":output_ref,"correctnessCertified":false});
            let parent_event_id=format!("parent-child-return:{child_id}:{expected_version}");
            let event_id=format!("parent-review-return:{expected_version}");
            let previous:Option<String>=connection.query_row("SELECT fingerprint FROM task_events WHERE task_id=? AND event_id=?",params![child_id,event_id],|row|row.get(0)).optional().map_err(|_|failure())?;
            if let Some(previous)=previous {
                if previous!=fingerprint(&json!(["task.child.review-return",payload]))? {
                    return Err(conflict("Child review identity was reused. / 子任务核对身份不能复用于不同结论。"));
                }
                append(connection,&mut parent,&parent_event_id,"task.child.review",&parent_review)?;
                return Ok(child);
            }
            if child.version!=expected_version { return Err(conflict("Child changed before parent review. / 父核对前子任务已变化。")); }
            if !matches!(child.state,TaskState::Completed|TaskState::Partial) { return Err(blocked("Only finished child output can be returned. / 只能退回已完成或部分完成的子任务产物。")); }
            if let Some(attempt_id)=child.current_attempt_id.as_deref() {
                let active:bool=connection.query_row("SELECT ended=0 AND lease_expires_at_ms>? FROM task_attempts WHERE attempt_id=?",params![now_ms(),attempt_id],|row|row.get(0)).map_err(|_|failure())?;
                if active { return Err(blocked("Child executor is still settling. / 子执行者仍在结算。")); }
            }
            let artifact:Option<String>=connection.query_row("SELECT document FROM task_artifacts WHERE task_id=? AND json_extract(document,'$.kind')='task-output' ORDER BY rowid DESC LIMIT 1",[child_id],|row|row.get(0)).optional().map_err(|_|failure())?;
            let artifact:TaskArtifact=decode(artifact.ok_or_else(||blocked("The reviewed child output is unavailable. / 被核对的子任务产物不存在。"))?)?;
            if artifact.reference!=*output_ref { return Err(conflict("Reviewed child output is stale. / 被核对的子任务产物已过期。")); }
            child.state=TaskState::Queued;
            child.paused_by_task_id=None;
            child.stop_reason=Some("parent-review-revision".into());
            child.version+=1;
            append(connection,&mut child,&event_id,"task.child.review-return",&payload)?;
            append(connection,&mut parent,&parent_event_id,"task.child.review",&parent_review)?;
            Ok(child)
        })
    }

    /// Record acknowledged unknown effects without marking success or replaying them; unblock recovery only.
    /// 记录用户确认的未知操作，不改记成功或重放，仅解除恢复阻塞。
    pub fn acknowledge_unknown_operations(
        &self,
        root_task_id: &str,
        expected_version: u64,
        operation_ids: &[String],
    ) -> Result<TaskRecord> {
        self.acknowledge_unknown_operation_selection(root_task_id, expected_version, operation_ids, None)
    }

    /// Qualify operation IDs by task; accept legacy bare IDs only when unambiguous.
    /// 操作 ID 带任务身份，旧裸 ID 仅在无歧义时兼容。
    pub fn acknowledge_unknown_task_operations(
        &self,
        root_task_id: &str,
        expected_version: u64,
        identities: &[TaskOperationIdentity],
    ) -> Result<TaskRecord> {
        for identity in identities { identifier(&identity.task_id)?; }
        let operation_ids = identities.iter().map(|identity|identity.operation_id.clone()).collect::<Vec<_>>();
        let task_ids = identities.iter().map(|identity|identity.task_id.clone()).collect::<Vec<_>>();
        self.acknowledge_unknown_operation_selection(root_task_id, expected_version, &operation_ids, Some(&task_ids))
    }

    fn acknowledge_unknown_operation_selection(
        &self,
        root_task_id: &str,
        expected_version: u64,
        operation_ids: &[String],
        task_ids: Option<&[String]>,
    ) -> Result<TaskRecord> {
        if operation_ids.is_empty() || operation_ids.len() > 64 {
            return Err(invalid("Choose between one and 64 unknown operations. / 须确认 1 至 64 个结果未知的操作。"));
        }
        let mut unique = std::collections::HashSet::new();
        for (index, operation_id) in operation_ids.iter().enumerate() {
            identifier(operation_id)?;
            if !unique.insert((task_ids.map(|ids|ids[index].as_str()), operation_id.as_str())) {
                return Err(invalid("An operation was listed twice. / 同一操作不能重复选择。"));
            }
        }
        self.write(|connection| {
            let mut root=row_task(connection,root_task_id)?;
            if root.parent_task_id.is_some() {return Err(invalid("Confirm unknown outcomes on the root task. / 须在根任务上确认未知结果。"));}
            let mut members=vec![root.clone()];
            {
                let mut query=connection.prepare_cached("WITH RECURSIVE tree(id) AS (SELECT task_id FROM tasks WHERE parent_task_id=? UNION ALL SELECT t.task_id FROM tasks t JOIN tree p ON t.parent_task_id=p.id) SELECT id FROM tree").map_err(|_|failure())?;
                let ids=query.query_map([root_task_id],|row|row.get::<_,String>(0)).map_err(|_|failure())?.collect::<std::result::Result<Vec<_>,_>>().map_err(|_|failure())?;
                drop(query);
                for id in ids {members.push(row_task(connection,&id)?);}
            }
            let mut targets=Vec::new();
            for (index, operation_id) in operation_ids.iter().enumerate() {
                let mut found=None;
                for member in members.iter().filter(|member|task_ids.is_none_or(|ids|ids[index]==member.task_id)) {
                    if let Some(operation)=find_operation(connection,&member.task_id,operation_id)? {
                        if found.is_some() { return Err(invalid("Operation identity is ambiguous; include its task ID. / 操作身份存在歧义，须同时提供任务ID。")); }
                        found=Some(operation);
                    }
                }
                let operation=found.ok_or_else(||invalid("Operation is not part of this task tree. / 该操作不属于此任务树。"))?;
                if operation.intent.effect==OperationEffect::Observation || operation.state!=OperationState::Unknown {
                    return Err(blocked("Only unknown non-observation outcomes can be confirmed. / 只能确认结果未知的非观察操作。"));
                }
                targets.push(operation);
            }
            // Replaying the same confirmation after success is idempotent even though the version moved.
            // 成功后重放同一确认为幂等操作，即使任务版本已经前进。
            if targets.iter().all(TaskOperation::is_unknown_acknowledged) {return Ok(root);}
            if root.version!=expected_version {return Err(conflict("Task changed before confirming unknown outcomes. / 确认未知结果前任务已变化。"));}
            if root.state!=TaskState::WaitingVerification {return Err(blocked("Task is not waiting for verification. / 任务并非等待核验状态。"));}
            for member in &members { verify_acknowledgement_executor(connection, member)?; }
            let timestamp=now_ms();
            root.version+=1;
            for mut operation in targets.into_iter().filter(|operation|!operation.is_unknown_acknowledged()) {
                operation.verification_reference=Some(json!({"kind":UNKNOWN_ACKNOWLEDGED_KIND,"outcome":"unknown",
                    "rootTaskId":root_task_id,"rootVersion":expected_version,"acknowledgedAtMs":timestamp}));
                operation.updated_at_ms=timestamp;
                save_operation(connection,&operation)?;
                let payload=json!({"operationId":operation.intent.operation_id,"toolCallId":operation.intent.tool_call_id,
                    "toolName":operation.intent.tool_name,"effect":operation.intent.effect,"state":operation.state,
                    "verificationReference":operation.verification_reference,"rootTaskId":root_task_id});
                let event_id=format!("operation:{}:unknown-acknowledged",operation.intent.operation_id);
                if operation.task_id==root.task_id {
                    append(connection,&mut root,&event_id,"task.operation.unknown-acknowledged",&payload)?;
                } else {
                    let mut member=row_task(connection,&operation.task_id)?;
                    append(connection,&mut member,&event_id,"task.operation.unknown-acknowledged",&payload)?;
                }
            }
            let payload=json!({"operationIds":operation_ids,"state":root.state});
            append(connection,&mut root,&format!("acknowledge-unknown:{root_task_id}:{expected_version}"),"task.unknown-acknowledged",&payload)?;
            Ok(root)
        })
    }
    /// Acknowledge listed unknown costs without inventing dispatch/usage; retain reservations for later proof.
    /// 确认指定未知成本，不推测派发或用量，保留预约供后续核验。
    pub fn acknowledge_unknown_model_reservations(
        &self,
        root_task_id: &str,
        expected_version: u64,
        identities: &[ModelReservationIdentity],
    ) -> Result<TaskRecord> {
        if identities.is_empty() || identities.len() > 128 {
            return Err(invalid("Choose 1 to 128 model reservations. / 须选择 1 至 128 个模型预约。"));
        }
        let mut unique = std::collections::HashSet::new();
        for identity in identities {
            identifier(&identity.task_id)?; identifier(&identity.reservation_id)?;
            if !unique.insert((&identity.task_id, &identity.reservation_id)) { return Err(invalid("A reservation was listed twice. / 同一预约不能重复选择。")); }
        }
        self.write(|connection| {
            let mut root = row_task(connection, root_task_id)?;
            if root.parent_task_id.is_some() { return Err(invalid("Confirm unknown costs on the root task. / 须在根任务上确认未知成本。")); }
            let mut targets = Vec::new();
            for identity in identities {
                let owner = row_task(connection, &identity.task_id)?;
                if ancestors(connection, &owner)?.last().is_none_or(|ancestor| ancestor.task_id != root_task_id) {
                    return Err(invalid("Reservation is outside this task tree. / 预约不属于该任务树。"));
                }
                let reservation = find_reservation(connection, &identity.task_id, &identity.reservation_id)?
                    .ok_or_else(|| invalid("Model reservation was not found. / 模型预约不存在。"))?;
                if reservation.state != ModelReservationState::Unknown {
                    return Err(blocked("Only an unknown model cost can be acknowledged. / 只能确认未知模型成本。"));
                }
                targets.push(reservation);
            }
            if targets.iter().all(ModelRequestReservation::is_unknown_acknowledged) { return Ok(root); }
            if root.version != expected_version { return Err(conflict("Task changed before confirming costs. / 确认成本前任务已变化。")); }
            if root.state != TaskState::WaitingVerification { return Err(blocked("Task is not awaiting verification. / 任务并非等待核验。")); }
            let mut query = connection.prepare_cached("WITH RECURSIVE tree(id) AS (SELECT ? UNION ALL SELECT t.task_id FROM tasks t JOIN tree p ON t.parent_task_id=p.id) SELECT t.document FROM tasks t JOIN tree p ON t.task_id=p.id").map_err(|_|failure())?;
            let members = query.query_map([root_task_id],|row|row.get::<_,String>(0)).map_err(|_|failure())?.collect::<std::result::Result<Vec<_>,_>>().map_err(|_|failure())?;
            drop(query);
            for member in members { verify_acknowledgement_executor(connection, &decode(member)?)?; }
            root.version += 1;
            for mut reservation in targets.into_iter().filter(|reservation| !reservation.is_unknown_acknowledged()) {
                reservation.verification_reference = Some(json!({"kind":UNKNOWN_MODEL_COST_ACKNOWLEDGED_KIND,
                    "outcome":"unknown","rootTaskId":root_task_id,"rootVersion":expected_version,"acknowledgedAtMs":now_ms()}));
                reservation.updated_at_ms = now_ms();
                connection.execute("UPDATE task_model_reservations SET document=? WHERE task_id=? AND reservation_id=?", params![raw(&reservation)?,reservation.task_id,reservation.reservation_id]).map_err(|_|failure())?;
                let payload = json!({"taskId":reservation.task_id,"reservationId":reservation.reservation_id,"state":"unknown","verificationReference":reservation.verification_reference});
                let event_id = format!("model:{}:unknown-cost-acknowledged", reservation.reservation_id);
                if reservation.task_id == root_task_id {
                    append(connection, &mut root, &event_id, "task.model.unknown-cost-acknowledged", &payload)?;
                } else {
                    let mut owner = row_task(connection, &reservation.task_id)?;
                    append(connection, &mut owner, &event_id, "task.model.unknown-cost-acknowledged", &payload)?;
                }
            }
            save_task(connection, &mut root)?;
            Ok(root)
        })
    }

    pub fn append_event(
        &self,
        token: &AttemptToken,
        event_id: &str,
        kind: &str,
        payload: &Value,
    ) -> Result<TaskEvent> {
        self.write(|connection| {
            let mut task = verify_fence(connection, token, false)?;
            append(connection, &mut task, event_id, kind, payload)
        })
    }
    /// Steering uses formal messages without changing scope, goal or the active attempt.
    /// 插话引用正式消息，不改变范围、原目标或活动尝试。
    pub fn record_user_steering(
        &self,
        task_id: &str,
        expected_version: u64,
        message_id: &str,
        action: &str,
        content_hash: &str,
    ) -> Result<TaskRecord> {
        super::identity(&json!(message_id))?;
        sha256(content_hash)?;
        if !["continue", "update"].contains(&action) {
            return Err(invalid("Unknown steering action. / 插话动作无效。"));
        }
        let event_id = format!("steer:{message_id}");
        let payload =
            json!({"userMessageId":message_id,"action":action,"contentHash":content_hash});
        self.write(|connection| {
            let mut task = row_task(connection, task_id)?;
            let existing: Option<String> = connection.query_row("SELECT document FROM task_events WHERE task_id=? AND event_id=?", params![task_id,event_id], |row| row.get(0)).optional().map_err(|_|failure())?;
            if let Some(existing) = existing {
                let event: TaskEvent = decode(existing)?;
                if event.payload != payload { return Err(conflict("Steering identity was reused. / 插话身份不能复用于其他内容。")); }
                return Ok(task);
            }
            // Preserve legacy receipts, but never bind the same chat message to another root.
            // 保留旧回执；同一聊天的同条消息不得新增绑定到另一根任务。
            if !steering_roots(connection, &task.scope.conversation_id, message_id)?.is_empty() {
                return Err(ContractError::new("Steering is already bound to another task. / 这条引导已关联其他任务，请重连原任务。", "TASK_STEERING_TARGET_CONFLICT", 409));
            }
            if task.version != expected_version {
                return Err(ContractError::new("Task changed before steering. / 保存插话前任务已变化。", "TASK_VERSION_CONFLICT", 409));
            }
            if task.parent_task_id.is_some() || task.state.is_terminal() { return Err(blocked("Steering requires an unfinished root task. / 插话须关联未结束的根任务。")); }
            task.version += 1;
            if task.state == TaskState::Paused { task.stop_reason = Some(event_id.clone()); }
            append(connection, &mut task, &event_id, "task.user-steering", &payload)?;
            if action == "update" && task.state == TaskState::Paused {
                // Plan changes retain old delegate receipts and cannot silently restart unknown effects.
                // 方案变更保留旧委派回执，不能静默重启未知操作。
                let mut query = connection.prepare_cached("WITH RECURSIVE tree(id) AS (SELECT task_id FROM tasks WHERE parent_task_id=? UNION ALL SELECT t.task_id FROM tasks t JOIN tree p ON t.parent_task_id=p.id) SELECT id FROM tree").map_err(|_|failure())?;
                let ids = query.query_map([task_id], |row|row.get::<_,String>(0)).map_err(|_|failure())?.collect::<std::result::Result<Vec<_>,_>>().map_err(|_|failure())?;
                drop(query);
                for id in ids {
                    let mut child = row_task(connection,&id)?;
                    if matches!(child.state,TaskState::Completed|TaskState::Cancelled) { continue; }
                    child.state = TaskState::Cancelled;
                    child.paused_by_task_id = None;
                    child.stop_reason = Some("superseded-by-user-steering".into());
                    child.version += 1;
                    let payload = json!({"state":child.state,"userMessageId":message_id,"reason":child.stop_reason});
                    append(connection,&mut child,&event_id,"task.plan-superseded",&payload)?;
                }
            }
            Ok(task)
        })
    }
    /// Read immutable steering ownership before changing formal messages.
    /// 修改正式消息前查询原有引导归属，事务写入仍会再次核验。
    pub fn user_steering_roots(&self, conversation_id: &str, message_id: &str) -> Result<Vec<String>> {
        identifier(conversation_id)?;
        super::identity(&json!(message_id))?;
        let connection = sqlite_connections::acquire(&self.root, &self.path, 30_000)?;
        steering_roots(&connection, conversation_id, message_id)
    }
    pub fn user_steering(&self, task_id: &str) -> Result<Vec<TaskEvent>> {
        let connection = sqlite_connections::acquire(&self.root, &self.path, 30_000)?;
        row_task(&connection, task_id)?;
        let mut query = connection.prepare_cached("SELECT document FROM task_events WHERE task_id=? AND json_extract(document,'$.kind')='task.user-steering' ORDER BY sequence").map_err(|_|failure())?;
        let rows = query
            .query_map([task_id], |row| row.get::<_, String>(0))
            .map_err(|_| failure())?;
        rows.map(|row| decode(row.map_err(|_| failure())?))
            .collect()
    }
    pub fn append_events(
        &self,
        token: &AttemptToken,
        events: &[(String, String, Value)],
    ) -> Result<Vec<TaskEvent>> {
        if events.len() > 512 {
            return Err(invalid("Event batch is too large. / 事件批次过大。"));
        }
        self.write(|connection| {
            let mut task = verify_fence(connection, token, true)?;
            events
                .iter()
                .map(|(id, kind, payload)| append(connection, &mut task, id, kind, payload))
                .collect()
        })
    }
    pub fn read_events(
        &self,
        task_id: &str,
        after_sequence: u64,
        limit: usize,
    ) -> Result<Vec<TaskEvent>> {
        let connection = sqlite_connections::acquire(&self.root, &self.path, 30_000)?;
        row_task(&connection, task_id)?;
        let mut query=connection.prepare_cached("SELECT document FROM task_events WHERE task_id=? AND sequence>? ORDER BY sequence LIMIT ?").map_err(|_|failure())?;
        let rows = query
            .query_map(
                params![task_id, after_sequence, limit.clamp(1, 512) as i64],
                |row| row.get::<_, String>(0),
            )
            .map_err(|_| failure())?;
        rows.map(|row| decode(row.map_err(|_| failure())?))
            .collect()
    }
    /// Read committed display deltas after the message watermark without model settlement or tool replay.
    /// 从消息水位后读取已提交展示增量，不结算模型或重放工具。
    pub fn stream_projection(&self, task_id: &str, request_id: &str, reply_id: &str, after_stream_sequence: u64) -> Result<Value> {
        identifier(task_id)?; identifier(request_id)?; identifier(reply_id)?;
        let connection = sqlite_connections::acquire(&self.root, &self.path, 30_000)?;
        let task = row_task(&connection, task_id)?;
        if task.request_id != request_id && !(task.parent_task_id.is_some() && task.task_id == request_id) {
            return Err(invalid("Stream request does not belong to this task. / 流请求不属于本任务。"));
        }
        let mut projection = stream_projection::StreamProjection::new(&task.scope.conversation_id, request_id, reply_id, after_stream_sequence);
        let mut query = connection.prepare_cached("SELECT document FROM task_events WHERE task_id=? AND json_extract(document,'$.kind') IN ('text_delta','reasoning_delta','assistant_segment','content_snapshot') ORDER BY sequence")
            .map_err(|_| failure())?;
        let rows = query.query_map([task_id], |row| row.get::<_, String>(0)).map_err(|_| failure())?;
        for row in rows { projection.observe(&decode::<TaskEvent>(row.map_err(|_| failure())?)?); }
        Ok(projection.finish())
    }

    /// Page committed events by task keyset; callers verify pages and persist watermarks.
    /// 按任务键集分页读取已提交事件，调用方逐页核验并保存水位。
    pub fn stream_projection_page(&self, task_id: &str, request_id: &str, reply_id: &str,
        after_task_sequence: u64, through_task_sequence: Option<u64>, maximum_events: usize,
        maximum_bytes: usize) -> Result<Value> {
        identifier(task_id)?; identifier(request_id)?; identifier(reply_id)?;
        let connection = sqlite_connections::acquire(&self.root,&self.path,30_000)?;
        let task = row_task(&connection,task_id)?;
        if task.request_id != request_id && !(task.parent_task_id.is_some() && task.task_id == request_id) {
            return Err(invalid("Stream request does not belong to this task. / 流请求不属于本任务。"));
        }
        let horizon = through_task_sequence.unwrap_or(task.event_sequence).min(task.event_sequence);
        let limit = maximum_events.clamp(1,512);
        let byte_limit = maximum_bytes.clamp(1,8*1024*1024);
        let mut query = connection.prepare_cached("SELECT sequence,document FROM task_events WHERE task_id=? AND sequence>? AND sequence<=? AND json_extract(document,'$.kind') IN ('text_delta','reasoning_delta','assistant_segment','content_snapshot') ORDER BY sequence LIMIT ?")
            .map_err(|_|failure())?;
        let rows = query.query_map(params![task_id,after_task_sequence,horizon,(limit+1) as i64],
            |row|Ok((row.get::<_,u64>(0)?,row.get::<_,String>(1)?))).map_err(|_|failure())?;
        let mut events = Vec::<TaskEvent>::new();
        let mut cursor = after_task_sequence;
        let mut bytes = 0usize;
        let mut visited = 0usize;
        let mut has_more = false;
        let mut stream_sequence = 0u64;
        for row in rows {
            let (sequence,document) = row.map_err(|_|failure())?;
            if visited == limit {has_more=true;break;}
            let event:TaskEvent = decode(document)?;
            let owned = event.payload["conversationId"] == task.scope.conversation_id
                && event.payload["requestId"] == request_id && event.payload["replyMessageId"] == reply_id;
            let size = if owned {raw(&event)?.len()} else {0};
            // Return oversized events intact within the 64-KiB record cap; do not split UTF-8 or metadata.
            // 大事件在 64 KiB 记录限额内完整返回，不拆 UTF-8 或元数据。
            if owned && !events.is_empty() && bytes.saturating_add(size)>byte_limit {has_more=true;break;}
            visited+=1; cursor=sequence;
            if owned {
                stream_sequence=stream_sequence.max(event.payload["eventSequence"].as_u64().unwrap_or(0));
                bytes=bytes.saturating_add(size); events.push(event);
            }
        }
        if !has_more {cursor=horizon.max(cursor);}
        Ok(json!({"schemaVersion":1,"kind":"committed-stream-page","taskId":task_id,
            "conversationId":task.scope.conversation_id,"requestId":request_id,"replyMessageId":reply_id,
            "afterTaskSequence":after_task_sequence,"nextTaskSequence":cursor,"throughTaskSequence":horizon,
            "throughStreamSequence":stream_sequence,"hasMore":has_more,"events":events,
            "pageBytes":bytes,"maximumPageBytes":byte_limit,"indivisibleEventBytes":MAX_RECORD_BYTES,
            "evidence":"committed-display-only; no execution or completion proof"}))
    }

    /// Publish restored pages only after prefix/overlap checks; cancellation or conflict preserves the watermark.
    /// 分页恢复须通过前缀及重叠核验，取消或冲突不推进水位。
    pub fn recover_stream(&self, task_id:&str, request_id:&str, reply_id:&str, after_stream_sequence:u64,
        baseline:Value, cancelled:impl FnMut()->bool) -> Result<Value> {
        self.recover_stream_with_limits(task_id,request_id,reply_id,after_stream_sequence,baseline,256,1024*1024,cancelled)
    }

    fn recover_stream_with_limits(&self, task_id:&str, request_id:&str, reply_id:&str, after_stream_sequence:u64,
        baseline:Value, maximum_events:usize, maximum_bytes:usize, mut cancelled:impl FnMut()->bool) -> Result<Value> {
        let mut recovery=stream_projection::StreamRecovery::new(baseline,after_stream_sequence);
        if !recovery.complete() {return Ok(recovery.finish());}
        let mut cursor=0u64;let mut horizon=None;let mut pages=0u64;
        loop {
            if cancelled() {return Ok(json!({"complete":false,"cancelled":true,"incompleteReasons":["recovery-cancelled"]}));}
            let mut page=self.stream_projection_page(task_id,request_id,reply_id,cursor,horizon,maximum_events,maximum_bytes)?;
            horizon=page["throughTaskSequence"].as_u64();pages=pages.saturating_add(1);
            let events=page["events"].as_array_mut().map(std::mem::take).unwrap_or_default();
            for event in events {
                recovery.observe(&serde_json::from_value::<TaskEvent>(event).map_err(|_|failure())?);
                if !recovery.complete() {return Ok(recovery.finish());}
            }
            let next=page["nextTaskSequence"].as_u64().ok_or_else(failure)?;
            if page["hasMore"]!=true {
                let mut result=recovery.finish();result["pageCount"]=json!(pages);
                return Ok(result);
            }
            if next<=cursor {return Err(invalid("Stream page made no keyset progress. / 流分页键集未推进。"));}
            cursor=next;
        }
    }

    pub fn operations(&self, task_id: &str) -> Result<Vec<TaskOperation>> {
        let connection = sqlite_connections::acquire(&self.root, &self.path, 30_000)?;
        read_operations(&connection, task_id)
    }
    pub fn operation(&self, task_id: &str, operation_id: &str) -> Result<Option<TaskOperation>> {
        let connection = sqlite_connections::acquire(&self.root, &self.path, 30_000)?;
        row_task(&connection, task_id)?;
        find_operation(&connection, task_id, operation_id)
    }

    /// Read the latest event of one kind without paging the full stream.
    /// 直接读取指定类型的最新事件，不分页遍历整段流。
    pub fn latest_event(&self, task_id: &str, kind: &str) -> Result<Option<TaskEvent>> {
        identifier(kind)?;
        let connection = sqlite_connections::acquire(&self.root, &self.path, 30_000)?;
        row_task(&connection, task_id)?;
        connection
            .query_row(
                "SELECT document FROM task_events WHERE task_id=? AND json_extract(document,'$.kind')=? ORDER BY sequence DESC LIMIT 1",
                params![task_id, kind],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|_| failure())?
            .map(decode)
            .transpose()
    }

    /// Read dispatch/receipt evidence without replaying token events.
    /// 仅查派发及回执证据，不重放 token 事件。
    pub fn recovery_receipts(&self, task_id: &str) -> Result<Vec<TaskEvent>> {
        let connection = sqlite_connections::acquire(&self.root, &self.path, 30_000)?;
        row_task(&connection, task_id)?;
        let mut query = connection.prepare_cached("SELECT document FROM task_events WHERE task_id=? AND json_extract(document,'$.kind') IN ('task.model.receipt','task.operation.receipt','task.operation.dispatched','mcp_task_checkpoint') ORDER BY sequence").map_err(|_| failure())?;
        let rows = query.query_map([task_id], |row| row.get::<_, String>(0)).map_err(|_| failure())?;
        rows.map(|row| decode(row.map_err(|_| failure())?)).collect()
    }
    pub fn record_operation(
        &self,
        token: &AttemptToken,
        intent: &OperationIntent,
    ) -> Result<TaskOperation> {
        identifier(&intent.operation_id)?;
        identifier(&intent.tool_call_id)?;
        identifier(&intent.tool_name)?;
        sha256(&intent.input_sha256)?;
        raw(intent)?;
        self.write(|connection| {
            let mut task=verify_new_work_fence(connection,token)?;
            if let Some(mut existing)=find_operation(connection,&token.task_id,&intent.operation_id)? {
                if fingerprint(&existing.intent)?!=fingerprint(intent)? {return Err(conflict("Operation identity was reused for different input. / 操作身份不能复用于不同输入。"));}
                if existing.state==OperationState::Pending && existing.attempt_id!=token.attempt_id {existing.attempt_id=token.attempt_id.clone();existing.updated_at_ms=now_ms();save_operation(connection,&existing)?;}
                return Ok(existing);
            }
            if intent.effect!=OperationEffect::Observation && tree_has_unknown_mutation(connection,&task)? {return Err(blocked("Verify unknown effects before another mutation in this task tree. / 本任务树再次修改前须核验结果未知的副作用。"));}
            let operation=TaskOperation {task_id:token.task_id.clone(),attempt_id:token.attempt_id.clone(),intent:intent.clone(),state:OperationState::Pending,result_reference:None,verification_reference:None,updated_at_ms:now_ms()};
            connection.execute("INSERT INTO task_operations(task_id,operation_id,state,effect,attempt_id,fingerprint,document) VALUES(?,?,'pending',?,?,?,?)",params![token.task_id,intent.operation_id,state_text(&intent.effect),token.attempt_id,fingerprint(intent)?,raw(&operation)?]).map_err(|_|failure())?;
            append(connection,&mut task,&format!("operation:{}:intent",intent.operation_id),"task.operation.intent",&json!({"operationId":intent.operation_id,"toolCallId":intent.tool_call_id,"toolName":intent.tool_name}))?;
            Ok(operation)
        })
    }
    pub fn dispatch_operation(
        &self,
        token: &AttemptToken,
        operation_id: &str,
    ) -> Result<TaskOperation> {
        self.write(|connection| {
            let mut task=verify_new_work_fence(connection,token)?;
            let mut operation=find_operation(connection,&token.task_id,operation_id)?.ok_or_else(||invalid("Operation intent is missing. / 操作意图尚未登记。"))?;
            if operation.state!=OperationState::Pending || operation.attempt_id!=token.attempt_id {return Err(blocked("Operation was dispatched, settled or belongs to another attempt. / 操作已派发、已结算或属于其他尝试。"));}
            if operation.intent.effect!=OperationEffect::Observation && tree_has_unknown_mutation(connection,&task)? {return Err(blocked("Unknown effects in the task tree must be verified. / 必须先核验任务树内结果未知的副作用。"));}
            operation.state=OperationState::Dispatched;operation.updated_at_ms=now_ms();save_operation(connection,&operation)?;
            append(connection,&mut task,&format!("operation:{operation_id}:dispatch"),"task.operation.dispatched",&json!({"operationId":operation_id}))?;Ok(operation)
        })
    }
    pub fn finish_operation(
        &self,
        token: &AttemptToken,
        operation_id: &str,
        state: OperationState,
        result_reference: Option<Value>,
        verification_reference: Option<Value>,
    ) -> Result<TaskOperation> {
        if !matches!(
            state,
            OperationState::Succeeded | OperationState::Failed | OperationState::Unknown
        ) {
            return Err(invalid("Invalid operation outcome. / 操作结果状态无效。"));
        }
        self.write(|connection| {
            let mut task=verify_fence(connection,token,true)?;
            let mut operation=find_operation(connection,&token.task_id,operation_id)?.ok_or_else(||invalid("Operation does not exist. / 操作不存在。"))?;
            if matches!(operation.state,OperationState::Succeeded|OperationState::Failed) {
                return if operation.state==state && operation.result_reference==result_reference {Ok(operation)} else {Err(conflict("Settled operation cannot be overwritten. / 已结算的操作不能覆盖。"))};
            }
            if operation.state==OperationState::Unknown && state!=OperationState::Unknown && verification_reference.as_ref().is_none_or(|reference|!reference.is_object()) {return Err(blocked("Unknown outcome requires a verified original receipt reference. / 结果未知须提供已核验的原始回执引用。"));}
            if operation.state==OperationState::Pending || (operation.state!=OperationState::Unknown && operation.attempt_id!=token.attempt_id) {return Err(blocked("Operation was not dispatched by this attempt. / 当前尝试尚未派发该操作。"));}
            if state==OperationState::Succeeded && result_reference.as_ref().is_none_or(|reference|!reference.is_object()) {return Err(invalid("Success requires an original result reference. / 成功结果须保留原始回执引用。"));}
            operation.state=state;operation.result_reference=result_reference;operation.verification_reference=verification_reference;operation.updated_at_ms=now_ms();save_operation(connection,&operation)?;
            append(connection,&mut task,&uuid::Uuid::new_v4().to_string(),"task.operation.result",&json!({"operationId":operation_id,"state":operation.state,"resultReference":operation.result_reference,"verificationReference":operation.verification_reference}))?;Ok(operation)
        })
    }
    /// Extend authorized operating segments monotonically; audit changes and preserve token/ancestor caps.
    /// 按授权单调扩展执行段并记审计，保留 token 及祖先上限。
    pub fn extend_operational_budget(
        &self,
        token: &AttemptToken,
        min_model_requests: u32,
        min_execution_ms: u64,
        reason: &str,
    ) -> Result<TaskRecord> {
        identifier(reason)?;
        self.write(|connection| {
            let mut task = verify_fence(connection, token, false)?;
            let previous = task.budget.clone();
            task.budget.max_model_requests = task.budget.max_model_requests.max(u64::from(min_model_requests));
            // None means unlimited execution time; do not replace it with a smaller cap.
            // None 已表示无执行时长上限，不能改成更小限额。
            task.budget.max_execution_ms = task.budget.max_execution_ms.map(|limit|limit.max(min_execution_ms));
            if task.budget == previous { return Ok(task); }
            task.version += 1;
            let event_id = format!("budget:{}:{}", token.attempt_id, task.version);
            let payload = json!({"previous":previous,"budget":task.budget,"reason":reason,"attemptId":token.attempt_id});
            append(connection, &mut task, &event_id, "task.budget.extended", &payload)?;
            Ok(task)
        })
    }

    /// Add only new effective wall time above the high-water mark, excluding parallel child totals.
    /// 仅累计高水位以上新增有效墙钟，不叠加并行子任务耗时。
    pub fn record_active_duration(&self, token: &AttemptToken, cumulative_attempt_ms: u64) -> Result<TaskRecord> {
        self.write(|connection| {
            let mut task = verify_fence(connection, token, true)?;
            let document: String = connection.query_row("SELECT document FROM task_attempts WHERE attempt_id=?", [&token.attempt_id], |row| row.get(0))
                .map_err(|_| failure())?;
            let mut attempt: TaskAttempt = decode(document)?;
            if cumulative_attempt_ms <= attempt.active_duration_ms && task.usage.active_duration_ms.is_some() {
                return Ok(task);
            }
            let delta = cumulative_attempt_ms.saturating_sub(attempt.active_duration_ms);
            attempt.active_duration_ms = attempt.active_duration_ms.max(cumulative_attempt_ms);
            task.usage.active_duration_ms = Some(task.usage.active_duration_ms.unwrap_or(0).saturating_add(delta));
            connection.execute("UPDATE task_attempts SET document=? WHERE attempt_id=?", params![raw(&attempt)?,token.attempt_id]).map_err(|_|failure())?;
            save_task(connection, &mut task)?;
            Ok(task)
        })
    }

    /// Settle each usage ID once, including child failures/retries/compactions; track unknown tokens separately.
    /// 每个计量 ID 仅结算一次，含子任务失败、重试及压缩；未知 token 单独统计。
    pub fn record_usage(
        &self,
        token: &AttemptToken,
        usage_id: &str,
        delta: &TaskUsage,
    ) -> Result<TaskRecord> {
        identifier(usage_id)?;
        if delta.active_duration_ms.is_some() || delta.failed_requests > delta.model_requests
            || delta.unknown_token_requests > delta.model_requests
        {
            return Err(invalid(
                "Usage counters are inconsistent. / 用量计数不一致。",
            ));
        }
        self.write(|connection| {
            let task = verify_fence(connection, token, true)?;
            let digest = fingerprint(delta)?;
            let old: Option<String> = connection
                .query_row(
                    "SELECT fingerprint FROM task_usage WHERE task_id=? AND usage_id=?",
                    params![task.task_id, usage_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|_| failure())?;
            if let Some(old) = old {
                return if old == digest {
                    Ok(task)
                } else {
                    Err(conflict("Usage identity was reused. / 用量身份不能复用。"))
                };
            }
            connection
                .execute(
                    "INSERT INTO task_usage(task_id,usage_id,fingerprint) VALUES(?,?,?)",
                    params![task.task_id, usage_id, digest],
                )
                .map_err(|_| failure())?;
            for mut record in ancestors(connection, &task)? {
                record.usage.add(delta);
                save_task(connection, &mut record)?;
            }
            row_task(connection, &token.task_id)
        })
    }
    /// Count dispatched requests and outstanding child reservations; recovery cannot reset the parent budget.
    /// 同时计入已派发请求与子任务未兑现预约，恢复不重置父预算。
    pub fn reserve_model_request(
        &self,
        token: &AttemptToken,
        reservation_id: &str,
    ) -> Result<ModelRequestReservation> {
        identifier(reservation_id)?;
        self.write(|connection| {
            let task=verify_new_work_fence(connection,token)?;
            if let Some(existing)=find_reservation(connection,&token.task_id,reservation_id)? {return Ok(existing);}
            for ancestor in ancestors(connection,&task)? {
                if ancestor.state.is_terminal() || ancestor.state==TaskState::Paused {return Err(blocked("Parent task is controlled or ended. / 父任务已暂停、取消或结束。"));}
                let pending=pending_reservations(connection,&ancestor.task_id)?;
                if ancestor.usage.is_exhausted(&ancestor.budget) || ancestor.usage.model_requests.saturating_add(pending)>=ancestor.budget.max_model_requests {return Err(ContractError::new("Task-tree request budget is exhausted or reserved. / 任务树请求预算已耗尽或已预约。","TASK_BUDGET_EXHAUSTED",409));}
            }
            let timestamp=now_ms();
            let reservation=ModelRequestReservation {task_id:token.task_id.clone(),reservation_id:reservation_id.to_owned(),attempt_id:token.attempt_id.clone(),state:ModelReservationState::Reserved,dispatched:None,usage:None,provider_usage:None,verification_reference:None,created_at_ms:timestamp,updated_at_ms:timestamp};
            connection.execute("INSERT INTO task_model_reservations(task_id,reservation_id,attempt_id,state,document) VALUES(?,?,?,'reserved',?)",params![token.task_id,reservation_id,token.attempt_id,raw(&reservation)?]).map_err(|_|failure())?;Ok(reservation)
        })
    }
    pub fn settle_model_request(
        &self,
        token: &AttemptToken,
        reservation_id: &str,
        dispatched: bool,
        usage: &TaskUsage,
    ) -> Result<ModelRequestReservation> {
        self.settle_reservation(token, reservation_id, dispatched, usage, None, None)
    }
    pub fn settle_model_request_with_provider_usage(
        &self, token: &AttemptToken, reservation_id: &str, dispatched: bool,
        usage: &TaskUsage, provider_usage: Option<ProviderTokenUsage>,
    ) -> Result<ModelRequestReservation> {
        self.settle_reservation(token, reservation_id, dispatched, usage, provider_usage, None)
    }
    /// After model I/O ends without a durable receipt, retain unknown dispatch and cost.
    /// 模型 I/O 结束但回执未落盘时，派发及成本保持未知。
    pub fn mark_model_request_unknown(&self, token: &AttemptToken, reservation_id: &str, reason: &str) -> Result<ModelRequestReservation> {
        identifier(reservation_id)?;
        identifier(reason)?;
        self.write(|connection| {
            let mut task=verify_fence(connection,token,true)?;
            let mut reservation=find_reservation(connection,&token.task_id,reservation_id)?.ok_or_else(||invalid("Model reservation was not found. / 模型预约不存在。"))?;
            if reservation.attempt_id!=token.attempt_id { return Err(stale()); }
            if reservation.state==ModelReservationState::Unknown { return Ok(reservation); }
            if reservation.state!=ModelReservationState::Reserved { return Err(blocked("A settled model reservation cannot become unknown. / 已结算模型预约不能改为未知。")); }
            reservation.state=ModelReservationState::Unknown;
            reservation.dispatched=None;
            reservation.usage=None;
            reservation.provider_usage=None;
            reservation.updated_at_ms=now_ms();
            connection.execute("UPDATE task_model_reservations SET state='unknown',document=? WHERE task_id=? AND reservation_id=?",params![raw(&reservation)?,token.task_id,reservation_id]).map_err(|_|failure())?;
            let payload=json!({"reservationId":reservation_id,"attemptId":token.attempt_id,"state":"unknown","reason":reason});
            append(connection,&mut task,&format!("model:{reservation_id}:unknown"),"task.model.unknown",&payload)?;
            Ok(reservation)
        })
    }
    pub fn reconcile_model_request(
        &self,
        token: &AttemptToken,
        reservation_id: &str,
        dispatched: bool,
        usage: &TaskUsage,
        verification_reference: Value,
    ) -> Result<ModelRequestReservation> {
        if !verification_reference.is_object() {
            return Err(invalid(
                "Unknown dispatch needs a verified original reference. / 未知派发须提供已核验的原文引用。",
            ));
        }
        self.settle_reservation(
            token,
            reservation_id,
            dispatched,
            usage,
            None,
            Some(verification_reference),
        )
    }
    pub fn reconcile_model_request_with_provider_usage(
        &self, token: &AttemptToken, reservation_id: &str, dispatched: bool,
        usage: &TaskUsage, provider_usage: Option<ProviderTokenUsage>, verification_reference: Value,
    ) -> Result<ModelRequestReservation> {
        if !verification_reference.is_object() {
            return Err(invalid("Unknown dispatch needs a verified original reference. / 未知派发须提供已核验的原文引用。"));
        }
        self.settle_reservation(token, reservation_id, dispatched, usage, provider_usage, Some(verification_reference))
    }
    fn settle_reservation(
        &self,
        token: &AttemptToken,
        reservation_id: &str,
        dispatched: bool,
        usage: &TaskUsage,
        provider_usage: Option<ProviderTokenUsage>,
        verification_reference: Option<Value>,
    ) -> Result<ModelRequestReservation> {
        identifier(reservation_id)?;
        if usage.active_duration_ms.is_some() || usage.model_requests > 1
            || usage.failed_requests > 1
            || usage.unknown_token_requests > 1
            || (!dispatched && *usage != TaskUsage::default())
        {
            return Err(invalid(
                "One reservation settles one actual request; unsent work has no usage. / 一次预约结算一次实际请求；未发送不计用量。",
            ));
        }
        let mut delta = usage.clone();
        delta.model_requests = u64::from(dispatched);
        if let Some(provider) = &provider_usage {
            let observed_input = [provider.cache_read_tokens, provider.cache_creation_tokens, provider.uncached_input_tokens]
                .into_iter().flatten().try_fold(0_u64, u64::checked_add);
            if !dispatched
                || provider.input_tokens.is_some_and(|input| input != usage.input_tokens)
                || provider.output_tokens.is_some_and(|output| output != usage.output_tokens)
                || provider.cache_read_tokens.is_some_and(|read| provider.input_tokens.is_some_and(|input| read > input))
                || observed_input.is_none_or(|sum| provider.input_tokens.is_some_and(|input| sum > input)) {
                return Err(invalid("Provider usage must match its actual request receipt. / 供应商用量必须匹配本次真实回执。"));
            }
        }
        // Keep legacy settlement fingerprints; new provider facts form part of new receipt IDs.
        // 旧结算沿用原指纹，新供应商事实计入新回执身份。
        let digest = fingerprint(&if provider_usage.is_some() { json!([dispatched, delta, provider_usage]) } else { json!([dispatched, delta]) })?;
        self.write(|connection| {
            let task=verify_fence(connection,token,true)?;
            let mut reservation=find_reservation(connection,&token.task_id,reservation_id)?.ok_or_else(||invalid("Model request was not reserved. / 模型请求尚未预约。"))?;
            let old:Option<String>=connection.query_row("SELECT settlement_fingerprint FROM task_model_reservations WHERE task_id=? AND reservation_id=?",params![token.task_id,reservation_id],|row|row.get(0)).map_err(|_|failure())?;
            if let Some(old)=old {return if old==digest {Ok(reservation)} else {Err(conflict("Model settlement identity was reused. / 模型结算身份不能复用于不同结果。"))};}
            if reservation.state==ModelReservationState::Unknown && !dispatched && verification_reference.is_none() {return Err(blocked("Unknown dispatch cannot be released without verification. / 未知派发未核验不能释放预约。"));}
            if reservation.state==ModelReservationState::Reserved && reservation.attempt_id!=token.attempt_id {return Err(stale());}
            if dispatched {for mut record in ancestors(connection,&task)? {record.usage.add(&delta);save_task(connection,&mut record)?;}}
            reservation.state=if dispatched {ModelReservationState::Settled} else {ModelReservationState::Released};
            reservation.dispatched=Some(dispatched);reservation.usage=Some(delta);reservation.provider_usage=provider_usage;reservation.verification_reference=verification_reference;reservation.updated_at_ms=now_ms();
            connection.execute("UPDATE task_model_reservations SET state=?,settlement_fingerprint=?,document=? WHERE task_id=? AND reservation_id=?",params![state_text(&reservation.state),digest,raw(&reservation)?,token.task_id,reservation_id]).map_err(|_|failure())?;
            Ok(reservation)
        })
    }
    /// Count each reservation once; parent totals already include descendants.
    /// 每个预约只统计一次，父任务总量已包含后代。
    pub fn usage_summary(&self, conversation_id: Option<&str>, project_id: Option<&str>) -> Result<Value> {
        if conversation_id.is_none() && project_id.is_none() {
            return Err(invalid("Usage needs an explicit chat or work scope. / 用量须限定聊天或工作范围。"));
        }
        if let Some(id) = conversation_id { identifier(id)?; }
        if let Some(id) = project_id { identifier(id)?; }
        let connection = sqlite_connections::acquire(&self.root, &self.path, 30_000)?;
        // Concrete scope predicates let SQLite seek scoped tasks before their unique reservations.
        // 具体范围等值条件允许 SQLite 先定位聊天或工作任务，再读取各任务的唯一预约。
        let (sql, scope_parameters) = match (conversation_id, project_id) {
            (Some(chat), Some(project)) => (
                "SELECT r.document FROM tasks t JOIN task_model_reservations r ON r.task_id=t.task_id WHERE t.conversation_id=?1 AND t.project_id=?2",
                vec![chat, project],
            ),
            (Some(chat), None) => (
                "SELECT r.document FROM tasks t JOIN task_model_reservations r ON r.task_id=t.task_id WHERE t.conversation_id=?1",
                vec![chat],
            ),
            (None, Some(project)) => (
                "SELECT r.document FROM tasks t JOIN task_model_reservations r ON r.task_id=t.task_id WHERE t.project_id=?1",
                vec![project],
            ),
            (None, None) => unreachable!("usage scope was checked above"),
        };
        let mut query = connection.prepare_cached(sql).map_err(|_| failure())?;
        let rows = query.query_map(rusqlite::params_from_iter(scope_parameters), |row| row.get::<_, String>(0))
            .map_err(|_| failure())?;
        let (mut requests, mut input, mut output, mut known, mut unknown, mut pending) = (0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
        let (mut cache_read, mut cache_input, mut cache_known, mut updated) = (0u64, 0u64, 0u64, 0i64);
        let (mut cache_creation, mut uncached_input, mut creation_known, mut uncached_known) = (0u64, 0u64, 0u64, 0u64);
        let (mut first_visible_total, mut first_visible_known) = (0u64, 0u64);
        let mut prefix_counts = json!({"baseline":0,"none":0,"route":0,"system":0,"tools":0,"message":0});
        let (mut prefix_observed, mut prefix_incomplete, mut prefix_lost_estimate) = (0u64, 0u64, 0u64);
        for row in rows {
            let reservation: ModelRequestReservation = decode(row.map_err(|_| failure())?)?;
            updated = updated.max(reservation.updated_at_ms);
            if matches!(reservation.state, ModelReservationState::Reserved | ModelReservationState::Unknown) {
                pending = pending.saturating_add(1);
                if reservation.state == ModelReservationState::Unknown { unknown = unknown.saturating_add(1); }
                continue;
            }
            if reservation.dispatched != Some(true) { continue; }
            requests = requests.saturating_add(1);
            if let Some(usage) = &reservation.usage {
                input = input.saturating_add(usage.input_tokens);
                output = output.saturating_add(usage.output_tokens);
                if usage.unknown_token_requests == 0 { known = known.saturating_add(1); }
                else { unknown = unknown.saturating_add(1); }
            } else { unknown = unknown.saturating_add(1); }
            if let Some(provider) = &reservation.provider_usage {
                // Treat client prefix estimates as diagnostics, not actual provider cache hits.
                // 客户端前缀估算仅作诊断，不算供应商实际缓存命中。
                if let Some(prefix) = provider.client_metrics.as_ref()
                    .and_then(|metrics| metrics.cache_policy.as_ref()).and_then(|policy| policy.get("prefix"))
                    .filter(|prefix| prefix["schemaVersion"] == 1 && prefix["scope"] == "main-request")
                {
                    if let Some(segment) = prefix["breakAt"].as_str().filter(|segment| prefix_counts.get(*segment).is_some()) {
                        prefix_counts[segment] = json!(prefix_counts[segment].as_u64().unwrap_or(0).saturating_add(1));
                        prefix_observed = prefix_observed.saturating_add(1);
                        if prefix["state"] == "incomplete" { prefix_incomplete = prefix_incomplete.saturating_add(1); }
                        prefix_lost_estimate = prefix_lost_estimate.saturating_add(prefix["lostTokensEstimate"].as_u64().unwrap_or(0));
                    }
                }
                if let Some(milliseconds) = provider.client_metrics.as_ref().and_then(|metrics| metrics.first_visible_text_ms) {
                    first_visible_total = first_visible_total.saturating_add(milliseconds);
                    first_visible_known = first_visible_known.saturating_add(1);
                }
                if let Some(tokens) = provider.cache_creation_tokens {
                    cache_creation = cache_creation.saturating_add(tokens); creation_known += 1;
                }
                if let Some(tokens) = provider.uncached_input_tokens {
                    uncached_input = uncached_input.saturating_add(tokens); uncached_known += 1;
                }
                if let (Some(tokens), Some(read)) = (provider.input_tokens, provider.cache_read_tokens) {
                    if read <= tokens {
                        cache_read = cache_read.saturating_add(read);
                        cache_input = cache_input.saturating_add(tokens);
                        cache_known = cache_known.saturating_add(1);
                    }
                }
            }
        }
        Ok(json!({"scope":if conversation_id.is_some() {"conversation"} else {"project"},
            "conversationId":conversation_id,"projectId":project_id,"modelRequests":requests,
            "inputTokens":input,"outputTokens":output,"knownTokenRequests":known,"unknownTokenRequests":unknown,
            "pendingRequests":pending,"cacheReadTokens":cache_read,"cacheInputTokens":cache_input,
            "knownCacheRequests":cache_known,"cacheHitRate":(cache_input>0).then(||cache_read as f64/cache_input as f64),
            "cacheCreationTokens":(creation_known>0).then_some(cache_creation),
            "uncachedInputTokens":(uncached_known>0).then_some(uncached_input),
            "knownCacheCreationRequests":creation_known,"knownUncachedRequests":uncached_known,
            "firstVisibleTextMs":(first_visible_known>0).then(||first_visible_total as f64 / first_visible_known as f64),
            "firstVisibleTextMsTotal":(first_visible_known>0).then_some(first_visible_total),
            "knownFirstVisibleTextRequests":first_visible_known,
            "prefixDiagnostics":{"observedMainRequests":prefix_observed,"bySegment":prefix_counts,
                "incompleteRequests":prefix_incomplete,"lostTokensEstimate":prefix_lost_estimate,
                "coverage":"dispatched main requests with saved client diagnostics; summaries excluded",
                "method":"structural estimates; separate from provider cacheHitRate"},
            "updatedAtMs":updated}))
    }

    pub fn model_reservations(&self, task_id: &str) -> Result<Vec<ModelRequestReservation>> {
        let connection = sqlite_connections::acquire(&self.root, &self.path, 30_000)?;
        read_reservations(&connection, task_id)
    }
    pub fn save_checkpoint(
        &self,
        token: &AttemptToken,
        checkpoint: &TaskCheckpoint,
    ) -> Result<TaskCheckpoint> {
        identifier(&checkpoint.checkpoint_id)?;
        raw(checkpoint)?;
        if !checkpoint.context_reference.is_object() {
            return Err(invalid(
                "Checkpoint needs an original context reference. / 检查点须保留上下文原文引用。",
            ));
        }
        self.write(|connection| {
            let task=verify_fence(connection,token,true)?;
            if checkpoint.through_event_sequence>task.event_sequence {return Err(invalid("Checkpoint refers to events not yet persisted. / 检查点指向尚未持久化的事件。"));}
            let digest=fingerprint(checkpoint)?;
            let old:Option<(String,String)>=connection.query_row("SELECT fingerprint,document FROM task_checkpoints WHERE task_id=? AND checkpoint_id=?",params![token.task_id,checkpoint.checkpoint_id],|row|Ok((row.get(0)?,row.get(1)?))).optional().map_err(|_|failure())?;
            if let Some((old,document))=old {if old!=digest {return Err(conflict("Checkpoint identity was reused. / 检查点身份不能复用。"));}return decode(document);}
            connection.execute("INSERT INTO task_checkpoints(task_id,checkpoint_id,event_sequence,fingerprint,document) VALUES(?,?,?,?,?)",params![token.task_id,checkpoint.checkpoint_id,checkpoint.through_event_sequence,digest,raw(checkpoint)?]).map_err(|_|failure())?;Ok(checkpoint.clone())
        })
    }
    pub fn latest_checkpoint(&self, task_id: &str) -> Result<Option<TaskCheckpoint>> {
        let connection = sqlite_connections::acquire(&self.root, &self.path, 30_000)?;
        let document:Option<String>=connection.query_row("SELECT document FROM task_checkpoints WHERE task_id=? ORDER BY event_sequence DESC,rowid DESC LIMIT 1",[task_id],|row|row.get(0)).optional().map_err(|_|failure())?;
        document.map(decode).transpose()
    }
    pub fn save_artifact(
        &self,
        token: &AttemptToken,
        artifact: &TaskArtifact,
    ) -> Result<TaskArtifact> {
        identifier(&artifact.artifact_id)?;
        identifier(&artifact.kind)?;
        sha256(&artifact.sha256)?;
        raw(artifact)?;
        if !artifact.reference.is_object() {
            return Err(invalid(
                "Artifact needs an owned original-content reference. / 产物须使用受控原文引用。",
            ));
        }
        self.write(|connection| {
            verify_fence(connection,token,true)?;
            let digest=fingerprint(artifact)?;
            let old:Option<(String,String)>=connection.query_row("SELECT fingerprint,document FROM task_artifacts WHERE task_id=? AND artifact_id=?",params![token.task_id,artifact.artifact_id],|row|Ok((row.get(0)?,row.get(1)?))).optional().map_err(|_|failure())?;
            if let Some((old,document))=old {if old!=digest {return Err(conflict("Artifact identity was reused. / 产物身份不能复用。"));}return decode(document);}
            connection.execute("INSERT INTO task_artifacts(task_id,artifact_id,fingerprint,document) VALUES(?,?,?,?)",params![token.task_id,artifact.artifact_id,digest,raw(artifact)?]).map_err(|_|failure())?;Ok(artifact.clone())
        })
    }
    pub fn artifacts(&self, task_id: &str) -> Result<Vec<TaskArtifact>> {
        let connection = sqlite_connections::acquire(&self.root, &self.path, 30_000)?;
        let mut query = connection
            .prepare_cached(
                "SELECT document FROM task_artifacts WHERE task_id=? ORDER BY artifact_id",
            )
            .map_err(|_| failure())?;
        let rows = query
            .query_map([task_id], |row| row.get::<_, String>(0))
            .map_err(|_| failure())?;
        rows.map(|row| decode(row.map_err(|_| failure())?))
            .collect()
    }
    pub fn latest_artifact(&self, task_id: &str, kind: &str) -> Result<Option<TaskArtifact>> {
        identifier(task_id)?;
        identifier(kind)?;
        let connection = sqlite_connections::acquire(&self.root, &self.path, 30_000)?;
        let document:Option<String>=connection.query_row("SELECT document FROM task_artifacts WHERE task_id=? AND json_extract(document,'$.kind')=? ORDER BY rowid DESC LIMIT 1",params![task_id,kind],|row|row.get(0)).optional().map_err(|_|failure())?;
        document.map(decode).transpose()
    }

    /// Read the child's latest parent acceptance without scanning unrelated event history.
    /// 直接读取子任务最新父级验收，不扫描无关事件历史。
    pub fn latest_child_review(&self, parent_id: &str, child_id: &str) -> Result<Option<TaskEvent>> {
        identifier(parent_id)?; identifier(child_id)?;
        let connection=sqlite_connections::acquire(&self.root,&self.path,30_000)?;
        let document:Option<String>=connection.query_row("SELECT document FROM task_events WHERE task_id=? AND json_extract(document,'$.kind')='task.child.review' AND json_extract(document,'$.payload.childTaskId')=? ORDER BY sequence DESC LIMIT 1",params![parent_id,child_id],|row|row.get(0)).optional().map_err(|_|failure())?;
        document.map(decode).transpose()
    }

    /// Restore revision instructions from formal events, not in-memory overlays.
    /// 从正式事件恢复修订要求，不依赖内存覆盖层。
    pub fn latest_child_return(&self, child_id: &str) -> Result<Option<TaskEvent>> {
        self.latest_event(child_id, "task.child.review-return")
    }
}
fn read_operations(connection: &Connection, task_id: &str) -> Result<Vec<TaskOperation>> {
    let mut query = connection
        .prepare_cached("SELECT document FROM task_operations WHERE task_id=? ORDER BY rowid")
        .map_err(|_| failure())?;
    let rows = query
        .query_map([task_id], |row| row.get::<_, String>(0))
        .map_err(|_| failure())?;
    rows.map(|row| decode(row.map_err(|_| failure())?))
        .collect()
}
fn find_operation(
    connection: &Connection,
    task_id: &str,
    operation_id: &str,
) -> Result<Option<TaskOperation>> {
    let document: Option<String> = connection
        .query_row(
            "SELECT document FROM task_operations WHERE task_id=? AND operation_id=?",
            params![task_id, operation_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| failure())?;
    document.map(decode).transpose()
}
fn save_operation(connection: &Connection, operation: &TaskOperation) -> Result<()> {
    connection.execute("UPDATE task_operations SET state=?,attempt_id=?,document=? WHERE task_id=? AND operation_id=?",params![state_text(&operation.state),operation.attempt_id,raw(operation)?,operation.task_id,operation.intent.operation_id]).map_err(|_|failure())?;
    Ok(())
}
fn read_reservations(
    connection: &Connection,
    task_id: &str,
) -> Result<Vec<ModelRequestReservation>> {
    let mut query = connection
        .prepare_cached(
            "SELECT document FROM task_model_reservations WHERE task_id=? ORDER BY rowid",
        )
        .map_err(|_| failure())?;
    let rows = query
        .query_map([task_id], |row| row.get::<_, String>(0))
        .map_err(|_| failure())?;
    rows.map(|row| decode(row.map_err(|_| failure())?))
        .collect()
}
fn find_reservation(
    connection: &Connection,
    task_id: &str,
    reservation_id: &str,
) -> Result<Option<ModelRequestReservation>> {
    let document: Option<String> = connection
        .query_row(
            "SELECT document FROM task_model_reservations WHERE task_id=? AND reservation_id=?",
            params![task_id, reservation_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| failure())?;
    document.map(decode).transpose()
}
fn preserve_unsettled_reservations(
    connection: &Connection,
    task_id: &str,
    attempt_id: &str,
) -> Result<()> {
    for mut reservation in
        read_reservations(connection, task_id)?
            .into_iter()
            .filter(|reservation| {
                reservation.attempt_id == attempt_id
                    && reservation.state == ModelReservationState::Reserved
            })
    {
        reservation.state = ModelReservationState::Unknown;
        reservation.updated_at_ms = now_ms();
        connection.execute("UPDATE task_model_reservations SET state='unknown',document=? WHERE task_id=? AND reservation_id=?",params![raw(&reservation)?,task_id,reservation.reservation_id]).map_err(|_|failure())?;
    }
    Ok(())
}

#[cfg(test)]
mod stream_page_tests {
    use super::*;

    #[test]
    fn stream_projection_pages_are_bounded_lossless_owned_and_use_a_stable_keyset_horizon() {
        let root=std::env::temp_dir().join(format!("kynxa-stream-pages-{}",uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let result=std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let repository=TaskRepository::open(&root).unwrap();
            let request=uuid::Uuid::new_v4().to_string();let chat=uuid::Uuid::new_v4().to_string();
            let reply=uuid::Uuid::new_v4().to_string();
            let task=repository.accept_task(&NewTask {request_id:request.clone(),goal:"fixture".into(),
                acceptance:vec![],scope:TaskScope {project_id:None,conversation_id:chat.clone(),permission_snapshot:json!({})},
                parent_task_id:None,dependency_ids:vec![],budget:TaskBudget::default(),input_reference:None}).unwrap();
            let attempt=repository.start_attempt(&task.task_id,task.version,"fixture",60_000).unwrap();
            repository.append_event(&attempt.token,"segment","assistant_segment",&json!({"conversationId":chat,
                "requestId":request,"replyMessageId":reply,"eventSequence":1,"segment":{"id":"segment",
                    "round":1,"order":1,"phase":"commentary","status":"streaming","emptyAtEmission":true}})).unwrap();
            let mut expected=String::new();
            for index in 0..5 {
                let delta=if index==0 {"甲🏰".repeat(1024)} else {format!("第{index}页🏰")};
                expected.push_str(&delta);
                repository.append_event(&attempt.token,&format!("owned-{index}"),"text_delta",&json!({
                    "conversationId":chat,"requestId":request,"replyMessageId":reply,
                    "eventSequence":index+2,"segmentId":"segment","delta":delta})).unwrap();
            }
            repository.append_event(&attempt.token,"foreign","text_delta",&json!({"conversationId":chat,
                "requestId":request,"replyMessageId":"another-reply","eventSequence":7,"delta":"FOREIGN"})).unwrap();
            let first=repository.stream_projection_page(&task.task_id,&request,&reply,0,None,2,1).unwrap();
            assert_eq!(first["events"].as_array().unwrap().len(),1);
            assert!(first["pageBytes"].as_u64().unwrap()>first["maximumPageBytes"].as_u64().unwrap());
            assert!(first["pageBytes"].as_u64().unwrap()<=first["indivisibleEventBytes"].as_u64().unwrap());
            let horizon=first["throughTaskSequence"].as_u64().unwrap();
            repository.append_event(&attempt.token,"later","text_delta",&json!({"conversationId":chat,
                "requestId":request,"replyMessageId":reply,"eventSequence":8,"segmentId":"segment","delta":"LATER"})).unwrap();
            let mut page=first;let mut actual=String::new();let mut count=0;
            loop {
                for event in page["events"].as_array().unwrap() {
                    if let Some(delta)=event["payload"]["delta"].as_str() {actual.push_str(delta);count+=1;}
                }
                if page["hasMore"]!=true {break;}
                let after=page["nextTaskSequence"].as_u64().unwrap();
                let next=repository.stream_projection_page(&task.task_id,&request,&reply,after,Some(horizon),2,1).unwrap();
                assert!(next["nextTaskSequence"].as_u64().unwrap()>after);
                let repeated=repository.stream_projection_page(&task.task_id,&request,&reply,after,Some(horizon),2,1).unwrap();
                assert_eq!(next,repeated,"a committed page has stable identity for retry before saving its watermark");
                page=next;
            }
            assert_eq!(actual,expected);assert_eq!(count,5);
            assert_eq!(page["nextTaskSequence"],horizon);
            let baseline=json!({"content":"","reasoning":"","segments":[]});
            let recovered=repository.recover_stream_with_limits(&task.task_id,&request,&reply,0,baseline.clone(),2,1,||false).unwrap();
            assert_eq!(recovered["complete"],true,"{recovered}");
            assert_eq!(recovered["content"],expected+"LATER");
            assert!(recovered["pageCount"].as_u64().unwrap()>2);
            let mut checks=0;
            let cancelled=repository.recover_stream_with_limits(&task.task_id,&request,&reply,0,baseline,2,1,||{checks+=1;checks>1}).unwrap();
            assert_eq!(cancelled["cancelled"],true);
            assert!(cancelled.get("content").is_none(),"cancelled shadows cannot advance the formal message or watermark");
            assert!(repository.stream_projection_page(&task.task_id,&uuid::Uuid::new_v4().to_string(),&reply,0,None,2,1).is_err());
        }));
        let _=super::sqlite_connections::close(&root,&root.join("Agent/tasks.sqlite"));
        let _=std::fs::remove_dir_all(&root);
        if let Err(error)=result {std::panic::resume_unwind(error);}
    }

}

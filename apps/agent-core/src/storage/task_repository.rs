//! Transactional task tree and recovery references owned by the Rust gateway.
//! Rust 网关拥有事务化任务树；执行者更换不改变任务身份，派发未知不能被续接当作未执行。

#[path = "task_repository_cleanup.rs"]
mod cleanup;
#[path = "task_repository_contracts.rs"]
mod contracts;
#[path = "task_repository_schema.rs"]
mod schema;
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
    serde_json::from_str(&raw).map_err(|_| failure())
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
fn tree_has_unknown_mutation(connection: &Connection, task: &TaskRecord) -> Result<bool> {
    let lineage = ancestors(connection, task)?;
    let root = lineage.last().ok_or_else(failure)?;
    connection.query_row("WITH RECURSIVE tree(id) AS (SELECT ? UNION ALL SELECT t.task_id FROM tasks t JOIN tree p ON t.parent_task_id=p.id) SELECT EXISTS(SELECT 1 FROM task_operations o JOIN tree t ON o.task_id=t.id WHERE o.state='unknown' AND o.effect<>'observation')",[&root.task_id],|row|row.get(0)).map_err(|_|failure())
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
    connection.execute("INSERT INTO task_events(task_id,sequence,event_id,fingerprint,document) VALUES(?,?,?,?,?)",params![task.task_id,event.sequence,event_id,digest,raw(&event)?]).map_err(|_| failure())?;
    save_task(connection, task)?;
    Ok(event)
}

/// The caller supplies GatewayConfig.data_root; no personal path or second tool/body store is created.
/// 调用者传入 GatewayConfig.data_root；不写死个人路径，不建立第二套工具正文或向量账本。
#[derive(Clone)]
pub struct TaskRepository {
    root: PathBuf,
    path: PathBuf,
}

impl TaskRepository {
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
        if version > 1 {
            return Err(ContractError::new(
                "Task database uses a newer version. / 任务库来自更新版本，原库已保留。",
                "UNSUPPORTED_TASK_STORE_VERSION",
                409,
            ));
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
    /// Read at most two paused roots in this exact chat/work scope; two is sufficient to detect ambiguity.
    /// 精确查询此聊天及工作内最多两个暂停根任务；两个即可识别歧义，不扫描或分页全部任务。
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
            let mut task=TaskRecord { task_id:uuid::Uuid::new_v4().to_string(),request_id:input.request_id.clone(),goal:input.goal.clone(),acceptance:input.acceptance.clone(),scope:input.scope.clone(),parent_task_id:input.parent_task_id.clone(),dependency_ids:input.dependency_ids.clone(),budget:input.budget.clone(),usage:TaskUsage::default(),input_reference:input.input_reference.clone(),state:TaskState::Queued,version:1,execution_version:0,current_attempt_id:None,event_sequence:0,stop_reason:None,paused_by_task_id:None,created_at_ms:timestamp,updated_at_ms:timestamp };
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

    /// Expired ownership fences internal writes, not external effects; takeover preserves uncertainty.
    /// 执行权过期仅围栏内部写入，不能证明外部副作用停止；接管必须保留结果未知。
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
                && (read_operations(connection,task_id)?.iter().any(|operation|matches!(operation.state,OperationState::Dispatched|OperationState::Unknown))
                    || read_reservations(connection,task_id)?.iter().any(|reservation|matches!(reservation.state,ModelReservationState::Reserved|ModelReservationState::Unknown)));
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
            let attempt=TaskAttempt {token:token.clone(),executor_id:executor_id.to_owned(),lease_expires_at_ms:timestamp.saturating_add(lease_ms as i64),started_at_ms:timestamp,ended_at_ms:None,stop_reason:None};
            task.current_attempt_id=Some(token.attempt_id.clone());
            task.state=if read_operations(connection,task_id)?.iter().any(|operation|operation.state==OperationState::Unknown && operation.intent.effect!=OperationEffect::Observation)
                || read_reservations(connection,task_id)?.iter().any(|reservation|reservation.state==ModelReservationState::Unknown) {TaskState::WaitingVerification} else {TaskState::Running};
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
            if state==TaskState::Completed {
                if read_operations(connection,&task.task_id)?.iter().any(|operation|matches!(operation.state,OperationState::Dispatched|OperationState::Unknown)) {return Err(blocked("Unknown operations prevent verified completion. / 结果未知的操作阻止核验完成。"));}
                if tree_has_unknown_mutation(connection,&task)? { return Err(blocked("Unknown delegated effects still require verification after a plan update. / 更新方案后，子任务未知副作用仍须核验。")); }
                let incomplete:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM tasks WHERE parent_task_id=? AND state<>'completed' AND NOT(state='cancelled' AND json_extract(document,'$.stopReason')='superseded-by-user-steering'))",[&task.task_id],|row|row.get(0)).map_err(|_|failure())?;
                if incomplete {return Err(blocked("Child tasks remain incomplete; use a partial outcome. / 子任务未全部完成，应记录部分结果。"));}
            }
            task.state=state;task.version+=1;task.stop_reason=reason.map(str::to_owned);
            let payload=json!({"state":task.state,"reason":task.stop_reason});
            append(connection,&mut task,&uuid::Uuid::new_v4().to_string(),"task.state",&payload)?;Ok(task)
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
            if task.state.is_terminal() {return if control==TaskControl::Cancel && task.state==TaskState::Cancelled {Ok(task)} else {Err(blocked("Ended tasks cannot be resumed. / 已结束的任务不能继续。"))};}
            if control==TaskControl::Resume && !matches!(task.state,TaskState::Paused|TaskState::WaitingNetwork|TaskState::WaitingConfig|TaskState::WaitingApproval|TaskState::WaitingVerification|TaskState::WaitingChildren) {return Err(blocked("Task is not waiting or paused. / 任务并非等待或暂停状态。"));}
            if control==TaskControl::Resume && ancestors(connection,&task)?.iter().skip(1).any(|ancestor|ancestor.state.is_terminal() || ancestor.state==TaskState::Paused) {return Err(blocked("The parent remains paused or ended. / 父任务仍然暂停或已经结束。"));}
            let mut ids=vec![task_id.to_owned()];
            {
                let mut query=connection.prepare_cached("WITH RECURSIVE tree(id) AS (SELECT task_id FROM tasks WHERE parent_task_id=? UNION ALL SELECT t.task_id FROM tasks t JOIN tree p ON t.parent_task_id=p.id) SELECT id FROM tree").map_err(|_|failure())?;
                ids.extend(query.query_map([task_id],|row|row.get::<_,String>(0)).map_err(|_|failure())?.collect::<std::result::Result<Vec<_>,_>>().map_err(|_|failure())?);
            }
            for id in ids {
                let mut child=row_task(connection,&id)?;
                if child.state.is_terminal() {continue;}
                if control==TaskControl::Resume && child.task_id!=task_id && (child.state!=TaskState::Paused || child.paused_by_task_id.as_deref()!=Some(task_id)) {continue;}
                if control==TaskControl::Pause && child.state==TaskState::Paused && child.task_id!=task_id {continue;}
                child.state=match control {TaskControl::Pause=>TaskState::Paused,TaskControl::Resume=>TaskState::Queued,TaskControl::Cancel=>TaskState::Cancelled};
                child.paused_by_task_id=if control==TaskControl::Pause {Some(task_id.to_owned())} else {None};
                child.version+=1;child.stop_reason=Some(match control{TaskControl::Pause=>"user-paused",TaskControl::Resume=>"user-resumed",TaskControl::Cancel=>"user-cancelled"}.into());
                let payload=json!({"state":child.state,"reason":child.stop_reason});
                append(connection,&mut child,&event_id,"task.control",&payload)?;
            }
            row_task(connection,task_id)
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
    /// User steering references formal messages; it never grants new tool scope or replaces the original goal.
    /// 用户插话引用正式消息，不授予新工具范围，也不替换原始目标；先暂停再以版本校验登记。
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
            if task.version != expected_version { return Err(conflict("Task changed before steering. / 保存插话前任务已变化。")); }
            if task.parent_task_id.is_some() || task.state != TaskState::Paused { return Err(blocked("Steering requires a paused root task. / 插话须暂停根任务。")); }
            task.version += 1;
            task.stop_reason = Some(event_id.clone());
            append(connection, &mut task, &event_id, "task.user-steering", &payload)?;
            if action == "update" {
                // A changed plan cannot silently restart old delegates; their effects and receipts remain recorded.
                // 更新方案不静默重启旧委派；旧操作与回执仍保留，未知副作用继续阻止未经核验的修改。
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
    pub fn operations(&self, task_id: &str) -> Result<Vec<TaskOperation>> {
        let connection = sqlite_connections::acquire(&self.root, &self.path, 30_000)?;
        read_operations(&connection, task_id)
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
            let mut task=verify_fence(connection,token,false)?;
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
            let mut task=verify_fence(connection,token,false)?;
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
    /// Meter actual attempts exactly once and include descendant failures, retries and compactions.
    /// 真实调用按计量 ID 只结算一次；后代失败、重试、压缩纳入祖先总账，未知 token 单独计数。
    pub fn record_usage(
        &self,
        token: &AttemptToken,
        usage_id: &str,
        delta: &TaskUsage,
    ) -> Result<TaskRecord> {
        identifier(usage_id)?;
        if delta.failed_requests > delta.model_requests
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
    /// Admission counts both paid/dispatched attempts and outstanding descendant reservations.
    /// 准入同时核对实际派发次数与后代未兑现预约；重启或拆分不能重置父预算。
    pub fn reserve_model_request(
        &self,
        token: &AttemptToken,
        reservation_id: &str,
    ) -> Result<ModelRequestReservation> {
        identifier(reservation_id)?;
        self.write(|connection| {
            let task=verify_fence(connection,token,false)?;
            if let Some(existing)=find_reservation(connection,&token.task_id,reservation_id)? {return Ok(existing);}
            for ancestor in ancestors(connection,&task)? {
                if ancestor.state.is_terminal() || ancestor.state==TaskState::Paused {return Err(blocked("Parent task is controlled or ended. / 父任务已暂停、取消或结束。"));}
                let pending=pending_reservations(connection,&ancestor.task_id)?;
                if ancestor.usage.is_exhausted(&ancestor.budget) || ancestor.usage.model_requests.saturating_add(pending)>=ancestor.budget.max_model_requests {return Err(ContractError::new("Task-tree request budget is exhausted or reserved. / 任务树请求预算已耗尽或已预约。","TASK_BUDGET_EXHAUSTED",409));}
            }
            let timestamp=now_ms();
            let reservation=ModelRequestReservation {task_id:token.task_id.clone(),reservation_id:reservation_id.to_owned(),attempt_id:token.attempt_id.clone(),state:ModelReservationState::Reserved,dispatched:None,usage:None,verification_reference:None,created_at_ms:timestamp,updated_at_ms:timestamp};
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
        self.settle_reservation(token, reservation_id, dispatched, usage, None)
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
            Some(verification_reference),
        )
    }
    fn settle_reservation(
        &self,
        token: &AttemptToken,
        reservation_id: &str,
        dispatched: bool,
        usage: &TaskUsage,
        verification_reference: Option<Value>,
    ) -> Result<ModelRequestReservation> {
        identifier(reservation_id)?;
        if usage.model_requests > 1
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
        let digest = fingerprint(&json!([dispatched, delta]))?;
        self.write(|connection| {
            let task=verify_fence(connection,token,true)?;
            let mut reservation=find_reservation(connection,&token.task_id,reservation_id)?.ok_or_else(||invalid("Model request was not reserved. / 模型请求尚未预约。"))?;
            let old:Option<String>=connection.query_row("SELECT settlement_fingerprint FROM task_model_reservations WHERE task_id=? AND reservation_id=?",params![token.task_id,reservation_id],|row|row.get(0)).map_err(|_|failure())?;
            if let Some(old)=old {return if old==digest {Ok(reservation)} else {Err(conflict("Model settlement identity was reused. / 模型结算身份不能复用于不同结果。"))};}
            if reservation.state==ModelReservationState::Unknown && !dispatched && verification_reference.is_none() {return Err(blocked("Unknown dispatch cannot be released without verification. / 未知派发未核验不能释放预约。"));}
            if reservation.state==ModelReservationState::Reserved && reservation.attempt_id!=token.attempt_id {return Err(stale());}
            if dispatched {for mut record in ancestors(connection,&task)? {record.usage.add(&delta);save_task(connection,&mut record)?;}}
            reservation.state=if dispatched {ModelReservationState::Settled} else {ModelReservationState::Released};
            reservation.dispatched=Some(dispatched);reservation.usage=Some(delta);reservation.verification_reference=verification_reference;reservation.updated_at_ms=now_ms();
            connection.execute("UPDATE task_model_reservations SET state=?,settlement_fingerprint=?,document=? WHERE task_id=? AND reservation_id=?",params![state_text(&reservation.state),digest,raw(&reservation)?,token.task_id,reservation_id]).map_err(|_|failure())?;
            Ok(reservation)
        })
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

//! Formal deletion records exact owned paths before task rows disappear; failed cleanup is retryable.
//! 正式删除在任务行消失前保存精确托管路径；清理失败保留计划，绝不递归删除工作目录。

use super::*;
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskCleanupFile {
    pub relative_path: String,
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskCleanupPlan {
    pub cleanup_id: String,
    pub conversation_id: String,
    pub task_ids: Vec<String>,
    pub files: Vec<TaskCleanupFile>,
    pub created_at_ms: i64,
    pub last_error: Option<String>,
}

fn selected_tasks(connection: &Connection, conversation_id: &str) -> Result<Vec<TaskRecord>> {
    let mut query=connection.prepare_cached("WITH RECURSIVE owned(id) AS (SELECT task_id FROM tasks WHERE conversation_id=? UNION SELECT t.task_id FROM tasks t JOIN owned p ON t.parent_task_id=p.id) SELECT document FROM tasks WHERE task_id IN owned ORDER BY task_id").map_err(|_|failure())?;
    let rows = query
        .query_map([conversation_id], |row| row.get::<_, String>(0))
        .map_err(|_| failure())?;
    rows.map(|row| decode(row.map_err(|_| failure())?))
        .collect()
}

fn affected_tasks(connection: &Connection, conversation_id: &str) -> Result<Vec<TaskRecord>> {
    let mut query=connection.prepare_cached("WITH RECURSIVE affected(id) AS (SELECT task_id FROM tasks WHERE conversation_id=? UNION SELECT t.task_id FROM tasks t JOIN affected p ON t.parent_task_id=p.id UNION SELECT d.task_id FROM task_dependencies d JOIN affected p ON d.dependency_id=p.id) SELECT document FROM tasks WHERE task_id IN affected ORDER BY task_id").map_err(|_|failure())?;
    let rows = query
        .query_map([conversation_id], |row| row.get::<_, String>(0))
        .map_err(|_| failure())?;
    rows.map(|row| decode(row.map_err(|_| failure())?))
        .collect()
}

fn conversation_identifier(value: &str) -> Result<()> {
    uuid::Uuid::parse_str(value).map_err(|_| {
        invalid("Cleanup requires a formal conversation ID. / 清理须指定正式聊天 ID。")
    })?;
    Ok(())
}

fn planned_file(reference: &Value, expected_path: &str) -> Result<TaskCleanupFile> {
    if reference["relativePath"].as_str() != Some(expected_path) {
        return Err(invalid(
            "Task-owned cleanup path is inconsistent. / 任务自己的清理路径不一致。",
        ));
    }
    let hash = reference["sha256"]
        .as_str()
        .ok_or_else(|| invalid("Task-owned cleanup hash is missing. / 任务自己的清理哈希缺失。"))?;
    sha256(hash)?;
    let size = reference["bytes"]
        .as_u64()
        .filter(|size| *size <= 32 * 1024 * 1024)
        .ok_or_else(|| invalid("Task-owned cleanup size is invalid. / 任务自己的清理大小无效。"))?;
    Ok(TaskCleanupFile {
        relative_path: expected_path.into(),
        sha256: hash.into(),
        size_bytes: size,
    })
}

impl TaskRepository {
    /// Includes dependency consumers so deletion cannot leave a live task using a removed premise.
    /// 同时列出依赖使用者，避免删除后有效任务仍使用已经撤销的前置条件。
    pub fn cleanup_targets(&self, conversation_id: &str) -> Result<Vec<TaskRecord>> {
        conversation_identifier(conversation_id)?;
        let connection = sqlite_connections::acquire(&self.root, &self.path, 30_000)?;
        affected_tasks(&connection, conversation_id)
    }

    /// Caller persists cancellation and waits for real owned workers before this atomic metadata purge.
    /// 调用者先持久取消并等待真实所属 Worker，随后在一个事务中清理正式元数据。
    pub fn purge_conversation(&self, conversation_id: &str) -> Result<TaskCleanupPlan> {
        conversation_identifier(conversation_id)?;
        self.write(|connection| {
            let old:Option<String>=connection.query_row("SELECT document FROM task_cleanup_plans WHERE conversation_id=?",[conversation_id],|row|row.get(0)).optional().map_err(|_|failure())?;
            if let Some(old)=old {return decode(old);}
            let tasks=selected_tasks(connection,conversation_id)?;
            for task in affected_tasks(connection,conversation_id)? {
                if !task.state.is_terminal() {return Err(blocked("Cancel affected tasks before deleting their records. / 删除记录前须取消受影响任务。"));}
                if let Some(attempt)=task.current_attempt_id.as_deref() {
                    let live:bool=connection.query_row("SELECT ended=0 AND lease_expires_at_ms>? FROM task_attempts WHERE attempt_id=?",params![now_ms(),attempt],|row|row.get(0)).map_err(|_|failure())?;
                    if live {return Err(blocked("A live executor is still recording late facts. / 有效执行者仍在保存晚到事实。"));}
                }
            }
            let mut files=std::collections::BTreeMap::<String,TaskCleanupFile>::new();
            for task in &tasks {
                if let Some(reference)=task.input_reference.as_ref().filter(|reference|reference["kind"]=="task-input") {
                    let relative=reference["relativePath"].as_str().ok_or_else(failure)?;
                    let suffix=relative.strip_prefix("Agent/task-inputs/").filter(|suffix|suffix.ends_with(".json") && !suffix.contains('/') && !suffix.contains('\\') && !suffix.contains(".."))
                        .ok_or_else(||invalid("Task input path is outside its owned directory. / 任务输入路径超出自己的目录。"))?;
                    if suffix.chars().any(|character|!(character.is_ascii_alphanumeric() || matches!(character,'-'|'.'))) {return Err(invalid("Task input path is invalid. / 任务输入路径无效。"));}
                    files.insert(relative.into(),planned_file(reference,relative)?);
                }
                let mut query=connection.prepare_cached("SELECT document FROM task_artifacts WHERE task_id=?").map_err(|_|failure())?;
                let artifacts:Vec<TaskArtifact>=query.query_map([&task.task_id],|row|row.get::<_,String>(0)).map_err(|_|failure())?.map(|row|decode(row.map_err(|_|failure())?)).collect::<Result<_>>()?;
                for artifact in artifacts {
                    if artifact.reference["kind"]!="task-blob" {continue;}
                    if artifact.artifact_id.chars().any(|character|!(character.is_ascii_alphanumeric() || character=='-')) {return Err(invalid("Task artifact identity is invalid. / 任务产物身份无效。"));}
                    let relative=format!("Agent/task-artifacts/{}/{}.json",task.task_id,artifact.artifact_id);
                    files.insert(relative.clone(),planned_file(&artifact.reference,&relative)?);
                }
            }
            let plan=TaskCleanupPlan {cleanup_id:uuid::Uuid::new_v4().to_string(),conversation_id:conversation_id.into(),task_ids:tasks.iter().map(|task|task.task_id.clone()).collect(),files:files.into_values().collect(),created_at_ms:now_ms(),last_error:None};
            // Cleanup plans may span many small artifacts; they are not model context or event payloads.
            // 清理计划可以覆盖多个小产物；它不是模型上下文或事件正文，不套用 64KiB 事件上限。
            let document=serde_json::to_string(&plan).map_err(|_|failure())?;
            connection.execute("INSERT INTO task_cleanup_plans(cleanup_id,conversation_id,document) VALUES(?,?,?)",params![plan.cleanup_id,conversation_id,document]).map_err(|_|failure())?;
            connection.execute_batch("CREATE TEMP TABLE IF NOT EXISTS task_cleanup_ids(task_id TEXT PRIMARY KEY); DELETE FROM task_cleanup_ids;").map_err(|_|failure())?;
            {let mut insert=connection.prepare_cached("INSERT INTO task_cleanup_ids(task_id) VALUES(?)").map_err(|_|failure())?;for task in &tasks {insert.execute([&task.task_id]).map_err(|_|failure())?;}}
            connection.execute("DELETE FROM task_dependencies WHERE dependency_id IN (SELECT task_id FROM task_cleanup_ids)",[]).map_err(|_|failure())?;
            connection.execute("DELETE FROM tasks WHERE task_id IN (SELECT task_id FROM task_cleanup_ids)",[]).map_err(|_|failure())?;
            connection.execute("DELETE FROM task_cleanup_ids",[]).map_err(|_|failure())?;
            Ok(plan)
        })
    }

    pub fn pending_cleanups(&self, limit: usize) -> Result<Vec<TaskCleanupPlan>> {
        let connection = sqlite_connections::acquire(&self.root, &self.path, 30_000)?;
        let mut query = connection
            .prepare_cached("SELECT document FROM task_cleanup_plans ORDER BY cleanup_id LIMIT ?")
            .map_err(|_| failure())?;
        let rows = query
            .query_map([limit.clamp(1, 512) as i64], |row| row.get::<_, String>(0))
            .map_err(|_| failure())?;
        rows.map(|row| decode(row.map_err(|_| failure())?))
            .collect()
    }

    /// Exact managed files only. Missing files are idempotent; changed/link paths keep the durable plan.
    /// 只删除精确托管文件；文件已不存在视为幂等成功，内容变化或链接路径保留持久清理计划。
    pub fn retry_cleanup_files(&self, cleanup_id: &str) -> Result<TaskCleanupPlan> {
        identifier(cleanup_id)?;
        let plan = {
            let connection = sqlite_connections::acquire(&self.root, &self.path, 30_000)?;
            let document: String = connection
                .query_row(
                    "SELECT document FROM task_cleanup_plans WHERE cleanup_id=?",
                    [cleanup_id],
                    |row| row.get(0),
                )
                .map_err(|_| failure())?;
            decode::<TaskCleanupPlan>(document)?
        };
        for file in &plan.files {
            let cleaned = (|| -> Result<()> {
                if !(file.relative_path.starts_with("Agent/task-artifacts/")
                    || file.relative_path.starts_with("Agent/task-inputs/"))
                    || file.relative_path.contains("..")
                    || file.relative_path.contains('\\')
                {
                    return Err(invalid(
                        "Cleanup path escaped managed task storage. / 清理路径超出托管任务存储。",
                    ));
                }
                let path = self.root.join(&file.relative_path);
                let Some(bytes) = managed_files::read(&self.root, &path, 32 * 1024 * 1024)? else {
                    return Ok(());
                };
                if bytes.len() as u64 != file.size_bytes
                    || managed_files::digest(&bytes) != file.sha256
                {
                    return Err(conflict(
                        "Task file changed before cleanup; the original is preserved. / 任务文件清理前已变化，原文件已保留。",
                    ));
                }
                managed_files::inspect(&self.root, &path, false, true, 32 * 1024 * 1024)?;
                std::fs::remove_file(path).map_err(|_| failure())?;
                Ok(())
            })();
            if let Err(error) = cleaned {
                self.write(|connection| {
                    let mut failed = plan.clone();
                    failed.last_error = Some(error.code().to_owned());
                    connection
                        .execute(
                            "UPDATE task_cleanup_plans SET document=? WHERE cleanup_id=?",
                            params![
                                serde_json::to_string(&failed).map_err(|_| failure())?,
                                cleanup_id
                            ],
                        )
                        .map_err(|_| failure())?;
                    Ok(())
                })?;
                return Err(error);
            }
        }
        self.write(|connection| {
            connection
                .execute(
                    "DELETE FROM task_cleanup_plans WHERE cleanup_id=?",
                    [cleanup_id],
                )
                .map_err(|_| failure())?;
            Ok(())
        })?;
        Ok(plan)
    }
}

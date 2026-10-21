//! One relational task store; original messages and tool bodies remain in their existing owners.
//! 唯一任务关系库仅保存状态和引用；消息原文及工具正文仍由现有正式存储拥有。

pub(super) const SCHEMA: &str = r#"
PRAGMA foreign_keys=ON;
PRAGMA journal_mode=WAL;
PRAGMA synchronous=FULL;
BEGIN IMMEDIATE;
CREATE TABLE IF NOT EXISTS tasks (
 task_id TEXT NOT NULL PRIMARY KEY,
 request_key TEXT NOT NULL UNIQUE,
 fingerprint TEXT NOT NULL,
 parent_task_id TEXT REFERENCES tasks(task_id),
 project_id TEXT,
 conversation_id TEXT NOT NULL,
 permission_hash TEXT NOT NULL,
 state TEXT NOT NULL,
 version INTEGER NOT NULL,
 execution_version INTEGER NOT NULL,
 current_attempt_id TEXT,
 event_sequence INTEGER NOT NULL,
 document TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS tasks_unfinished ON tasks(state,task_id);
CREATE INDEX IF NOT EXISTS tasks_parent ON tasks(parent_task_id,task_id);
CREATE INDEX IF NOT EXISTS tasks_conversation ON tasks(conversation_id,task_id);
CREATE INDEX IF NOT EXISTS tasks_project ON tasks(project_id,task_id);
CREATE INDEX IF NOT EXISTS tasks_paused_roots ON tasks(conversation_id,project_id,task_id)
 WHERE parent_task_id IS NULL AND state='paused';
CREATE TABLE IF NOT EXISTS task_dependencies (
 task_id TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE CASCADE,
 dependency_id TEXT NOT NULL REFERENCES tasks(task_id),
 PRIMARY KEY(task_id,dependency_id),
 CHECK(task_id<>dependency_id)
);
CREATE TABLE IF NOT EXISTS task_attempts (
 attempt_id TEXT NOT NULL PRIMARY KEY,
 task_id TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE CASCADE,
 execution_version INTEGER NOT NULL,
 ended INTEGER NOT NULL,
 lease_expires_at_ms INTEGER NOT NULL,
 document TEXT NOT NULL,
 UNIQUE(task_id,execution_version),
 UNIQUE(task_id,attempt_id)
);
CREATE TABLE IF NOT EXISTS task_events (
 task_id TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE CASCADE,
 sequence INTEGER NOT NULL,
 event_id TEXT NOT NULL,
 fingerprint TEXT NOT NULL,
 document TEXT NOT NULL,
 PRIMARY KEY(task_id,sequence),
 UNIQUE(task_id,event_id)
);
CREATE TABLE IF NOT EXISTS task_operations (
 task_id TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE CASCADE,
 operation_id TEXT NOT NULL,
 state TEXT NOT NULL,
 effect TEXT NOT NULL,
 attempt_id TEXT NOT NULL,
 fingerprint TEXT NOT NULL,
 document TEXT NOT NULL,
 PRIMARY KEY(task_id,operation_id),
 FOREIGN KEY(task_id,attempt_id) REFERENCES task_attempts(task_id,attempt_id)
);
CREATE INDEX IF NOT EXISTS task_events_kind ON task_events(task_id,json_extract(document,'$.kind'),sequence);
CREATE INDEX IF NOT EXISTS task_child_reviews ON task_events(task_id,json_extract(document,'$.payload.childTaskId'),sequence)
 WHERE json_extract(document,'$.kind')='task.child.review';
CREATE INDEX IF NOT EXISTS task_operations_unsettled ON task_operations(task_id,state,effect);
CREATE TABLE IF NOT EXISTS task_checkpoints (
 task_id TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE CASCADE,
 checkpoint_id TEXT NOT NULL,
 event_sequence INTEGER NOT NULL,
 fingerprint TEXT NOT NULL,
 document TEXT NOT NULL,
 PRIMARY KEY(task_id,checkpoint_id)
);
CREATE INDEX IF NOT EXISTS task_checkpoints_latest ON task_checkpoints(task_id,event_sequence DESC);
CREATE TABLE IF NOT EXISTS task_artifacts (
 task_id TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE CASCADE,
 artifact_id TEXT NOT NULL,
 fingerprint TEXT NOT NULL,
 document TEXT NOT NULL,
 PRIMARY KEY(task_id,artifact_id)
);
CREATE TABLE IF NOT EXISTS task_usage (
 task_id TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE CASCADE,
 usage_id TEXT NOT NULL,
 fingerprint TEXT NOT NULL,
 PRIMARY KEY(task_id,usage_id)
);
CREATE TABLE IF NOT EXISTS task_model_reservations (
 task_id TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE CASCADE,
 reservation_id TEXT NOT NULL,
 attempt_id TEXT NOT NULL,
 state TEXT NOT NULL,
 settlement_fingerprint TEXT,
 document TEXT NOT NULL,
 PRIMARY KEY(task_id,reservation_id),
 FOREIGN KEY(task_id,attempt_id) REFERENCES task_attempts(task_id,attempt_id)
);
CREATE INDEX IF NOT EXISTS task_model_reservations_pending ON task_model_reservations(task_id,state);
CREATE TABLE IF NOT EXISTS task_cleanup_plans (
 cleanup_id TEXT NOT NULL PRIMARY KEY,
 conversation_id TEXT NOT NULL UNIQUE,
 document TEXT NOT NULL
);
PRAGMA user_version=2;
COMMIT;
"#;

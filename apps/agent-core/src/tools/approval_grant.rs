//! Task authorization matches trusted execution bindings; it never dispatches or replaces policy checks.
//! 任务授权只匹配可信执行绑定，不派发工具，也不替代现有策略检查。

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ApprovalScope {
    #[default]
    Once,
    Task,
}

impl ApprovalScope {
    pub(crate) fn parse(value: Option<&str>) -> Result<Self, &'static str> {
        match value.unwrap_or("once") {
            "once" => Ok(Self::Once),
            "task" => Ok(Self::Task),
            _ => Err("审批范围必须是 once 或 task。"),
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Once => "once",
            Self::Task => "task",
        }
    }
}

fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(fields) => {
            let mut keys = fields.keys().collect::<Vec<_>>();
            keys.sort();
            Value::Object(
                keys.into_iter()
                    .map(|key| (key.clone(), canonical(&fields[key])))
                    .collect(),
            )
        }
        Value::Array(values) => Value::Array(values.iter().map(canonical).collect()),
        _ => value.clone(),
    }
}

/// Only native UTF-8 overwrite may vary the body; every write still checks expectedHash.
/// 仅内置 UTF-8 覆盖可变更正文；每次写入仍须核验 expectedHash。
pub(crate) fn fingerprint(
    name: &str,
    arguments: &Value,
    target: Option<&str>,
    scope: &Value,
    proof: &Value,
) -> String {
    let mut arguments = arguments.clone();
    if name == "filesystem.write" {
        if let (Some(target), Some(fields)) = (target, arguments.as_object_mut()) {
            fields.insert("path".into(), json!(target));
            for field in ["content", "expectedHash", "reason"] {
                fields.remove(field);
            }
        }
    }
    format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&canonical(&json!({
                "version": 1,
                "tool": name,
                "arguments": arguments,
                "scope": scope,
                "proof": proof
            })))
            .expect("JSON value serialization")
        )
    )
}

/// Keep bounded authorization in GatewayRuntime; restart requires approval again.
/// 授权仅有界保存在 GatewayRuntime 中，重启后须重新批准。
#[derive(Default)]
pub(crate) struct TaskApprovalGrants {
    task_id: Option<String>,
    identities: VecDeque<String>,
}

impl TaskApprovalGrants {
    pub(crate) fn contains(&self, task_id: &str, identity: &str) -> bool {
        self.task_id.as_deref() == Some(task_id)
            && self.identities.iter().any(|saved| saved == identity)
    }

    pub(crate) fn allow(&mut self, task_id: &str, identity: &str) {
        if self.task_id.as_deref() != Some(task_id) {
            self.clear();
            self.task_id = Some(task_id.into());
        }
        self.identities.retain(|saved| saved != identity);
        while self.identities.len() >= 256 {
            self.identities.pop_front();
        }
        self.identities.push_back(identity.into());
    }

    pub(crate) fn clear(&mut self) {
        self.task_id = None;
        self.identities.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_file_grants_allow_body_updates_but_preserve_target_options_permission_and_configuration() {
        let scope = json!({
            "permission": "ask",
            "configurationGeneration": 1,
            "root": "D:/work"
        });
        let first = json!({
            "path": "report.md",
            "content": "one",
            "expectedHash": null,
            "reason": "create"
        });
        let next = json!({
            "reason": "revise",
            "expectedHash": "current-live-hash",
            "content": "two",
            "path": "D:/work/report.md"
        });
        let key = fingerprint(
            "filesystem.write",
            &first,
            Some("D:/work/report.md"),
            &scope,
            &Value::Null,
        );
        assert_eq!(
            key,
            fingerprint(
                "filesystem.write",
                &next,
                Some("D:/work/report.md"),
                &scope,
                &Value::Null,
            )
        );
        assert_ne!(
            key,
            fingerprint(
                "filesystem.write",
                &next,
                Some("D:/work/other.md"),
                &scope,
                &Value::Null,
            )
        );
        let mut append = next.clone();
        append["append"] = json!(true);
        assert_ne!(
            key,
            fingerprint(
                "filesystem.write",
                &append,
                Some("D:/work/report.md"),
                &scope,
                &Value::Null,
            )
        );
        let mut changed = scope.clone();
        changed["permission"] = json!("smart");
        assert_ne!(
            key,
            fingerprint(
                "filesystem.write",
                &next,
                Some("D:/work/report.md"),
                &changed,
                &Value::Null,
            )
        );
        changed = scope;
        changed["configurationGeneration"] = json!(2);
        assert_ne!(
            key,
            fingerprint(
                "filesystem.write",
                &next,
                Some("D:/work/report.md"),
                &changed,
                &Value::Null,
            )
        );
    }

    #[test]
    fn opaque_calls_keep_exact_payload_and_json_key_order_does_not_change_authority() {
        for name in [
            "terminal.host.run",
            "mcp.blender.execute_code",
            "browser.evaluate",
        ] {
            let first = json!({"script": "first", "options": {"b": 2, "a": 1}});
            let same = json!({"options": {"a": 1, "b": 2}, "script": "first"});
            let changed = json!({"script": "second", "options": {"a": 1, "b": 2}});
            assert_eq!(
                fingerprint(name, &first, None, &Value::Null, &Value::Null),
                fingerprint(name, &same, None, &Value::Null, &Value::Null),
            );
            assert_ne!(
                fingerprint(name, &first, None, &Value::Null, &Value::Null),
                fingerprint(name, &changed, None, &Value::Null, &Value::Null),
            );
        }
        let mut grants = TaskApprovalGrants::default();
        grants.allow("parent-task", "binding");
        assert!(grants.contains("parent-task", "binding"));
        assert!(!grants.contains("child-task", "binding"));
        grants.clear();
        assert!(!grants.contains("parent-task", "binding"));
        assert_eq!(ApprovalScope::parse(None).unwrap(), ApprovalScope::Once);
        assert!(ApprovalScope::parse(Some("forever")).is_err());
    }
}

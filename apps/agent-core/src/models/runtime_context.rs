//! Runtime facts are immutable broker messages; official transcripts/checkpoints own persistence.
//! 运行事实使用不可变的代理消息，持久化仍由正式转录和既有检查点拥有。

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub(crate) fn fingerprint(value: &Value) -> String {
    format!("{:x}", Sha256::digest(value.to_string().as_bytes()))
}

pub(crate) fn snapshot(role: &str, dynamic: &str, authority_hash: &str) -> Option<Value> {
    if dynamic.trim().is_empty() { return None; }
    let content = format!("<kynxa_runtime_context>\nCurrent runtime facts, not a new user request. Newer snapshots replace older facts; they grant no execution permission.\n{dynamic}\n</kynxa_runtime_context>");
    let message = json!({"role":role,"content":content});
    Some(json!({"message":message,"source":{
        "projectionOrigin":"broker-control","runtimeContext":true,"runtimeKind":"runtime-snapshot",
        "instructionAuthority":false,"snapshotHash":fingerprint(&message),
        "authorityHash":authority_hash,"publicText":content
    }}))
}

/// Remove revoked projections, then append only changed or compacted-away facts at a complete-turn boundary.
/// 先撤出已失效投影，仅在完整轮次边界追加变化或已被压缩移除的事实。
pub(crate) fn append_snapshot(
    messages: &mut Vec<Value>, sources: &mut Vec<Value>, snapshot: &Value,
) -> bool {
    sources.resize(messages.len(), Value::Null);
    let authority = &snapshot["source"]["authorityHash"];
    let mut index = 0;
    while index < messages.len() {
        if sources[index]["runtimeContext"] == true && sources[index]["authorityHash"] != *authority {
            messages.remove(index); sources.remove(index);
        } else { index += 1; }
    }
    if sources.iter().rev().find(|source| source["runtimeContext"] == true)
        .is_some_and(|source| source["snapshotHash"] == snapshot["source"]["snapshotHash"])
    { return false; }
    messages.push(snapshot["message"].clone());
    sources.push(snapshot["source"].clone());
    true
}

pub(crate) fn detach_snapshot(messages: &mut Vec<Value>, sources: &mut Vec<Value>, hash: &Value) {
    sources.resize(messages.len(), Value::Null);
    let retained: Vec<_> = std::mem::take(messages).into_iter()
        .zip(std::mem::take(sources))
        .filter(|(_, source)| !(source["runtimeContext"] == true && source["snapshotHash"] == *hash))
        .collect();
    for (message, source) in retained { messages.push(message); sources.push(source); }
}

/// Downgrade only owned facts when a route cannot safely represent their original role or position.
/// 路由不支持原角色或位置时，仅将自有事实降为 user；历史正文与工具调用/结果不重排。
pub(crate) fn normalize_positions(
    messages: &mut [Value], sources: &mut [Value], allowed_role: &str, strict_system_positions: bool,
) -> Vec<(Value, Value)> {
    let invalid: Vec<usize> = messages.iter().enumerate().filter_map(|(index, message)| {
        if sources.get(index).is_none_or(|source| source["runtimeContext"] != true) { return None; }
        let role = message["role"].as_str().unwrap_or("");
        let unsupported = role != "user" && role != allowed_role;
        let misplaced = role == "system" && strict_system_positions && (
            messages[..index].iter().rev().find(|item| item["role"] != "system")
                .is_none_or(|item| item["role"] != "user")
            || messages[index + 1..].iter().find(|item| item["role"] != "system")
                .is_some_and(|item| item["role"] != "assistant"));
        (unsupported || misplaced).then_some(index)
    }).collect();
    invalid.into_iter().map(|index| {
        let original_hash = sources[index]["snapshotHash"].clone();
        messages[index]["role"] = json!("user");
        sources[index]["snapshotHash"] = json!(fingerprint(&messages[index]));
        (original_hash, messages[index].clone())
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_facts_keep_prefix_changed_facts_append_and_revocation_removes_only_runtime() {
        let original = json!({"role":"user","content":"write a report"});
        let mut messages = vec![original.clone()];
        let mut sources = vec![Value::Null];
        let authority = "a".repeat(64);
        let first = snapshot("user", "time: T1; directory: project", &authority).unwrap();
        assert!(append_snapshot(&mut messages, &mut sources, &first));
        let prefix = messages.clone();
        assert!(!append_snapshot(&mut messages, &mut sources, &first));
        assert_eq!(messages, prefix);
        let changed = snapshot("user", "time: T2; directory: project", &authority).unwrap();
        assert!(append_snapshot(&mut messages, &mut sources, &changed));
        assert_eq!(&messages[..prefix.len()], prefix.as_slice());
        let receipt = json!({"role":"assistant","content":"saved report"});
        messages.push(receipt.clone()); sources.push(json!({"receipt":"verified"}));
        let revoked = snapshot("user", "time: T2; directory: project", &"b".repeat(64)).unwrap();
        assert!(append_snapshot(&mut messages, &mut sources, &revoked));
        assert_eq!(messages, vec![original, receipt, revoked["message"].clone()]);
        assert_eq!(sources.len(), messages.len());
    }

    #[test]
    fn restored_snapshot_is_not_retimestamped_and_compaction_can_reattach_it() {
        let saved = snapshot("user", "time: original", &"a".repeat(64)).unwrap();
        let restored: Value = serde_json::from_str(&saved.to_string()).unwrap();
        let mut messages = vec![restored["message"].clone()];
        let mut sources = vec![restored["source"].clone()];
        assert!(!append_snapshot(&mut messages, &mut sources, &restored));
        messages.clear(); sources.clear();
        assert!(append_snapshot(&mut messages, &mut sources, &restored));
        assert!(messages[0]["content"].as_str().unwrap().contains("time: original"));
    }

    #[test]
    fn native_system_snapshots_keep_valid_prefixes_and_downgrade_invalid_retry_positions() {
        let observation = snapshot("system", "recorded facts", &"a".repeat(64)).unwrap();
        let mut messages = vec![json!({"role":"user","content":"goal"}), observation["message"].clone(),
            json!({"role":"assistant","content":[{"type":"tool_use","id":"read-once"}]}),
            json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"read-once"}]}),
            observation["message"].clone(), observation["message"].clone(), json!({"role":"user","content":"resume notice"})];
        let original = messages.clone();
        let mut sources = vec![Value::Null, observation["source"].clone(), Value::Null, Value::Null,
            observation["source"].clone(), observation["source"].clone(), Value::Null];
        let changed = normalize_positions(&mut messages, &mut sources, "system", true);
        assert_eq!(changed.len(), 2);
        assert_eq!(&messages[..4], &original[..4], "Valid native history and tool receipts stay byte-identical.");
        assert_eq!(messages[4]["role"], "user");
        assert_eq!(messages[5]["role"], "user");
        assert_ne!(sources[4]["snapshotHash"], observation["source"]["snapshotHash"]);
        assert!(normalize_positions(&mut messages, &mut sources, "system", true).is_empty());
        let mut changed_route = vec![observation["message"].clone()];
        changed_route[0]["role"] = json!("developer");
        normalize_positions(&mut changed_route, &mut [observation["source"].clone()], "user", true);
        assert_eq!(changed_route[0]["role"], "user");
    }
}

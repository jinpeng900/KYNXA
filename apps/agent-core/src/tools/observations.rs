use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const MAX_OBSERVATIONS: usize = 256;

/// The broker and the guard share the same canonical source identity calculations.
/// 代理与重复读取保护共用同一来源身份计算，避免接口投影与控制账本各自维护规则。
pub fn calculate(operation: &str, arguments: &Value) -> Result<Value, String> {
    match operation {
        "observations.fingerprint" => Ok(arguments
            .get("value")
            .and_then(fingerprint)
            .map_or(Value::Null, |value| json!(value))),
        "observations.source" => Ok(source_observation(
            &arguments["call"],
            &json!({"value": arguments["value"]}),
        )
        .map_or(
            Value::Null,
            |(key, hash)| json!({"observationKey": key, "observationHash": hash}),
        )),
        _ => Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
    }
}

/// Successful and failed observations have separate request-local progress histories.
/// 成功观察与失败读取使用独立的请求级历史；不共享跨请求缓存，也不授予执行权限。
#[derive(Default)]
pub struct ObservationGuard {
    observations: Vec<(String, String)>,
    stagnant_rounds: u64,
    previous_targets: Option<String>,
    failed_rounds: u64,
}

impl ObservationGuard {
    pub fn apply(&mut self, operation: &str, arguments: &Value) -> Result<Value, String> {
        match operation {
            "observations.success" => self.observe_success(arguments),
            "observations.failure" => self.observe_failure(arguments),
            "observations.state" => Ok(json!({"stagnantRounds": self.stagnant_rounds,
                "failedRounds": self.failed_rounds, "previousTargets": self.previous_targets,
                "observations": self.observations.iter().map(|(key, fingerprint)|
                    json!({"key": key, "fingerprint": fingerprint})).collect::<Vec<_>>() })),
            _ => Err("AGENT_CORE_OPERATION_UNKNOWN".into()),
        }
    }

    fn observe_success(&mut self, arguments: &Value) -> Result<Value, String> {
        let pairs = required_results(arguments)?;
        let mut repeated = true;
        let mut observed = false;
        let mut changed_state = false;
        let mut discovery_only = true;
        let mut incoming = Vec::new();
        for pair in pairs {
            let call = required_object(pair, "call")?;
            let result = required_object(pair, "result")?;
            if !pair["observation"].as_bool().unwrap_or(false)
                || is_truthy(&result["isError"])
                || is_truthy(&result["code"])
                || (is_truthy(&result["status"]) && result["status"] != "completed")
            {
                // Discovery and failed reads do not erase prior source observations.
                // 发现工具和失败读取不会抹掉已读来源；真实写入重置无进展轮次，仍保留版本指纹。
                changed_state |= !pair["observation"].as_bool().unwrap_or(false)
                    && !is_truthy(&result["isError"])
                    && !is_truthy(&result["code"])
                    && !matches!(call["name"].as_str(), Some("tool.search" | "tool.load"));
                continue;
            }
            observed = true;
            discovery_only &= matches!(call["name"].as_str(), Some("tool.search" | "tool.load"));
            // Broker-created source identity takes precedence over call explanations and changing archive IDs.
            // 优先使用代理生成的来源身份；调用理由和每次变化的归档 ID 不构成新信息。
            let source = source_observation(call, result);
            let key = result
                .get("observationKey")
                .filter(|value| !value.is_null())
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| source.as_ref().map(|(key, _)| key.clone()))
                .or_else(|| call_key(call));
            let fingerprint = result
                .get("observationHash")
                .filter(|value| !value.is_null())
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| source.as_ref().map(|(_, fingerprint)| fingerprint.clone()))
                .or_else(|| {
                    result
                        .get("resultRef")
                        .and_then(|reference| reference.get("sha256"))
                        .filter(|value| !value.is_null())
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .or_else(|| fingerprint(&result["content"]));
            match (&key, &fingerprint) {
                (Some(key), Some(fingerprint)) if !key.is_empty() && !fingerprint.is_empty() => {
                    if !self.observations.iter().any(|(previous_key, previous)| {
                        previous_key == key && previous == fingerprint
                    }) {
                        repeated = false;
                    }
                    set_observation(&mut incoming, key.clone(), fingerprint.clone());
                }
                _ => {
                    repeated = false;
                }
            }
        }
        for (key, fingerprint) in incoming {
            self.observations
                .retain(|(previous_key, _)| previous_key != &key);
            self.observations.push((key, fingerprint));
            if self.observations.len() > MAX_OBSERVATIONS {
                self.observations.remove(0);
            }
        }
        if !observed && !changed_state {
            return Ok(progress_result(false, 0));
        }
        if observed && discovery_only && !changed_state {
            return Ok(progress_result(repeated, 0));
        }
        repeated &= observed && !changed_state;
        self.stagnant_rounds = if repeated {
            self.stagnant_rounds.saturating_add(1)
        } else if observed || changed_state {
            0
        } else {
            self.stagnant_rounds
        };
        Ok(progress_result(repeated, self.stagnant_rounds))
    }

    fn observe_failure(&mut self, arguments: &Value) -> Result<Value, String> {
        let pairs = required_results(arguments)?;
        if pairs.is_empty() || pairs.iter().any(|pair| !is_observation_failure(pair)) {
            self.previous_targets = None;
            self.failed_rounds = 0;
            return Ok(progress_result(false, 0));
        }
        let mut targets = Vec::with_capacity(pairs.len());
        for pair in pairs {
            let call = required_object(pair, "call")?;
            let result = required_object(pair, "result")?;
            let name = call["name"]
                .as_str()
                .ok_or("INVALID_AGENT_CORE_ARGUMENTS")?;
            let input = call_input(call);
            let target = if is_desktop_observation(name) {
                json!([
                    input["windowId"],
                    input["processId"],
                    input["elementId"],
                    input["region"],
                    input["crop"]
                ])
            } else {
                json!([
                    result["browser"]["connectionId"],
                    result["browser"]["tabId"],
                    input["url"],
                    input["uid"],
                    input["ref"],
                    input["selector"]
                ])
            };
            targets.push(fingerprint(&json!([name, target])).unwrap_or_default());
        }
        targets.sort();
        let targets = targets.join(":");
        let repeated = self.previous_targets.as_deref() == Some(targets.as_str());
        self.failed_rounds = if repeated {
            self.failed_rounds.saturating_add(1)
        } else {
            1
        };
        self.previous_targets = Some(targets);
        Ok(progress_result(repeated, self.failed_rounds))
    }
}

fn required_results(arguments: &Value) -> Result<&Vec<Value>, String> {
    arguments
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| "INVALID_AGENT_CORE_ARGUMENTS".into())
}

fn required_object<'a>(arguments: &'a Value, name: &str) -> Result<&'a Value, String> {
    arguments
        .get(name)
        .filter(|value| value.is_object())
        .ok_or_else(|| "INVALID_AGENT_CORE_ARGUMENTS".into())
}

fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::String(value) => !value.is_empty(),
        Value::Number(value) => value.as_f64().unwrap_or(0.0) != 0.0,
        _ => true,
    }
}

fn progress_result(repeated: bool, rounds: u64) -> Value {
    // Successful observations warn after two unchanged rounds; failed reads count the first failure.
    // 成功读取在两轮无新增信息后告警；失败读取从第一次失败开始计数。
    json!({"repeated": repeated, "warning": rounds == 2, "finalize": rounds >= 3})
}

fn set_observation(items: &mut Vec<(String, String)>, key: String, fingerprint: String) {
    if let Some((_, previous)) = items
        .iter_mut()
        .find(|(previous_key, _)| previous_key == &key)
    {
        *previous = fingerprint;
    } else {
        items.push((key, fingerprint));
    }
}

fn canonical(value: &Value, depth: usize) -> Option<Value> {
    if depth > 64 {
        return None;
    }
    match value {
        Value::Array(items) => items
            .iter()
            .map(|item| canonical(item, depth + 1))
            .collect::<Option<Vec<_>>>()
            .map(Value::Array),
        Value::Object(items) => {
            let mut keys = items.keys().collect::<Vec<_>>();
            keys.sort();
            let mut sorted = serde_json::Map::new();
            for key in keys {
                sorted.insert(key.clone(), canonical(&items[key], depth + 1)?);
            }
            Some(Value::Object(sorted))
        }
        _ => Some(value.clone()),
    }
}

fn fingerprint(value: &Value) -> Option<String> {
    let bytes = serde_json::to_vec(&canonical(value, 0)?).ok()?;
    Some(format!("{:x}", Sha256::digest(bytes)))
}

fn call_input(call: &Value) -> &Value {
    if call["name"]
        .as_str()
        .is_some_and(|name| name.starts_with("mcp."))
    {
        &call["arguments"]["arguments"]
    } else {
        &call["arguments"]
    }
}

fn call_key(call: &Value) -> Option<String> {
    let name = call["name"].as_str()?;
    let mut input = call_input(call).clone();
    if matches!(
        name,
        "filesystem.read"
            | "document.read"
            | "knowledge.read"
            | "tool.result.read"
            | "conversation.history.read"
            | "memory.read"
    ) {
        // Intent descriptions never alter a known read's target; result fingerprints still detect version changes.
        // 说明参数不改变已知读取目标；实际结果指纹仍识别文件或来源版本变化。
        if let Some(fields) = input.as_object_mut() {
            fields.remove("reason");
            fields.remove("gap");
            fields.remove("policy");
        }
    }
    fingerprint(&json!([name, input]))
}

fn source_fields(item: &Value) -> Value {
    json!([
        item["scopeKey"],
        item["sourceId"],
        item["sourceRevision"],
        item["contentHash"],
        item["derivationSignature"],
        item["bindingRevision"],
        item.get("offset")
            .filter(|value| !value.is_null())
            .unwrap_or(&item["locator"]["startOffset"]),
        item.get("nextOffset")
            .filter(|value| !value.is_null())
            .unwrap_or(&item["locator"]["endOffset"]),
        item.get("text")
            .filter(|value| !value.is_null())
            .unwrap_or(&item["excerpt"])
    ])
}

fn source_observation(call: &Value, result: &Value) -> Option<(String, String)> {
    let parsed;
    let value = if let Some(value) = result.get("value").filter(|value| value.is_object()) {
        value
    } else if result["content"].is_object() {
        &result["content"]
    } else if let Some(content) = result["content"].as_str() {
        parsed = serde_json::from_str::<Value>(content).ok()?;
        if !parsed.is_object() {
            return None;
        }
        &parsed
    } else {
        result
    };
    let value = value
        .get("structuredContent")
        .filter(|value| value.is_object())
        .unwrap_or(value);
    let name = call["name"].as_str()?;
    let observation = match name {
        "tool.search" | "tool.load" => {
            let tools = value["tools"].as_array()?;
            // Capability identity ignores query scores and call IDs, but includes schemas and authority versions.
            // 能力身份不含查询分数及调用 ID，保留声明和权限版本变化。
            let mut definitions = tools.iter().map(|tool| json!({
                "name":tool["name"],"description":tool["description"],
                "inputSchema":tool["inputSchema"],"parameters":tool["parameters"],
                "annotations":tool["annotations"],"execution":tool["execution"],
                "version":tool["version"],"fingerprint":tool["fingerprint"],
                "identity":tool["identity"],"configurationGeneration":tool["configurationGeneration"]
            })).collect::<Vec<_>>();
            definitions.sort_by_cached_key(Value::to_string);
            json!([definitions, value["configurationGeneration"],value["mcpConfigurationGeneration"],
                value["permissionMode"],value["permissions"]])
        }
        "knowledge.read"
            if value["text"].is_string()
                && is_truthy(&value["sourceId"])
                && is_truthy(&value["contentHash"]) =>
        {
            source_fields(value)
        }
        "knowledge.search" => {
            let items = value["items"].as_array()?;
            if items.is_empty()
                || items.iter().any(|item| {
                    !is_truthy(&item["sourceId"])
                        || !is_truthy(&item["contentHash"])
                        || !item["excerpt"].is_string()
                })
            {
                return None;
            }
            let mut sources = items.iter().map(source_fields).collect::<Vec<_>>();
            sources.sort_by_cached_key(|item| serde_json::to_string(item).unwrap_or_default());
            Value::Array(sources)
        }
        "filesystem.read" | "document.read"
            if is_truthy(&value["path"])
                && is_truthy(&value["sha256"])
                && (value["text"].is_string() || value["content"].is_string()) =>
        {
            json!([
                value["path"],
                value["sha256"],
                value["offset"],
                value["nextOffset"],
                value["locator"]["extraction"]["pageWindow"],
                value.get("text").filter(|value| value.is_string()).unwrap_or(&value["content"])
            ])
        }
        "tool.result.read"
            if is_truthy(&value["resultRef"]["id"])
                && is_truthy(&value["resultRef"]["sha256"])
                && value["text"].is_string() =>
        {
            // Page identity belongs to the original immutable archive, never the new read receipt.
            // 页身份使用原始不可变归档与实际范围，新回执 ID 或调用说明不能伪装成新证据。
            json!([
                value["resultRef"]["id"],
                value["resultRef"]["sha256"],
                value["sourceTool"],
                value["offset"],
                value["nextOffset"],
                value["text"]
            ])
        }
        _ => return None,
    };
    let hash = fingerprint(&json!([name, observation]))?;
    Some((format!("{name}:{hash}"), hash))
}

fn is_desktop_observation(name: &str) -> bool {
    matches!(
        name,
        "computer.apps" | "computer.windows" | "computer.read" | "computer.screenshot"
    )
}

fn is_observation_failure(pair: &Value) -> bool {
    let call = &pair["call"];
    let result = &pair["result"];
    let Some(name) = call["name"].as_str() else {
        return false;
    };
    if pair.get("observationFailure").and_then(Value::as_bool) == Some(false)
        || !is_truthy(&result["isError"])
        || result["status"] == "unknown"
        || result["code"] == "TOOL_CANCELLED"
    {
        return false;
    }
    let code = result["code"].as_str().unwrap_or_default();
    if is_desktop_observation(name) {
        matches!(
            code,
            "DESKTOP_TIMEOUT"
                | "DESKTOP_TIMED_OUT"
                | "DESKTOP_READ_TIMEOUT"
                | "DESKTOP_NOT_RESPONDING"
                | "DESKTOP_WINDOW_UNAVAILABLE"
                | "DESKTOP_TARGET_CHANGED"
                | "DESKTOP_CAPTURE_FAILED"
        )
    } else {
        name.starts_with("mcp.")
            && result["browser"]["readOnly"] == true
            && matches!(
                code,
                "MCP_TIMEOUT" | "MCP_CONNECTION_CLOSED" | "MCP_CONNECTION_LOST" | "MCP_TOOL_FAILED"
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changing_read_explanations_does_not_bypass_progress_detection() {
        let mut guard = ObservationGuard::default();
        for round in 0..4 {
            let result = guard.apply("observations.success", &json!({"results": [{
                "call": {"name": "knowledge.read", "arguments": {"sourceRef": "source", "offset": 0,
                    "limit": 84, "gap": format!("description {round}")}},
                "result": {"content": "unchanged source", "status": "completed"}, "observation": true
            }]})).unwrap();
            assert_eq!(result["warning"], round == 2);
            assert_eq!(result["finalize"], round >= 3);
        }
    }

    #[test]
    fn discovery_identity_uses_schema_and_authority_instead_of_call_or_query() {
        let mut guard = ObservationGuard::default();
        let capability = |id: &str, version: u64| json!({"call":{"id":id,"name":"tool.search",
            "arguments":{"query":id}},"observation":true,"result":{"status":"completed",
            "content":{"configurationGeneration":version,"tools":[{"name":"filesystem.read",
                "score":id,"inputSchema":{"type":"object"}}]}}});
        assert_eq!(guard.apply("observations.success",&json!({"results":[capability("one",1)]})).unwrap()["repeated"],false);
        assert_eq!(guard.apply("observations.success",&json!({"results":[capability("two",1)]})).unwrap()["repeated"],true);
        assert_eq!(guard.apply("observations.success",&json!({"results":[capability("three",2)]})).unwrap()["repeated"],false);
    }

    #[test]
    fn source_changes_and_pagination_are_new_observations() {
        let mut guard = ObservationGuard::default();
        for (offset, hash) in [(0, "first"), (84, "first"), (84, "second")] {
            let result = guard.apply("observations.success", &json!({"results": [{
                "call": {"name": "filesystem.read", "arguments": {"path": "fixture", "offset": offset}},
                "result": {"content": serde_json::to_string(&json!({"path": "fixture", "sha256": hash,
                    "offset": offset, "nextOffset": offset + 84, "content": "same heading"})).unwrap()},
                "observation": true
            }]})).unwrap();
            assert_eq!(result["repeated"], false);
        }
    }

    #[test]
    fn archived_pages_use_original_identity_across_discovery_and_new_receipts() {
        let mut guard = ObservationGuard::default();
        for round in 0..4 {
            let discovery = json!({"results": [{"call": {"name": "tool.load"},
                "result": {"content": {"tools": []}, "status": "completed"}, "observation": true}]});
            assert_eq!(
                guard.apply("observations.success", &discovery).unwrap()["warning"],
                false
            );
            let results = ["first", "second", "third", "fourth"].map(|id| json!({
                "call": {"name": "tool.result.read", "arguments": {"id": id,
                    "limit": 20000 + round, "gap": format!("recover {round}")}},
                "result": {"status": "completed", "resultRef": {"id": format!("new-{round}-{id}"),
                    "sha256": format!("receipt-{round}")}, "content": {"sourceTool": "filesystem.read",
                    "resultRef": {"id": id, "sha256": "original-version"},
                    "offset": 0, "nextOffset": 84, "text": "same completed page"}}, "observation": true
            }));
            let result = guard
                .apply("observations.success", &json!({"results": results}))
                .unwrap();
            assert_eq!(result["repeated"], round > 0);
            assert_eq!(result["warning"], round == 2);
            assert_eq!(result["finalize"], round == 3);
        }
        let next = json!({"results": [{"call": {"name": "tool.result.read"},
            "result": {"content": {"sourceTool": "filesystem.read", "resultRef": {
                "id": "first", "sha256": "original-version"}, "offset": 84,
                "nextOffset": 168, "text": "new page"}}, "observation": true}]});
        assert_eq!(
            guard.apply("observations.success", &next).unwrap()["repeated"],
            false
        );
    }

    #[test]
    fn structured_file_receipts_ignore_outer_archive_rotation() {
        let mut guard = ObservationGuard::default();
        for round in 0..4 {
            let body = match round {
                0 => json!({"path":"fixture","sha256":"same-version","offset":0,
                    "nextOffset":84,"content":"same source"}),
                1 => json!({"path":"fixture","sha256":"same-version","offset":0,
                    "nextOffset":84,"text":"same source"}),
                _ => json!({"structuredContent":{"path":"fixture","sha256":"same-version",
                    "offset":0,"nextOffset":84,"text":"same source"}}),
            };
            let result = guard.apply("observations.success", &json!({"results": [{
                "call": {"name": "filesystem.read", "arguments": {"path": "fixture",
                    "reason": format!("explanation {round}")}}, "observation": true,
                "result": {"content": body,
                    "resultRef": {"id": format!("new-{round}"), "sha256": format!("new-hash-{round}")}}
            }]})).unwrap();
            assert_eq!(result["warning"], round == 2);
        }
    }

    #[test]
    fn real_progress_and_effects_reset_the_stagnant_streak() {
        let mut guard = ObservationGuard::default();
        let pair = json!({"call": {"name": "filesystem.read", "arguments": {"path": "fixture"}},
            "result": {"content": "first"}, "observation": true});
        for _ in 0..3 {
            guard
                .apply("observations.success", &json!({"results": [pair]}))
                .unwrap();
        }
        let mut changed = pair.clone();
        changed["result"]["content"] = json!("second");
        assert_eq!(
            guard
                .apply("observations.success", &json!({"results": [changed]}))
                .unwrap()["repeated"],
            false
        );
        guard
            .apply(
                "observations.success",
                &json!({"results": [{"call": {"name": "filesystem.write"},
            "result": {"content": "written"}, "observation": false}]}),
            )
            .unwrap();
        assert_eq!(
            guard.apply("observations.state", &json!({})).unwrap()["stagnantRounds"],
            0
        );
    }

    #[test]
    fn failed_reads_count_only_the_same_current_target() {
        let mut guard = ObservationGuard::default();
        let pair = |process_id| {
            json!({"call": {"name": "computer.read", "arguments": {
            "windowId": "window", "processId": process_id}}, "result": {"isError": true,
            "code": "DESKTOP_NOT_RESPONDING"}, "observationFailure": true})
        };
        assert_eq!(
            guard
                .apply("observations.failure", &json!({"results": [pair(1)]}))
                .unwrap()["warning"],
            false
        );
        assert_eq!(
            guard
                .apply("observations.failure", &json!({"results": [pair(1)]}))
                .unwrap()["warning"],
            true
        );
        assert_eq!(
            guard
                .apply("observations.failure", &json!({"results": [pair(2)]}))
                .unwrap()["warning"],
            false
        );
        assert_eq!(
            guard
                .apply(
                    "observations.failure",
                    &json!({"results": [{"call": {"name": "computer.type"},
            "result": {"isError": true, "code": "DESKTOP_TIMEOUT", "status": "unknown"},
            "observationFailure": false}]})
                )
                .unwrap()["finalize"],
            false
        );
        assert_eq!(
            guard.apply("observations.state", &json!({})).unwrap()["failedRounds"],
            0
        );
    }
}

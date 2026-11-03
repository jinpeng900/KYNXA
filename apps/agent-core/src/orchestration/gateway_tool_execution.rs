//! 根任务与委派任务共用执行上下文和回执语义，正式操作仍由原任务日志持有。

use super::{
    gateway_runtime::{GatewayRuntime, RequestRun},
    gateway_work_progress::{RoundProgress, WorkProgress, WorkProgressPlan},
    task::{completed_output_paths, successful_completion_receipt},
};
use crate::{
    gateway_contract::GatewayError,
    storage::{gateway as data, task_repository::OperationState},
    tools::gateway as tools,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::sync::Arc;

pub(super) struct ToolProgressOutcome {
    pub plan: WorkProgressPlan,
    pub observation: Value,
}

pub(super) fn progress_interruption(plan: &WorkProgressPlan) -> Option<GatewayError> {
    plan.wait_for_user.then(|| {
        GatewayError::new(
            "TASK_CONTINUATION_NO_PROGRESS",
            "任务还没有全部完成，已有结果和进度已保留，可以让它继续。",
            409,
        )
    })
}

/// 派发成功不代表产物变化，要看文件原生前置版本和实际哈希。
pub(super) fn artifact_version_changed(
    call: &Value,
    receipt: &Value,
    before: &Value,
    after: &Value,
) -> bool {
    if receipt["reused"] == true
        || receipt["executed"] == false
        || !successful_completion_receipt(receipt)
    {
        return false;
    }
    let name = call["name"].as_str().unwrap_or("");
    let value = &receipt["content"];
    if value["reused"] == true || value["executed"] == false {
        return false;
    }
    if matches!(
        name,
        "filesystem.write"
            | "filesystem.edit"
            | "filesystem.delete"
            | "filesystem.mkdir"
            | "document.create"
            | "attachment.export"
    ) {
        if value["path"].as_str().is_some_and(|path| !path.is_empty()) {
            if value["deleted"] == true || value["created"] == true || value["changed"] == true {
                return true;
            }
            if matches!(
                name,
                "filesystem.write" | "filesystem.edit" | "document.create" | "attachment.export"
            ) && let Some(hash) = value["sha256"].as_str().filter(|hash| {
                hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
            }) {
                // 原生写入只接受不存在目标的 null，或覆盖前真实版本哈希。
                let expected = &call["arguments"]["expectedHash"];
                if expected.is_null() {
                    return name != "filesystem.edit";
                }
                if let Some(previous) = expected.as_str().filter(|previous| {
                    previous.len() == 64 && previous.bytes().all(|byte| byte.is_ascii_hexdigit())
                }) {
                    return !previous.eq_ignore_ascii_case(hash);
                }
            }
        }
    }
    before["complete"] == true
        && after["complete"] == true
        && before["signature"]
            .as_str()
            .filter(|value| !value.is_empty())
            .zip(
                after["signature"]
                    .as_str()
                    .filter(|value| !value.is_empty()),
            )
            .is_some_and(|(previous, current)| previous != current)
}

/// 用已有正式观察指纹及已接受的文档事实派生进展，不为调用次数或新归档 ID 记功。
pub(super) fn observe_progress_round(
    work: &mut WorkProgress,
    observations: &mut crate::tools::observations::ObservationGuard,
    mut round: RoundProgress,
    results: &[Value],
) -> Result<ToolProgressOutcome, GatewayError> {
    let before = observation_facts(observations)?;
    let mut observed = results.to_vec();
    for pair in &mut observed {
        if matches!(
            pair["call"]["name"].as_str(),
            Some("tool.search" | "tool.load")
        ) {
            pair["observation"] = json!(true);
        }
    }
    let observation = observations
        .apply("observations.success", &json!({"results":observed}))
        .map_err(|code| {
            GatewayError::new(
                code,
                "Tool progress projection failed. / 工具进展投影失败。",
                500,
            )
        })?;
    let after = observation_facts(observations)?;
    round.waiting_only = !results.is_empty() && results.iter().all(active_wait_receipt);
    if !round.waiting_only {
        for (key, hash) in after.difference(&before) {
            if key.starts_with("tool.search:") || key.starts_with("tool.load:") {
                round.strategy_changed = true;
            } else if !hash.is_empty() {
                round.evidence_changed = true;
            }
        }
    }
    if !round.waiting_only {
        // 命令和桌面操作不属于观察类工具，但会改变状态，按实际结果判断。
        let outcomes = results
            .iter()
            .filter(|pair| {
                pair["observation"] != true
                    && pair["result"]["executed"] != false
                    && pair["result"]["reused"] != true
                    && !pair["call"]["name"].as_str().is_some_and(|name| {
                        name.starts_with("task.")
                            || matches!(name, "tool.search" | "tool.load" | "context.compact")
                    })
            })
            .map(|pair| {
                (
                    &pair["call"],
                    &pair["result"],
                    successful_completion_receipt(&pair["result"]),
                )
            })
            .collect::<Vec<_>>();
        let (successful, failed) = work.record_outcomes(&outcomes);
        round.action_outcomes_only = successful > 0
            && !round.evidence_changed
            && !round.artifact_updated
            && !round.new_read_range;
        round.evidence_changed |= successful > 0;
        round.new_failed_outcomes = failed;
    }
    let repeated_calls = if observation["repeated"] == true {
        observed
            .iter()
            .filter(|pair| {
                pair["observation"] == true
                    && successful_completion_receipt(&pair["result"])
                    && !active_wait_receipt(pair)
            })
            .count() as u64
    } else {
        0
    };
    round.repeated_reads = round.repeated_reads.max(repeated_calls);
    round.suppressed_calls = results
        .iter()
        .filter(|pair| pair["result"]["code"] == "TOOL_ACTION_REQUIRES_CORRECTION")
        .count() as u64;
    Ok(ToolProgressOutcome {
        plan: work.observe_round(&round),
        observation,
    })
}

fn observation_facts(
    observations: &mut crate::tools::observations::ObservationGuard,
) -> Result<std::collections::BTreeSet<(String, String)>, GatewayError> {
    let state = observations
        .apply("observations.state", &json!({}))
        .map_err(|code| {
            GatewayError::new(
                code,
                "Tool observation state failed. / 工具观察状态读取失败。",
                500,
            )
        })?;
    Ok(state["observations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            Some((
                item["key"].as_str()?.to_owned(),
                item["fingerprint"].as_str()?.to_owned(),
            ))
        })
        .collect())
}

fn active_wait_receipt(pair: &Value) -> bool {
    let receipt = &pair["result"];
    let mut nodes = vec![receipt];
    let mut cursor = 0;
    while cursor < nodes.len() && nodes.len() < 32 {
        for field in [
            "content",
            "structuredContent",
            "value",
            "result",
            "details",
            "error",
        ] {
            if let Some(value) = nodes[cursor].get(field).filter(|value| value.is_object()) {
                nodes.push(value);
            }
        }
        for field in ["tasks", "children", "operations", "jobs"] {
            if let Some(items) = nodes[cursor].get(field).and_then(Value::as_array) {
                for value in items
                    .iter()
                    .filter(|value| value.is_object())
                    .take(32usize.saturating_sub(nodes.len()))
                {
                    nodes.push(value);
                }
            }
        }
        cursor += 1;
    }
    let transient = nodes.iter().any(|node| {
        node["retryAfterMs"].as_u64().is_some_and(|delay| delay > 0)
            || node["code"].as_str().is_some_and(|code| {
                matches!(
                    code,
                    "RESOURCE_WAIT"
                        | "RESOURCE_UNAVAILABLE"
                        | "RESOURCE_LIMIT"
                        | "MODEL_NETWORK_FAILED"
                        | "MODEL_RESPONSE_READ_FAILED"
                        | "MODEL_RESPONSE_TIMEOUT"
                        | "MODEL_IDLE_TIMEOUT"
                        | "MODEL_STREAM_INCOMPLETE"
                        | "MCP_TIMEOUT"
                        | "MCP_CONNECTION_CLOSED"
                        | "MCP_CONNECTION_LOST"
                )
            })
    });
    if transient {
        return true;
    }
    nodes.iter().any(|node| {
        ["status", "state"].iter().any(|field| {
            matches!(
                node[*field].as_str(),
                Some("running" | "pending" | "queued" | "waiting" | "waiting-network")
            )
        }) && [
            "taskId",
            "operationId",
            "jobId",
            "processId",
            "sessionId",
            "recoveryHandle",
        ]
        .iter()
        .any(|field| {
            node.get(field).is_some_and(|value| {
                !value.is_null() && value.as_str().is_none_or(|text| !text.is_empty())
            })
        })
    })
}

#[cfg(test)]
mod work_progress_tests {
    use super::*;
    use crate::tools::observations::ObservationGuard;

    fn read(id: &str, offset: u64) -> Value {
        json!({"call":{"id":id,"name":"filesystem.read","arguments":{"path":"notes.txt",
            "offset":offset,"reason":id}},"observation":true,
            "result":{"status":"completed","resultRef":{"id":id,"sha256":id},
                "content":{"path":"notes.txt","sha256":"source-version","offset":offset,
                    "nextOffset":offset+20,"text":"actual page"}}})
    }

    #[test]
    fn canonical_read_progress_ignores_new_call_ids_and_accepts_new_ranges() {
        let mut work = WorkProgress::default();
        let mut observations = ObservationGuard::default();
        let facts = RoundProgress {
            work_calls: 1,
            source_reads: 1,
            ..Default::default()
        };
        for index in 0..5 {
            let result = observe_progress_round(
                &mut work,
                &mut observations,
                facts.clone(),
                &[read(&format!("call-{index}"), 0)],
            )
            .unwrap();
            assert_eq!(result.plan.made_progress, index == 0);
            assert_eq!(result.plan.wait_for_user, index == 4);
        }
        let alternative = json!({"call":{"id":"new-method","name":"terminal.run",
            "arguments":{"command":"try-a-different-source"}},"observation":false,
            "result":{"status":"error","isError":true,"content":{"exitCode":1}}});
        let mixed = observe_progress_round(
            &mut work,
            &mut observations,
            RoundProgress {
                work_calls: 2,
                source_reads: 1,
                ..Default::default()
            },
            &[read("same", 0), alternative],
        )
        .unwrap();
        assert!(!mixed.plan.wait_for_user);
        let advanced = observe_progress_round(
            &mut work,
            &mut observations,
            facts.clone(),
            &[read("next", 20)],
        )
        .unwrap();
        assert!(advanced.plan.made_progress);
        assert!(!advanced.plan.wait_for_user);
        work.resume_after_user_instruction();
        assert!(
            !observe_progress_round(
                &mut work,
                &mut observations,
                facts,
                &[read("continued", 20)]
            )
            .unwrap()
            .plan
            .wait_for_user
        );
    }

    fn action(id: &str, command: &str, process: u64, output: &str, failed: bool) -> Value {
        json!({"call":{"id":id,"name":"terminal.host.run","arguments":{"script":command,"reason":id}},"observation":false,
            "result":{"status":if failed {"error"} else {"completed"},"isError":failed,
                "content":{"value":{"exitCode":i32::from(failed),"stdout":output,"processId":process,"elapsedMs":process}}}})
    }

    #[test]
    fn host_actions_count_by_outcome_and_identical_repeats_still_stall() {
        let mut work = WorkProgress::default();
        let mut observations = ObservationGuard::default();
        let facts = RoundProgress {
            work_calls: 1,
            ..Default::default()
        };
        let first = observe_progress_round(
            &mut work,
            &mut observations,
            facts.clone(),
            &[action("a", "g++ sol.cpp", 1, "built", false)],
        )
        .unwrap();
        assert!(
            first.plan.made_progress,
            "a new successful command is progress"
        );
        let second = observe_progress_round(
            &mut work,
            &mut observations,
            facts.clone(),
            &[action("b", "./sol", 2, "10 5", false)],
        )
        .unwrap();
        assert!(second.plan.made_progress);
        let diagnosis = observe_progress_round(
            &mut work,
            &mut observations,
            facts.clone(),
            &[action("c", "g++ -v", 3, "", true)],
        )
        .unwrap();
        assert!(!diagnosis.plan.made_progress);
        assert_eq!(
            diagnosis.plan.no_progress_rounds, 0,
            "new diagnostics are not stagnation"
        );
        let mut last = diagnosis.plan;
        for index in 0..4 {
            // The same command and output with a new process ID and call ID is a repeat.
            last = observe_progress_round(
                &mut work,
                &mut observations,
                facts.clone(),
                &[action(
                    &format!("r{index}"),
                    "./sol",
                    10 + index,
                    "10 5",
                    false,
                )],
            )
            .unwrap()
            .plan;
            assert!(!last.made_progress);
        }
        assert_eq!(
            last.no_progress_rounds, 4,
            "identical repeats still accumulate stagnation"
        );
    }

    #[test]
    fn only_actual_active_handles_and_transient_receipts_are_waits() {
        assert!(!active_wait_receipt(&json!({"call":{"name":"task.status"},
            "result":{"status":"completed","content":{"state":"running"}}})));
        assert!(active_wait_receipt(&json!({"result":{"status":"completed",
            "content":{"state":"running","taskId":"actual-child"}}})));
        assert!(active_wait_receipt(&json!({"result":{"status":"completed",
            "content":{"tasks":[{"state":"running","taskId":"actual-child"}]}}})));
        assert!(active_wait_receipt(
            &json!({"result":{"status":"error","code":"RESOURCE_WAIT"}})
        ));
        assert!(active_wait_receipt(
            &json!({"result":{"status":"error","code":"MODEL_NETWORK_FAILED"}})
        ));
        assert!(!active_wait_receipt(
            &json!({"result":{"status":"error","code":"INVALID_TOOL_ARGUMENTS"}})
        ));
    }

    #[test]
    fn successful_same_bytes_are_not_progress_but_new_or_deleted_files_are() {
        let hash = "a".repeat(64);
        let call =
            json!({"name":"filesystem.write","arguments":{"path":"output","expectedHash":hash}});
        let receipt =
            json!({"status":"completed","executed":true,"content":{"path":"output","sha256":hash}});
        let before = json!({"complete":true,"signature":"known"});
        let after = json!({"complete":true,"signature":"newly-registered-target"});
        assert!(!artifact_version_changed(&call, &receipt, &before, &after));
        let mut changed = receipt.clone();
        changed["content"]["sha256"] = json!("b".repeat(64));
        assert!(artifact_version_changed(
            &call,
            &changed,
            &Value::Null,
            &Value::Null
        ));
        let mut create = call.clone();
        create["arguments"]["expectedHash"] = Value::Null;
        assert!(artifact_version_changed(
            &create,
            &receipt,
            &Value::Null,
            &Value::Null
        ));
        assert!(artifact_version_changed(
            &json!({"name":"filesystem.delete"}),
            &json!({"status":"completed","content":{"path":"output","deleted":true}}),
            &Value::Null,
            &Value::Null
        ));
        let mut reused = receipt;
        reused["reused"] = json!(true);
        assert!(!artifact_version_changed(&create, &reused, &before, &after));
    }

    #[test]
    fn attachment_exports_report_native_creates_but_not_reused_commits_as_progress() {
        let call = json!({"name":"attachment.export","arguments":{"path":"copy.png","expectedSha256":"a".repeat(64)}});
        let receipt = json!({"status":"completed","executed":true,
            "content":{"path":"copy.png","sha256":"a".repeat(64),"bytes":20}});
        assert!(artifact_version_changed(
            &call,
            &receipt,
            &Value::Null,
            &Value::Null
        ));
        for marker in [json!({"reused":true}), json!({"executed":false})] {
            let mut reused = receipt.clone();
            reused["content"]
                .as_object_mut()
                .unwrap()
                .extend(marker.as_object().unwrap().clone());
            assert!(!artifact_version_changed(
                &call,
                &reused,
                &json!({"complete":true,"signature":"before"}),
                &json!({"complete":true,"signature":"after"})
            ));
        }
    }
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
enum ReceiptStatus {
    Completed,
    Error,
    Unknown,
}

impl ReceiptStatus {
    fn from_value(value: &Value) -> Option<Self> {
        match value.as_str() {
            Some("completed") => Some(Self::Completed),
            Some("error") => Some(Self::Error),
            Some("unknown") => Some(Self::Unknown),
            _ => None,
        }
    }
}

/// 原生执行事实使用类型约束，供应商载荷与原始错误正文保持不变。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolExecutionReceipt {
    status: ReceiptStatus,
    executed: bool,
    is_error: bool,
    code: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    recovery_handle: Option<Value>,
    content: Value,
}
/// 复用旧回执时按当前错误语义判断，原始正文保持不变。
pub(super) fn normalize_reused_receipt(name: &str, receipt: &mut Value) {
    if receipt["status"] == "completed" && tools::explicit_tool_error(name, &receipt["content"]) {
        receipt["isError"] = json!(true);
        receipt["status"] = json!("error");
    }
}

/// 记忆撤回使待执行方案失效，保留已完成操作和历史。
pub(super) fn memory_replanning_result() -> Value {
    json!({"isError":true,"executed":false,"requestDispatched":false,"recoverable":true,
        "code":"TASK_MEMORY_UPDATED",
        "notice":"Not dispatched: confirmed memory changed while waiting for approval. Replan remaining work from the current confirmed memory and user request in this same task; retain successful receipts and output, and do not replay completed operations or this superseded batch."})
}

pub(super) fn tool_execution_receipt(result: Result<Value, GatewayError>) -> Value {
    match result {
        Ok(value) => {
            let unknown = value["status"] == "unknown"
                || value["outcome"] == "unknown"
                || value["code"]
                    .as_str()
                    .is_some_and(|code| code.contains("UNKNOWN"));
            json!(ToolExecutionReceipt {
                status: if unknown {
                    ReceiptStatus::Unknown
                } else if value["isError"] == true {
                    ReceiptStatus::Error
                } else {
                    ReceiptStatus::Completed
                },
                executed: unknown || value["executed"] != false,
                is_error: value["isError"] == true || unknown,
                code: value["code"].clone(),
                recovery_handle: Some(value["recoveryHandle"].clone()),
                content: value,
            })
        }
        Err(error) => {
            let unknown = error.code.contains("UNKNOWN")
                || error.details["status"] == "unknown"
                || error.details["outcome"] == "unknown";
            let parameter_error =
                error.code == "INVALID_TOOL_ARGUMENTS" || error.code.starts_with("INVALID_SKILL_");
            // 只有可信的派发前校验能证明无副作用。
            let not_dispatched = !unknown
                && parameter_error
                && error.details["executed"] == false
                && error.details["requestDispatched"] == false;
            let mut content = error.response();
            if not_dispatched {
                content["executed"] = json!(false);
                content["requestDispatched"] = json!(false);
                content["correction"] = json!(
                    "This call was rejected before execution. Keep the current task, prior output and successful receipts. Correct only the reported parameter using the declared tool schema and supplied bounds, then issue a new valid call. Do not repeat successful operations or guess permissions, paths, resource IDs, commands or write contents."
                );
            }
            json!(ToolExecutionReceipt {
                status: if unknown {
                    ReceiptStatus::Unknown
                } else {
                    ReceiptStatus::Error
                },
                executed: !not_dispatched,
                is_error: true,
                code: json!(error.code),
                recovery_handle: None,
                content,
            })
        }
    }
}

/// 请求退出时统一释放其发现与工具状态，错误和取消路径同样生效。
pub(crate) struct RequestToolGuard(pub(crate) tools::GatewayToolContext);
impl Drop for RequestToolGuard {
    fn drop(&mut self) {
        tools::release_request(&self.0);
    }
}

/// 专用 I/O 绑定已确认范围和本次尝试的取消令牌。
pub(crate) fn specialist_executor(
    runtime: &Arc<GatewayRuntime>,
    run: &Arc<RequestRun>,
    ctx: &tools::GatewayToolContext,
    permission: &str,
    text: &str,
) -> tools::ExternalTool {
    let config = runtime.config.clone();
    let hub = runtime.tasks.clone();
    let parent = run.clone();
    let mut tool_context = ctx.clone();
    tool_context.external_execute = None;
    let key = run.cancellation_key.clone();
    // project_id 和绑定快照取自正式归属。
    let context = json!({
        "conversationId": run.conversation_id,
        "projectId": ctx.project_id,
        "requestId": run.request_id,
        "permissionMode": permission,
        "trustedUserText": text,
        "acceptanceRequirements": ctx.config["acceptanceRequirements"],
        "bindingSnapshot": ctx.config["bindingSnapshot"],
        "workspaceMounts": ctx.config["workspaceMounts"],
        "expectedProjectId": ctx.config["bindingSnapshot"]["projectId"],
        "expectedFolderPath": ctx.config["bindingSnapshot"]["folderPath"]
    });
    Arc::new(move |name, args| {
        let config = config.clone();
        let context = context.clone();
        let key = key.clone();
        let hub = hub.clone();
        let parent = parent.clone();
        let tool_context = tool_context.clone();
        Box::pin(async move {
            if name.starts_with("task.") {
                return hub.tool(parent, tool_context, &name, args).await;
            }
            tokio::task::spawn_blocking(move || {
                let lease = crate::service_cancellation::CancellationLease::acquire(&key)
                    .map_err(|code| GatewayError::new(code, "取消状态无法读取。", 503))?;
                let _scope = lease.enter();
                if name == "tool.result.read" {
                    // 核对范围、哈希及 UTF-16 分页后，仅回读当前聊天公共归档。
                    return data::read_tool_result(
                        &config.data_root,
                        context["conversationId"].as_str().unwrap_or(""),
                        args["id"].as_str().unwrap_or(""),
                        args["offset"].as_u64().unwrap_or(0),
                        args["limit"].as_u64().unwrap_or(6000),
                    )
                    .map_err(GatewayError::from);
                }
                if name == "image.retain" {
                    let reason = args["reason"].as_str().unwrap_or("").trim();
                    if reason.is_empty() || reason.chars().count() > 2000 {
                        return Err(GatewayError::new("INVALID_IMAGE_RETENTION", "保留图片须提供 1–2000 字符的原因。", 400));
                    }
                    let chat = context["conversationId"].as_str().unwrap_or("");
                    let id = args["resultId"].as_str().unwrap_or("");
                    // 图像引用只取自当前聊天原始归档，不接受调用方提供路径或媒体作为保留对象。
                    let archive = data::load_private_tool_result(&config.data_root, chat, id)?;
                    data::public_archive(&archive)?;
                    let mut retained = data::pin_model_images(&config.data_root, chat, &archive["canonical"], reason)?;
                    retained["resultId"] = json!(id);
                    return Ok(retained);
                }
                if name == "artifact.open" && args["resultId"].is_string() {
                    let archive = data::load_private_tool_result(&config.data_root,
                        context["conversationId"].as_str().unwrap_or(""), args["resultId"].as_str().unwrap_or(""))?;
                    data::public_archive(&archive)?;
                    if archive["canonical"]["isError"] == true || !super::task::receipt_has_visual_observation(&archive["canonical"]) {
                        return Err(GatewayError::new("IMAGE_RESULT_REQUIRED", "该回执没有可展示的真实图像。", 400));
                    }
                    return Ok(json!({"kind":"screenshot","presentation":"workspace","title":args["title"],
                        "sourceResultRef":{"id":archive["id"],"bytes":archive["bytes"],"sha256":archive["sha256"]},
                        "sourceToolName":archive["toolName"],"sourceToolCallId":archive["toolCallId"]}));
                }
                if name == "work.folder.bind" {
                    return crate::retrieval::gateway::bind_work(&config, &context, &args)
                        .map_err(GatewayError::from);
                }
                crate::retrieval::gateway::handle_tool(&name, &args, &context, &config)
                    .ok_or_else(|| {
                        GatewayError::new("TOOL_NOT_IMPLEMENTED", "此工具没有原生执行入口。", 503)
                    })?
                    .map_err(GatewayError::from)
            })
            .await
            .map_err(|_| GatewayError::new("RETRIEVAL_WORKER_FAILED", "资料工具线程退出。", 500))?
        })
    })
}

pub(super) fn tool_failure_environment(
    ctx: &tools::GatewayToolContext,
    version: &Value,
    mutation_revision: u64,
) -> Value {
    json!({"workspace":ctx.workdir,"permission":ctx.permission_mode,"authorizedRoots":ctx.authorized_roots,
        "configuration":ctx.config,"version":{"signature":version["signature"],"complete":version["complete"]},
        "mutationRevision":mutation_revision})
}

pub(super) fn tool_failure_definition(call: &Value, catalog: &[Value], cached: &[Value]) -> Value {
    cached.iter().chain(catalog.iter()).find(|definition| definition["name"] == call["name"])
        .map(|definition| json!({"name":definition["name"],"inputSchema":definition["inputSchema"],
            "parameters":definition["parameters"],"annotations":definition["annotations"],"execution":definition["execution"]}))
        .unwrap_or_else(|| json!({"name":call["name"],"definitionAvailable":false}))
}

/// 根据真实回执结算操作状态，未知结果单独保留。
pub(crate) fn operation_outcome(receipt: &Value) -> OperationState {
    match ReceiptStatus::from_value(&receipt["status"]) {
        Some(ReceiptStatus::Completed) if receipt["isError"] != true => OperationState::Succeeded,
        Some(ReceiptStatus::Completed | ReceiptStatus::Error) => OperationState::Failed,
        Some(ReceiptStatus::Unknown) | None => OperationState::Unknown,
    }
}

#[cfg(test)]
mod receipt_tests {
    use super::*;

    #[test]
    fn missing_or_invalid_receipt_status_never_settles_an_operation_as_successful() {
        for receipt in [
            Value::Null,
            json!({"isError":false}),
            json!({"status":"running"}),
            json!({"status":false}),
        ] {
            assert_eq!(operation_outcome(&receipt), OperationState::Unknown);
        }
        assert_eq!(
            operation_outcome(&json!({"status":"completed","isError":false})),
            OperationState::Succeeded
        );
        assert_eq!(
            operation_outcome(&json!({"status":"completed","isError":true})),
            OperationState::Failed
        );
        assert_eq!(
            operation_outcome(&json!({"status":"error"})),
            OperationState::Failed
        );
        assert_eq!(
            operation_outcome(&json!({"status":"unknown","isError":true})),
            OperationState::Unknown
        );
    }
}

pub(crate) fn is_read_only_tool(name: &str) -> bool {
    // 执行策略统一判定只读操作；发现和上下文维护无外部副作用。
    crate::tools::policy::is_read_only(name)
        || matches!(name, "tool.load" | "task.document" | "context.compact")
}

fn completion_evidence_needs_restore(observation: &Value) -> bool {
    observation.get("verifiedSuccess").is_none()
        || observation.get("completionProof").is_none()
        || observation["completionProofVerification"] == "pending"
        || preview_proof_needs_upgrade(observation)
        || observation["verifiedSuccess"] == true
            && observation["name"]
                .as_str()
                .and_then(|name| name.strip_prefix("mcp."))
                .and_then(|name| name.split_once('.'))
                .is_some_and(|(_, tool)| tool == "execute_blender_code")
}

fn preview_proof_needs_upgrade(observation: &Value) -> bool {
    observation["name"] == "artifact.open"
        && observation["completionProof"]["targets"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|target| target["kind"] == "file" && target["version"].is_null())
}

/// 恢复旧证据并排除已知的假成功包装，不重放操作。
pub(super) async fn restore_completion_evidence(
    root: &std::path::Path,
    conversation: &str,
    request: &str,
    transcript: &mut Vec<Value>,
) {
    if !transcript
        .iter()
        .flat_map(|round| round["observations"].as_array().into_iter().flatten())
        .any(completion_evidence_needs_restore)
    {
        return;
    }
    let root = root.to_owned();
    let conversation = conversation.to_owned();
    let request = request.to_owned();
    let mut restored_transcript = transcript.clone();
    let restored = tokio::task::spawn_blocking(move || {
        for round in &mut restored_transcript {
            for observation in round["observations"].as_array_mut().into_iter().flatten()
                .filter(|observation| completion_evidence_needs_restore(observation)) {
                if observation["status"] != "completed" { continue; }
                let legacy = observation.get("verifiedSuccess").is_none() || observation.get("completionProof").is_none()
                    || preview_proof_needs_upgrade(observation);
                if !legacy {
                    // 未核验的旧包装保留历史事实，暂不参与当前验收。
                    observation["verifiedSuccess"] = json!(false);
                    observation["completionProofVerification"] = json!("pending");
                }
                let Some(id) = observation["ref"]["id"].as_str() else { continue; };
                let Ok(archive) = data::load_private_tool_result(&root, &conversation, id) else { continue; };
                // 存储不可读时保留重试入口，核验正文后才补证据。
                if legacy {
                    observation["verifiedSuccess"] = json!(false);
                    observation["completionEvidence"] = json!([]);
                }
                if archive["conversationId"] != conversation || archive["requestId"] != request
                    || archive["toolCallId"] != observation["id"] || archive["toolName"] != observation["name"]
                    || archive["sha256"] != observation["ref"]["sha256"] || archive["bytes"] != observation["ref"]["bytes"] {
                    continue;
                }
                let failed = tools::explicit_tool_error(
                    observation["name"].as_str().unwrap_or(""), &archive["canonical"]);
                let receipt = json!({"status":"completed","isError":failed,"content":archive["canonical"],"resultRef":observation["ref"]});
                if !legacy {
                    observation.as_object_mut().unwrap().remove("completionProofVerification");
                    let success = successful_completion_receipt(&receipt);
                    observation["verifiedSuccess"] = json!(success);
                    if !success {
                        observation["verifiedSuccess"] = json!(false);
                        observation["completionEvidence"] = json!([]);
                        observation["completionProof"] = Value::Null;
                    }
                    continue;
                }
                observation["verifiedSuccess"] = json!(successful_completion_receipt(&receipt));
                observation["completionEvidence"] = json!(completed_output_paths(observation["name"].as_str().unwrap_or(""), &receipt));
                let call = json!({"id":observation["id"],"name":observation["name"]});
                let mut proof = super::completion_evidence::from_receipt(&call, &receipt);
                if proof.is_object() {
                    proof["mutationSequence"] = observation["mutationSequence"].clone();
                    for field in ["mutationSequence", "mutating", "current", "observationCurrent", "visualObserved", "validation"] {
                        if let Some(value) = observation["completionProof"].get(field) { proof[field] = value.clone(); }
                    }
                }
                observation["completionProof"] = proof;
            }
        }
        restored_transcript
    }).await;
    if let Ok(restored) = restored {
        *transcript = restored;
    }
}

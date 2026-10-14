use serde_json::{Value, json};

use super::intent::matches;
use super::service_error::{ServiceError, ServiceResult};
use super::service_skill_environment::SkillEnvironment;
use super::service_web::provider_kind;
use crate::models::protocol::{array, string, truthy};

fn safe_code(value: &Value, fallback: &str) -> String {
    value
        .as_str()
        .filter(|code| matches(r"^[A-Z0-9_]{1,100}$", code))
        .unwrap_or(fallback)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::apply;
    use serde_json::json;

    #[test]
    fn cancellation_distinguishes_admission_from_actual_external_dispatch() {
        let facts = json!({"name":"mcp.external.mutate","signalAborted":true,"executionStarted":true,"error":{"name":"AbortError"}});
        let admission = apply("broker.completion.failure", &facts).unwrap();
        assert_eq!(admission["notDispatched"], true);
        assert_eq!(admission["unknown"], false);
        let mut dispatched = facts;
        dispatched["mcpDispatchAttempted"] = json!(true);
        let completed_dispatch = apply("broker.completion.failure", &dispatched).unwrap();
        assert_eq!(completed_dispatch["notDispatched"], false);
        assert_eq!(completed_dispatch["unknown"], true);
        let receipt = apply(
            "broker.completion.externalFailure",
            &json!({"plan":completed_dispatch,"message":"cancelled","outsideWorkspace":true}),
        )
        .unwrap();
        assert!(receipt.get("executed").is_none());
        assert_eq!(receipt["status"], "unknown");
    }

    #[test]
    fn cancellation_keeps_the_completed_sandbox_receipt() {
        let receipt = json!({"protocolVersion":1,"sandbox":"appcontainer","tokenVerified":true,"workspaceCopy":true,
            "activeProcessesAfterExit":0,"cancelled":false,"timedOut":false,"exitCode":0,"stdout":"effect completed","stderr":""});
        let plan = apply(
            "broker.completion.failure",
            &json!({"name":"terminal.run","signalAborted":true,
            "executionStarted":true,"error":{"name":"AbortError","sandboxResult":receipt}}),
        )
        .unwrap();
        assert_eq!(plan["partial"]["isError"], false);
        assert_eq!(plan["partial"]["value"]["stdout"], "effect completed");
        assert!(plan["partial"].get("code").is_none());
    }

    #[test]
    fn only_delivered_originals_and_owned_job_receipts_supply_proof() {
        let originals=apply("broker.completion.visibleOriginals",&json!({"name":"knowledge.search","hasResultRef":true,
            "items":[{},{}],"expected":[{"sourceRef":"S1","excerpt":"complete"},{"sourceRef":"S2","excerpt":"other"}],
            "visible":[{"sourceRef":"S1","excerpt":"complete"},{"sourceRef":"S2","excerpt":"other","navigationOnly":true}]})).unwrap();
        assert_eq!(originals, json!([0]));
        let proof = apply(
            "broker.completion.jobProof",
            &json!({"job":{"status":"unknown","processId":12},"observationId":"read"}),
        )
        .unwrap();
        assert!(proof.is_null());
        let proof=apply("broker.completion.jobProof",&json!({"job":{"status":"completed","receipt":{"processId":12}},"observationId":"read"})).unwrap();
        assert_eq!(proof["dispatchConfirmed"], true);
        assert_eq!(proof["taskSuccessCertified"], false);
    }
}

fn optional(target: &mut Value, source: &Value, fields: &[&str]) {
    for field in fields {
        if let Some(value) = source.get(*field) {
            target[*field] = value.clone();
        }
    }
}

fn skill_verified(result: &Value, prepared: &Value) -> ServiceResult<bool> {
    Ok(SkillEnvironment::default().apply(
        "environment.verifies",
        &json!({"result":result,"prepared":prepared}),
    )? == true)
}

fn sandbox_partial(name: &str, result: &Value, prepared: &Value) -> ServiceResult<bool> {
    Ok(["terminal.run", "skill.run"].contains(&name)
        && result["protocolVersion"] == 1
        && result["sandbox"] == "appcontainer"
        && result["tokenVerified"] == true
        && result["workspaceCopy"] == true
        && result["activeProcessesAfterExit"] == 0
        && result["cancelled"].is_boolean()
        && result["timedOut"].is_boolean()
        && result["stdout"].is_string()
        && result["stderr"].is_string()
        && result["exitCode"]
            .as_f64()
            .is_some_and(|number| number.fract() == 0.0)
        && (name != "skill.run" || skill_verified(result, prepared)?))
}

fn failure_plan(arguments: &Value) -> ServiceResult<Value> {
    let name = string(&arguments["name"]);
    let error = &arguments["error"];
    let partial = &error["sandboxResult"];
    if sandbox_partial(name, partial, &arguments["preparedSkill"])? {
        let mut receipt = json!({"value":partial,"isError":partial["cancelled"] == true || partial["timedOut"] == true || partial["exitCode"] != 0,"sandbox":"appcontainer"});
        if partial["cancelled"] == true {
            receipt["code"] = json!("TOOL_CANCELLED");
        } else if partial["timedOut"] == true {
            receipt["code"] = json!("TOOL_TIMED_OUT");
        }
        return Ok(json!({"partial":receipt}));
    }
    let web_expired =
        arguments["webDeadlineAborted"] == true && arguments["originalAborted"] != true;
    // Closing an unissued approval has a typed refusal, even though SDK shutdown also aborts its signal.
    // 尚未派发的审批在关闭时返回明确拒绝码，即使 SDK 关闭同时取消了信号。
    let closed_before_dispatch = arguments["executionStarted"] != true
        && error["codeIsString"] != false
        && error["code"] == "TOOL_SERVICE_CLOSED";
    let cancelled = !web_expired
        && !closed_before_dispatch
        && (arguments["signalAborted"] == true || error["name"] == "AbortError");
    let desktop_timeout = name.starts_with("computer.")
        && [
            "DESKTOP_TIMED_OUT",
            "DESKTOP_TIMEOUT",
            "DESKTOP_READ_TIMEOUT",
        ]
        .contains(&string(&error["code"]));
    let external_lost = name.starts_with("mcp.")
        && ["MCP_TIMEOUT", "MCP_CONNECTION_LOST"].contains(&string(&error["code"]));
    let desktop_observation =
        super::policy::calculate("tools.desktopObservation", &json!({"name":name}))
            .map_err(|code| ServiceError::new(&code, "工具观察规则不可用。", 400))?
            == true;
    let file_read = [
        "filesystem.read",
        "filesystem.list",
        "filesystem.search",
        "filesystem.stat",
    ]
    .contains(&name);
    let may_have_effect = name.starts_with("computer.") && !desktop_observation
        || name.starts_with("mcp.")
        || [
            "terminal.run",
            "terminal.host.run",
            "terminal.host.start",
            "terminal.host.stop",
            "skill.run",
        ]
        .contains(&name)
        || name.starts_with("filesystem.") && !file_read;
    let not_dispatched = arguments["executionStarted"] != true
        || name.starts_with("mcp.")
            && (arguments["mcpDispatchAttempted"] != true || arguments["mcpNotDispatched"] == true);
    let unknown = !web_expired
        && !not_dispatched
        && may_have_effect
        && (cancelled || desktop_timeout || external_lost || error["outcomeUnknown"] == true);
    let code = if web_expired {
        "WEB_STAGE_BUDGET_EXHAUSTED".to_owned()
    } else if cancelled {
        "TOOL_CANCELLED".to_owned()
    } else if error["codeIsString"] == false {
        "TOOL_FAILED".to_owned()
    } else {
        safe_code(&error["code"], "TOOL_FAILED")
    };
    let message = if web_expired {
        "网页检索阶段已到期，已有证据与其他代码任务可以继续。"
    } else if cancelled {
        "工具调用已取消。"
    } else if error["messageIsString"] == false {
        "工具执行失败。"
    } else {
        error["message"].as_str().unwrap_or("工具执行失败。")
    };
    Ok(
        json!({"code":code,"message":message,"unknown":unknown,"notDispatched":not_dispatched,"cancelled":cancelled,
        "dnsHint":!cancelled && !web_expired && error["webFailureReason"] == "dns-non-public",
        "archive":arguments["executionStarted"] == true && (name.starts_with("computer.") || name.starts_with("mcp.")),
        "jobRecovery":unknown && name == "terminal.host.start",
        "revokedRecoverable":error["toolConfigurationRevoked"] == true && (arguments["executionStarted"] != true || name == "tool.load")}),
    )
}

#[cfg(test)]
mod lifecycle_regression_tests {
    use super::failure_plan;
    use serde_json::json;

    #[test]
    fn service_close_preserves_unissued_approval_refusal() {
        let mut facts = json!({"name":"filesystem.mkdir","signalAborted":true,"executionStarted":false,
            "error":{"name":"Error","code":"TOOL_SERVICE_CLOSED","codeIsString":true,
                "message":"工具服务已关闭。","messageIsString":true}});
        let refused = failure_plan(&facts).unwrap();
        assert_eq!(refused["code"], "TOOL_SERVICE_CLOSED");
        assert_eq!(refused["notDispatched"], true);
        assert_eq!(refused["unknown"], false);
        assert_eq!(refused["cancelled"], false);

        facts["executionStarted"] = json!(true);
        let issued = failure_plan(&facts).unwrap();
        assert_eq!(issued["code"], "TOOL_CANCELLED");
        assert_eq!(issued["unknown"], true);
    }
}

fn public_descriptor(descriptor: &Value) -> Value {
    let mut value = json!({"name":descriptor["name"],"description":descriptor["description"],"inputSchema":descriptor["inputSchema"],"source":descriptor["source"],"enabled":descriptor["enabled"] != false});
    for field in ["modelExposure", "executionEnvironment"] {
        if truthy(&descriptor[field]) {
            value[field] = descriptor[field].clone();
        }
    }
    if truthy(&descriptor["toolName"]) {
        value["rawName"] = descriptor["toolName"].clone();
    }
    if let Some(available) = descriptor.get("available") {
        value["available"] = available.clone();
        if truthy(&descriptor["unavailableCode"]) {
            value["unavailableCode"] = descriptor["unavailableCode"].clone();
        }
    }
    value
}

/// Completion plans use actual dispatch and SDK receipt facts; cancellation never invents an unexecuted effect.
/// 完成计划依据实际派发与 SDK 回执事实；取消不能虚构副作用尚未执行。
pub(super) fn apply(operation: &str, arguments: &Value) -> ServiceResult<Value> {
    match operation {
        "broker.completion.safeCode" => Ok(json!(safe_code(
            if arguments["codeIsString"] == false {
                &Value::Null
            } else {
                &arguments["code"]
            },
            arguments["fallback"].as_str().unwrap_or("TOOL_FAILED")
        ))),
        "broker.completion.publicDescriptor" => Ok(public_descriptor(&arguments["descriptor"])),
        "broker.completion.assertContext" => {
            if arguments["closed"] == true || arguments["registered"] != true {
                return Err(ServiceError::new(
                    "INVALID_TOOL_CONTEXT",
                    "工具上下文无效或已关闭。",
                    409,
                ));
            }
            Ok(Value::Null)
        }
        "broker.completion.releaseContext" => Ok(
            json!({"release":arguments["registered"]==true,"takeStages":arguments["closed"]!=true,"closeBroker":arguments["closed"]!=true}),
        ),
        "broker.completion.dependencies" => {
            if arguments["conversationRoot"] != true || arguments["dataHome"] != true {
                return Err(ServiceError::new(
                    "INVALID_TOOL_ARGUMENTS",
                    "缺少工具存储上下文。",
                    400,
                ));
            }
            Ok(Value::Null)
        }
        "broker.completion.capabilitiesFailure" => Ok(match string(&arguments["kind"]) {
            "sandbox" => json!({"available":false,"reason":"SANDBOX_UNAVAILABLE"}),
            "desktop" => json!({"available":false,"boundary":"host-desktop","operations":[]}),
            _ => json!({"available":false,"boundary":"host-terminal","shells":[]}),
        }),
        "broker.completion.workspacePlan" => Ok(
            json!({"isolated":arguments["ownership"]["workspaceRoot"].is_null(),"inspect":!arguments["ownership"]["workspaceRoot"].is_null() && arguments["ownership"]["managedWorkspace"] == true}),
        ),
        "broker.completion.workspaceInspect" => {
            let code = string(&arguments["code"]);
            if code == "UNSAFE_TOOL_PATH" {
                Ok(json!(true))
            } else {
                Ok(json!(false))
            }
        }
        "broker.completion.workspaceDiagnostic" => {
            let code = string(&arguments["code"]);
            if !["ENOENT", "ENOTDIR", "EACCES", "EPERM", "UNSAFE_TOOL_PATH"].contains(&code) {
                return Ok(Value::Null);
            }
            Ok(json!(safe_code(
                &arguments["code"],
                "WORKSPACE_UNAVAILABLE"
            )))
        }
        "broker.completion.userMessages" => Ok(json!(
            array(&arguments["messages"])
                .iter()
                .filter(|message| message["Role"] == "user")
                .map(|message| message
                    .get("Content")
                    .filter(|value| !value.is_null())
                    .cloned()
                    .unwrap_or(json!("")))
                .collect::<Vec<_>>()
        )),
        "broker.completion.readableVersion" => {
            if arguments["credential"] == true
                || arguments["privateResult"] == true
                || arguments["managed"] != true && arguments["owned"] == true
            {
                return Err(ServiceError::new(
                    "PROTECTED_APP_DATA",
                    "验证目标已受保护。",
                    403,
                ));
            }
            Ok(Value::Null)
        }
        "broker.completion.approval" => {
            if arguments["interactive"] != true || arguments["hasEmit"] != true {
                return Err(ServiceError::new(
                    "TOOL_APPROVAL_REQUIRED",
                    "此工具需要交互审批，本次没有执行。",
                    403,
                ));
            }
            Ok(if arguments["sensitiveRead"] == true {
                json!({"summary":"读取可能含密钥的文件，批准后内容可能进入工具记录并发送给当前模型。"})
            } else {
                json!({})
            })
        }
        "broker.completion.approvalWait" => Ok(json!(
            arguments["elapsed"].as_f64().unwrap_or(0.0).ceil().max(0.0)
        )),
        "broker.completion.approved" => {
            if !truthy(&arguments["approved"]) {
                return Err(ServiceError::new(
                    "TOOL_DENIED",
                    "用户拒绝了此工具调用。",
                    403,
                ));
            }
            Ok(Value::Null)
        }
        "broker.completion.webDeadline" => {
            let remaining = arguments["remainingMs"].as_f64().unwrap_or(0.0);
            if remaining <= 0.0 {
                return Err(ServiceError::new(
                    "WEB_STAGE_BUDGET_EXHAUSTED",
                    "网页检索预算已用完。",
                    409,
                ));
            }
            Ok(json!(remaining.ceil().max(1.0)))
        }
        "broker.completion.binding" => {
            if arguments["name"] == "work.folder.bind" && arguments["directory"] != true {
                return Err(ServiceError::new(
                    "WORK_BINDING_NOT_DIRECTORY",
                    "目标目录已变化，请重新检查关联路径。",
                    409,
                ));
            }
            Ok(Value::Null)
        }
        "broker.completion.available" => {
            let kind = string(&arguments["kind"]);
            if arguments["available"] != true {
                let (code, message) = match kind {
                    "retrieval" => ("RETRIEVAL_UNAVAILABLE", "本地检索不可用。"),
                    "memory" => ("MEMORY_UNAVAILABLE", "记忆服务不可用。"),
                    "catalog" => ("TOOL_CATALOG_UNAVAILABLE", "当前请求没有模型工具预算。"),
                    _ => (
                        "SANDBOX_UNAVAILABLE",
                        "已验证的 AppContainer 沙箱不可用，未在宿主执行。",
                    ),
                };
                return Err(ServiceError::new(
                    code,
                    message,
                    if kind == "catalog" { 409 } else { 503 },
                ));
            }
            Ok(Value::Null)
        }
        "broker.completion.retrievalOperation" => {
            if !truthy(&arguments["operation"]) {
                return Err(ServiceError::new(
                    "MODEL_TOOL_UNAVAILABLE",
                    "未知检索操作。",
                    400,
                ));
            }
            Ok(Value::Null)
        }
        "broker.completion.conversationMissing" => Err(ServiceError::new(
            "CONVERSATION_NOT_FOUND",
            "聊天不存在。",
            404,
        )),
        "broker.completion.modelWire" => Ok(json!(
            array(&arguments["tools"])
                .iter()
                .enumerate()
                .filter(|(index, _)| arguments["facts"][*index]["enabled"] == true
                    && arguments["facts"][*index]["webAvailable"] == true)
                .map(|(index, _)| index)
                .collect::<Vec<_>>()
        )),
        "broker.completion.promptServers" => Ok(json!(
            array(&arguments["servers"])
                .iter()
                .enumerate()
                .filter(|(_, server)| array(&arguments["descriptors"])
                    .iter()
                    .any(|tool| tool["serverId"] == server["id"]))
                .map(|(index, _)| index)
                .collect::<Vec<_>>()
        )),
        "broker.completion.foregroundPlan" => {
            let context = &arguments["context"];
            let action = string(&arguments["call"]["name"])
                .strip_prefix("computer.")
                .unwrap_or("");
            let applies = context["foregroundForbidden"] == true
                && (["activate", "move", "click", "scroll", "drag", "type", "key"]
                    .contains(&action)
                    || action == "window"
                        && ["maximize", "restore"]
                            .contains(&string(&arguments["call"]["arguments"]["mode"])));
            if !applies {
                return Ok(json!({"check":false}));
            }
            let global = super::intent::calculate(
                "tools.foregroundForbidden",
                &json!({"message":context["message"],"target":{"kind":"application"}}),
            )
            .map_err(|code| ServiceError::new(&code, "前台约束核验失败。", 400))?
                == true;
            Ok(json!({"check":true,"observeWindow":!global,"target":{"kind":"application"}}))
        }
        "broker.completion.foregroundTarget" => {
            let input = &arguments["input"];
            let window = array(&arguments["windows"])
                .iter()
                .find(|window| {
                    window["windowId"] == input["windowId"]
                        && window["processId"] == input["processId"]
                })
                .ok_or_else(|| {
                    ServiceError::new(
                        "DESKTOP_TARGET_CHANGED",
                        "目标窗口身份无法确认，请重新列出窗口。",
                        409,
                    )
                })?;
            Ok(json!({"window":window}))
        }
        "broker.completion.foregroundGuard" => {
            if arguments["forbidden"] == true {
                return Err(ServiceError::new(
                    "DESKTOP_FOREGROUND_FORBIDDEN",
                    "用户明确禁止激活此目标窗口，请使用后台操作方式。",
                    403,
                ));
            }
            Ok(Value::Null)
        }
        "broker.completion.promptCatalog" => {
            let available = arguments["availableCount"].as_u64().unwrap_or(0);
            let selected = arguments["selectedCount"].as_u64();
            let mut value = json!({"availableCount":available,"availableNames":arguments["availableNames"],"selectionState":if selected.is_some(){"configured"}else{"pending"},"tools":arguments["tools"],"servers":array(&arguments["servers"]).iter().map(|server|json!({"name":server["name"],"state":server["state"],"authentication":"unknown","loadName":server["name"]})).collect::<Vec<_>>()});
            if let Some(selected) = selected {
                value["selectedCount"] = json!(selected);
                value["deferredCount"] = json!(available.saturating_sub(selected));
            }
            Ok(value)
        }
        "broker.completion.capabilitySnapshot" => Ok(
            json!({"checkedBy":"kynxa-runtime","discoveryRequiresApproval":false,"discoveryExecutesCommands":false,"scope":"gateway-host","userDeviceRelationship":"unverified","tools":array(&arguments["tools"]).iter().map(|tool|{let availability=&tool["availability"];let mut value=json!({"name":tool["name"],"state":if tool["discovered"]==true{"available"}else if availability["available"]==false{"unavailable"}else if arguments["hasSnapshot"]==true{"disabled"}else{"not-discovered"},"executionEnvironment":tool["executionEnvironment"],"schema":if arguments["hasModel"]!=true{"selection-pending"}else if tool["selected"]==true{"loaded"}else{"deferred"},"approval":if arguments["permissionMode"]=="full" || ["web.fetch","computer.windows"].contains(&string(&tool["name"])){"not-required-by-current-mode"}else{"execution-policy-applies"}});if truthy(&availability["unavailableCode"]){value["code"]=availability["unavailableCode"].clone();}value}).collect::<Vec<_>>()}),
        ),
        "broker.completion.trackVersion" => Ok(json!(
            string(&arguments["name"]).starts_with("filesystem.")
                && ![
                    "filesystem.read",
                    "filesystem.list",
                    "filesystem.search",
                    "filesystem.stat"
                ]
                .contains(&string(&arguments["name"]))
        )),
        "broker.completion.searchProtected" => Ok(
            if arguments["outsideWorkspace"] == true || arguments["managedWorkspace"] == true {
                json!([])
            } else {
                arguments["roots"].clone()
            },
        ),
        "broker.completion.denySearchRead" => Ok(json!(
            arguments["credential"] == true
                || arguments["privateResult"] == true
                || array(&arguments["controlPaths"])
                    .iter()
                    .any(|value| *value == true)
                || arguments["permissionMode"] != "full"
                    && arguments["sensitiveRead"] != true
                    && arguments["sensitiveFile"] == true
        )),
        "broker.completion.rememberObservation" => Ok(json!(
            arguments["reusable"] == true
                && !truthy(&arguments["isError"])
                && !truthy(&arguments["code"])
                && arguments["configured"] == true
        )),
        "broker.completion.sandboxReceipt" => {
            let name = string(&arguments["name"]);
            let value = &arguments["value"];
            if name == "skill.run" && !skill_verified(value, &arguments["prepared"])? {
                return Err(ServiceError::new(
                    "SANDBOX_INVALID_RESULT",
                    "技能执行结果缺少完整的包验证记录。",
                    500,
                ));
            }
            if value["sandbox"] != "appcontainer" {
                return Err(ServiceError::new(
                    "SANDBOX_INVALID_RESULT",
                    "执行结果缺少真实 AppContainer 证明。",
                    500,
                ));
            }
            let mut receipt = json!({"isError":value["exitCode"]!=0 || value["timedOut"]==true || value["cancelled"]==true,"sandbox":"appcontainer","outsideWorkspace":false});
            if truthy(&value["cancelled"]) {
                receipt["code"] = json!("TOOL_CANCELLED");
            }
            if value["stagingDirectory"].is_string() {
                receipt["stage"] = value["stagingDirectory"].clone();
            }
            Ok(receipt)
        }
        "broker.completion.mcpResources" => Ok(
            json!({"taskId":format!("mcp:{}",string(&arguments["callId"])),"workspaceId":arguments["projectId"].as_str().map(|id|json!(id)).unwrap_or_else(||arguments["conversationId"].clone()),"kind":"foreground","cpuThreads":1,"memoryBytes":32*1024*1024}),
        ),
        "broker.completion.workPreparation" => {
            let preparation = &arguments["preparation"];
            let state =
                preparation["state"]
                    .as_str()
                    .unwrap_or(if arguments["hasRetrieval"] == true {
                        "pending"
                    } else {
                        "unavailable"
                    });
            let mut value = json!({"automaticPreparation":(["scheduled","unchanged"].contains(&state)),"preparationState":state,"currentTurnScopeChanged":false,"continuation":"Use absolute paths with reason for this folder in this turn; normal path approval still applies. The next turn uses the new work folder."});
            if truthy(&preparation["reason"]) {
                value["preparationReason"] = preparation["reason"].clone();
            }
            if truthy(&preparation["jobId"]) {
                value["preparationJobId"] = preparation["jobId"].clone();
            }
            Ok(value)
        }
        "broker.completion.searchProvider" => {
            if array(&arguments["descriptors"])
                .iter()
                .any(|tool| provider_kind(tool).is_some())
            {
                return Ok(Value::Null);
            }
            let provider = &arguments["providerId"];
            Ok(array(&arguments["servers"])
                .iter()
                .position(|server| {
                    super::service_web::search_server(server)
                        && server["enabled"] != false
                        && (!truthy(provider) || provider == "auto" || *provider == server["id"])
                })
                .map(|index| json!(index))
                .unwrap_or(Value::Null))
        }
        "broker.completion.failure" => failure_plan(arguments),
        "broker.completion.dnsMessage" => {
            let has_browser = arguments["allowHint"] == true
                && array(&arguments["tools"]).iter().any(|tool| {
                    matches(
                        r"(?:take_snapshot|evaluate_script|browser_snapshot|browser_evaluate)$",
                        string(&tool["toolName"]),
                    ) && tool["background"] == true
                });
            Ok(json!(format!(
                "当前 DNS 答案包含无效或非公共地址，可能来自代理的虚拟 IP；本次 HTTP 读取已拒绝。{}",
                if has_browser {
                    "可以发现并加载已允许的后台网页读取工具，不会打开本机浏览器窗口。"
                } else {
                    "请使用已有搜索证据或说明读取阻碍，不会自动打开本机浏览器。"
                }
            )))
        }
        "broker.completion.externalFailure" => {
            let plan = &arguments["plan"];
            let mut value = json!({"value":{"completed":false,"error":{"code":plan["code"],"message":arguments["message"]},"outcome":if plan["unknown"]==true{"unknown"}else if plan["cancelled"]==true{"cancelled"}else{"failed"}},"isError":true,"code":plan["code"],"outsideWorkspace":arguments["outsideWorkspace"]});
            if plan["notDispatched"] == true {
                value["executed"] = json!(false);
                value["value"]["executed"] = json!(false);
            }
            if plan["unknown"] == true {
                value["status"] = json!("unknown");
            }
            Ok(value)
        }
        "broker.completion.recovery" => {
            let job = &arguments["job"];
            Ok(
                if arguments["plan"]["jobRecovery"] == true
                    && !job.is_null()
                    && job["conversationId"] == arguments["conversationId"]
                {
                    json!({"kind":"host-terminal-job","jobId":job["id"]})
                } else {
                    Value::Null
                },
            )
        }
        "broker.completion.recoveryMessage" => Ok(if !arguments["handle"].is_null() {
            json!(format!(
                "Host terminal job {}: query terminal.host.read with this jobId; do not replay the command. / 查询此后台任务状态，不能重新执行原命令。\n{}",
                string(&arguments["handle"]["jobId"]),
                string(&arguments["message"])
            ))
        } else {
            arguments["message"].clone()
        }),
        "broker.completion.failureReceipt" => {
            let plan = &arguments["plan"];
            let mut value = json!({"content":arguments["content"],"isError":true,"code":plan["code"],"outsideWorkspace":arguments["outsideWorkspace"],"executionEnvironment":arguments["executionEnvironment"]});
            if !arguments["recoveryHandle"].is_null() {
                value["recoveryHandle"] = arguments["recoveryHandle"].clone();
            }
            if plan["notDispatched"] == true || plan["revokedRecoverable"] == true {
                value["executed"] = json!(false);
            }
            if plan["revokedRecoverable"] == true {
                value["recoverable"] = json!(true);
            }
            if plan["unknown"] == true {
                value["status"] = json!("unknown");
            }
            Ok(value)
        }
        "broker.completion.recoveryCandidates" => {
            let observation = &arguments["observation"];
            if observation["call"]["name"] != "terminal.host.read"
                || truthy(&observation["result"]["isError"])
            {
                return Ok(json!([]));
            }
            Ok(json!(
                array(&arguments["records"])
                    .iter()
                    .enumerate()
                    .filter(
                        |(_, record)| record["call"]["name"] == "terminal.host.start"
                            && record["result"]["recoveryHandle"]["kind"] == "host-terminal-job"
                            && observation["call"]["arguments"]["jobId"]
                                == record["result"]["recoveryHandle"]["jobId"]
                    )
                    .map(|(index, _)| index)
                    .collect::<Vec<_>>()
            ))
        }
        "broker.completion.jobProof" => {
            let job = &arguments["job"];
            let pid = job
                .get("processId")
                .filter(|value| !value.is_null())
                .unwrap_or(&job["receipt"]["processId"]);
            if pid
                .as_u64()
                .is_none_or(|pid| pid == 0 || pid > 9_007_199_254_740_991)
                || !["running", "completed", "error", "cancelled"].contains(&string(&job["status"]))
            {
                return Ok(Value::Null);
            }
            let mut value = job.clone();
            value["processId"] = pid.clone();
            value["dispatchConfirmed"] = json!(true);
            value["verificationToolCallId"] = arguments["observationId"].clone();
            value["taskSuccessCertified"] = json!(false);
            Ok(value)
        }
        "broker.completion.startResult" => {
            let result = &arguments["result"];
            let canonical = result
                .get("canonical")
                .filter(|value| !value.is_null())
                .cloned()
                .unwrap_or_else(|| {
                    let mut canonical = json!({"content":[],"isError":truthy(&result["isError"])});
                    if let Some(value) = result.get("value") {
                        canonical["structuredContent"] = value.clone();
                    }
                    canonical
                });
            let status = if result["status"] == "unknown" {
                "unknown"
            } else if result["code"] == "TOOL_CANCELLED" {
                "cancelled"
            } else if truthy(&result["isError"]) {
                "error"
            } else {
                "completed"
            };
            let mut canonical = canonical;
            if !result
                .get("canonical")
                .is_some_and(|value| !value.is_null())
                && truthy(&result["code"])
            {
                canonical["code"] = result["code"].clone();
            }
            Ok(json!({"canonical":canonical,"status":status}))
        }
        "broker.completion.unsavedValue" => {
            let mut value = arguments["value"].clone();
            if arguments["hasProjection"] == true && truthy(&value["items"]) {
                if let Some(fields) = value.as_object_mut() {
                    fields.remove("evidenceArchiveId");
                }
                if let Some(items) = value["items"].as_array_mut() {
                    for item in items {
                        if let Some(fields) = item.as_object_mut() {
                            fields.remove("modelSourceRef");
                        }
                    }
                }
            }
            Ok(value)
        }
        "broker.completion.visibleOriginals" => {
            if arguments["name"] != "knowledge.search"
                || truthy(&arguments["isError"])
                || arguments["storageError"] == true
                || arguments["hasResultRef"] != true
                || !arguments["items"].is_array()
                || !arguments["visible"].is_array()
            {
                return Ok(json!([]));
            }
            Ok(json!(
                array(&arguments["items"])
                    .iter()
                    .enumerate()
                    .filter(
                        |(index, _)| array(&arguments["visible"]).iter().any(
                            |visible| visible["sourceRef"]
                                == arguments["expected"][*index]["sourceRef"]
                                && visible["excerpt"] == arguments["expected"][*index]["excerpt"]
                                && !truthy(&visible["navigationOnly"])
                        )
                    )
                    .map(|(index, _)| index)
                    .collect::<Vec<_>>()
            ))
        }
        "broker.completion.originalsEligible" => Ok(json!(
            arguments["name"] == "knowledge.search"
                && !truthy(&arguments["isError"])
                && arguments["storageError"] != true
                && arguments["hasResultRef"] == true
                && arguments["items"].is_array()
        )),
        "broker.completion.observedValue" => {
            if arguments["name"] == "knowledge.search" && arguments["value"]["items"].is_array() {
                let mut value = json!({"items":arguments["value"]["items"]});
                optional(&mut value, &arguments["value"], &["strategy"]);
                for item in value["items"].as_array_mut().unwrap() {
                    if let Some(fields) = item.as_object_mut() {
                        fields.remove("modelSourceRef");
                    }
                }
                Ok(value)
            } else {
                Ok(arguments["publicCanonical"].clone())
            }
        }
        "broker.completion.finishResult" => {
            let result = &arguments["result"];
            let mut value = json!({"content":arguments["content"],"isError":truthy(&result["isError"]),"status":arguments["status"],"executionEnvironment":arguments["executionEnvironment"],"observationHash":arguments["observationHash"],"outsideWorkspace":result.get("outsideWorkspace").filter(|value|!value.is_null()).cloned().unwrap_or(json!(false))});
            if truthy(&arguments["resultRef"]) {
                value["resultRef"] = arguments["resultRef"].clone();
            }
            if result["executed"] == false {
                value["executed"] = json!(false);
            }
            if let Some(fields) = arguments["sourceObservation"].as_object() {
                for (key, field) in fields {
                    value[key] = field.clone();
                }
            }
            if truthy(&result["code"]) || truthy(&arguments["storageError"]) {
                value["code"] = result
                    .get("code")
                    .filter(|value| !value.is_null())
                    .cloned()
                    .unwrap_or(json!("TOOL_RESULT_SAVE_FAILED"));
            }
            for field in ["sandbox", "browser"] {
                if truthy(&result[field]) {
                    value[field] = result[field].clone();
                }
            }
            Ok(value)
        }
        _ => Err(ServiceError::new(
            "AGENT_CORE_UNKNOWN_OPERATION",
            "Unknown tool completion operation.",
            400,
        )),
    }
}

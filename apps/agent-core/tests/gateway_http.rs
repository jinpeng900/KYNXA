//! Real loopback HTTP/SSE checks against a local deterministic provider, without Node or credentials.
//! 使用本机确定性供应商验证真实 HTTP/SSE、工具续接与持久化，不调用 Node 或真实模型凭据。

use axum::{
    Json, Router,
    extract::State,
    response::{IntoResponse, Response},
    routing::post,
};
use futures_util::StreamExt;
use kynxa_agent_core_native::{
    gateway,
    gateway_contract::{GatewayConfig, new_id},
    orchestration::gateway_runtime::GatewayRuntime,
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{sync::Mutex, task::JoinHandle};

#[derive(Clone)]
struct Provider {
    requests: Arc<Mutex<Vec<Value>>>,
    calls: Arc<AtomicUsize>,
    tool: bool,
    cancel: bool,
    mcp: bool,
    malformed_calls: Arc<AtomicUsize>,
    transient_http_failures: Arc<AtomicUsize>,
    unloaded_cycle: Arc<std::sync::atomic::AtomicBool>,
    archived_read_ids: Arc<Mutex<Vec<String>>>,
    context_pressure: Arc<std::sync::atomic::AtomicBool>,
    pressure_step: Arc<AtomicUsize>,
    pressure_read_reentry: Arc<std::sync::atomic::AtomicBool>,
    steering_cycle: Arc<std::sync::atomic::AtomicBool>,
    output_continuation: Arc<std::sync::atomic::AtomicBool>,
    control_note_cycle: Arc<std::sync::atomic::AtomicBool>,
    cancel_summary: Arc<std::sync::atomic::AtomicBool>,
    summary_requests: Arc<AtomicUsize>,
    skill_pagination_cycle: Arc<std::sync::atomic::AtomicBool>,
    parallel_cycle: Arc<std::sync::atomic::AtomicBool>,
    buffered_cycle: Arc<std::sync::atomic::AtomicBool>,
    readback_gap_cycle: Arc<std::sync::atomic::AtomicBool>,
    readback_gap_required: Arc<std::sync::atomic::AtomicBool>,
    readback_gap_step: Arc<AtomicUsize>,
    annotated_evidence_cycle: Arc<std::sync::atomic::AtomicBool>,
    resume_generation: Arc<AtomicUsize>,
    resume_generation_gate: Arc<tokio::sync::Notify>,
    hold_next_provider: Arc<std::sync::atomic::AtomicBool>,
    native_stop_cycle: Arc<std::sync::atomic::AtomicBool>,
    native_stop_emissions: Arc<AtomicUsize>,
    dsml_review_cycle: Arc<std::sync::atomic::AtomicBool>,
    native_review_cycle: Arc<std::sync::atomic::AtomicBool>,
    native_completion_control: Arc<std::sync::atomic::AtomicBool>,
    dsml_review_emissions: Arc<AtomicUsize>,
    memory_approval_target: Arc<Mutex<Option<String>>>,
}
fn system_text(request: &Value) -> String {
    request["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|message| message["role"] == "system")
        .map(|message| message["content"].to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_completion_review_request(request: &Value) -> bool {
    // Reviews keep tools; the latest control revision identifies the current phase.
    // 核验保留工具声明，当前阶段由最新控制提示及其撤销记录决定。
    request["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .rev().filter_map(|message| message["content"].as_str())
        .find(|content| content.starts_with("Runtime control completionReviewNotice [")
            || content.starts_with("Broker: compare the original requested work"))
        .is_some_and(|content| content.contains("Expected acceptance requirements: "))
}

/// The mock reviews only the broker's actual checklist and verified receipts, without inventing evidence.
/// 模拟核对仅读取实际发送的验收清单与成功回执，不凭空补造要求或证据来掩盖缺口。
fn completion_review_reply(request: &Value, reason: &str) -> Value {
    let note = request["messages"].as_array().unwrap().iter().rev()
        .filter_map(|message| message["content"].as_str())
        .find(|content| content.contains("Expected acceptance requirements: ")
            && content.contains("Verified completion evidence: ")).unwrap();
    let requirements: Value = serde_json::from_str(note.split("Expected acceptance requirements: ").nth(1).unwrap()
        .lines().next().unwrap()).unwrap();
    let evidence: Value = serde_json::from_str(note.split("Verified completion evidence: ").nth(1).unwrap()
        .lines().next().unwrap()).unwrap();
    assert!(!evidence.as_array().unwrap().is_empty(), "a real effect needs a successful receipt");
    let met = requirements.as_array().unwrap().iter().map(|requirement| json!({
        "requirementId":requirement["requirementId"],"item":"verified fixture output",
        "evidence":completion_review_evidence(request, requirement["item"].as_str().unwrap(), evidence.as_array().unwrap())
    })).collect::<Vec<_>>();
    json!({"role":"assistant","content":format!("<kynxa_task>{}</kynxa_task>",
        json!({"state":"complete","next":"","reason":reason,"unmet":[],"met":met}))})
}

fn completion_review_evidence(request: &Value, requirement: &str, verified: &[Value]) -> Vec<Value> {
    let mut selected = Vec::new();
    let mut retain = |reference: &Value| {
        if selected.len() < 8 && !selected.contains(reference) { selected.push(reference.clone()); }
    };
    let requirement = requirement.to_lowercase();
    // Prefer actual target receipts; compaction may remove earlier tool bodies but keeps verified identities.
    // 优先选实际目标回执；压缩可能移除旧工具正文，但保留的真实身份仍可用于核验。
    for (index, reference) in verified.iter().enumerate() {
        let Some(path) = reference.as_str().filter(|path| path.contains(['/', '\\'])) else { continue; };
        let name = path.rsplit(['/', '\\']).next().unwrap_or("").to_lowercase();
        if !name.is_empty() && requirement.contains(&name) {
            retain(reference);
            if let Some(next) = verified.get(index + 1) { retain(next); }
            if index > 0 { retain(&verified[index - 1]); }
        }
    }
    let mut targets = std::collections::HashSet::new();
    for message in request["messages"].as_array().into_iter().flatten().rev()
        .filter(|message| message["role"] == "tool") {
        let id = &message["tool_call_id"];
        if !verified.contains(id) { continue; }
        let Some(body) = message["content"].as_str().and_then(|content| serde_json::from_str::<Value>(content).ok()) else { continue; };
        let path = body["path"].as_str().or_else(|| body["locator"]["path"].as_str())
            .or_else(|| body["sourceMetadata"]["path"].as_str());
        let Some(path) = path else { continue; };
        let name = path.rsplit(['/', '\\']).next().unwrap_or("").to_lowercase();
        if !name.is_empty() && requirement.contains(&name) && targets.insert(path.to_lowercase()) {
            retain(id);
        }
    }
    // Balance old source evidence with recent delivery receipts instead of discarding the first read.
    // 兼顾早期来源与最近交付的证据，不因最后八条都是新调用而丢掉最初读取。
    for (first, last) in verified.iter().zip(verified.iter().rev()) {
        retain(first);
        retain(last);
    }
    selected
}

/// Follow the actual broker navigation, so the fixture cannot invent a missing document identity.
/// 使用实际发送的文档导航；夹具不能自行补造压缩后已经丢失的定位。
fn saved_document(request: &Value, path_suffix: &str) -> Value {
    request["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|message| {
            let text = message["content"].as_str()?;
            let marker = "Saved document work: ";
            let start = text.find(marker)? + marker.len();
            let navigation = serde_json::Deserializer::from_str(&text[start..])
                .into_iter::<Value>()
                .next()?
                .ok()?;
            navigation["documents"]
                .as_array()?
                .iter()
                .find(|entry| {
                    entry["source"]
                        .as_str()
                        .is_some_and(|path| path.ends_with(path_suffix))
                })
                .cloned()
        })
        .next_back()
        .unwrap_or_else(|| {
            panic!("The current document is missing from actual broker navigation: {path_suffix}")
        })
}

async fn mock_provider(State(state): State<Provider>, Json(body): Json<Value>) -> Response {
    state.calls.fetch_add(1, Ordering::SeqCst);
    state.requests.lock().await.push(body.clone());
    if state.hold_next_provider.swap(false, Ordering::AcqRel) {
        state.resume_generation_gate.notified().await;
    }
    if state.native_stop_cycle.load(Ordering::Acquire) {
        let current_guidance = body["messages"].as_array().into_iter().flatten()
            .filter_map(|message| message["content"].as_str())
            .any(|content| content.contains("交付已有成果，结束不要再检查"));
        let first_acknowledgement = current_guidance
            && state.native_stop_emissions.fetch_add(1, Ordering::AcqRel) == 0;
        let content = if first_acknowledgement || !current_guidance {
            "现有文件已保留，先核对原验收。\n<kynxa_task>{\"state\":\"complete\",\"next\":\"\",\"reason\":\"旧目标尚待核对\",\"unmet\":[]}</kynxa_task>"
        } else {
            "交付已有文件，未核验项仍保留，不再执行检查。\n<kynxa_task>{\"state\":\"complete\",\"met\":\"invalid old evidence\",\"disposition\":\"deliver-existing\"}</kynxa_task>"
        };
        let call_id = if first_acknowledgement { "withdrawn-check-before-control" } else { "withdrawn-check-after-control" };
        return Json(json!({"choices":[{"index":0,"message":{"role":"assistant",
            "content":content,"tool_calls":[{"id":call_id,"type":"function","function":{
                "name":kynxa_agent_core_native::models::protocol::wire_name("filesystem.stat"),
                "arguments":json!({"path":"annotated-result.txt"}).to_string()}}]},"finish_reason":"tool_calls"}],
            "usage":{"prompt_tokens":100,"completion_tokens":20,"total_tokens":120}})).into_response();
    }
    if state.annotated_evidence_cycle.load(Ordering::Acquire) || state.dsml_review_cycle.load(Ordering::Acquire)
        || state.native_review_cycle.load(Ordering::Acquire) {
        // A starved review cannot fit its structured evidence; retain the same completed effects.
        // 验收预算过小时无法容纳结构化证据；截断不能让已经成功的操作重新执行。
        if is_completion_review_request(&body) && body["max_tokens"].as_u64().unwrap_or(0) < 2048 {
            return Json(json!({"choices":[{"index":0,"message":{"role":"assistant",
                "content":"<kynxa_task>{\"state\":\"complete\",\"met\":["},"finish_reason":"length"}],
                "usage":{"prompt_tokens":100,"completion_tokens":20,"total_tokens":120}})).into_response();
        }
        let has_receipt = |id: &str| body["messages"].as_array().into_iter().flatten()
            .any(|message| message["role"] == "tool" && message["tool_call_id"] == id);
        let dsml_review = state.dsml_review_cycle.load(Ordering::Acquire);
        let native_review = state.native_review_cycle.load(Ordering::Acquire);
        let resume_generation = state.resume_generation.load(Ordering::Acquire);
        let resumed_call_id = format!("resume-stat-{resume_generation}");
        let current_file_guidance = body["messages"].as_array().unwrap().iter().any(|message|
            message["role"] == "user" && message["content"] == "继续核对原文件并保留全部成功回执。");
        if resume_generation == 2 && !has_receipt(&resumed_call_id)
            && !current_file_guidance {
            // Keep one real provider request in flight so guidance exercises cancellation and re-dispatch.
            // 留住一次真实供应商请求，让插话覆盖取消旧生成及重新派发的实际链路。
            state.resume_generation_gate.notified().await;
        }
        let call = |id: &str, name: &str, arguments: Value| json!({"role":"assistant","content":if current_file_guidance {
                "继续完成当前文件。\n<kynxa_task>{\"state\":\"continue\",\"next\":\"核对当前文件\",\"disposition\":\"continue\"}</kynxa_task>"
            } else { "继续完成当前文件。" },
            "tool_calls":[{"id":id,"type":"function","function":{
                "name":kynxa_agent_core_native::models::protocol::wire_name(name),"arguments":arguments.to_string()}}]});
        let message = if is_completion_review_request(&body) {
            if native_review && !has_receipt("dsml-native-stat") {
                let message = call("dsml-native-stat", "filesystem.stat", json!({"path":"annotated-result.txt"}));
                return Json(json!({"choices":[{"index":0,"message":message,"finish_reason":"tool_calls"}],
                    "usage":{"prompt_tokens":100,"completion_tokens":20,"total_tokens":120}})).into_response();
            }
            if dsml_review && !has_receipt("dsml-native-stat") {
                state.dsml_review_emissions.fetch_add(1, Ordering::AcqRel);
                let name = kynxa_agent_core_native::models::protocol::wire_name("filesystem.stat");
                return Json(json!({"choices":[{"index":0,"message":{"role":"assistant","content":format!(
                    "继续核对当前文件。\n\n<｜｜DSML｜｜ calls>\n<｜｜DSML｜｜ invoke name=\"{name}\">\n<｜｜DSML｜｜ parameter name=\"path\" string=\"true\">annotated-result.txt</｜｜DSML｜｜ parameter>\n</｜｜DSML｜｜ invoke>\n</｜｜DSML｜｜ calls>")},"finish_reason":"stop"}],
                    "usage":{"prompt_tokens":100,"completion_tokens":20,"total_tokens":120}})).into_response();
            }
            let mut reply = completion_review_reply(&body, "The original write and readback both have successful receipts");
            let content = reply["content"].as_str().unwrap();
            let mut assessment: Value = serde_json::from_str(content.strip_prefix("<kynxa_task>").unwrap()
                .strip_suffix("</kynxa_task>").unwrap()).unwrap();
            if current_file_guidance { assessment["disposition"] = json!("continue"); }
            for item in assessment["met"].as_array_mut().unwrap() {
                let references = item["evidence"].as_array().unwrap();
                assert!(references.contains(&json!("annotated-write")) && references.contains(&json!("annotated-read")),
                    "Annotations must refer to the broker's actual successful calls: {references:?}");
                if dsml_review || native_review {
                    assert!(references.contains(&json!("dsml-native-stat")), "The final review needs a real native receipt.");
                }
                item["evidence"] = json!(["annotated-write: 已成功写入当前文件", "annotated-read: 已回读相同文件内容"]);
                if dsml_review || native_review {
                    item["evidence"].as_array_mut().unwrap().push(json!("dsml-native-stat: 已核对当前文件元数据"));
                }
            }
            if state.native_completion_control.load(Ordering::Acquire) && !has_receipt("native-completion-control") {
                // A misplaced completion control is metadata, never an external capability to discover or execute.
                // 放错通道的完成控制仍是元数据，不能当作外部能力去搜索或执行。
                json!({"role":"assistant","content":"核对已保存的成果。","tool_calls":[{
                    "id":"native-completion-control","type":"function","function":{
                        "name":"kynxa_task","arguments":assessment.to_string()}}]})
            } else {
                reply["content"] = json!(format!("<kynxa_task>{assessment}</kynxa_task>"));
                reply
            }
        } else if resume_generation > 0 && !has_receipt(&resumed_call_id) {
            call(&resumed_call_id, "filesystem.stat", json!({"path":"annotated-result.txt"}))
        } else if !has_receipt("annotated-write") {
            call("annotated-write", "filesystem.write", json!({"path":"annotated-result.txt","content":"ANNOTATED_RECEIPT_BODY","expectedHash":null}))
        } else if !has_receipt("annotated-read") {
            call("annotated-read", "filesystem.read", json!({"path":"annotated-result.txt"}))
        } else if dsml_review && !has_receipt("dsml-native-stat")
            && state.dsml_review_emissions.load(Ordering::Acquire) > 0 {
            call("dsml-native-stat", "filesystem.stat", json!({"path":"annotated-result.txt"}))
        } else {
            json!({"role":"assistant","content":"文件已写入并回读，内容为 ANNOTATED_RECEIPT_BODY。\n<kynxa_task>{\"state\":\"complete\",\"next\":\"\",\"reason\":\"原写入和回读均完成\"}</kynxa_task>"})
        };
        let has_calls = message["tool_calls"].is_array();
        return Json(json!({"choices":[{"index":0,"message":message,"finish_reason":if has_calls {"tool_calls"} else {"stop"}}],
            "usage":{"prompt_tokens":100,"completion_tokens":20,"total_tokens":120}})).into_response();
    }
    if state.readback_gap_cycle.load(Ordering::Acquire) {
        let messages = body["messages"].as_array().unwrap();
        let context_followup = messages.iter().filter(|message| message["role"] == "user")
            .filter_map(|message| message["content"].as_str()).any(|content| content.contains("只根据刚才的内容"));
        let gap_step = state.readback_gap_step.load(Ordering::Acquire);
        let requires_readback = state.readback_gap_required.load(Ordering::Acquire);
        let read_windows = messages.iter().filter(|message| message["role"] == "tool")
            .filter_map(|message| serde_json::from_str::<Value>(message["content"].as_str()?).ok())
            .filter(|receipt| receipt["path"].as_str().is_some_and(|path| path.ends_with("gap-output.md")))
            .collect::<Vec<_>>();
        let head_read = read_windows.iter().any(|receipt| receipt["offset"] == 0
            && receipt["nextOffset"].as_u64().is_some_and(|end| end >= 3000));
        let tail_read = read_windows.iter().any(|receipt| receipt["offset"].as_u64().is_some_and(|start| start <= 4000)
            && receipt["nextOffset"].as_u64().is_some_and(|end| end >= 4800));
        let recovered_gap = head_read && tail_read
            || messages.iter().filter_map(|message| message["content"].as_str()).filter_map(|content| {
                let marker = "Saved document work: ";
                let start = content.find(marker)? + marker.len();
                serde_json::Deserializer::from_str(&content[start..]).into_iter::<Value>().next()?.ok()
            }).any(|navigation| navigation["documents"].as_array().into_iter().flatten().any(|entry|
                entry["source"].as_str().is_some_and(|path| path.ends_with("gap-output.md")) && entry["readState"] == "complete"));
        let call = |id: &str, name: &str, arguments: Value| json!({"role":"assistant","content":"继续处理当前文档。",
            "tool_calls":[{"id":id,"type":"function","function":{
                "name":kynxa_agent_core_native::models::protocol::wire_name(name),"arguments":arguments.to_string()}}]});
        let missing_read = || if !head_read {
            call("gap-head", "document.read", json!({"path":"gap-output.md","offset":0,"maxChars":3000}))
        } else {
            call("gap-tail", "document.read", json!({"path":"gap-output.md","offset":4000,"maxChars":1000}))
        };
        let message = if context_followup {
            json!({"role":"assistant","content":"已保留原文档和成功回执；摘要：文件已生成，缺口已补齐。"})
        } else if is_completion_review_request(&body) {
            completion_review_reply(&body, "Assess only the requested write or full read against the actual receipts")
        } else if gap_step == 0 {
            state.readback_gap_step.store(1, Ordering::Release);
            call("gap-write", "filesystem.write", json!({"path":"gap-output.md","content":"检索和恢复记录。".repeat(600),"expectedHash":null}))
        } else if requires_readback && gap_step == 1 {
            state.readback_gap_step.store(2, Ordering::Release);
            call("gap-read", "document.read", json!({"path":"gap-output.md","offset":3000,"maxChars":1000}))
        } else if !requires_readback {
            json!({"role":"assistant","content":"文档已生成。\n<kynxa_task>{\"state\":\"complete\",\"next\":\"\",\"unmet\":[]}</kynxa_task>"})
        } else if recovered_gap {
            json!({"role":"assistant","content":"文档已生成，当前版本的未读范围已补齐。\n<kynxa_task>{\"state\":\"complete\",\"next\":\"\",\"reason\":\"保留原写入并完成实际回读\"}</kynxa_task>"})
        } else if gap_step == 2 {
            // A valid but premature completion used to alternate reviews and prose without filling the real gap.
            // 合法但提前的完成声明曾使验收和空话反复循环；夹具必须由真实缺口回读推进，不能靠轮次结束。
            state.readback_gap_step.store(3, Ordering::Release);
            json!({"role":"assistant","content":"文档已保存。\n<kynxa_task>{\"state\":\"complete\",\"next\":\"\",\"reason\":\"模型自报完成但实际回读有缺口\"}</kynxa_task>"})
        } else { missing_read() };
        let has_calls = message["tool_calls"].is_array();
        return Json(json!({"choices":[{"index":0,"message":message,"finish_reason":if has_calls {"tool_calls"} else {"stop"}}],
            "usage":{"prompt_tokens":100,"completion_tokens":20,"total_tokens":120}})).into_response();
    }
    if state.buffered_cycle.load(Ordering::Acquire) {
        let reads = body["messages"].as_array().into_iter().flatten()
            .filter(|message| message["role"] == "tool").count();
        let message = if is_completion_review_request(&body) {
            completion_review_reply(&body, "Both requested reads have actual successful receipts")
        } else if reads < 2 {
            json!({"role":"assistant","content":if reads == 0 {"正在读取两个已授权文件。"} else {"INTERNAL_BUFFERED_PROGRESS_MUST_STAY_PRIVATE"},
                "tool_calls":[{"id":format!("buffered-read-{reads}"),"type":"function","function":{
                    "name":kynxa_agent_core_native::models::protocol::wire_name("filesystem.read"),
                    "arguments":json!({"path":if reads == 0 {"fixture.txt"} else {"second.txt"}}).to_string()}}]})
        } else {
            json!({"role":"assistant","content":"已读取 native-file-body 和 second-body，两个结果均已保留。\n<kynxa_task>{\"state\":\"complete\",\"next\":\"\",\"reason\":\"两次真实读取完成\"}</kynxa_task>"})
        };
        let has_calls = message["tool_calls"].is_array();
        return Json(json!({"choices":[{"index":0,"message":message,"finish_reason":if has_calls {"tool_calls"} else {"stop"}}],
            "usage":{"prompt_tokens":100,"completion_tokens":20,"total_tokens":120}})).into_response();
    }
    if state.transient_http_failures.fetch_update(Ordering::SeqCst, Ordering::SeqCst,
        |remaining| remaining.checked_sub(1)).is_ok() {
        return (axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":{"message":"fixture temporary provider failure"}}))).into_response();
    }
    if let Some(target)=state.memory_approval_target.lock().await.clone() {
        let receipt=|id:&str|body["messages"].as_array().into_iter().flatten()
            .find(|message|message["role"]=="tool" && message["tool_call_id"]==id);
        let call=|id:&str,name:&str,args:Value|json!({"id":id,"type":"function","function":{
            "name":kynxa_agent_core_native::models::protocol::wire_name(name),"arguments":args.to_string()}});
        let message=if is_completion_review_request(&body) {
            completion_review_reply(&body,"The refreshed plan retained the real source receipt and skipped obsolete writes")
        } else if receipt("memory-refreshed-read").is_some() {
            json!({"role":"assistant","content":"记忆撤销后未执行过期写入，已保留先前读取的 native-file-body。\n<kynxa_task>{\"state\":\"complete\",\"next\":\"\",\"reason\":\"按最新记忆与真实读取完成\"}</kynxa_task>"})
        } else if let Some(stale)=receipt("memory-approval-stale") {
            assert!(stale["content"].to_string().contains("TASK_MEMORY_UPDATED"),"Old approval must settle with the memory-change receipt: {stale}");
            assert_eq!(serde_json::from_str::<Value>(stale["content"].as_str().unwrap()).unwrap()["executed"],false);
            let later=receipt("memory-after-stale").expect("Every remaining call in the obsolete step must have a paired receipt");
            assert!(later["content"].to_string().contains("TASK_MEMORY_UPDATED"),"Later old-plan calls must be skipped: {later}");
            assert_eq!(serde_json::from_str::<Value>(later["content"].as_str().unwrap()).unwrap()["executed"],false);
            assert!(!system_text(&body).contains("MEMORY_APPROVAL_OLD_POLICY_v1"),"The next model request must receive refreshed confirmed memory");
            assert!(receipt("memory-prior-read").unwrap()["content"].to_string().contains("native-file-body"),"The successful earlier read must survive replanning");
            json!({"role":"assistant","content":"按最新记忆继续，保留已读结果。","tool_calls":[call("memory-refreshed-read","filesystem.read",json!({"path":"fixture.txt"}))]})
        } else if receipt("memory-prior-read").is_some() {
            assert!(system_text(&body).contains("MEMORY_APPROVAL_OLD_POLICY_v1"));
            json!({"role":"assistant","content":"已读取原文件，继续按当前确认记忆安排后续。","tool_calls":[
                call("memory-approval-stale","filesystem.write",json!({"path":target,"content":"obsolete-approved-write","expectedHash":null,"reason":"按当前已确认记忆保存隔离测试文件。"})),
                call("memory-after-stale","filesystem.write",json!({"path":"memory-stale-after.txt","content":"obsolete-following-write","expectedHash":null}))]})
        } else {
            assert!(system_text(&body).contains("MEMORY_APPROVAL_OLD_POLICY_v1"));
            json!({"role":"assistant","content":"先读取当前文件，保留真实进度。","tool_calls":[call("memory-prior-read","filesystem.read",json!({"path":"fixture.txt"}))]})
        };
        let has_calls=message["tool_calls"].is_array();
        return Json(json!({"choices":[{"index":0,"message":message,"finish_reason":if has_calls {"tool_calls"}else{"stop"}}],"usage":{"prompt_tokens":100,"completion_tokens":20,"total_tokens":120}})).into_response();
    }
    if state.skill_pagination_cycle.load(Ordering::Acquire) {
        // Follow the actual failure receipt before correcting the page; no synthetic tool success.
        // 先读取真实失败回执再纠正分页，不伪造工具成功结果。
        let has_receipt = |id: &str| body["messages"].as_array().into_iter().flatten()
            .any(|message| message["role"] == "tool" && message["tool_call_id"] == id);
        let message = if is_completion_review_request(&body) || has_receipt("skill-page-corrected") {
            json!({"role":"assistant","content":"已按声明纠正分页并完成技能列表读取，原有上下文保留。\n<kynxa_task>{\"state\":\"complete\",\"next\":\"\",\"reason\":\"已核对真实分页回执\"}</kynxa_task>"})
        } else {
            let correcting = has_receipt("skill-page-invalid");
            let arguments = if correcting { json!({"offset":0,"limit":1}) }
                else { json!({"offset":-1,"limit":1}) };
            json!({"role":"assistant","content":"继续当前技能读取任务。","tool_calls":[{
                "id":if correcting {"skill-page-corrected"}else{"skill-page-invalid"},"type":"function",
                "function":{"name":kynxa_agent_core_native::models::protocol::wire_name("skill.list"),"arguments":arguments.to_string()}}]})
        };
        let has_calls = message["tool_calls"].is_array();
        return Json(json!({"choices":[{"index":0,"message":message,"finish_reason":if has_calls {"tool_calls"}else{"stop"}}],
            "usage":{"prompt_tokens":100,"completion_tokens":20,"total_tokens":120}})).into_response();
    }
    if state.parallel_cycle.load(Ordering::Acquire) {
        // One step mixes independent reads with a write barrier and a dependent read-back.
        // 同一步骤混合独立读取、写入屏障与依赖写入结果的读回。
        let answered = body["messages"].as_array().into_iter().flatten()
            .any(|message| message["role"] == "tool");
        let call = |id: &str, name: &str, arguments: Value| json!({"id":id,"type":"function","function":{
            "name":kynxa_agent_core_native::models::protocol::wire_name(name),"arguments":arguments.to_string()}});
        let message = if is_completion_review_request(&body) {
            completion_review_reply(&body, "Ordered parallel reads and the write barrier have real receipts")
        } else if answered {
            json!({"role":"assistant","content":"已按原调用顺序取得读取与写入回执。\n<kynxa_task>{\"state\":\"complete\",\"next\":\"\",\"reason\":\"已核对真实回执\"}</kynxa_task>"})
        } else {
            json!({"role":"assistant","content":"同时读取两个文件，写入结果后再读回。","tool_calls":[
                call("parallel-read-a", "filesystem.read", json!({"path":"fixture.txt"})),
                call("parallel-read-b", "filesystem.read", json!({"path":"second.txt"})),
                call("parallel-write", "filesystem.write", json!({"path":"parallel-out.txt","content":"written-once","expectedHash":null})),
                call("parallel-read-c", "filesystem.read", json!({"path":"parallel-out.txt"})),
                call("parallel-read-d", "filesystem.read", json!({"path":"second.txt","maxChars":6}))]})
        };
        let has_calls = message["tool_calls"].is_array();
        return Json(json!({"choices":[{"index":0,"message":message,"finish_reason":if has_calls {"tool_calls"}else{"stop"}}],
            "usage":{"prompt_tokens":100,"completion_tokens":20,"total_tokens":120}})).into_response();
    }
    if state.control_note_cycle.load(Ordering::Acquire)
        && system_text(&body)
            .contains("Summarize public evidence for continuation of the same user task")
    {
        state.summary_requests.fetch_add(1, Ordering::SeqCst);
        let payload: Value = serde_json::from_str(
            body["messages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|message| message["role"] == "user")
                .unwrap()["content"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        let mut facts = payload["priorFacts"].as_array().unwrap().clone();
        let current = json!({"resultId":payload["source"]["resultId"],"text":"The current authorized file body was read; preserve its original source reference."});
        if !facts
            .iter()
            .any(|fact| fact["resultId"] == current["resultId"])
        {
            facts.push(current);
        }
        return Json(json!({"choices":[{"index":0,"message":{"role":"assistant","content":json!({"facts":facts}).to_string()},"finish_reason":"stop"}],"usage":{"prompt_tokens":100,"completion_tokens":20,"total_tokens":120}})).into_response();
    }
    if state.cancel_summary.load(Ordering::Acquire)
        && system_text(&body)
            .contains("Summarize public evidence for continuation of the same user task")
    {
        state.summary_requests.fetch_add(1, Ordering::SeqCst);
        // The real HTTP request has arrived, but its response is still incomplete when the user cancels.
        // 真实 HTTP 摘要请求已到达；用户取消时响应仍不完整，不将其记成未调用或成功摘要。
        let chunk = json!({"id":"summary-cancel-fixture","choices":[{"index":0,"delta":{"content":"{\"facts\":["},"finish_reason":null}]}).to_string();
        let events =
            futures_util::stream::unfold(Some(format!("data: {chunk}\n\n")), |chunk| async move {
                if let Some(chunk) = chunk {
                    Some((Ok::<_, std::convert::Infallible>(chunk), None))
                } else {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    None
                }
            });
        return (
            [("content-type", "text/event-stream")],
            axum::body::Body::from_stream(events),
        )
            .into_response();
    }
    if state
        .malformed_calls
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
            remaining.checked_sub(1)
        })
        .is_ok()
    {
        return Json(json!({"choices":[{"index":0,"message":{"role":"assistant","content":"","tool_calls":[{
            "id":"incomplete-fixture","type":"function","function":{"name":kynxa_agent_core_native::models::protocol::wire_name("tool.search"),"arguments":"{"}}]},
            "finish_reason":"tool_calls"}]})).into_response();
    }
    if state.steering_cycle.load(Ordering::Acquire) && is_completion_review_request(&body) {
        // Steered replies receive the real independent completion review, rather than another chat answer.
        // 被引导的回复仍经过真实独立完成审核；夹具按审核协议返回标记，不重复普通聊天正文。
        return Json(json!({"id":"steering-review-fixture","choices":[{"index":0,"message":{"role":"assistant","content":"<kynxa_task>{\"state\":\"complete\",\"next\":\"\",\"reason\":\"The final guided answer is complete\",\"disposition\":\"continue\"}</kynxa_task>"},"finish_reason":"stop"}],"usage":{"prompt_tokens":100,"completion_tokens":20,"total_tokens":120}})).into_response();
    }
    if state.cancel
        && (!state.steering_cycle.load(Ordering::Acquire)
            || state.calls.load(Ordering::SeqCst) <= 2)
    {
        let mut delta = json!({"content":"已开始，保留这段正文。"});
        if state.steering_cycle.load(Ordering::Acquire) {
            delta["tool_calls"] = json!([{"index":0,"id":"obsolete-partial-write","type":"function","function":{"name":kynxa_agent_core_native::models::protocol::wire_name("filesystem.write"),"arguments":"{\"path\":\"obsolete.txt\",\"content\":\""}}]);
        }
        let chunk = json!({"id":"cancel-fixture","choices":[{"index":0,"delta":delta,"finish_reason":null}]}).to_string();
        if state.steering_cycle.load(Ordering::Acquire) {
            let completed = json!({"id":"cancel-fixture","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":100,"completion_tokens":20,"total_tokens":120}}).to_string();
            let events = futures_util::stream::unfold(
                (Some(format!("data: {chunk}\n\n")), Some(format!("data: {completed}\n\ndata: [DONE]\n\n"))),
                |(first, last)| async move {
                    if let Some(first) = first {
                        return Some((Ok::<_, std::convert::Infallible>(first), (None, last)));
                    }
                    if let Some(last) = last {
                        tokio::time::sleep(Duration::from_secs(30)).await;
                        return Some((Ok::<_, std::convert::Infallible>(last), (None, None)));
                    }
                    None
                },
            );
            return (
                [("content-type", "text/event-stream")],
                axum::body::Body::from_stream(events),
            )
                .into_response();
        }
        let events = futures_util::stream::unfold(
            (Some(format!("data: {chunk}\n\n")), false),
            |(chunk, delayed)| async move {
                if let Some(chunk) = chunk {
                    return Some((Ok::<_, std::convert::Infallible>(chunk), (None, false)));
                }
                if !delayed {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                }
                None
            },
        );
        return (
            [("content-type", "text/event-stream")],
            axum::body::Body::from_stream(events),
        )
            .into_response();
    }
    let previous_result = body["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|message| message["role"] == "tool");
    let archive_ids = state.archived_read_ids.lock().await.clone();
    let continuation = state.output_continuation.load(Ordering::Acquire);
    let control_cycle = state.control_note_cycle.load(Ordering::Acquire);
    let control_step = control_cycle.then(|| state.pressure_step.fetch_add(1, Ordering::SeqCst));
    let continuation_first =
        (continuation && state.calls.load(Ordering::Acquire) == 1) || control_step == Some(0);
    let mut message = if let Some(step) = control_step {
        match step {
            0 => {
                json!({"role":"assistant","content":"Initial answer body; the authorized source check remains unfinished."})
            }
            1 | 4 => {
                json!({"role":"assistant","content":"Read the next authorized source.","tool_calls":[{"id":format!("control-source-{step}"),"type":"function","function":{"name":kynxa_agent_core_native::models::protocol::wire_name("filesystem.read"),"arguments":json!({"path":if step == 1 {"fixture.txt"}else{"second.txt"},"maxChars":100}).to_string()}}]})
            }
            2 => {
                json!({"role":"assistant","content":"The first current source was read; the second source remains to be checked."})
            }
            3 => {
                json!({"role":"assistant","content":"<kynxa_task>{\"state\":\"continue\",\"next\":\"Read second.txt and finish the original answer\",\"reason\":\"The second current source has not been checked\"}</kynxa_task>"})
            }
            5 => {
                json!({"role":"assistant","content":"Both authorized current sources were checked.\n<kynxa_task>{\"state\":\"complete\",\"next\":\"\",\"reason\":\"Both source reads are complete\"}</kynxa_task>"})
            }
            _ => {
                if is_completion_review_request(&body) { completion_review_reply(&body, "Both current sources have real read receipts") }
                else { json!({"role":"assistant","content":"<kynxa_task>{\"state\":\"complete\",\"next\":\"\",\"reason\":\"Both source reads are complete\"}</kynxa_task>"}) }
            }
        }
    } else if continuation {
        json!({"role":"assistant","content":if continuation_first {"第一段正文：保留这一段。"}
            else {"第二段正文：续写已完成。\n<kynxa_task>{\"state\":\"complete\",\"next\":\"\",\"reason\":\"两段正文完成\"}</kynxa_task>"}})
    } else if state.context_pressure.load(Ordering::Acquire) {
        // Real effects now end with a completion review; answer it with the same verified outcome.
        // 真实操作后会核验完成情况，使用相同已核验结果作答。
        if is_completion_review_request(&body) {
            completion_review_reply(&body, "Output written and read back")
        } else if body["tools"].as_array().is_none_or(Vec::is_empty) {
            json!({"role":"assistant","content":"Public evidence summary; pending target remains second.txt and original receipts remain readable."})
        } else {
            let step = state.pressure_step.fetch_add(1, Ordering::SeqCst);
            let read_reentry = state.pressure_read_reentry.load(Ordering::Acquire);
            if step < if read_reentry { 15 } else { 4 } {
                let (name, arguments) = match step {
                    0 => (
                        "filesystem.read",
                        json!({"path":"fixture.txt","maxChars":16000}),
                    ),
                    1 => (
                        "tool.load",
                        json!({"names":[kynxa_agent_core_native::models::protocol::wire_name("filesystem.read"),"tool.result.read"]}),
                    ),
                    2 => (
                        "filesystem.read",
                        json!({"path":"second.txt","maxChars":100}),
                    ),
                    3 if read_reentry => (
                        "tool.load",
                        json!({"names":["filesystem.write","terminal.host.run","filesystem.edit","filesystem.read","filesystem.list"]}),
                    ),
                    4 | 5 => ("tool.load", json!({"names":["filesystem.write"]})),
                    6 => ("tool.load", json!({"names":["filesystem.read"]})),
                    7 => (
                        "filesystem.read",
                        json!({"path":"second.txt","maxChars":100}),
                    ),
                    8 => ("tool.load", json!({"names":["task.document"]})),
                    9 => (
                        "task.document",
                        json!({"action":"record","resultId":saved_document(&body,"second.txt")["resultId"],"conclusions":["SECOND_CURRENT_FILE is the current source body."],"unresolved":[]}),
                    ),
                    10 => ("task.document", json!({"action":"read","offset":0,"limit":4000})),
                    11 => (
                        "task.document",
                        json!({"action":"read","sourceIdentity":saved_document(&body,"second.txt")["sourceIdentity"],"offset":0,"limit":4000}),
                    ),
                    13 => (
                        "filesystem.write",
                        json!({"path":"pressure-result.txt","content":"SECOND_CURRENT_FILE","expectedHash":null}),
                    ),
                    14 => (
                        "filesystem.read",
                        json!({"path":"pressure-result.txt","maxChars":100}),
                    ),
                    _ => (
                        "tool.load",
                        json!({"names":["filesystem.write","filesystem.read","filesystem.list"]}),
                    ),
                };
                json!({"role":"assistant","content":"Continue using the loaded file reader after context preparation.","tool_calls":[{"id":format!("pressure-{step}"),"type":"function","function":{"name":kynxa_agent_core_native::models::protocol::wire_name(name),"arguments":arguments.to_string()}}]})
            } else {
                json!({"role":"assistant","content":if read_reentry {
                    "The saved analysis was recovered after context preparation; pressure-result.txt contains SECOND_CURRENT_FILE and was read back.\n<kynxa_task>{\"state\":\"complete\",\"next\":\"\",\"reason\":\"Current sources were read, their analysis restored, and the output written and read back\"}</kynxa_task>"
                } else {"The second current file was read after context preparation."}})
            }
        }
    } else if !archive_ids.is_empty() {
        let count = body["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|message| message["role"] == "tool")
            .count();
        if count == 0 {
            json!({"role":"assistant","content":"Load saved-result paging.","tool_calls":[{"id":"archive-load","type":"function","function":{"name":kynxa_agent_core_native::models::protocol::wire_name("tool.load"),"arguments":json!({"names":["tool.result.read"]}).to_string()}}]})
        } else if count == 1 {
            let calls = archive_ids.iter().enumerate().map(|(index,id)| json!({"id":format!("archive-page-{index}"),"type":"function","function":{"name":kynxa_agent_core_native::models::protocol::wire_name("tool.result.read"),"arguments":json!({"id":id,"offset":0,"limit":6000}).to_string()}})).collect::<Vec<_>>();
            json!({"role":"assistant","content":"Read the public saved result and check archive boundaries.","tool_calls":calls})
        } else {
            json!({"role":"assistant","content":"Public result read; foreign and private archives rejected."})
        }
    } else if state.unloaded_cycle.load(Ordering::Acquire) {
        let count = body["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|message| message["role"] == "tool")
            .count();
        if count < 2 {
            json!({"role":"assistant","content":"Check the authorized file metadata and continue.","tool_calls":[{"id":format!("unloaded-{count}"),"type":"function","function":{"name":kynxa_agent_core_native::models::protocol::wire_name("filesystem.stat"),"arguments":json!({"path":"fixture.txt"}).to_string()}}]})
        } else {
            json!({"role":"assistant","content":"Tool loading recovered and the authorized file metadata was read."})
        }
    } else if state.mcp {
        let count = body["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|message| message["role"] == "tool")
            .count();
        if count < 2 {
            let name = if count == 0 {
                "tool.search"
            } else {
                "tool.load"
            };
            let arguments = if count == 0 {
                json!({"query":"offline-fixture"})
            } else {
                json!({"names":["mcp.offline-fixture"]})
            };
            json!({"role":"assistant","content":"检查隔离 MCP 的可用性。","tool_calls":[{"id":format!("mcp-fixture-{count}"),"type":"function","function":{
                "name":kynxa_agent_core_native::models::protocol::wire_name(name),"arguments":arguments.to_string()}}]})
        } else {
            json!({"role":"assistant","content":"离线 MCP 不可用，仍能继续完成回答。\n<kynxa_task>{\"state\":\"complete\",\"next\":\"\",\"reason\":\"已报告隔离服务不可用\"}</kynxa_task>"})
        }
    } else if is_completion_review_request(&body) {
        completion_review_reply(&body, "The actual requested read has a verified receipt")
    } else if state.tool && !previous_result {
        json!({"role":"assistant","content":"先读取已授权文件。","tool_calls":[{"id":"read-fixture","type":"function","function":{
            "name":kynxa_agent_core_native::models::protocol::wire_name("filesystem.read"),"arguments":"{\"path\":\"fixture.txt\"}"}}]})
    } else {
        json!({"role":"assistant","content":if state.tool {"已读取 native-file-body。\n<kynxa_task>{\"state\":\"complete\",\"next\":\"\",\"reason\":\"已取得当前原文\"}</kynxa_task>"} else {"你好！"}})
    };
    if state.steering_cycle.load(Ordering::Acquire) && body["messages"].as_array().into_iter().flatten()
        .any(|message| message["role"] == "user" && matches!(message["content"].as_str(),
            Some("更新方案：保留原范围，只解释结果，不再继续写入。" | "再补充：用简短结论回答，接着当前工作。"))) {
        message["content"] = json!(format!("{}\n<kynxa_task>{{\"state\":\"complete\",\"disposition\":\"continue\"}}</kynxa_task>",
            message["content"].as_str().unwrap_or("")));
    }
    let has_calls = message["tool_calls"].is_array();
    if (state.cancel_summary.load(Ordering::Acquire) && state.calls.load(Ordering::Acquire) == 1)
        || (control_cycle && matches!(control_step, Some(1 | 4)))
    {
        // An opaque native continuation forces the settled current-step checkpoint after the actual file read.
        // 大原生续传使真实文件读取后的当前步骤进入检查点，摘要仍只读取公开原件，不能依赖伪造压缩回执。
        message["reasoning_content"] = json!("PRIVATE_FIXTURE_CONTINUATION ".repeat(4096));
    }
    Json(json!({"id":"fixture-response","choices":[{"index":0,"message":message,"finish_reason":if has_calls {"tool_calls"}else if continuation_first {"length"}else{"stop"}}],"usage":{"prompt_tokens":100,"completion_tokens":20,"total_tokens":120}})).into_response()
}

struct Fixture {
    runtime: Arc<GatewayRuntime>,
    client: reqwest::Client,
    url: String,
    conversation: String,
    user_message_id: String,
    provider: Provider,
    servers: Vec<JoinHandle<()>>,
    root: PathBuf,
}
impl Fixture {
    async fn new(tool: bool, cancel: bool) -> Self {
        Self::with_mcp(tool, cancel, false).await
    }
    async fn with_mcp(tool: bool, cancel: bool, mcp: bool) -> Self {
        let root = std::env::temp_dir().join(format!("kynxa-native-http-{}", new_id()));
        for directory in ["Models", "Data", "Extensions", "work"] {
            std::fs::create_dir_all(root.join(directory)).unwrap();
        }
        std::fs::write(root.join("work/fixture.txt"), "native-file-body").unwrap();
        let provider = Provider {
            requests: Default::default(),
            calls: Default::default(),
            tool,
            cancel,
            mcp,
            malformed_calls: Default::default(),
            transient_http_failures: Default::default(),
            unloaded_cycle: Default::default(),
            archived_read_ids: Default::default(),
            context_pressure: Default::default(),
            pressure_step: Default::default(),
            pressure_read_reentry: Default::default(),
            steering_cycle: Default::default(),
            output_continuation: Default::default(),
            control_note_cycle: Default::default(),
            cancel_summary: Default::default(),
            summary_requests: Default::default(),
            skill_pagination_cycle: Default::default(),
            parallel_cycle: Default::default(),
            buffered_cycle: Default::default(),
            readback_gap_cycle: Default::default(),
            readback_gap_required: Default::default(),
            readback_gap_step: Default::default(),
            annotated_evidence_cycle: Default::default(),
            resume_generation: Default::default(),
            resume_generation_gate: Default::default(),
            hold_next_provider: Default::default(),
            native_stop_cycle: Default::default(),
            native_stop_emissions: Default::default(),
            dsml_review_cycle: Default::default(),
            native_review_cycle: Default::default(),
            native_completion_control: Default::default(),
            dsml_review_emissions: Default::default(),
            memory_approval_target: Default::default(),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let provider_url = format!("http://{}", listener.local_addr().unwrap());
        let service = Router::new()
            .route("/chat/completions", post(mock_provider))
            .with_state(provider.clone());
        let provider_server = tokio::spawn(async move {
            axum::serve(listener, service).await.unwrap();
        });
        let configuration = GatewayConfig {
            data_home: root.join("Models"),
            data_root: root.join("Data"),
            extension_root: root.join("Extensions"),
            application_root: root.join("Application"),
            port: 0,
            toolhost_exe: root.join("missing-toolhost.exe"),
            vector_extension: root.join("missing-vec0.dll"),
            inference_library: root.join("missing-onnxruntime.dll"),
        };
        let runtime = GatewayRuntime::new(configuration);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let service = gateway::router(runtime.clone());
        let gateway_server = tokio::spawn(async move {
            axum::serve(listener, service).await.unwrap();
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(20))
            .build()
            .unwrap();
        let saved = client.post(format!("{url}/api/models")).json(&json!({"providerId":"native-fixture","displayName":"Isolated fixture",
            "baseUrl":provider_url,"apiKey":"","protocol":"openai-completions","models":["fixture-model"],"contextWindowTokens":32768,"maxOutputTokens":4096})).send().await.unwrap();
        assert!(
            saved.status().is_success(),
            "{}",
            saved.text().await.unwrap()
        );
        let catalog: Value = client
            .get(format!(
                "{url}/api/conversations/catalog?includeMessages=false"
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let conversation = new_id();
        let user_message_id = new_id();
        let initial_message = if mcp {
            "检查隔离 MCP；不可用时也继续回答。"
        } else if tool {
            "读取文件 fixture.txt 并告诉我内容。"
        } else if cancel {
            "开始取消验证。"
        } else {
            "你好"
        };
        // A formal work chat is retained only once it has a user message; mimic the desktop send contract.
        // 正式工作聊天有用户消息才保留；夹具遵循桌面发送合同，不假设空聊天会被持久化。
        let saved = client.put(format!("{url}/api/conversations/catalog")).json(&json!({"Revision":catalog["Revision"],"Chats":[],"Projects":[{
            "Id":new_id(),"Name":"Isolated work","FolderPath":root.join("work"),"Chats":[{"Id":conversation,"Title":"Native HTTP fixture","Draft":"","Messages":[{"Id":user_message_id,"Role":"user","Content":initial_message,"CreatedAt":"2026-10-12T00:00:00Z"}]}]}]})).send().await.unwrap();
        assert!(
            saved.status().is_success(),
            "{}",
            saved.text().await.unwrap()
        );
        let saved: Value = saved.json().await.unwrap();
        assert_eq!(
            saved["Projects"][0]["Chats"][0]["Id"], conversation,
            "{saved}"
        );
        Self {
            runtime,
            client,
            url,
            conversation,
            user_message_id,
            provider,
            servers: vec![provider_server, gateway_server],
            root,
        }
    }
    fn request(&self, message: &str) -> Value {
        json!({"conversationId":self.conversation,"requestId":new_id(),"userMessageId":self.user_message_id,"provider":"native-fixture","model":"fixture-model","message":message,"permissionMode":"smart"})
    }
    async fn messages(&self) -> Vec<Value> {
        let response: Value = self
            .client
            .get(format!(
                "{}/api/conversations/{}/messages?limit=200",
                self.url, self.conversation
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        response["messages"]
            .as_array()
            .unwrap_or_else(|| panic!("Unexpected message page: {response}"))
            .clone()
    }
    async fn close(self) {
        self.runtime.shutdown().await;
        for task in self.servers {
            task.abort();
            let _ = task.await;
        }
        // Native SQLite caches can retain test-only handles until process exit; preserve this isolated fixture for diagnostics.
        // 原生 SQLite 测试句柄可能保留到进程退出；仅保留独立临时夹具用于诊断，不触碰日常数据。
        assert!(self.root.starts_with(std::env::temp_dir()));
    }
}

fn event_values(buffer: &mut Vec<u8>) -> Vec<Value> {
    let mut events = Vec::new();
    // Decode only complete frames so a network packet may split a UTF-8 character.
    // 只解码完整事件帧，允许网络分包落在 UTF-8 中文字符内部。
    while let Some(end) = buffer.windows(2).position(|bytes| bytes == b"\n\n") {
        let frame = String::from_utf8(buffer[..end].to_vec()).unwrap();
        buffer.drain(..end + 2);
        for line in frame.lines().filter_map(|line| line.strip_prefix("data: ")) {
            if let Ok(value) = serde_json::from_str(line) {
                events.push(value);
            }
        }
    }
    events
}

/// A quiet round may omit prose, but it must never leave the next tool or reply behind a streaming segment.
/// 静默轮次可以省略正文，但不能遗留生成中的分段，挡住下一次工具事件或最终回复。
fn assert_settled_public_segments(events: &[Value]) {
    let mut segments = std::collections::HashMap::<String, Value>::new();
    for event in events {
        match event["type"].as_str() {
            Some("assistant_segment") => {
                let segment = &event["segment"];
                let id = segment["id"].as_str().expect("public segments need an identity");
                if !segments.contains_key(id) {
                    assert!(segments.values().all(|previous| previous["status"] != "streaming"),
                        "a new segment cannot overtake an unfinished prior round: {event}");
                }
                segments.insert(id.to_owned(), segment.clone());
            }
            Some("tool_call") => {
                assert!(segments.values().any(|segment| segment["round"] == event["tool"]["round"]
                    && segment["status"] == "completed" && segment["phase"] == "commentary"),
                    "tools need a settled commentary anchor in their actual round: {event}");
            }
            Some("completed") => {
                assert!(segments.values().all(|segment| segment["status"] != "streaming"),
                    "the final event must not preserve unfinished public rounds");
                assert!(event["assistantSegments"].as_array().unwrap().iter()
                    .all(|segment| segment["status"] != "streaming"),
                    "the durable final snapshot must also close every public round");
            }
            _ => {}
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_context_is_private_and_replayed_at_its_original_position_across_user_turns() {
    use kynxa_agent_core_native::storage::gateway as data;
    let fixture = Fixture::new(false, false).await;
    let goal = "你好";
    let mut inputs = Vec::new();
    for turn in 0..2 {
        let mut input = fixture.request(goal);
        if turn > 0 {
            let user = new_id();
            data::upsert_message(&fixture.root.join("Data"), &fixture.conversation,
                &json!({"Id":user,"Role":"user","Status":"completed","Content":goal})).unwrap();
            input["userMessageId"] = json!(user);
        }
        let response = fixture.client.post(format!("{}/api/chat/stream", fixture.url))
            .json(&input).send().await.unwrap();
        assert!(response.status().is_success());
        let public = response.text().await.unwrap();
        let events = event_values(&mut public.as_bytes().to_vec());
        assert!(events.iter().any(|event| event["type"] == "completed"), "{public}");
        assert!(!events.iter().any(|event| matches!(event["type"].as_str(), Some("error" | "interrupted"))), "{public}");
        for private_field in ["ModelTranscript", "contextBefore", "snapshotHash", "authorityHash", "<kynxa_runtime_context>"] {
            assert!(!public.contains(private_field), "Private runtime state leaked in SSE: {private_field}");
        }
        inputs.push(input);
    }
    let requests = fixture.provider.requests.lock().await.clone();
    assert_eq!(requests.len(), 2, "Runtime snapshots must not add planning or model requests.");
    let first = requests[0]["messages"].as_array().unwrap();
    let second = requests[1]["messages"].as_array().unwrap();
    let formal = data::read_model_messages(&fixture.root.join("Data"), &fixture.conversation).unwrap();
    let first_reply = formal.iter().find(|message| message["Role"] == "assistant"
        && message["RequestId"] == inputs[0]["requestId"]).unwrap();
    let snapshot = first_reply["ModelTranscript"]["rounds"].as_array().unwrap().iter()
        .flat_map(|round| round["contextBefore"].as_array().into_iter().flatten()).next().unwrap();
    assert_eq!(snapshot["message"]["role"], "user", "Unknown compatible routes keep runtime data out of new system/developer messages.");
    assert_eq!(snapshot["source"]["runtimeContext"], true);
    assert_eq!(snapshot["source"]["instructionAuthority"], false);
    assert_eq!(first.as_slice(), &second[..first.len()], "Unchanged old request messages must survive as an exact prefix.");
    let old_position = second.iter().position(|message| *message == snapshot["message"]).unwrap();
    let original_goal = second.iter().position(|message| message["role"] == "user" && message["content"] == goal).unwrap();
    let old_answer = second.iter().position(|message| message["role"] == "assistant" && message["content"] == first_reply["Content"]).unwrap();
    let new_goal = second.iter().rposition(|message| message["role"] == "user" && message["content"] == goal).unwrap();
    assert!(original_goal < old_position && old_position < old_answer && old_answer < new_goal);
    assert_eq!(second.iter().filter(|message| **message == snapshot["message"]).count(), 1);
    for request in &requests {
        assert_eq!(request["messages"].as_array().unwrap().iter().filter(|message| message["role"] == "system").count(), 1);
        assert!(!request["messages"].as_array().unwrap().iter().any(|message| message["role"] == "developer"));
        for unsupported in ["prompt_cache_key", "prompt_cache_options", "cache_control", "defer_loading"] {
            assert!(request.get(unsupported).is_none(), "Unknown routes must not receive unsupported provider controls: {unsupported}");
        }
    }
    let public = fixture.messages().await;
    assert_eq!(public.iter().filter(|message| message["Role"] == "user" && message["Content"] == goal).count(), 2);
    for private_field in ["ModelTranscript", "contextBefore", "snapshotHash", "authorityHash", "<kynxa_runtime_context>"] {
        assert!(!json!(public).to_string().contains(private_field), "Private runtime state leaked in public messages: {private_field}");
    }
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_output_limit_preserves_marked_body_continuation_in_final_and_formal_log() {
    let fixture = Fixture::new(false, false).await;
    fixture
        .provider
        .output_continuation
        .store(true, Ordering::Release);
    let input = fixture.request("你好");
    let response: Value = fixture
        .client
        .post(format!("{}/api/chat", fixture.url))
        .json(&input)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(response["type"], "completed", "{response}");
    // An output-length continuation of a plain text answer needs no separate tool-work completion review.
    // 普通文字回答的输出长度续写不需要单独的工具工作完成核验，只有两次正文生成。
    assert_eq!(fixture.provider.calls.load(Ordering::Acquire), 2);
    let segments = response["assistantSegments"].as_array().unwrap();
    assert_eq!(segments.len(), 2);
    assert_eq!(segments[0]["phase"], "commentary");
    assert_eq!(segments[0]["isAnswerContinuation"], true);
    assert_eq!(segments[0]["content"], "第一段正文：保留这一段。");
    assert_eq!(segments[1]["phase"], "final_answer");
    assert!(
        segments[1]["content"]
            .as_str()
            .unwrap()
            .contains("第二段正文")
    );
    assert!(!response["content"].as_str().unwrap().contains("kynxa_task"));
    let messages = fixture.messages().await;
    let assistant = messages
        .iter()
        .find(|message| message["Role"] == "assistant")
        .unwrap();
    assert_eq!(
        assistant["AssistantSegments"][0]["isAnswerContinuation"],
        true
    );
    assert!(
        assistant["Content"]
            .as_str()
            .unwrap()
            .contains("第一段正文")
    );
    assert!(
        assistant["Content"]
            .as_str()
            .unwrap()
            .contains("第二段正文")
    );
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn automatic_continuation_and_review_notes_do_not_change_compaction_user_goals() {
    let fixture = Fixture::new(true, false).await;
    fixture
        .provider
        .control_note_cycle
        .store(true, Ordering::Release);
    std::fs::write(fixture.root.join("work/second.txt"), "SECOND_CURRENT_FILE").unwrap();
    let config: Value = fixture
        .client
        .get(format!("{}/api/models", fixture.url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut profile = config["providers"][0].clone();
    profile["contextWindowTokens"] = json!(16384);
    profile["maxOutputTokens"] = json!(4096);
    profile["apiKey"] = json!("");
    let saved = fixture
        .client
        .post(format!("{}/api/models", fixture.url))
        .json(&profile)
        .send()
        .await
        .unwrap();
    assert_eq!(saved.status(), 200);
    let original_goal = "读取文件 fixture.txt 并告诉我内容。";
    let input = fixture.request(original_goal);
    let response: Value = fixture
        .client
        .post(format!("{}/api/chat", fixture.url))
        .json(&input)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(response["type"], "completed", "{response}");
    assert!(
        response["content"]
            .as_str()
            .unwrap()
            .contains("Both authorized current sources were checked")
    );
    let requests = fixture.provider.requests.lock().await;
    let main_requests: Vec<&Value> = requests
        .iter()
        .filter(|request| {
            !system_text(request)
                .contains("Summarize public evidence for continuation of the same user task")
        })
        .collect();
    assert_eq!(main_requests.len(), 7);
    assert!(
        main_requests[1]
            .to_string()
            .contains("Continue the original authorized task")
    );
    assert!(
        main_requests[3]
            .to_string()
            .contains("Broker: compare the original requested work")
    );
    assert!(
        main_requests[4]
            .to_string()
            .contains("Broker: verified remaining work")
    );
    let summaries: Vec<Value> = requests
        .iter()
        .filter(|request| {
            system_text(request)
                .contains("Summarize public evidence for continuation of the same user task")
        })
        .map(|request| {
            let message = request["messages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|message| message["role"] == "user")
                .unwrap();
            serde_json::from_str(message["content"].as_str().unwrap()).unwrap()
        })
        .collect();
    // Both opaque continuations force real current-step compaction after actual tool receipts.
    // 两次大原生续传均在真实工具回执之后触发压缩，程序提示只能作为导航，不能变成新用户目标。
    assert!(summaries.len() >= 2, "{requests:?}");
    for summary in &summaries {
        assert_eq!(summary["userGoals"], json!([original_goal]), "{summary}");
    }
    drop(requests);
    let repository = kynxa_agent_core_native::storage::task_repository::TaskRepository::open(
        &fixture.runtime.config.data_root,
    )
    .unwrap();
    let task = repository
        .find_by_request_id(&fixture.conversation, input["requestId"].as_str().unwrap())
        .unwrap()
        .unwrap();
    assert!(task.usage.compactions >= 2);
    assert_eq!(
        repository
            .operations(&task.task_id)
            .unwrap()
            .iter()
            .filter(|operation| operation.intent.tool_name == "filesystem.read"
                && operation.state
                    == kynxa_agent_core_native::storage::task_repository::OperationState::Succeeded)
            .count(),
        2,
        "Program guidance must not restart the source inventory or replay the original reads."
    );
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_result_http_wraps_media_and_paginates_the_public_utf16_view() {
    use kynxa_agent_core_native::storage::gateway as data;
    let fixture = Fixture::new(false, false).await;
    let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jJesAAAAASUVORK5CYII=";
    let canonical = json!({"content":[{"type":"image","mimeType":"image/png","data":png}],
        "structuredContent":{"notice":"前😀后"},"isError":false});
    let reference = data::save_tool_result(
        &fixture.root.join("Data"),
        &fixture.conversation,
        &new_id(),
        &json!({"id":"image-fixture","name":"computer.screenshot","arguments":{}}),
        &canonical,
        None,
    )
    .unwrap();
    let endpoint = format!(
        "{}/api/conversations/{}/tool-results/{}",
        fixture.url,
        fixture.conversation,
        reference["id"].as_str().unwrap()
    );
    let response = fixture.client.get(&endpoint).send().await.unwrap();
    assert_eq!(response.status(), 200);
    let complete: Value = response.json().await.unwrap();
    assert!(complete["result"].is_object(), "{complete}");
    assert_eq!(complete["result"]["content"][0]["data"], png);
    assert_eq!(complete["result"]["structuredContent"]["notice"], "前😀后");
    let projected = data::public_tool_result(&canonical, Some(&reference), None, false)
        .unwrap()
        .to_string();
    let total = projected.encode_utf16().count();
    let mut offset = 0;
    let mut assembled = String::new();
    loop {
        let response = fixture
            .client
            .get(&endpoint)
            .query(&[("offset", offset.to_string()), ("limit", "47".into())])
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let page: Value = response.json().await.unwrap();
        let text = page["text"].as_str().unwrap();
        assert_eq!(page["id"], reference["id"]);
        assert_eq!(page["resultRef"], reference);
        assert_eq!(page["offset"], offset);
        assert_eq!(page["totalCharacters"], total);
        assert_eq!(page["offsetUnit"], "utf16-code-units");
        assert!(!text.contains('\u{fffd}'));
        let next = page["nextOffset"].as_u64().unwrap() as usize;
        assert_eq!(next, offset + text.encode_utf16().count());
        assert_eq!(page["truncated"], next < total);
        assembled.push_str(text);
        if next == total {
            break;
        }
        assert!(next > offset);
        offset = next;
    }
    assert_eq!(assembled, projected);
    assert!(
        !assembled.contains(png),
        "Paged model view must retain only media references"
    );
    let emoji = projected[..projected.find('😀').unwrap()]
        .encode_utf16()
        .count();
    let response = fixture
        .client
        .get(&endpoint)
        .query(&[("offset", (emoji + 1).to_string()), ("limit", "1".into())])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let page: Value = response.json().await.unwrap();
    assert_eq!(page["offset"], emoji);
    assert_eq!(page["nextOffset"], emoji + 2);
    assert_eq!(page["text"], "😀");
    assert_eq!(
        data::load_private_tool_result(
            &fixture.root.join("Data"),
            &fixture.conversation,
            reference["id"].as_str().unwrap()
        )
        .unwrap()["canonical"],
        canonical
    );
    assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 0);
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_result_http_rejects_invalid_pages_foreign_scope_and_private_archives() {
    use kynxa_agent_core_native::storage::gateway as data;
    let fixture = Fixture::new(false, false).await;
    let root = fixture.root.join("Data");
    let reference = data::save_tool_result(
        &root,
        &fixture.conversation,
        &new_id(),
        &json!({"id":"text-fixture","name":"filesystem.read","arguments":{}}),
        &json!({"text":"SCOPED_RESULT_MUST_NOT_LEAK"}),
        None,
    )
    .unwrap();
    let endpoint = format!(
        "{}/api/conversations/{}/tool-results/{}",
        fixture.url,
        fixture.conversation,
        reference["id"].as_str().unwrap()
    );
    for query in [
        "offset=-1",
        "offset=+1",
        "offset=wrong",
        "offset=18446744073709551616",
        "offset=999999999",
        "limit=0",
        "limit=16001",
        "limit=-1",
        "limit=1.5",
        "limit=",
    ] {
        let response = fixture
            .client
            .get(format!("{endpoint}?{query}"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400, "{query}");
    }
    for query in ["offset=0", "limit=5"] {
        let response = fixture
            .client
            .get(format!("{endpoint}?{query}"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "{query}");
        assert!(response.json::<Value>().await.unwrap()["text"].is_string());
    }
    let invalid = fixture
        .client
        .get(format!(
            "{}/api/conversations/{}/tool-results/not-a-reference",
            fixture.url, fixture.conversation
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(invalid.status(), 400);
    let other = new_id();
    data::ensure_conversation(&root, &other, "other isolated chat").unwrap();
    for suffix in ["", "?offset=0&limit=16"] {
        let response = fixture
            .client
            .get(format!(
                "{}/api/conversations/{other}/tool-results/{}{suffix}",
                fixture.url,
                reference["id"].as_str().unwrap()
            ))
            .send()
            .await
            .unwrap();
        assert!(response.status().is_client_error());
        assert!(
            !response
                .text()
                .await
                .unwrap()
                .contains("SCOPED_RESULT_MUST_NOT_LEAK")
        );
    }
    let private = data::save_tool_result(
        &root,
        &fixture.conversation,
        &new_id(),
        &json!({"id":"private-fixture","name":"model.continuation","arguments":{}}),
        &json!({"text":"PRIVATE_CONTINUATION_MUST_NOT_LEAK"}),
        None,
    )
    .unwrap();
    for suffix in ["", "?offset=0&limit=16"] {
        let response = fixture
            .client
            .get(format!(
                "{}/api/conversations/{}/tool-results/{}{suffix}",
                fixture.url,
                fixture.conversation,
                private["id"].as_str().unwrap()
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 403);
        assert!(
            !response
                .text()
                .await
                .unwrap()
                .contains("PRIVATE_CONTINUATION_MUST_NOT_LEAK")
        );
    }
    assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 0);
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn message_attachments_reach_model_and_keep_original_user_message() {
    use kynxa_agent_core_native::storage::gateway as data;
    use base64::Engine;
    let fixture = Fixture::new(false, false).await;
    let doc = std::fs::read(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/documents/account-guide.docx")).unwrap();
    let png = base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jJesAAAAASUVORK5CYII=").unwrap();
    let message = new_id();
    let document_id = new_id();
    let image_id = new_id();
    let files = [(document_id.clone(), "作业.docx", doc), (image_id.clone(), "图.png", png)];
    let clipboard_directory = fixture.root.join("Temp").join("AttachmentDrafts");
    std::fs::create_dir_all(&clipboard_directory).unwrap();
    let source_paths = [fixture.root.join("work").join(files[0].1), clipboard_directory.join(files[1].1)];
    for ((_, _, bytes), path) in files.iter().zip(&source_paths) { std::fs::write(path, bytes).unwrap(); }
    data::upsert_message(&fixture.runtime.config.data_root, &fixture.conversation,
        &json!({"Id":message,"Role":"user","Content":"","Attachments":files.iter().map(|(id,name,bytes)|json!({"Id":id,"Name":name,"SizeBytes":bytes.len()})).collect::<Vec<_>>()})).unwrap();
    // Clipboard sources must be external temporary files; application-private sources remain forbidden.
    // 剪贴板来源必须使用外部临时文件，应用私有数据来源仍然禁止附加。
    let private_directory = fixture.runtime.config.data_root.join("Desktop").join("Temp").join("AttachmentDrafts");
    std::fs::create_dir_all(&private_directory).unwrap();
    let private_image = private_directory.join(files[1].1);
    std::fs::write(&private_image, &files[1].2).unwrap();
    let denied = fixture.client.post(format!("{}/api/conversations/{}/attachments/import", fixture.url, fixture.conversation))
        .json(&json!({"messageId":message,"files":[{"id":image_id,"name":files[1].1,"path":private_image}]}))
        .send().await.unwrap();
    assert_eq!(denied.status(), 403);
    assert_eq!(denied.json::<Value>().await.unwrap()["code"], "PROTECTED_APP_DATA");
    let response = fixture.client.post(format!("{}/api/conversations/{}/attachments/import", fixture.url, fixture.conversation))
        .json(&json!({"messageId":message,"files":files.iter().zip(&source_paths).map(|((id,name,_),path)|json!({"id":id,"name":name,"path":path})).collect::<Vec<_>>()}))
        .send().await.unwrap();
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    let imported: Value = response.json().await.unwrap();
    let relationship = data::relationship(&fixture.runtime.config.data_root, &fixture.conversation).unwrap();
    let project = relationship["projectId"].as_str().unwrap();
    // Aggregation preserves source message identities, stable paging and the existing message-specific route.
    // 聚合保留原消息身份、稳定分页以及已有消息级接口，不把附件重新绑定到当前聊天。
    for route in [format!("conversations/{}", fixture.conversation), format!("projects/{project}")] {
        let page: Value = fixture.client.get(format!("{}/api/{route}/attachments?limit=1", fixture.url))
            .send().await.unwrap().json().await.unwrap();
        assert_eq!(page["total"], 2, "{page}"); assert_eq!(page["hasMore"], true);
        assert_eq!(page["attachments"][0]["conversationId"], fixture.conversation);
        assert_eq!(page["attachments"][0]["messageId"], message);
        let next: Value = fixture.client.get(format!("{}/api/{route}/attachments?limit=1&offset=1", fixture.url))
            .send().await.unwrap().json().await.unwrap();
        assert_eq!(next["hasMore"], false, "{next}");
        assert_ne!(page["attachments"][0]["id"], next["attachments"][0]["id"]);
    }
    let legacy: Value = fixture.client.get(format!("{}/api/conversations/{}/attachments?messageId={message}",
        fixture.url, fixture.conversation)).send().await.unwrap().json().await.unwrap();
    assert_eq!(legacy["attachments"].as_array().unwrap().len(), 2);
    let version = imported["attachments"].as_array().unwrap().iter().find(|item| item["id"] == document_id)
        .unwrap()["sha256"].as_str().unwrap();
    let preview_url = format!("{}/api/conversations/{}/attachments/{document_id}/preview?messageId={message}&version={version}", fixture.url, fixture.conversation);
    let preview: Value = fixture.client.get(&preview_url).send().await.unwrap().json().await.unwrap();
    assert_eq!(preview["kind"], "docx", "{preview}");
    assert!(preview.get("text").is_none(), "direct previews must not wait for text extraction");
    let original_url = format!("{}{}", fixture.url, preview["contentUrl"].as_str().unwrap());
    let original = fixture.client.get(&original_url).send().await.unwrap();
    assert_eq!(original.status(), 200);
    assert_eq!(original.bytes().await.unwrap().as_ref(), files[0].2.as_slice());
    assert_eq!(fixture.client.get(preview_url.replace(version, "stale")).send().await.unwrap().status(), 409);
    let unrelated = fixture.client.get(format!("{}/api/conversations/{}/attachments/{document_id}/preview?messageId={message}", fixture.url, new_id()))
        .send().await.unwrap();
    assert_ne!(unrelated.status(), 200);
    // A prepared page representation remains version-bound and shares the same authorized attachment route.
    // 已准备的页面表示仍绑定原件版本，并复用同一消息范围附件授权，不开放私有文件路径。
    let cache = fixture.runtime.config.data_root.join("Cache/Previews/office-pdf-v1");
    std::fs::create_dir_all(&cache).unwrap();
    let pdf = b"%PDF-1.4\n% cached page fixture\n%%EOF";
    std::fs::write(cache.join(format!("{version}.pdf")), pdf).unwrap();
    let converted: Value = fixture.client.get(format!("{preview_url}&representation=pdf")).send().await.unwrap().json().await.unwrap();
    assert_eq!(converted["kind"], "pdf", "{converted}");
    let converted_response = fixture.client.get(format!("{}{}", fixture.url, converted["contentUrl"].as_str().unwrap())).send().await.unwrap();
    assert_eq!(converted_response.headers()["content-type"], "application/pdf");
    assert_eq!(converted_response.bytes().await.unwrap().as_ref(), pdf);
    let mut input = fixture.request("");
    input["userMessageId"] = json!(message);
    input["attachmentIds"] = json!([document_id, image_id]);
    let response: Value = fixture.client.post(format!("{}/api/chat", fixture.url)).json(&input).send().await.unwrap().json().await.unwrap();
    assert_eq!(response["type"], "completed", "{response}");
    let usage: Value = fixture.client.get(format!("{}/api/usage/summary?conversationId={}", fixture.url, fixture.conversation))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(usage["scope"], "conversation", "{usage}");
    assert_eq!(usage["conversationId"], fixture.conversation);
    assert_eq!(usage["projectId"], project);
    assert!(usage["modelRequests"].as_u64().is_some_and(|value| value > 0), "{usage}");
    assert_eq!(usage["knownCacheRequests"], 0, "missing cache receipts cannot become measured misses");
    assert!(usage["cacheCreationTokens"].is_null() && usage["uncachedInputTokens"].is_null());
    let denied = fixture.client.get(format!("{}/api/usage/summary?conversationId={}&projectId={}",
        fixture.url, fixture.conversation, new_id())).send().await.unwrap();
    assert_eq!(denied.status(), 403);
    let requests = fixture.provider.requests.lock().await;
    let wire = requests.first().unwrap();
    assert!(wire.to_string().contains("How to reset an account password"), "{wire}");
    assert!(!wire.to_string().contains("attachmentMessageId"));
    assert!(!wire.to_string().contains("deferredAttachmentImages"));
    assert!(wire["messages"].as_array().unwrap().iter().any(|message| message["content"].as_array()
        .is_some_and(|parts| parts.iter().any(|part| part["type"] == "image_url"))));
    drop(requests);
    let stored = data::read_messages(&fixture.runtime.config.data_root, &fixture.conversation, true).unwrap();
    let original = stored.iter().find(|item| item["Id"] == message).unwrap();
    assert_eq!(original["Content"], "");
    assert_eq!(original["Attachments"].as_array().unwrap().len(), 2);
    assert!(!original.to_string().contains("base64"));
    let markup_message = new_id();
    let markup = [(new_id(), "图表.svg", "<svg xmlns='http://www.w3.org/2000/svg'><rect width='20' height='10' fill='red'/></svg>"),
        (new_id(), "页面.html", "<!doctype html><style>body{background:#fafafa}h1{color:#146e49}</style><h1>预览样式</h1><button onclick='this.textContent=Number(this.textContent)+1'>0</button><script>document.body.dataset.chart='ready'</script>")];
    data::upsert_message(&fixture.runtime.config.data_root, &fixture.conversation,
        &json!({"Id":markup_message,"Role":"user","Content":"preview markup","Attachments":markup.iter().map(|(id,name,text)|json!({"Id":id,"Name":name,"SizeBytes":text.len()})).collect::<Vec<_>>()})).unwrap();
    for (id, name, text) in markup {
        std::fs::write(fixture.root.join("work").join(name), text).unwrap();
        let imported: Value = fixture.client.post(format!("{}/api/conversations/{}/attachments/import", fixture.url, fixture.conversation))
            .json(&json!({"messageId":markup_message,"files":[{"id":id,"name":name,"path":fixture.root.join("work").join(name)}]})).send().await.unwrap().json().await.unwrap();
        let sha = imported["attachments"][0]["sha256"].as_str().unwrap();
        let content_url = format!("{}/api/conversations/{}/attachments/{id}/content?messageId={markup_message}", fixture.url, fixture.conversation);
        let response = fixture.client.get(format!("{content_url}&version={sha}")).send().await.unwrap();
        assert_eq!(response.status(), 200);
        let policy = response.headers().get("Content-Security-Policy").unwrap().to_str().unwrap();
        assert!(policy.contains("sandbox") && policy.contains("style-src 'unsafe-inline'"));
        assert!(!policy.contains("allow-same-origin") && !policy.contains("allow-top-navigation"));
        if name.ends_with(".html") { assert!(policy.contains("sandbox allow-scripts") && policy.contains("script-src 'unsafe-inline'") && policy.contains("connect-src 'none'")); }
        else { assert!(!policy.contains("allow-scripts")); }
        assert_eq!(response.text().await.unwrap(), text);
        assert_eq!(fixture.client.get(format!("{content_url}&version=stale")).send().await.unwrap().status(), 409);
    }
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn greeting_reuses_request_and_persists_original_messages() {
    let fixture = Fixture::new(false, false).await;
    let health: Value = fixture
        .client
        .get(format!("{}/health", fixture.url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["runtime"], "rust");
    assert_eq!(health["nodeRuntime"], false);
    let forbidden = fixture
        .client
        .get(format!("{}/health", fixture.url))
        .header("Origin", "https://untrusted.invalid")
        .send()
        .await
        .unwrap();
    assert_eq!(forbidden.status(), reqwest::StatusCode::FORBIDDEN);
    let input = fixture.request("你好");
    for _ in 0..2 {
        let value: Value = fixture
            .client
            .post(format!("{}/api/chat", fixture.url))
            .json(&input)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(value["type"], "completed", "{value}");
        assert_eq!(value["content"], "你好！");
    }
    assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 1);
    let messages = fixture.messages().await;
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["Content"], "你好");
    assert_eq!(messages[1]["Content"], "你好！");
    let timing = &messages[1]["ToolRun"]["diagnostics"]["requestTiming"];
    assert!(timing["acceptedToModelMs"].is_u64());
    assert!(timing["modelLoopMs"].is_u64());
    assert!(
        timing["acceptedToFinalizationMs"].as_u64().unwrap()
            >= timing["acceptedToModelMs"].as_u64().unwrap()
    );
    assert_eq!(timing["includesDesktopCatalogSave"], false);
    assert_eq!(timing["includesFinalPersistence"], false);
    let stages = &timing["preparationStages"];
    assert_eq!(stages["automaticEvidenceSkipped"], true);
    assert_eq!(stages["preparationParallel"], true);
    assert!(stages["automaticEvidenceMs"].is_u64());
    assert!(stages["historyAndAttachmentsMs"].is_u64());
    // A finite greeting with an artificial time demand still finishes in one actual model request.
    // 有限问候即使附加人为时长要求，也只需一次实际模型请求，不能空等或循环凑足一百小时。
    let mut finite_input = fixture.request("请至少连续说你好100个小时");
    finite_input["userMessageId"] = json!(new_id());
    let finite_result: Value = tokio::time::timeout(Duration::from_secs(20), async {
        fixture.client.post(format!("{}/api/chat", fixture.url)).json(&finite_input)
            .send().await.unwrap().json().await.unwrap()
    }).await.expect("A finite greeting must not become a time-filling task.");
    assert_eq!(finite_result["type"], "completed", "{finite_result}");
    assert_eq!(finite_result["content"], "你好！");
    assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 2);
    let messages = fixture.messages().await;
    assert_eq!(messages.iter().filter(|message| message["Role"] == "user"
        && message["Content"] == finite_input["message"]).count(), 1);
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn parallel_preparation_keeps_authorized_local_evidence_in_the_first_model_request() {
    use kynxa_agent_core_native::retrieval::gateway_index;

    let fixture = Fixture::new(false, false).await;
    let settings: Value = fixture.client.get(format!("{}/api/retrieval/settings", fixture.url))
        .send().await.unwrap().json().await.unwrap();
    let saved = fixture.client.patch(format!("{}/api/retrieval/settings", fixture.url))
        .json(&json!({"expectedRevision":settings["revision"],
            "patch":{"local":{"semantic":"off","rerankProfileId":null}}}))
        .send().await.unwrap();
    assert!(saved.status().is_success(), "{}", saved.text().await.unwrap());
    let mut catalog: Value = fixture.client
        .get(format!("{}/api/conversations/catalog?includeMessages=false", fixture.url))
        .send().await.unwrap().json().await.unwrap();
    let private_chat = new_id();
    std::fs::create_dir(fixture.root.join("other-work")).unwrap();
    catalog["Projects"].as_array_mut().unwrap().push(json!({
        "Id":new_id(),"Name":"Other isolated work","FolderPath":fixture.root.join("other-work"),
        "Chats":[{"Id":private_chat,"Title":"Private fixture","Messages":[{
            "Id":new_id(),"Role":"user","Content":"Private fixture conversation",
            "CreatedAt":"2026-10-16T00:00:00Z"}]}]
    }));
    let saved = fixture.client.put(format!("{}/api/conversations/catalog", fixture.url))
        .json(&catalog).send().await.unwrap();
    assert!(saved.status().is_success(), "{}", saved.text().await.unwrap());
    // Use real indexed fragments in distinct formal scopes; no embedding model or synthetic search receipt is needed.
    // 在不同正式范围发布真实索引片段；此检查不运行嵌入模型，也不伪造搜索回执。
    let prepared = [
        (&fixture.conversation, "authorized-local", "auroraaccel 的索引队列采用 LOCAL_EVIDENCE_431 方案。"),
        (&private_chat, "private-local", "auroraaccel 的索引队列采用 PRIVATE_EVIDENCE_972 方案。"),
    ].into_iter().map(|(chat, source, text)| {
        gateway_index::prepare_source(&json!({"sourceId":source,
            "scopeKey":format!("chat:{chat}"),"sourceType":"document",
            "title":"auroraaccel 本地资料","text":text}), None).unwrap()
    }).collect::<Vec<_>>();
    gateway_index::publish_prepared(&fixture.runtime.config, &prepared).unwrap();
    let mut input = fixture.request("根据本地文档，auroraaccel 的索引队列采用什么方案？");
    input["userMessageId"] = json!(new_id());
    let response: Value = fixture.client.post(format!("{}/api/chat", fixture.url))
        .json(&input).send().await.unwrap().json().await.unwrap();
    assert_eq!(response["type"], "completed", "{response}");
    let requests = fixture.provider.requests.lock().await;
    let wire = requests.first().unwrap().to_string();
    assert!(wire.contains("LOCAL_EVIDENCE_431"), "{wire}");
    assert!(!wire.contains("PRIVATE_EVIDENCE_972"), "{wire}");
    drop(requests);
    let messages = fixture.messages().await;
    let assistant = messages.iter().find(|message| message["Id"] == input["requestId"]).unwrap();
    assert!(assistant["RetrievalResultRef"].is_object(), "{assistant}");
    let stages = &assistant["ToolRun"]["diagnostics"]["requestTiming"]["preparationStages"];
    assert_eq!(stages["automaticEvidenceSkipped"], false);
    assert_eq!(stages["preparationParallel"], true);
    assert!(stages["automaticEvidenceMs"].is_u64());
    assert!(stages["historyAndAttachmentsMs"].is_u64());
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_completed_chat_generates_a_source_bound_draft_without_blocking_the_reply() {
    let fixture = Fixture::new(false, false).await;
    let data = fixture.root.join("Data");
    let mut expected_users = Vec::new();
    for index in 0..11 {
        let user = new_id();
        expected_users.push(user.clone());
        kynxa_agent_core_native::storage::gateway::upsert_message(&data, &fixture.conversation, &json!({"Id":user,"Role":"user","Status":"completed","Content":format!("我喜欢第 {index} 种合成展示约定。")})).unwrap();
        kynxa_agent_core_native::storage::gateway::upsert_message(&data, &fixture.conversation, &json!({"Id":new_id(),"Role":"assistant","Status":"completed","Content":"已收到合成历史讨论。"})).unwrap();
    }
    let mut catalog: Value = fixture
        .client
        .get(format!(
            "{}/api/conversations/catalog?includeMessages=false",
            fixture.url
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let user = new_id();
    let text = "我喜欢最后一项合成约定。";
    expected_users.push(user.clone());
    catalog["Projects"][0]["Chats"][0]["Messages"] =
        json!([{"Id":user,"Role":"user","Content":text,"CreatedAt":"2026-10-13T00:00:00Z"}]);
    let saved = fixture
        .client
        .put(format!("{}/api/conversations/catalog", fixture.url))
        .json(&catalog)
        .send()
        .await
        .unwrap();
    assert!(
        saved.status().is_success(),
        "{}",
        saved.text().await.unwrap()
    );
    let mut request = fixture.request(text);
    request["userMessageId"] = json!(user);
    let completed: Value = fixture
        .client
        .post(format!("{}/api/chat", fixture.url))
        .json(&request)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(completed["type"], "completed", "{completed}");
    assert_eq!(completed["content"], "你好！");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let memory = loop {
        let candidate_response = fixture
            .client
            .get(format!("{}/api/memory/candidates", fixture.url))
            .send()
            .await
            .unwrap();
        let candidate_status = candidate_response.status();
        let status: Value = candidate_response.json().await.unwrap();
        assert!(
            candidate_status.is_success(),
            "candidate status {candidate_status}: {status}"
        );
        let memory_response = fixture
            .client
            .get(format!(
                "{}/api/conversations/{}/memory",
                fixture.url, fixture.conversation
            ))
            .send()
            .await
            .unwrap();
        let memory_status = memory_response.status();
        let memory: Value = memory_response.json().await.unwrap();
        assert!(
            memory_status.is_success(),
            "memory status {memory_status}: {memory}"
        );
        let scopes = memory["scopes"]
            .as_array()
            .unwrap_or_else(|| panic!("memory response is missing scopes: {memory}"));
        if scopes.iter().any(|scope| {
            scope["scope"] == "chat" && !scope["entries"].as_array().unwrap().is_empty()
        }) {
            assert_eq!(status["settings"]["enabled"], true);
            break memory;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "background candidate did not appear: {status}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let chat = memory["scopes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|scope| scope["scope"] == "chat")
        .unwrap();
    assert_eq!(chat["entries"].as_array().unwrap().len(), 1);
    let entry = &chat["entries"][0];
    assert_eq!(entry["status"], "draft");
    assert_eq!(entry["source"]["conversationId"], fixture.conversation);
    assert_eq!(entry["source"]["messageId"], user);
    assert_eq!(entry["candidate"]["batchThroughMessageId"], user);
    assert_eq!(entry["candidate"]["quotes"].as_array().unwrap().len(), 12);
    assert!(entry["candidate"]["quotes"].as_array().unwrap().iter().all(|quote| expected_users.contains(&quote["messageId"].as_str().unwrap().to_string())));
    assert_eq!(entry["active"], false);
    assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 1);
    let requests = fixture.provider.requests.lock().await;
    assert!(!system_text(&requests[0]).contains("我喜欢第 0 种合成展示约定。"));
    assert!(!system_text(&requests[0]).contains(text));
    drop(requests);
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn memory_revocation_during_approval_skips_the_entire_stale_step_and_keeps_prior_progress() {
    use kynxa_agent_core_native::storage::gateway as data;
    let fixture=Fixture::new(false,false).await;
    let outside=fixture.root.join("outside/memory-approval-stale.txt");
    std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
    *fixture.provider.memory_approval_target.lock().await=Some(outside.to_string_lossy().into_owned());
    let memory_user=new_id();
    let remembered=data::upsert_message(&fixture.root.join("Data"),&fixture.conversation,&json!({
        "Id":memory_user,"Role":"user","Status":"completed",
        "Content":"全局记住：本次文件操作遵循 MEMORY_APPROVAL_OLD_POLICY_v1，先读取原文件，再写入两个旧计划文件；记忆撤销后停止旧计划，只保留已读内容。"})).unwrap();
    assert_eq!(remembered["memoryInstruction"]["entry"]["status"],"confirmed");
    let goal="先读取 fixture.txt，根据当前已确认全局记忆决定是否写入两个文件；记忆撤销后只报告已读内容，不能继续旧计划。";
    let user=new_id();
    data::upsert_message(&fixture.root.join("Data"),&fixture.conversation,&json!({"Id":user,"Role":"user","Status":"completed","Content":goal})).unwrap();
    let mut input=fixture.request(goal);input["userMessageId"]=json!(user);
    let response=fixture.client.post(format!("{}/api/chat/stream",fixture.url)).json(&input).send().await.unwrap();
    assert!(response.status().is_success());
    let mut stream=response.bytes_stream();let mut buffer=Vec::new();let mut events=Vec::new();
    let approval=tokio::time::timeout(Duration::from_secs(10),async {
        loop {
            let bytes=stream.next().await.expect("stream ended before the real approval").unwrap();
            buffer.extend_from_slice(&bytes);
            let received=event_values(&mut buffer);
            let approval=received.iter().find(|event|event["type"]=="approval_required").cloned();
            events.extend(received);
            if let Some(approval)=approval {break approval;}
        }
    }).await.expect("the actual outside-workspace write must request approval");
    assert_eq!(approval["tool"]["toolCallId"],"memory-approval-stale");
    assert!(events.iter().any(|event|event["type"]=="tool_result" && event["tool"]["toolCallId"]=="memory-prior-read" && event["tool"]["status"]=="completed"));
    assert!(!outside.exists());assert!(!fixture.root.join("work/memory-stale-after.txt").exists());
    let memory:Value=fixture.client.get(format!("{}/api/memory/user",fixture.url)).send().await.unwrap().json().await.unwrap();
    let entry=memory["entries"].as_array().unwrap().iter().find(|entry|entry["content"].as_str().is_some_and(|s|s.contains("MEMORY_APPROVAL_OLD_POLICY_v1"))).unwrap();
    let deleted=fixture.client.delete(format!("{}/api/memory/user/{}",fixture.url,entry["id"].as_str().unwrap()))
        .json(&json!({"expectedRevision":memory["revision"]})).send().await.unwrap();
    assert!(deleted.status().is_success(),"{}",deleted.text().await.unwrap());
    let approved=fixture.client.post(format!("{}/api/agent/approvals",fixture.url)).json(&json!({
        "approvalId":approval["tool"]["approvalId"],"conversationId":fixture.conversation,
        "requestId":input["requestId"],"toolCallId":"memory-approval-stale","approved":true})).send().await.unwrap();
    assert!(approved.status().is_success(),"{}",approved.text().await.unwrap());
    tokio::time::timeout(Duration::from_secs(10),async {
        while let Some(bytes)=stream.next().await {buffer.extend_from_slice(&bytes.unwrap());events.extend(event_values(&mut buffer));}
    }).await.expect("memory refresh must continue the same request after approval");
    let stale_ui=events.iter().filter(|event|event["type"]=="tool_result" && event["tool"]["toolCallId"]=="memory-approval-stale").collect::<Vec<_>>();
    assert_eq!(stale_ui.len(),1,"The announced approval call needs one public completion receipt");
    assert_eq!(stale_ui[0]["tool"]["code"],"TASK_MEMORY_UPDATED");
    assert_eq!(stale_ui[0]["tool"]["executed"],false,"Approving an obsolete call must not execute it");
    let archive=data::load_private_tool_result(&fixture.root.join("Data"),&fixture.conversation,stale_ui[0]["tool"]["resultRef"]["id"].as_str().unwrap()).unwrap();
    assert_eq!(archive["canonical"]["code"],"TASK_MEMORY_UPDATED");assert_eq!(archive["canonical"]["executed"],false);
    assert!(!outside.exists());assert!(!fixture.root.join("work/memory-stale-after.txt").exists());
    assert!(!events.iter().any(|event|["error","interrupted"].contains(&event["type"].as_str().unwrap_or(""))),"{events:?}");
    let completed=events.iter().find(|event|event["type"]=="completed").expect("same request must complete after replanning");
    assert_eq!(completed["requestId"],input["requestId"]);
    assert!(completed["content"].as_str().unwrap().contains("native-file-body"));
    let requests=fixture.provider.requests.lock().await.clone();
    let refreshed=requests.iter().find(|request|request["messages"].as_array().into_iter().flatten()
        .any(|message|message["role"]=="tool" && message["tool_call_id"]=="memory-approval-stale")).unwrap();
    // Remaining calls were never announced as public activities, but still require exact model protocol pairing.
    // 尚未对界面宣布的后续调用仍须在模型协议中逐项配对，不能据此虚构额外界面活动。
    for id in ["memory-approval-stale","memory-after-stale"] {
        let paired=refreshed["messages"].as_array().unwrap().iter().filter(|message|message["role"]=="tool" && message["tool_call_id"]==id).collect::<Vec<_>>();
        assert_eq!(paired.len(),1,"Each old-plan call needs exactly one model protocol result");
        let canonical:Value=serde_json::from_str(paired[0]["content"].as_str().unwrap()).unwrap();
        assert_eq!(canonical["code"],"TASK_MEMORY_UPDATED");assert_eq!(canonical["executed"],false);
    }
    assert!(!system_text(refreshed).contains("MEMORY_APPROVAL_OLD_POLICY_v1"));
    assert!(refreshed["messages"].to_string().contains("native-file-body"));
    // Revocation removes obsolete runtime projections from the next request, while the private log retains their history.
    // 撤销后下一请求移除旧运行投影，私有正式日志仍保留历史；既有成功读取不得重做或丢失。
    let formal = data::read_model_messages(&fixture.root.join("Data"), &fixture.conversation).unwrap();
    let snapshots = formal.iter().filter(|message| message["Role"] == "assistant")
        .flat_map(|message| message["ModelTranscript"]["rounds"].as_array().into_iter().flatten())
        .flat_map(|round| round["contextBefore"].as_array().into_iter().flatten()).collect::<Vec<_>>();
    let authorities = snapshots.iter().map(|snapshot| snapshot["source"]["authorityHash"].as_str().unwrap())
        .collect::<std::collections::HashSet<_>>();
    assert!(authorities.len() >= 2, "Confirmed memory revocation must change the runtime authority fence.");
    let current_snapshot = snapshots.last().unwrap();
    let runtime_messages = refreshed["messages"].as_array().unwrap().iter().filter(|message|
        message["content"].as_str().is_some_and(|text| text.starts_with("<kynxa_runtime_context>"))).collect::<Vec<_>>();
    assert_eq!(runtime_messages.len(), 1, "The obsolete runtime projection must not remain beside its replacement.");
    assert_eq!(*runtime_messages[0], current_snapshot["message"]);
    let messages=fixture.messages().await;
    let prior=messages.iter().flat_map(|message|message["ToolActivities"].as_array().into_iter().flatten())
        .find(|tool|tool["toolCallId"]=="memory-prior-read").expect("successful prior receipt must remain in the formal log");
    assert_eq!(prior["status"],"completed");assert!(prior["resultRef"]["id"].is_string());
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_remember_survives_disabled_candidates_replay_and_a_later_memory_rejection() {
    let fixture = Fixture::new(false, false).await;
    let settings: Value = fixture
        .client
        .patch(format!("{}/api/memory/candidates/settings", fixture.url))
        .json(&json!({"expectedRevision":0,"patch":{"enabled":false}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(settings["settings"]["enabled"], false);
    let user = new_id();
    let text = "全局记住：回答使用中文😀。";
    let saved = kynxa_agent_core_native::storage::gateway::upsert_message(
        &fixture.root.join("Data"),
        &fixture.conversation,
        &json!({"Id":user,"Role":"user","Status":"completed","Content":text}),
    )
    .unwrap();
    assert_eq!(saved["memoryInstruction"]["created"], true);
    assert_eq!(saved["memoryInstruction"]["entry"]["status"], "confirmed");
    let mut request = fixture.request(text);
    request["userMessageId"] = json!(user);
    for _ in 0..2 {
        let completed: Value = fixture
            .client
            .post(format!("{}/api/chat", fixture.url))
            .json(&request)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(completed["type"], "completed", "{completed}");
        assert_eq!(completed["content"], "你好！");
    }
    assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 1);
    let rejected_user = new_id();
    let oversized = format!("全局记住：{}", "长".repeat(4001));
    let rejected = kynxa_agent_core_native::storage::gateway::upsert_message(
        &fixture.root.join("Data"),
        &fixture.conversation,
        &json!({"Id":rejected_user,"Role":"user","Status":"completed","Content":oversized}),
    )
    .unwrap();
    assert_eq!(rejected["memoryInstruction"]["reason"], "save-failed");
    assert_eq!(
        rejected["memoryInstruction"]["error"]["code"],
        "INVALID_MEMORY"
    );
    let mut ordinary_reply = fixture.request(&oversized);
    ordinary_reply["userMessageId"] = json!(rejected_user);
    let completed: Value = fixture
        .client
        .post(format!("{}/api/chat", fixture.url))
        .json(&ordinary_reply)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(completed["type"], "completed", "{completed}");
    let memory: Value = fixture
        .client
        .get(format!("{}/api/memory/user", fixture.url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(memory["entries"].as_array().unwrap().len(), 1);
    assert_eq!(memory["entries"][0]["content"], "回答使用中文😀。");
    assert_eq!(memory["entries"][0]["status"], "confirmed");
    assert_eq!(memory["entries"][0]["source"]["messageId"], user);
    assert_eq!(memory["candidateSettings"]["enabled"], false);
    assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 2);
    let messages = fixture.messages().await;
    assert!(
        messages
            .iter()
            .any(|message| message["Id"] == rejected_user && message["Content"] == oversized)
    );
    let deleted = fixture
        .client
        .delete(format!(
            "{}/api/memory/user/{}",
            fixture.url,
            memory["entries"][0]["id"].as_str().unwrap()
        ))
        .json(&json!({"expectedRevision":memory["revision"]}))
        .send()
        .await
        .unwrap();
    assert!(
        deleted.status().is_success(),
        "{}",
        deleted.text().await.unwrap()
    );
    let follow_up_user = new_id();
    let follow_up_text = "验证撤销后的普通聊天回复。";
    kynxa_agent_core_native::storage::gateway::upsert_message(
        &fixture.root.join("Data"),
        &fixture.conversation,
        &json!({"Id":follow_up_user,"Role":"user","Status":"completed","Content":follow_up_text}),
    )
    .unwrap();
    let mut follow_up = fixture.request(follow_up_text);
    follow_up["userMessageId"] = json!(follow_up_user);
    let completed: Value = fixture
        .client
        .post(format!("{}/api/chat", fixture.url))
        .json(&follow_up)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(completed["type"], "completed", "{completed}");
    let requests = fixture.provider.requests.lock().await;
    assert_eq!(requests.len(), 3);
    assert!(system_text(&requests[0]).contains("回答使用中文😀。"));
    assert!(system_text(&requests[1]).contains("回答使用中文😀。"));
    assert!(!system_text(&requests[2]).contains("回答使用中文😀。"));
    drop(requests);
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_model_identity_tracks_switches_without_rewriting_history_or_confirmed_memory() {
    use kynxa_agent_core_native::storage::gateway as data;

    let fixture = Fixture::new(false, false).await;
    let config: Value = fixture
        .client
        .get(format!("{}/api/models", fixture.url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut profile = config["providers"][0].clone();
    let synthetic_key = "runtime-identity-fixture-private-key";
    profile["displayName"] = json!("Anthropic Claude stale fixture label");
    profile["models"] = json!(["fixture-model", "second-model"]);
    profile["apiKey"] = json!(synthetic_key);
    let saved = fixture
        .client
        .post(format!("{}/api/models", fixture.url))
        .json(&profile)
        .send()
        .await
        .unwrap();
    assert_eq!(saved.status(), 200);

    // Stale identities stay as historical data; switching must update the trusted request metadata.
    // 旧身份保留为历史资料；换模必须更新本次请求可信元数据，不能靠篡改原文消除冲突。
    let old_answer = "我是 Anthropic 的 Claude，这是一条旧模型的合成回复。";
    let old_assistant = new_id();
    data::upsert_message(
        &fixture.runtime.config.data_root,
        &fixture.conversation,
        &json!({"Id":old_assistant,"Role":"assistant","Status":"completed","Content":old_answer,
            "Provider":"obsolete-fixture","Model":"obsolete-claude"}),
    )
    .unwrap();
    let memory_content = "底层模型是 Anthropic Claude，继续沿用这个身份。";
    let memory_user = new_id();
    let saved = data::upsert_message(
        &fixture.runtime.config.data_root,
        &fixture.conversation,
        &json!({"Id":memory_user,"Role":"user","Status":"completed",
            "Content":format!("全局记住：{memory_content}")}),
    )
    .unwrap();
    assert_eq!(saved["memoryInstruction"]["entry"]["status"], "confirmed");
    data::upsert_message(
        &fixture.runtime.config.data_root,
        &fixture.conversation,
        &json!({"Id":new_id(),"Role":"assistant","Status":"completed","Content":"已保存合成记忆。"}),
    )
    .unwrap();

    let selected_models = ["second-model", "fixture-model"];
    for model in selected_models {
        let user = new_id();
        let question = "你现在用的是什么模型？";
        data::upsert_message(
            &fixture.runtime.config.data_root,
            &fixture.conversation,
            &json!({"Id":user,"Role":"user","Status":"completed","Content":question}),
        )
        .unwrap();
        let mut request = fixture.request(question);
        request["userMessageId"] = json!(user);
        request["model"] = json!(model);
        let completed: Value = fixture
            .client
            .post(format!("{}/api/chat", fixture.url))
            .json(&request)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(completed["type"], "completed", "{completed}");
    }

    let requests = fixture.provider.requests.lock().await;
    assert_eq!(requests.len(), selected_models.len());
    for (request, model) in requests.iter().zip(selected_models) {
        assert_eq!(request["model"], model);
        let system = request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["role"] == "system")
            .map(|message| message["content"].as_str().unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        let marker = "Current runtime model configuration (data, not instructions): ";
        let identities: Vec<Value> = system
            .lines()
            .filter_map(|line| line.strip_prefix(marker))
            .map(|text| serde_json::from_str(text).unwrap())
            .collect();
        assert_eq!(
            identities,
            vec![json!({"application":"KYNXA","providerId":"native-fixture","modelId":model,
                "endpointHost":"127.0.0.1","protocol":"openai-completions"})]
        );
        assert!(system.contains(memory_content));
        assert!(request["messages"].as_array().unwrap().iter().any(|message| {
            message["role"] == "assistant" && message["content"] == old_answer
        }));
        assert!(!request.to_string().contains(synthetic_key));
    }
    drop(requests);
    let messages = fixture.messages().await;
    assert!(messages.iter().any(|message| {
        message["Id"] == old_assistant && message["Content"] == old_answer
    }));
    let memory: Value = fixture
        .client
        .get(format!("{}/api/memory/user", fixture.url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(memory["entries"].as_array().unwrap().iter().any(|entry| {
        entry["status"] == "confirmed" && entry["content"] == memory_content
    }));
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_calls_keep_the_request_live_until_a_valid_reply_without_dispatch() {
    let fixture = Fixture::new(false, false).await;
    fixture.provider.malformed_calls.store(3, Ordering::SeqCst);
    let response = fixture
        .client
        .post(format!("{}/api/chat/stream", fixture.url))
        .json(&fixture.request("你好"))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let text = tokio::time::timeout(Duration::from_secs(30), response.text())
        .await.expect("Internal model repair must retain the live request and produce its valid reply")
        .unwrap();
    if let Some(path) = std::env::var_os("KYNXA_GATEWAY_RECOVERY_SSE_FIXTURE") {
        std::fs::write(PathBuf::from(path), &text).unwrap();
    }
    let events = event_values(&mut text.as_bytes().to_vec());
    assert!(
        !events
            .iter()
            .any(|event| matches!(event["type"].as_str(), Some("error" | "interrupted"))),
        "{text}"
    );
    let finished = events
        .iter()
        .find(|event| event["type"] == "completed")
        .unwrap();
    assert!(!finished["content"].as_str().unwrap().is_empty());
    assert_eq!(finished["content"], "你好！");
    assert_ne!(finished["taskCompletion"]["completionState"], "partial");
    assert!(events.iter().any(|event| event["type"] == "task_progress" && event["phase"] == "recovering"));
    assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 4);
    assert!(!events.iter().any(|event| event["type"] == "tool_call"));
    let requests = fixture.provider.requests.lock().await.clone();
    assert_eq!(requests[3]["messages"].as_array().unwrap().iter().filter(|message|
        message["role"] == "user" && message["content"] == "你好").count(), 1);
    let messages = fixture.messages().await;
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1]["Status"], "completed");
    assert!(messages[1]["Error"].as_str().is_none_or(str::is_empty));
    assert_eq!(messages[1]["ToolRun"]["diagnostics"]["modelCalls"], 4);
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn temporary_provider_server_failure_keeps_the_original_request_live() {
    let fixture = Fixture::new(false, false).await;
    fixture.provider.transient_http_failures.store(1, Ordering::SeqCst);
    let response = fixture.client.post(format!("{}/api/chat/stream", fixture.url))
        .json(&fixture.request("你好")).send().await.unwrap();
    let text = tokio::time::timeout(Duration::from_secs(30), response.text())
        .await.expect("A transient provider failure must not terminate the request").unwrap();
    let events = event_values(&mut text.as_bytes().to_vec());
    assert!(!events.iter().any(|event| matches!(event["type"].as_str(), Some("error" | "interrupted"))));
    assert!(events.iter().any(|event| event["type"] == "task_progress" && event["phase"] == "recovering"));
    let completed = events.iter().find(|event| event["type"] == "completed").unwrap();
    assert_eq!(completed["content"], "你好！");
    assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.messages().await.len(), 2);
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unavailable_startup_preparation_never_generates_or_blocks_chat() {
    let fixture = Fixture::new(false, false).await;
    let health = fixture
        .client
        .get(format!("{}/health", fixture.url))
        .send()
        .await
        .unwrap();
    assert!(health.status().is_success());
    // Core readiness schedules preparation immediately, even before the desktop submits its model selection.
    // 核心就绪后立即安排准备，不等待桌面提交模型选择，也不借用 MCP 的启动延迟。
    tokio::time::timeout(Duration::from_secs(2), async {
        while fixture.runtime.model_preparation.snapshot().await["models"]
            .as_array()
            .is_none_or(Vec::is_empty)
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Startup model preparation must not wait for the old three/five-second delays");
    let response = fixture
        .client
        .post(format!("{}/api/models/preparation", fixture.url))
        .json(&json!({"providerId":"native-fixture","modelId":"fixture-model"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 202);
    let accepted: Value = response.json().await.unwrap();
    assert_eq!(accepted["scheduled"], true);
    let (_owner, cancellation) = tokio::sync::watch::channel(false);
    assert!(
        fixture
            .runtime
            .model_preparation
            .wait_idle(cancellation, Duration::from_secs(5))
            .await
    );
    let status: Value = fixture
        .client
        .get(format!("{}/api/models/preparation", fixture.url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["isPreparing"], false);
    assert_eq!(status["models"][0]["inferenceVerified"], false);
    assert_ne!(status["models"][0]["modelReady"], true);
    assert!(!status.to_string().contains("apiKey"));
    assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 0);
    let reply: Value = fixture
        .client
        .post(format!("{}/api/chat", fixture.url))
        .json(&fixture.request("你好"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(reply["content"], "你好！");
    assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.messages().await[1]["Status"], "completed");
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_cycle_streams_real_read_receipt_into_provider_and_formal_log() {
    let fixture = Fixture::new(true, false).await;
    let mut input = fixture.request("读取文件 fixture.txt 并告诉我内容。");
    input["runLimits"]["maxToolCalls"] = json!(1);
    let response = fixture
        .client
        .post(format!("{}/api/chat/stream", fixture.url))
        .json(&input)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let text = response.text().await.unwrap();
    if let Some(path) = std::env::var_os("KYNXA_GATEWAY_SSE_FIXTURE") {
        // The desktop contract check consumes the actual emitted stream, not a handwritten approximation.
        // 桌面合同检查读取实际发出的消息流，不用手写样例代替真实后端输出。
        let path = std::path::PathBuf::from(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, &text).unwrap();
    }
    let mut buffer = text.as_bytes().to_vec();
    let events = event_values(&mut buffer);
    assert_eq!(events.first().unwrap()["type"], "started");
    assert_eq!(events.iter().filter(|event| event["type"] == "started").count(), 1);
    assert_settled_public_segments(&events);
    // The provider fallback emits a raw content snapshot; no public SSE event may expose its footer.
    // 供应商非流式回退发出原始正文快照；公开 SSE 事件不能泄露其内部任务尾部。
    assert!(!text.contains("<kynxa_task>"), "{text}");
    assert!(!text.contains("</kynxa_task>"), "{text}");
    assert!(events.iter().any(|event| {
        event["type"] == "text_delta"
            && event["delta"]
                .as_str()
                .is_some_and(|delta| delta.contains("native-file-body"))
    }));
    assert!(
        events.iter().any(|event| event["type"] == "tool_result"
            && event["tool"]["name"] == "filesystem.read"
            && event["tool"]["status"] == "completed"),
        "{text}"
    );
    assert!(
        events.iter().any(|event| event["type"] == "completed"
            && event["content"]
                .as_str()
                .is_some_and(|text| text.contains("native-file-body"))),
        "{text}"
    );
    let requests = fixture.provider.requests.lock().await.clone();
    let ordinary = requests.iter().filter(|request|!is_completion_review_request(request)).collect::<Vec<_>>();
    let reviews = requests.iter().filter(|request|is_completion_review_request(request)).collect::<Vec<_>>();
    assert_eq!(ordinary.len(), 2,"Only the original read and its ordinary answer may run");
    assert_eq!(reviews.len(), 1,"A real tool task must separately review its actual acceptance evidence");
    assert_eq!(fixture.provider.calls.load(Ordering::Acquire),3);
    assert!(reviews[0]["tools"].as_array().is_none_or(Vec::is_empty),"Acceptance review must not redispatch tools");
    assert!(ordinary[1]["tools"].as_array().is_none_or(Vec::is_empty),
        "A used tool allowance must retain a normal final answer without further declarations.");
    assert!(
        ordinary[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["role"] == "tool"
                && message.to_string().contains("native-file-body"))
    );
    let messages = fixture.messages().await;
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1]["ToolActivities"][0]["name"], "filesystem.read");
    assert!(
        messages[1]["ToolActivities"][0]["resultRef"]["id"].is_string(),
        "{}",
        messages[1]
    );
    for post_again in [false, true] {
        let response = if post_again {
            fixture.client.post(format!("{}/api/chat/stream", fixture.url)).json(&input).send().await.unwrap()
        } else {
            fixture.client.get(format!("{}/api/chat/stream?conversationId={}&requestId={}",
                fixture.url, fixture.conversation, input["requestId"].as_str().unwrap()))
                .send().await.unwrap()
        };
        assert!(response.status().is_success());
        let text = response.text().await.unwrap();
        let replay = event_values(&mut text.as_bytes().to_vec());
        assert_eq!(replay.first().unwrap()["type"], "started");
        assert_eq!(replay.iter().filter(|event| event["type"] == "started").count(), 1);
        assert_eq!(replay.last().unwrap()["type"], "completed");
        assert!(replay.iter().find(|event| event["type"] == "content_snapshot").unwrap()["content"]
            .as_str().unwrap().contains("native-file-body"));
        assert!(replay.iter().all(|event| event["conversationId"] == fixture.conversation
            && event["requestId"] == input["requestId"]));
        assert_eq!(fixture.provider.calls.load(Ordering::Acquire), 3,
            "observing or resubmitting the same completed request must not dispatch work");
        if !post_again && let Some(path) = std::env::var_os("KYNXA_GATEWAY_RECONNECT_SSE_FIXTURE") {
            let path = std::path::PathBuf::from(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, &text).unwrap();
        }
    }
    assert_eq!(fixture.messages().await.len(), 2);
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skill_pagination_correction_keeps_context_and_request_live_with_compatible_run_limits() {
    for run_limits in [Value::Null, json!({"maxRounds":"invalid","maxDurationMs":null})] {
        let fixture = Fixture::new(false, false).await;
        fixture.provider.skill_pagination_cycle.store(true, Ordering::Release);
        let original = "列出可用技能，参数有误时在当前任务内部纠正并继续。";
        let mut input = fixture.request(original);
        // A new user message owns a new ID; the fixture already persists its initial greeting.
        // 夹具已保存最初的问候，新消息使用新 ID，不改写已有正式消息。
        input["userMessageId"] = json!(new_id());
        input["runLimits"] = run_limits;
        let response = fixture.client.post(format!("{}/api/chat/stream", fixture.url))
            .json(&input).send().await.unwrap();
        assert!(response.status().is_success());
        let text = response.text().await.unwrap();
        let events = event_values(&mut text.as_bytes().to_vec());
        assert!(!events.iter().any(|event| matches!(event["type"].as_str(), Some("error" | "interrupted"))), "{text}");
        assert!(events.iter().any(|event| event["type"] == "completed"
            && event["content"].as_str().is_some_and(|text| text.contains("纠正分页"))), "{text}");
        let requests = fixture.provider.requests.lock().await.clone();
        let correction = requests.iter().find(|request| request["messages"].as_array().into_iter().flatten()
            .any(|message| message["role"] == "tool" && message["tool_call_id"] == "skill-page-invalid")).unwrap();
        let invalid_receipt = correction["messages"].as_array().unwrap().iter()
            .find(|message| message["tool_call_id"] == "skill-page-invalid").unwrap()["content"].as_str().unwrap();
        assert!(invalid_receipt.contains("INVALID_SKILL_LIST_PAGINATION"), "{invalid_receipt}");
        assert!(invalid_receipt.contains("\"executed\":false"), "{invalid_receipt}");
        assert!(correction["messages"].as_array().unwrap().iter()
            .any(|message| message["role"] == "user" && message["content"].as_str() == Some(original)));
        let messages = fixture.messages().await;
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["Content"], "你好");
        assert_eq!(messages[1]["Content"], original);
        let activities = messages[2]["ToolActivities"].as_array().unwrap();
        let pages = activities.iter().filter(|activity| activity["name"] == "skill.list").collect::<Vec<_>>();
        assert_eq!(pages.len(), 2, "{activities:?}");
        assert_eq!(pages[0]["status"], "error");
        assert_eq!(pages[1]["status"], "completed");
        let repository = kynxa_agent_core_native::storage::task_repository::TaskRepository::open(&fixture.root.join("Data")).unwrap();
        let task = repository.find_by_request_id(&fixture.conversation, input["requestId"].as_str().unwrap())
            .unwrap().unwrap();
        // Active wall time is separate from parallel model/tool costs.
        // 有效墙钟与并行模型/工具成本分开计算，不再按子代理数量放大。
        assert_eq!(task.budget.max_execution_ms, Some(1_800_000));
        let persisted: Value = serde_json::from_slice(&std::fs::read(fixture.root.join("Data")
            .join(task.input_reference.as_ref().unwrap()["relativePath"].as_str().unwrap())).unwrap()).unwrap();
        assert_eq!(persisted["input"]["runLimits"], input["runLimits"]);
        assert_eq!(persisted["input"]["message"], original);
        assert_eq!(persisted["resolvedRunLimits"]["maxDurationMs"], 1_800_000);
        let calls_before_replay = fixture.provider.calls.load(Ordering::SeqCst);
        let replay: Value = fixture.client.post(format!("{}/api/chat", fixture.url))
            .json(&input).send().await.unwrap().json().await.unwrap();
        assert_eq!(replay["type"], "completed", "{replay}");
        assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), calls_before_replay);
        fixture.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acceptance_readback_gap_executes_real_missing_reads_without_repeating_writes_or_summary_work() {
    let fixture = Fixture::new(false, false).await;
    fixture.provider.readback_gap_cycle.store(true, Ordering::Release);
    fixture.provider.readback_gap_required.store(true, Ordering::Release);
    let mut input = fixture.request("生成 gap-output.md 文档并完整回读当前文件，然后给我简短结果。");
    input["userMessageId"] = json!(new_id());
    let response = fixture.client.post(format!("{}/api/chat/stream", fixture.url))
        .json(&input).send().await.unwrap();
    assert!(response.status().is_success());
    let stream = response.text().await.unwrap();
    let events = event_values(&mut stream.as_bytes().to_vec());
    assert_settled_public_segments(&events);
    assert!(!events.iter().any(|event| matches!(event["type"].as_str(), Some("error" | "interrupted"))), "{stream}");
    let completed = events.iter().find(|event| event["type"] == "completed").expect("real readback must settle the live task");
    assert_eq!(completed["taskCompletion"]["completionState"], "completed", "{completed}");
    let requests = fixture.provider.requests.lock().await.clone();
    assert!(requests.len() <= 12, "the fixture must make actual progress rather than repeat valid reviews: {} requests", requests.len());
    assert!(requests.iter().filter(|request| is_completion_review_request(request)).count() <= 3);
    let messages = fixture.messages().await;
    let saved = messages.last().unwrap();
    let activities = saved["ToolActivities"].as_array().unwrap();
    assert_eq!(activities.iter().filter(|activity| activity["name"] == "filesystem.write").count(), 1,
        "gap recovery must retain the original successful write");
    assert_eq!(activities.iter().filter(|activity| activity["name"] == "document.read" && activity["status"] == "completed")
        .filter_map(|activity| activity["toolCallId"].as_str()).collect::<Vec<_>>(), ["gap-read", "gap-head", "gap-tail"],
        "the model must fill both actual gaps without repeating the already-read window");
    assert_eq!(fixture.provider.readback_gap_step.load(Ordering::Acquire), 3,
        "premature completion must not skip the missing reads needed by the full-read requirement");
    assert_eq!(std::fs::read_to_string(fixture.root.join("work/gap-output.md")).unwrap(), "检索和恢复记录。".repeat(600));
    let mut followup = fixture.request("只根据刚才的内容给我一句摘要，不要读取或修改文件。");
    followup["userMessageId"] = json!(new_id());
    let response: Value = fixture.client.post(format!("{}/api/chat", fixture.url)).json(&followup)
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(response["type"], "completed", "{response}");
    assert_eq!(response["taskCompletion"]["acceptanceReviewRequired"], false, "{response}");
    let repository = kynxa_agent_core_native::storage::task_repository::TaskRepository::open(&fixture.root.join("Data")).unwrap();
    let followup_task = repository.find_by_request_id(&fixture.conversation, followup["requestId"].as_str().unwrap()).unwrap().unwrap();
    let reservations = repository.model_reservations(&followup_task.task_id).unwrap();
    assert_eq!(reservations.iter().filter(|reservation|
        reservation.provider_usage.as_ref().and_then(|usage| usage.client_metrics.as_ref())
            .and_then(|metrics| metrics.cache_policy.as_ref())
            .is_some_and(|policy| policy["prefix"]["scope"] == "main-request")).count(), 1,
        "only one answer request is allowed; context-budget compaction stays separately metered");
    let messages = fixture.messages().await;
    assert!(messages.last().unwrap()["ToolActivities"].as_array().is_none_or(|activities|
        activities.iter().all(|activity| activity["name"] == "context.compact")),
        "context budgeting may compact history but must not start external work for this follow-up");
    assert!(messages.last().unwrap()["Content"].as_str().unwrap().contains("摘要："));
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acceptance_readback_gap_plain_generation_does_not_force_output_readback() {
    let fixture = Fixture::new(false, false).await;
    fixture.provider.readback_gap_cycle.store(true, Ordering::Release);
    let mut input = fixture.request("生成 gap-output.md 文档，然后给我简短结果。");
    input["userMessageId"] = json!(new_id());
    let response: Value = fixture.client.post(format!("{}/api/chat", fixture.url)).json(&input)
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(response["type"], "completed", "{response}");
    assert_eq!(response["taskCompletion"]["completionState"], "completed", "{response}");
    assert_eq!(response["taskCompletion"]["documentValidation"]["pending"], true,
        "unread output remains an honest coverage fact without adding a requirement");
    let messages = fixture.messages().await;
    let activities = messages.last().unwrap()["ToolActivities"].as_array().unwrap();
    assert_eq!(activities.iter().filter(|activity| activity["name"] == "filesystem.write" && activity["status"] == "completed").count(), 1);
    assert!(!activities.iter().any(|activity| matches!(activity["name"].as_str(), Some("filesystem.read" | "document.read"))),
        "plain generation must not trigger an automatic output read: {activities:?}");
    assert_eq!(std::fs::read_to_string(fixture.root.join("work/gap-output.md")).unwrap(), "检索和恢复记录。".repeat(600));
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn annotated_successful_evidence_finishes_without_archive_scans_or_repeating_writes() {
    let fixture = Fixture::new(false, false).await;
    fixture.provider.annotated_evidence_cycle.store(true, Ordering::Release);
    let mut input = fixture.request("生成 annotated-result.txt 文件并回读当前内容，然后简短回复。");
    input["userMessageId"] = json!(new_id());
    let response = fixture.client.post(format!("{}/api/chat/stream", fixture.url))
        .json(&input).send().await.unwrap();
    assert!(response.status().is_success());
    let stream = response.text().await.unwrap();
    let events = event_values(&mut stream.as_bytes().to_vec());
    assert_settled_public_segments(&events);
    assert!(!events.iter().any(|event| matches!(event["type"].as_str(), Some("error" | "interrupted"))), "{stream}");
    let completed = events.iter().find(|event| event["type"] == "completed").expect("annotated actual IDs must finish acceptance");
    assert_eq!(completed["taskCompletion"]["completionState"], "completed", "{completed}");
    let requests = fixture.provider.requests.lock().await.clone();
    assert_eq!(requests.iter().filter(|request| is_completion_review_request(request)).count(), 1,
        "valid annotated references need no repeated review or history scan");
    assert!(requests.iter().filter(|request| is_completion_review_request(request))
        .all(|request| request["max_tokens"].as_u64().is_some_and(|budget| budget >= 2048)),
        "real completion reviews need enough output budget for their evidence footer");
    let saved = fixture.messages().await.into_iter().find(|message| message["Id"] == input["requestId"]).unwrap();
    let activities = saved["ToolActivities"].as_array().unwrap();
    for name in ["filesystem.write", "filesystem.read"] {
        assert_eq!(activities.iter().filter(|activity| activity["name"] == name && activity["status"] == "completed").count(), 1,
            "acceptance must reuse the original settled {name} receipt");
    }
    assert!(!activities.iter().any(|activity| activity["name"] == "tool.result.read"),
        "a reference annotation is not an unread archived result");
    assert_eq!(std::fs::read_to_string(fixture.root.join("work/annotated-result.txt")).unwrap(), "ANNOTATED_RECEIPT_BODY");
    let repository = kynxa_agent_core_native::storage::task_repository::TaskRepository::open(&fixture.root.join("Data")).unwrap();
    let task = repository.find_by_request_id(&fixture.conversation, input["requestId"].as_str().unwrap()).unwrap().unwrap();
    assert_eq!(task.state, kynxa_agent_core_native::storage::task_repository::TaskState::Completed);
    // An HTTP reconnect must reuse this same completed task and its settled effects.
    // HTTP 重连复用同一已完成任务及成功副作用，不触发新模型请求或再次写入。
    let replay: Value = fixture.client.post(format!("{}/api/chat", fixture.url)).json(&input)
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(replay["type"], "completed", "{replay}");
    assert_eq!(replay["content"], completed["content"]);
    assert_eq!(replay["eventSequence"], completed["eventSequence"]);
    assert_eq!(fixture.provider.requests.lock().await.len(), requests.len());
    assert_eq!(repository.find_by_request_id(&fixture.conversation, input["requestId"].as_str().unwrap()).unwrap().unwrap().task_id, task.task_id);
    let original_operations = repository.operations(&task.task_id).unwrap();
    assert_eq!(original_operations.len(), 2);
    assert!(original_operations.iter().all(|operation|
        operation.state == kynxa_agent_core_native::storage::task_repository::OperationState::Succeeded));
    // An explicit revision must retire only the cached delivery and continue this original task.
    // 明确修订只退役缓存交付，仍沿原任务继续；不能复用旧完成结果或重做成功操作。
    let control = fixture.client.post(format!("{}/api/tasks/{}/control", fixture.url, task.task_id))
        .json(&json!({"conversationId":fixture.conversation,"expectedVersion":task.version,"action":"revise"}))
        .send().await.unwrap();
    assert!(control.status().is_success(), "{}", control.text().await.unwrap());
    let changed: Value = control.json().await.unwrap();
    assert_eq!(changed["task"]["taskId"], task.task_id);
    assert_eq!(changed["task"]["requestId"], input["requestId"]);
    let revised = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let current = repository.load(&task.task_id).unwrap();
            if current.state == kynxa_agent_core_native::storage::task_repository::TaskState::Completed
                && current.usage.model_requests > task.usage.model_requests { break current; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("Explicit revision must perform fresh model work and finish the same task.");
    assert_eq!(revised.task_id, task.task_id);
    assert_eq!(revised.request_id, input["requestId"].as_str().unwrap());
    assert!(fixture.provider.requests.lock().await.len() > requests.len(),
        "same-process completed delivery caching must not suppress explicit revision");
    let operations = repository.operations(&task.task_id).unwrap();
    for original in &original_operations {
        let retained = operations.iter().find(|operation| operation.intent.operation_id == original.intent.operation_id)
            .expect("Revision must retain each original operation identity.");
        assert_eq!(retained.state, kynxa_agent_core_native::storage::task_repository::OperationState::Succeeded);
        assert_eq!(retained.intent.tool_name, original.intent.tool_name);
    }
    // Recovery may observe the current hash once; this is not a replay of the successful read or write.
    // 恢复可核对一次当前哈希，不代表重做此前成功的读取或写入。
    let added = operations.iter().filter(|operation| !original_operations.iter()
        .any(|original| original.intent.operation_id == operation.intent.operation_id)).collect::<Vec<_>>();
    assert!(added.len() <= 1 && added.iter().all(|operation| operation.intent.tool_name == "filesystem.stat"
        && operation.intent.effect == kynxa_agent_core_native::storage::task_repository::OperationEffect::Observation
        && operation.state == kynxa_agent_core_native::storage::task_repository::OperationState::Succeeded),
        "Only a successful current-version metadata observation may be added: {added:?}");
    for name in ["filesystem.write", "filesystem.read"] {
        assert_eq!(operations.iter().filter(|operation| operation.intent.tool_name == name
            && operation.state == kynxa_agent_core_native::storage::task_repository::OperationState::Succeeded).count(), 1);
    }
    assert!(!operations.iter().any(|operation| operation.intent.tool_name == "tool.result.read"));
    assert_eq!(std::fs::read_to_string(fixture.root.join("work/annotated-result.txt")).unwrap(), "ANNOTATED_RECEIPT_BODY");
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn partial_http_resume_reopens_the_same_task_twice_and_steers_without_replaying_successful_effects() {
    use kynxa_agent_core_native::storage::task_repository::{OperationState, TaskRepository, TaskState};
    let fixture = Fixture::new(false, false).await;
    fixture.provider.annotated_evidence_cycle.store(true, Ordering::Release);
    let mut input = fixture.request("生成 annotated-result.txt 文件并回读当前内容，然后简短回复。");
    input["userMessageId"] = json!(new_id());
    let first: Value = fixture.client.post(format!("{}/api/chat", fixture.url)).json(&input)
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(first["type"], "completed", "{first}");
    let repository = TaskRepository::open(&fixture.root.join("Data")).unwrap();
    let original = repository.find_by_request_id(&fixture.conversation, input["requestId"].as_str().unwrap()).unwrap().unwrap();
    let original_operations = repository.operations(&original.task_id).unwrap();
    assert_eq!(original_operations.len(), 2);
    assert!(original_operations.iter().all(|operation| operation.state == OperationState::Succeeded));
    let saved = fixture.messages().await.into_iter().find(|message| message["Id"] == input["requestId"]).unwrap();
    assert_eq!(saved["Status"], "completed");
    let mut guided_reply_id = None;
    let mut guided_user_message_id = None;
    for generation in 1..=2 {
        let previous = repository.load(&original.task_id).unwrap();
        assert_eq!(previous.state, TaskState::Completed);
        assert!(repository.current_attempt(&original.task_id).unwrap().unwrap().ended_at_ms.is_some());
        // Reproduce a persisted legacy partial task with completed message transport and its finished run cache.
        // 重现旧任务的正式 partial 状态、已完成消息传输及仍在进程中的旧运行缓存；完整操作和检查点均保留。
        let mut partial = previous.clone();
        partial.state = TaskState::Partial;
        partial.version += 1;
        partial.stop_reason = Some("fixture-legacy-partial".into());
        let connection = rusqlite::Connection::open(fixture.root.join("Data/Agent/tasks.sqlite")).unwrap();
        connection.execute("UPDATE tasks SET state='partial',version=?1,document=?2 WHERE task_id=?3",
            rusqlite::params![partial.version, serde_json::to_string(&partial).unwrap(), partial.task_id]).unwrap();
        drop(connection);
        fixture.provider.resume_generation.store(generation, Ordering::Release);
        let calls_before = fixture.provider.calls.load(Ordering::Acquire);
        let control = fixture.client.post(format!("{}/api/tasks/{}/control", fixture.url, original.task_id))
            .json(&json!({"conversationId":fixture.conversation,"expectedVersion":partial.version,"action":"resume"}))
            .send().await.unwrap();
        let status = control.status();
        let control: Value = control.json().await.unwrap();
        assert!(status.is_success(), "{control}");
        assert_eq!(control["task"]["taskId"], original.task_id);
        assert_eq!(control["task"]["requestId"], input["requestId"]);
        if generation == 2 {
            tokio::time::timeout(Duration::from_secs(8), async {
                while fixture.provider.calls.load(Ordering::Acquire) == calls_before {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }).await.expect("Second HTTP resume must dispatch a new provider request, not reuse finished delivery.");
            let message_id = new_id();
            let reply_id = new_id();
            let steering = fixture.client.post(format!("{}/api/tasks/{}/steer", fixture.url, original.task_id))
                .json(&json!({"conversationId":fixture.conversation,"expectedVersion":repository.load(&original.task_id).unwrap().version,
                    "userMessageId":message_id,"replyMessageId":reply_id,"message":"继续核对原文件并保留全部成功回执。","action":"update"}))
                .send().await.unwrap();
            let status = steering.status();
            let accepted: Value = steering.json().await.unwrap();
            assert!(status.is_success(), "{accepted}");
            assert_eq!(accepted["steeringAccepted"], true);
            assert_eq!(accepted["task"]["taskId"], original.task_id);
            assert_eq!(accepted["replyMessageId"], reply_id);
            guided_reply_id = Some(reply_id);
            guided_user_message_id = Some(message_id);
            fixture.provider.resume_generation_gate.notify_waiters();
        }
        let resumed = tokio::time::timeout(Duration::from_secs(12), async {
            loop {
                let task = repository.load(&original.task_id).unwrap();
                if task.state == TaskState::Completed && task.execution_version > previous.execution_version
                    && repository.current_attempt(&original.task_id).unwrap().unwrap().ended_at_ms.is_some() { break task; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("HTTP resume must execute and settle the original task with its retained context.");
        assert_eq!(resumed.task_id, original.task_id);
        assert_eq!(resumed.request_id, original.request_id);
        assert!(resumed.usage.model_requests > previous.usage.model_requests);
        let requests = fixture.provider.requests.lock().await;
        assert!(requests[calls_before..].iter().any(|request| request["messages"].as_array().unwrap().iter()
            .any(|message| message["role"] == "tool" && message["tool_call_id"] == "annotated-write")),
            "Actual resumed requests must retain original successful write evidence.");
        drop(requests);
        let operations = repository.operations(&original.task_id).unwrap();
        for original_operation in &original_operations {
            let retained = operations.iter().find(|operation| operation.intent.operation_id == original_operation.intent.operation_id).unwrap();
            assert_eq!(retained.state, OperationState::Succeeded);
            assert_eq!(retained.intent.tool_name, original_operation.intent.tool_name);
        }
        for name in ["filesystem.write", "filesystem.read"] {
            assert_eq!(operations.iter().filter(|operation| operation.intent.tool_name == name).count(), 1,
                "Resuming and steering must not replay the original {name}.");
        }
        assert!(operations.iter().any(|operation| operation.intent.tool_call_id == format!("resume-stat-{generation}")
            && operation.state == OperationState::Succeeded), "Each resumed generation must really dispatch its new observation.");
    }
    let guided_reply_id = guided_reply_id.unwrap();
    let messages = fixture.messages().await;
    let guided = messages.iter().find(|message| message["Id"] == guided_reply_id).unwrap();
    assert_eq!(guided["Status"], "completed");
    assert_eq!(guided["RequestId"], input["requestId"]);
    assert_eq!(guided["ReplyTo"], guided_user_message_id.unwrap());
    assert!(!guided["Content"].as_str().unwrap().is_empty());
    assert_eq!(repository.user_steering(&original.task_id).unwrap().len(), 1);
    assert_eq!(std::fs::read_to_string(fixture.root.join("work/annotated-result.txt")).unwrap(), "ANNOTATED_RECEIPT_BODY");
    // Missing control cannot authorize an old check; malformed completion evidence cannot erase valid delivery control.
    // 未解释引导不能授权旧检查；完成证据格式错误也不能吞掉交付已有成果的有效控制。
    let previous = repository.load(&original.task_id).unwrap();
    let mut partial = previous.clone();
    partial.state = TaskState::Partial;
    partial.version += 1;
    let connection = rusqlite::Connection::open(fixture.root.join("Data/Agent/tasks.sqlite")).unwrap();
    connection.execute("UPDATE tasks SET state='partial',version=?1,document=?2 WHERE task_id=?3",
        rusqlite::params![partial.version, serde_json::to_string(&partial).unwrap(), partial.task_id]).unwrap();
    drop(connection);
    let calls_before_stop_resume = fixture.provider.calls.load(Ordering::Acquire);
    let operations_before_stop = repository.operations(&original.task_id).unwrap();
    fixture.provider.hold_next_provider.store(true, Ordering::Release);
    let resumed = fixture.client.post(format!("{}/api/tasks/{}/control", fixture.url, original.task_id))
        .json(&json!({"conversationId":fixture.conversation,"expectedVersion":partial.version,"action":"resume"})).send().await.unwrap();
    assert!(resumed.status().is_success());
    tokio::time::timeout(Duration::from_secs(5), async {
        while fixture.provider.calls.load(Ordering::Acquire) == calls_before_stop_resume {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("Resumed stop test needs a real in-flight provider request.");
    fixture.provider.native_stop_cycle.store(true, Ordering::Release);
    let stop_user = new_id();
    let stop_reply = new_id();
    let stopped = fixture.client.post(format!("{}/api/tasks/{}/steer", fixture.url, original.task_id))
        .json(&json!({"conversationId":fixture.conversation,"expectedVersion":repository.load(&original.task_id).unwrap().version,
            "userMessageId":stop_user,"replyMessageId":stop_reply,"action":"update",
            "message":"交付已有成果，结束不要再检查，保留原文件和成功回执，未验收的部分如实说明。"})).send().await.unwrap();
    assert!(stopped.status().is_success());
    fixture.provider.resume_generation_gate.notify_waiters();
    let result = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let task = repository.load(&original.task_id).unwrap();
            if task.state == TaskState::Paused {
                if let Some(status) = fixture.runtime.run_status(&fixture.conversation, &original.request_id).await {
                    if status["status"] == "completed" { break status["result"].clone(); }
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("A valid stop must finish and durably pause without another recovery generation.");
    assert_eq!(result["type"], "completed");
    assert!(result["taskPauseError"].is_null());
    assert_eq!(result["taskCompletion"]["userStopped"], true);
    assert_eq!(result["taskCompletion"]["userDirectedDelivery"], true);
    assert_eq!(result["taskCompletion"]["completionState"], "partial");
    let stopped = fixture.messages().await.into_iter().find(|message| message["Id"] == stop_reply).unwrap();
    assert_eq!(stopped["ReplyTo"], stop_user);
    assert_eq!(stopped["UserTaskDirective"]["userMessageId"], stop_user);
    assert_eq!(stopped["UserTaskDirective"]["stop"], true);
    assert_eq!(stopped["UserTaskDirective"]["deliverExisting"], true);
    assert_eq!(stopped["UserTaskDirective"]["minimumActiveMs"], 0);
    assert!(stopped["UserTaskDirective"]["requirements"].as_array().unwrap().is_empty());
    assert!(!stopped["UserTaskDirective"]["suspendedRequirements"].as_array().unwrap().is_empty());
    assert!(stopped["Content"].as_str().unwrap().contains("未核验项仍保留"));
    let calls_after_stop = fixture.provider.calls.load(Ordering::Acquire);
    assert_eq!(calls_after_stop, calls_before_stop_resume + 3,
        "Only the superseded request, unresolved old check proposal and delivery-control interpretation may run.");
    assert_eq!(fixture.provider.native_stop_emissions.load(Ordering::Acquire), 2);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(fixture.provider.calls.load(Ordering::Acquire), calls_after_stop, "Paused stop must never re-enter the model loop.");
    let operations = repository.operations(&original.task_id).unwrap();
    assert_eq!(operations.len(), operations_before_stop.len(),
        "Neither unresolved guidance nor accepted delivery control may dispatch an old stat/read against withdrawn acceptance.");
    assert!(!operations.iter().any(|operation| operation.intent.tool_call_id.starts_with("withdrawn-check-")),
        "Proposed checks must not enter the operation ledger before or after delivery control is applied.");
    for name in ["filesystem.write", "filesystem.read"] {
        assert_eq!(operations.iter().filter(|operation| operation.intent.tool_name == name).count(), 1);
    }
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Explicitly authorized live API validation; requires KYNXA_REAL_MODEL_HOME, KYNXA_REAL_PROVIDER and KYNXA_REAL_MODEL."]
async fn authorized_live_model_preserves_paused_work_and_obeys_latest_user_stop() {
    use kynxa_agent_core_native::{models::gateway_config::ModelConfigStore,
        storage::task_repository::{OperationState, TaskRepository, TaskState}};
    let source_home = PathBuf::from(std::env::var_os("KYNXA_REAL_MODEL_HOME").expect("Explicit model settings home is required."));
    let provider = std::env::var("KYNXA_REAL_PROVIDER").expect("Explicit provider is required.");
    let model = std::env::var("KYNXA_REAL_MODEL").expect("Explicit model is required.");
    let stop_only = std::env::var_os("KYNXA_REAL_STOP_ONLY").is_some();
    let mut fixture = Fixture::new(false, false).await;
    let reuse_task = std::env::var("KYNXA_REAL_REUSE_TASK").ok();
    if let Some(reuse_root) = std::env::var_os("KYNXA_REAL_REUSE_ROOT") {
        let old_server = fixture.servers.pop().unwrap();
        old_server.abort();
        let _ = old_server.await;
        fixture.runtime.shutdown().await;
        fixture.root = PathBuf::from(reuse_root);
        fixture.conversation = std::env::var("KYNXA_REAL_REUSE_CHAT").expect("Explicit isolated conversation is required.");
        let mut configuration = fixture.runtime.config.clone();
        configuration.data_home = fixture.root.join("Models");
        configuration.data_root = fixture.root.join("Data");
        configuration.extension_root = fixture.root.join("Extensions");
        configuration.application_root = fixture.root.join("Application");
        configuration.toolhost_exe = fixture.root.join("missing-toolhost.exe");
        configuration.vector_extension = fixture.root.join("missing-vec0.dll");
        configuration.inference_library = fixture.root.join("missing-onnxruntime.dll");
        fixture.runtime = GatewayRuntime::new(configuration);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        fixture.url = format!("http://{}", listener.local_addr().unwrap());
        let service = gateway::router(fixture.runtime.clone());
        fixture.servers.push(tokio::spawn(async move { axum::serve(listener, service).await.unwrap(); }));
    }
    assert!(!fixture.root.starts_with(std::env::current_dir().unwrap()), "Live credentials must stay outside the repository.");
    let mut connection = ModelConfigStore::new(&source_home).connection_for(&provider).await
        .expect("Read-only selected connection lookup failed.").expect("Selected connection is unavailable.");
    assert!(connection["apiKey"].as_str().is_some_and(|key| !key.is_empty()), "The authorized connection requires its saved credential.");
    // Copy only the selected connection into isolated temporary settings; never log credentials or edit their source.
    // 仅把所选连接写入隔离临时配置；不打印凭据，也不修改用户的原配置。
    connection["maxOutputTokens"] = json!(8192);
    ModelConfigStore::new(&fixture.runtime.config.data_home).save(connection).await.expect("Isolated model setup failed.");
    fixture.client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(180)).build().unwrap();
    std::fs::write(fixture.root.join("work/second.txt"), "SECOND_LIVE_SOURCE").unwrap();
    let mut input = fixture.request("请至少持续工作1小时完善一份技术研究报告，最终报告覆盖恢复、回执、缓存三部分。第一步读取 fixture.txt，把全文原样写到 progress.txt（只写一次），再读回 progress.txt。先完成这一步，随后按我追加的指导展开，不为凑时间重复读写文件。");
    input["userMessageId"] = json!(new_id());
    input["provider"] = json!(provider);
    input["model"] = json!(model);
    let request = input["requestId"].as_str().unwrap().to_owned();
    let started_at = std::time::Instant::now();
    let response = if reuse_task.is_none() {
        let response = fixture.client.post(format!("{}/api/chat/stream", fixture.url)).json(&input).send().await.unwrap();
        assert!(response.status().is_success(), "Live chat transport must start.");
        Some(response)
    } else { None };
    let repository = TaskRepository::open(&fixture.root.join("Data")).unwrap();
    let original = if let Some(task_id) = reuse_task { repository.load(&task_id).unwrap() } else { tokio::time::timeout(Duration::from_secs(75), async {
        loop {
            if let Some(task) = repository.find_by_request_id(&fixture.conversation, &request).unwrap() {
                let operations = repository.operations(&task.task_id).unwrap();
                if operations.iter().any(|operation| operation.intent.tool_name == "filesystem.write" && operation.state == OperationState::Succeeded)
                    && operations.iter().filter(|operation| operation.intent.tool_name == "filesystem.read" && operation.state == OperationState::Succeeded).count() >= 2 { break task; }
                assert!(!task.state.is_terminal(), "The initial live task must perform its requested writes and readbacks; state={:?}, code={:?}.", task.state, task.stop_reason);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }).await.expect("Live provider did not perform the initial authorized file operations in time.") };
    let original_operations = repository.operations(&original.task_id).unwrap();
    println!("{}", json!({"phase":"live-seed-verified","modelRequests":original.usage.model_requests,"elapsedMs":started_at.elapsed().as_millis()}));
    let input_reference = original.input_reference.as_ref().unwrap();
    let input_bytes = std::fs::read(fixture.root.join("Data").join(input_reference["relativePath"].as_str().unwrap())).unwrap();
    assert_eq!(kynxa_agent_core_native::storage::managed_files::digest(&input_bytes), input_reference["sha256"]);
    let original_input: Value = serde_json::from_slice(&input_bytes).unwrap();
    assert_eq!(original_input["resolvedRunLimits"]["minActiveMs"], 3_600_000,
        "The live task must really carry its original one-hour minimum before user revision.");
    assert_eq!(std::fs::read_to_string(fixture.root.join("work/progress.txt")).unwrap(), "native-file-body");
    let paused = fixture.client.post(format!("{}/api/tasks/{}/control", fixture.url, original.task_id))
        .json(&json!({"conversationId":fixture.conversation,"expectedVersion":repository.load(&original.task_id).unwrap().version,"action":"pause"}))
        .send().await.unwrap();
    assert!(paused.status().is_success(), "Live pause must preserve the current task.");
    let paused: Value = paused.json().await.unwrap();
    assert_eq!(paused["task"]["state"], "paused");
    println!("{}", json!({"phase":"live-paused","elapsedMs":started_at.elapsed().as_millis()}));
    let read_context = || {
        let checkpoint = repository.latest_checkpoint(&original.task_id).unwrap().unwrap();
        let reference = &checkpoint.context_reference["summaryReference"];
        let relative = reference["relativePath"].as_str().expect("Paused work needs its durable context reference.");
        let bytes = std::fs::read(fixture.root.join("Data").join(relative)).unwrap();
        assert_eq!(kynxa_agent_core_native::storage::managed_files::digest(&bytes), reference["sha256"]);
        serde_json::from_slice::<Value>(&bytes).unwrap()["context"].clone()
    };
    let original_requirements = if stop_only { original_input["acceptanceRequirements"].as_array().unwrap().clone() }
        else { read_context()["acceptanceRequirements"].as_array().unwrap().clone() };
    assert!(!original_requirements.is_empty());
    if !stop_only {
    let resumed = fixture.client.post(format!("{}/api/tasks/{}/control", fixture.url, original.task_id))
        .json(&json!({"conversationId":fixture.conversation,"expectedVersion":repository.load(&original.task_id).unwrap().version,"action":"resume"}))
        .send().await.unwrap();
    assert!(resumed.status().is_success(), "Live resume must reuse the original task.");
    println!("{}", json!({"phase":"live-resumed","elapsedMs":started_at.elapsed().as_millis()}));
    }
    if !stop_only {
    let append_id = new_id();
    submit_live_steering(&fixture, &repository, &original.task_id,
        json!({"conversationId":fixture.conversation,
            "userMessageId":append_id,"replyMessageId":new_id(),"action":"update",
            "message":"追加一项：请读取 second.txt，并保留原先研究报告的三个章节要求及至少一小时安排；已经成功写入的 progress.txt 不要重写。"})).await;
    tokio::time::timeout(Duration::from_secs(75), async {
        loop {
            let messages = match kynxa_agent_core_native::storage::gateway::read_messages(&fixture.root.join("Data"), &fixture.conversation, true) {
                Ok(messages) => messages,
                Err(error) if error.code() == "MEMORY_CONFLICT" => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
                Err(error) => panic!("Live observation failed: {}.", error.code()),
            };
            if messages.iter().flat_map(|message| message["ToolActivities"].as_array().into_iter().flatten())
                .any(|activity| activity["name"] == "filesystem.read" && activity["status"] == "completed"
                    && activity["arguments"]["path"].as_str().is_some_and(|path| path.ends_with("second.txt"))) { break; }
            assert!(!repository.load(&original.task_id).unwrap().state.is_terminal(), "Append guidance must not discard unrevoked work.");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }).await.expect("Live append guidance did not execute its new read in time.");
    println!("{}", json!({"phase":"live-append-read-verified","elapsedMs":started_at.elapsed().as_millis()}));
    let appended_context = read_context();
    let appended_requirements = appended_context["acceptanceRequirements"].as_array().unwrap();
    assert!(original_requirements.iter().all(|requirement| appended_requirements.contains(requirement)),
        "Ordinary append guidance must retain every unrevoked original requirement.");
    }
    let stop_id = new_id();
    let stop_reply = new_id();
    let stop_model_baseline = repository.load(&original.task_id).unwrap().usage.model_requests;
    submit_live_steering(&fixture, &repository, &original.task_id,
        json!({"conversationId":fixture.conversation,
            "userMessageId":stop_id,"replyMessageId":stop_reply,"action":"update",
            "message":"现在停止这个任务，取消原先至少一小时以及后续研究报告的所有未完成要求，不再继续执行工具；保留已成功的文件、上下文、进度和回执，只简短确认已停止。"})).await;
    println!("{}", json!({"phase":"live-stop-accepted","elapsedMs":started_at.elapsed().as_millis()}));
    let mut acknowledged_unknown_cost = false;
    let settled = tokio::time::timeout(Duration::from_secs(75), async {
        loop {
            let task = repository.load(&original.task_id).unwrap();
            if task.state == TaskState::Paused && repository.current_attempt(&task.task_id).unwrap()
                .is_some_and(|attempt| attempt.ended_at_ms.is_some()) { break task; }
            if task.state == TaskState::WaitingVerification && !acknowledged_unknown_cost {
                let identities = repository.model_reservations(&task.task_id).unwrap().into_iter()
                    .filter(|reservation| reservation.blocks_recovery())
                    .map(|reservation| json!({"taskId":task.task_id,"reservationId":reservation.reservation_id})).collect::<Vec<_>>();
                if !identities.is_empty() {
                    let acknowledgement = fixture.client.post(format!("{}/api/tasks/{}/control", fixture.url, task.task_id))
                        .json(&json!({"conversationId":fixture.conversation,"expectedVersion":task.version,
                            "action":"acknowledge-unknown-costs","modelReservationIds":identities})).send().await.unwrap();
                    assert!(acknowledgement.status().is_success(), "Authorized isolated validation must acknowledge real unknown costs without inventing usage.");
                    let acknowledged: Value = acknowledgement.json().await.unwrap();
                    assert_eq!(acknowledged["costUnknown"], true);
                    acknowledged_unknown_cost = true;
                    println!("{}", json!({"phase":"live-unknown-cost-acknowledged","costUnknown":true}));
                }
            }
            let stop_is_settling = repository.latest_event(&task.task_id, "task.user-directive").unwrap()
                .is_some_and(|event| event.payload["userMessageId"] == stop_id && event.payload["stop"] == true);
            if task.usage.model_requests.saturating_sub(stop_model_baseline) >= 3 && task.state != TaskState::Paused && !stop_is_settling {
                // Count reservations only as a cost guard, never as evidence that the response is ready.
                // 预约数只用于测试费用护栏，不能当作模型回复已返回的证据。
                let latest = repository.load(&task.task_id).unwrap();
                let settling = repository.latest_event(&task.task_id, "task.user-directive").unwrap()
                    .is_some_and(|event| event.payload["userMessageId"] == stop_id && event.payload["stop"] == true);
                if latest.state == TaskState::Paused || settling { continue; }
                let paused = fixture.client.post(format!("{}/api/tasks/{}/control", fixture.url, task.task_id))
                    .json(&json!({"conversationId":fixture.conversation,"expectedVersion":latest.version,"action":"pause"})).send().await.unwrap();
                assert!(paused.status().is_success());
                panic!("A third generation followed the stop guidance; isolated progress was paused and preserved.");
            }
            assert_ne!(task.state, TaskState::Completed, "A user stop must not certify unfinished report requirements.");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }).await.expect("Live stop must supersede the old minimum duration and settle the task promptly.");
    assert_eq!(settled.task_id, original.task_id);
    assert_eq!(settled.request_id, original.request_id);
    let delivered = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(status) = fixture.runtime.run_status(&fixture.conversation, &original.request_id).await {
                if status["status"] == "completed" && status["result"].is_object() { break status["result"].clone(); }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.expect("The stop acknowledgement must finish delivery after durable task pause.");
    assert!(delivered["taskPauseError"].is_null(), "The actual tree pause must succeed, not only the assistant's acknowledgement.");
    assert_eq!(delivered["type"], "completed");
    let operations = repository.operations(&original.task_id).unwrap();
    for original_operation in original_operations {
        assert!(operations.iter().any(|operation| operation.intent.operation_id == original_operation.intent.operation_id
            && operation.state == original_operation.state), "Pause, resume and stop must retain original receipts.");
    }
    let messages = fixture.messages().await;
    let seed_write_call_ids = messages.iter().flat_map(|message| message["ToolActivities"].as_array().into_iter().flatten())
        .filter(|activity| activity["name"] == "filesystem.write" && activity["arguments"]["path"].as_str()
            .is_some_and(|path| path.ends_with("progress.txt")))
        .filter_map(|activity| activity["toolCallId"].as_str()).collect::<std::collections::HashSet<_>>();
    assert_eq!(operations.iter().filter(|operation| operation.intent.tool_name == "filesystem.write"
        && seed_write_call_ids.contains(operation.intent.tool_call_id.as_str())).count(), 1,
        "The original progress.txt write must never be repeated.");
    assert_eq!(std::fs::read_to_string(fixture.root.join("work/progress.txt")).unwrap(), "native-file-body");
    let last = messages.iter().find(|message| message["Id"] == stop_reply).expect("The stop guidance needs its own public reply.");
    assert_eq!(last["ReplyTo"], stop_id);
    assert_eq!(last["Status"], "completed");
    assert_eq!(last["UserTaskDirective"]["userMessageId"], stop_id);
    assert_eq!(last["UserTaskDirective"]["minimumActiveMs"], 0);
    assert_eq!(last["UserTaskDirective"]["stop"], true);
    assert_eq!(last["ToolRun"]["taskCompletion"]["userStopped"], true);
    assert_eq!(last["ToolRun"]["taskCompletion"]["completionState"], "partial");
    assert!(original_requirements.iter().all(|requirement| last["UserTaskDirective"]["suspendedRequirements"]
        .as_array().is_some_and(|suspended| suspended.contains(requirement))), "Stopped requirements remain resumable evidence, not deleted progress.");
    assert!(!last["Content"].as_str().unwrap_or("").is_empty());
    println!("{}", json!({"liveProvider":provider,"model":model,"state":settled.state,"modelRequests":settled.usage.model_requests,
        "inputTokens":settled.usage.input_tokens,"outputTokens":settled.usage.output_tokens,"elapsedMs":started_at.elapsed().as_millis(),
        "preservedOperations":operations.len(),"seedWrites":1,"pauseResume":true,"appendRead":!stop_only,"latestStop":true,
        "newStopModelRequests":settled.usage.model_requests.saturating_sub(stop_model_baseline),"unknownTokenRequests":settled.usage.unknown_token_requests}));
    drop(response);
    fixture.close().await;
}

async fn submit_live_steering(fixture: &Fixture,
    repository: &kynxa_agent_core_native::storage::task_repository::TaskRepository, task_id: &str, mut input: Value) {
    // Match the desktop's optimistic concurrency retry while retaining the identical user/reply identities.
    // 遵循桌面乐观并发重试；状态重读时保留相同用户消息与回复身份，不重复发送为新消息。
    for attempt in 0..2 {
        input["expectedVersion"] = json!(repository.load(task_id).unwrap().version);
        let response = fixture.client.post(format!("{}/api/tasks/{task_id}/steer", fixture.url))
            .json(&input).send().await.unwrap();
        let status = response.status();
        if status.is_success() { return; }
        let error: Value = response.json().await.unwrap_or(Value::Null);
        let task = repository.load(task_id).unwrap();
        println!("{}", json!({"phase":"live-steering-retry","status":status.as_u16(),"code":error["code"],"state":task.state,"version":task.version}));
        assert!(attempt == 0 && matches!(error["code"].as_str(), Some("TASK_VERSION_CONFLICT" | "TASK_CONFLICT"))
            && task.version != input["expectedVersion"].as_u64().unwrap() && !task.state.is_terminal(),
            "Live steering failed; status={}, code={}, state={:?}.", status.as_u16(), error["code"], task.state);
    }
    unreachable!();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_completion_control_uses_acceptance_without_external_discovery_or_repeating_effects() {
    let fixture = Fixture::new(false, false).await;
    fixture.provider.annotated_evidence_cycle.store(true, Ordering::Release);
    fixture.provider.native_completion_control.store(true, Ordering::Release);
    let mut input = fixture.request("生成 annotated-result.txt 文件并回读当前内容，然后简短回复。");
    input["userMessageId"] = json!(new_id());
    let response = fixture.client.post(format!("{}/api/chat/stream", fixture.url)).json(&input).send().await.unwrap();
    assert!(response.status().is_success());
    let stream = tokio::time::timeout(Duration::from_secs(30), response.text()).await
        .expect("Completion metadata must reach acceptance instead of entering tool discovery.").unwrap();
    let events = event_values(&mut stream.as_bytes().to_vec());
    let completed = events.iter().find(|event| event["type"] == "completed").expect(&stream);
    assert_eq!(completed["taskCompletion"]["completionState"], "completed", "{completed}");
    assert_settled_public_segments(&events);
    assert!(!events.iter().any(|event| matches!(event["type"].as_str(), Some("error" | "interrupted"))), "{stream}");
    let repository = kynxa_agent_core_native::storage::task_repository::TaskRepository::open(&fixture.root.join("Data")).unwrap();
    let task = repository.find_by_request_id(&fixture.conversation, input["requestId"].as_str().unwrap()).unwrap().unwrap();
    let operations = repository.operations(&task.task_id).unwrap();
    assert_eq!(operations.len(), 2, "Completion control cannot add a dispatch or replay successful work: {operations:?}");
    for name in ["filesystem.write", "filesystem.read"] {
        assert_eq!(operations.iter().filter(|operation| operation.intent.tool_name == name
            && operation.state == kynxa_agent_core_native::storage::task_repository::OperationState::Succeeded).count(), 1);
    }
    let requests = fixture.provider.requests.lock().await.clone();
    assert!(requests.len() <= 6, "This fixture has two operations and one completion; protocol repair cannot become a review loop.");
    assert!(!requests.iter().any(|request| request["messages"].to_string().contains("TOOL_NOT_DISCOVERED")),
        "An internal completion control must not be misreported as a missing external tool.");
    assert_eq!(std::fs::read_to_string(fixture.root.join("work/annotated-result.txt")).unwrap(), "ANNOTATED_RECEIPT_BODY");
    fixture.close().await;
}

async fn review_execution_preserves_successful_effects(literal: bool) {
    let fixture = Fixture::new(false, false).await;
    if literal { fixture.provider.dsml_review_cycle.store(true, Ordering::Release); }
    else { fixture.provider.native_review_cycle.store(true, Ordering::Release); }
    let mut input = fixture.request("生成 annotated-result.txt 文件并回读当前内容，然后简短回复。");
    input["userMessageId"] = json!(new_id());
    let response = fixture.client.post(format!("{}/api/chat/stream", fixture.url))
        .json(&input).send().await.unwrap();
    assert!(response.status().is_success());
    let stream = response.text().await.unwrap();
    let events = event_values(&mut stream.as_bytes().to_vec());
    assert!(!events.iter().any(|event| matches!(event["type"].as_str(), Some("error" | "interrupted"))), "{stream}");
    let completed = events.iter().find(|event| event["type"] == "completed").expect("Literal DSML must return to real execution before completion.");
    assert_eq!(completed["taskCompletion"]["completionState"], "completed", "{completed}");
    assert_eq!(fixture.provider.dsml_review_emissions.load(Ordering::Acquire), usize::from(literal));
    let requests = fixture.provider.requests.lock().await.clone();
    for review in requests.iter().filter(|request| is_completion_review_request(request)) {
        assert!(review["tools"].as_array().is_some_and(|tools| !tools.is_empty()), "reviews retain the native tool channel");
        assert_ne!(review["tool_choice"], "none");
    }
    assert_eq!(requests.iter().filter(|request| is_completion_review_request(request)).count(), 2,
        "the final review follows one actual native action, not another assessment of the literal intent");
    let first_review = requests.iter().position(is_completion_review_request).unwrap();
    assert!(requests[first_review + 1..].iter().any(|request| !is_completion_review_request(request)
        && request["tools"].as_array().is_some_and(|tools| !tools.is_empty())),
        "review work must use the existing native execution protocol");
    let repository = kynxa_agent_core_native::storage::task_repository::TaskRepository::open(&fixture.root.join("Data")).unwrap();
    let task = repository.find_by_request_id(&fixture.conversation, input["requestId"].as_str().unwrap()).unwrap().unwrap();
    assert_eq!(task.state, kynxa_agent_core_native::storage::task_repository::TaskState::Completed);
    let operations = repository.operations(&task.task_id).unwrap();
    assert_eq!(operations.len(), 3, "literal markup must never create an operation: {operations:?}");
    for name in ["filesystem.write", "filesystem.read", "filesystem.stat"] {
        assert_eq!(operations.iter().filter(|operation| operation.intent.tool_name == name
            && operation.state == kynxa_agent_core_native::storage::task_repository::OperationState::Succeeded).count(), 1);
    }
    let native = operations.iter().find(|operation| operation.intent.tool_name == "filesystem.stat").unwrap();
    assert_eq!(native.intent.tool_call_id, "dsml-native-stat", "only the later real native call may execute");
    assert!(!operations.iter().any(|operation| operation.intent.tool_name == "tool.result.read"));
    assert_eq!(std::fs::read_to_string(fixture.root.join("work/annotated-result.txt")).unwrap(), "ANNOTATED_RECEIPT_BODY");
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn literal_dsml_in_review_returns_to_native_tools_without_repeating_successful_effects() {
    review_execution_preserves_successful_effects(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_calls_in_review_execute_once_without_dropping_the_tool_declarations() {
    review_execution_preserves_successful_effects(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn buffered_tool_rounds_keep_later_receipts_and_final_answer_visible() {
    let fixture = Fixture::new(true, false).await;
    std::fs::write(fixture.root.join("work/second.txt"), "second-body").unwrap();
    fixture.provider.buffered_cycle.store(true, Ordering::Release);
    let mut input = fixture.request("读取 fixture.txt 和 second.txt，返回两份文件的内容。");
    input["userMessageId"] = json!(new_id());
    input["runLimits"]["maxToolCalls"] = json!(2);
    let response = fixture.client.post(format!("{}/api/chat/stream", fixture.url))
        .json(&input).send().await.unwrap();
    assert!(response.status().is_success());
    let text = response.text().await.unwrap();
    let events = event_values(&mut text.as_bytes().to_vec());
    assert_settled_public_segments(&events);
    assert!(!text.contains("INTERNAL_BUFFERED_PROGRESS_MUST_STAY_PRIVATE"),
        "quiet intermediate prose must stay out of public events");
    for id in ["buffered-read-0", "buffered-read-1"] {
        assert_eq!(events.iter().filter(|event| event["type"] == "tool_result"
            && event["tool"]["toolCallId"] == id && event["tool"]["status"] == "completed").count(), 1,
            "each actual read must publish exactly one successful receipt");
    }
    let final_event = events.iter().find(|event| event["type"] == "completed").unwrap();
    assert!(final_event["content"].as_str().unwrap().contains("native-file-body"));
    assert!(final_event["content"].as_str().unwrap().contains("second-body"));
    assert!(final_event["assistantSegments"].as_array().unwrap().iter().any(|segment|
        segment["round"] == 2 && segment["content"] == "" && segment["status"] == "completed"),
        "the silent tool round still needs its completed commentary anchor");
    assert_eq!(fixture.provider.calls.load(Ordering::Acquire), 4,
        "only two reads, the ordinary answer and the acceptance review may request the model");
    let messages = fixture.messages().await;
    let saved = messages.last().unwrap();
    assert_eq!(saved["Status"], "completed");
    assert_eq!(saved["ToolActivities"].as_array().unwrap().len(), 2);
    assert!(saved["AssistantSegments"].as_array().unwrap().iter().all(|segment| segment["status"] != "streaming"));
    // The public history intentionally omits private transcripts; verify preservation at the formal store.
    // 公开历史刻意不提供私有转录；原文保留应核对正式仓储，不能要求公开接口泄露内部正文。
    let formal = kynxa_agent_core_native::storage::gateway::read_model_messages(
        &fixture.root.join("Data"), &fixture.conversation).unwrap();
    assert!(formal.last().unwrap()["ModelTranscript"].to_string().contains("INTERNAL_BUFFERED_PROGRESS_MUST_STAY_PRIVATE"),
        "withholding UI prose must preserve the original model transcript");
    if let Some(path) = std::env::var_os("KYNXA_GATEWAY_BUFFERED_SSE_FIXTURE") {
        // Feed real quiet tool rounds to the unchanged production desktop parser.
        // 将真实静默工具轮次交给未修改的桌面生产解析器，验证后续回执和最终答复确实可读。
        let path = std::path::PathBuf::from(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, &text).unwrap();
    }
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batched_reads_keep_call_order_pairing_and_write_barrier_in_history_and_formal_log() {
    let fixture = Fixture::new(true, false).await;
    std::fs::write(fixture.root.join("work/second.txt"), "second-body").unwrap();
    fixture.provider.parallel_cycle.store(true, Ordering::Release);
    // The fixture chat already holds this immutable user message; the scripted provider issues the calls.
    // 夹具聊天已保存这条不可变用户消息；调用由脚本化供应商发出。
    let input = fixture.request("读取文件 fixture.txt 并告诉我内容。");
    let response = fixture.client.post(format!("{}/api/chat/stream", fixture.url)).json(&input).send().await.unwrap();
    assert!(response.status().is_success());
    let text = response.text().await.unwrap();
    let events = event_values(&mut text.as_bytes().to_vec());
    assert_settled_public_segments(&events);
    let expected = ["parallel-read-a", "parallel-read-b", "parallel-write", "parallel-read-c", "parallel-read-d"];
    // Both batched reads are announced before either result is published.
    // 批次内两个读取先公布运行状态，再发布任一结果。
    let position = |kind: &str, id: &str| events.iter()
        .position(|event| event["type"] == kind && event["tool"]["toolCallId"] == id)
        .unwrap_or_else(|| panic!("missing {kind} {id}: {text}"));
    assert!(position("tool_call", "parallel-read-b") < position("tool_result", "parallel-read-a"), "{text}");
    assert!(position("tool_result", "parallel-write") < position("tool_call", "parallel-read-c"), "{text}");
    let requests = fixture.provider.requests.lock().await.clone();
    let receipts = requests.last().unwrap()["messages"].as_array().unwrap().iter()
        .filter(|message| message["role"] == "tool").cloned().collect::<Vec<_>>();
    assert_eq!(receipts.iter().map(|message| message["tool_call_id"].as_str().unwrap()).collect::<Vec<_>>(), expected);
    for (id, body) in [("parallel-read-a", "native-file-body"), ("parallel-read-b", "second-body"), ("parallel-read-c", "written-once")] {
        let receipt = receipts.iter().find(|message| message["tool_call_id"] == id).unwrap().to_string();
        assert!(receipt.contains(body), "{id} must pair with its own result: {receipt}");
    }
    assert!(!receipts[0].to_string().contains("second-body"), "{}", receipts[0]);
    assert_eq!(std::fs::read_to_string(fixture.root.join("work/parallel-out.txt")).unwrap(), "written-once");
    let messages = fixture.messages().await;
    let activities = messages[1]["ToolActivities"].as_array().unwrap();
    assert_eq!(activities.iter().map(|activity| activity["toolCallId"].as_str().unwrap()).collect::<Vec<_>>(), expected);
    assert!(activities.windows(2).all(|pair| pair[0]["order"].as_u64() < pair[1]["order"].as_u64()), "{activities:?}");
    assert!(activities.iter().all(|activity| activity["status"] == "completed" && activity["resultRef"]["id"].is_string()), "{activities:?}");
    assert_eq!(activities.iter().filter(|activity| activity["name"] == "filesystem.write").count(), 1);
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unloaded_tool_definition_loads_once_and_continues_with_original_call() {
    let fixture = Fixture::new(false, false).await;
    fixture
        .provider
        .unloaded_cycle
        .store(true, Ordering::Release);
    let request = fixture.request("你好");
    let request_id = request["requestId"].as_str().unwrap().to_owned();
    let response = fixture
        .client
        .post(format!("{}/api/chat/stream", fixture.url))
        .json(&request)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let text = response.text().await.unwrap();
    let events = event_values(&mut text.as_bytes().to_vec());
    assert!(
        events.iter().filter(|event| event["type"] == "tool_result"
            && event["tool"]["name"] == "filesystem.stat"
            && event["tool"]["status"] == "completed").count() == 2,
        "{text}"
    );
    assert!(!text.contains("TOOL_NOT_LOADED"), "{text}");
    assert!(
        events.iter().any(|event| event["type"] == "completed"
            && event["content"]
                .as_str()
                .is_some_and(|content| content.contains("Tool loading recovered"))),
        "{text}"
    );
    let repository = kynxa_agent_core_native::storage::task_repository::TaskRepository::open(
        &fixture.runtime.config.data_root,
    )
    .unwrap();
    let task = repository
        .find_by_request_id(&fixture.conversation, &request_id)
        .unwrap()
        .unwrap();
    let operations = repository.operations(&task.task_id).unwrap();
    assert_eq!(operations.len(), 2);
    assert!(operations.iter().all(|operation| operation.state
        == kynxa_agent_core_native::storage::task_repository::OperationState::Succeeded));
    assert_eq!(fixture.provider.calls.load(Ordering::Acquire), 3);
    let requests = fixture.provider.requests.lock().await;
    let wire_name = kynxa_agent_core_native::models::protocol::wire_name("filesystem.stat");
    assert!(!requests[0]["tools"].as_array().unwrap().iter().any(|tool| tool["function"]["name"] == wire_name));
    assert!(requests[1]["tools"].as_array().unwrap().iter().any(|tool| tool["function"]["name"] == wire_name));
    let result = requests[1]["messages"].as_array().unwrap().iter().find(|message| message["role"] == "tool").unwrap();
    assert_eq!(result["tool_call_id"], "unloaded-0");
    assert!(result["content"].as_str().unwrap().contains("fixture.txt"), "{result}");
    assert!(!requests.iter().any(|request| request["messages"].as_array().unwrap().iter().any(|message| {
        message["tool_calls"].as_array().is_some_and(|calls| calls.iter().any(|call| call["function"]["name"] == kynxa_agent_core_native::models::protocol::wire_name("tool.load")))
    })));
    drop(requests);
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_mcp_load_diagnostic_keeps_streaming_and_persists_final_reply() {
    let mut fixture = Fixture::with_mcp(false, false, true).await;
    // An owned loopback server rejects its handshake; no external MCP, process or credentials participate.
    // 自有本机服务拒绝握手，不连接外部 MCP、不启动脚本进程，也不读取真实凭据。
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mcp_url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let service = Router::new().route(
        "/mcp",
        post(|| async {
            (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error":"isolated unavailable MCP"})),
            )
        }),
    );
    fixture.servers.push(tokio::spawn(async move {
        axum::serve(listener, service).await.unwrap();
    }));
    let response = fixture
        .client
        .get(format!("{}/api/agent/config", fixture.url))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let mut configuration: Value = response.json().await.unwrap();
    configuration["expectedRevision"] = configuration["revision"].clone();
    configuration["mcpServers"] = json!([{"id":"offline-fixture","name":"Offline fixture","enabled":true,
        "transport":"streamable-http","url":mcp_url,"startupTimeoutMs":1000,"disabledTools":[]}]);
    let saved = fixture
        .client
        .put(format!("{}/api/agent/config", fixture.url))
        .json(&configuration)
        .send()
        .await
        .unwrap();
    assert!(
        saved.status().is_success(),
        "{}",
        saved.text().await.unwrap()
    );
    let response = fixture
        .client
        .post(format!("{}/api/chat/stream", fixture.url))
        .json(&fixture.request("检查隔离 MCP；不可用时也继续回答。"))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let text = response.text().await.unwrap();
    if let Some(path) = std::env::var_os("KYNXA_GATEWAY_MCP_SSE_FIXTURE") {
        let path = PathBuf::from(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, &text).unwrap();
    }
    let events = event_values(&mut text.as_bytes().to_vec());
    let status = events
        .iter()
        .position(|event| event["type"] == "mcp_status")
        .unwrap_or_else(|| panic!("{text}"));
    assert_eq!(events[status]["serverId"], "offline-fixture");
    assert_eq!(events[status]["ready"], false);
    assert!(
        events[status]["code"]
            .as_str()
            .is_some_and(|code| !code.is_empty())
    );
    assert!(
        events[status]["message"]
            .as_str()
            .is_some_and(|message| !message.is_empty())
    );
    assert_eq!(events[status]["conversationId"], fixture.conversation);
    assert!(
        events[status + 1..]
            .iter()
            .any(|event| event["type"] == "text_delta"
                && event["delta"]
                    .as_str()
                    .is_some_and(|text| text.contains("仍能继续"))),
        "{text}"
    );
    assert!(
        events.iter().any(|event| event["type"] == "completed"
            && event["content"]
                .as_str()
                .is_some_and(|text| text.contains("仍能继续"))),
        "{text}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event["type"] == "tool_call")
            .count(),
        2
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event["type"] == "tool_result")
            .count(),
        2
    );
    assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 3);
    let messages = fixture.messages().await;
    assert_eq!(messages.len(), 2);
    assert!(
        messages[1]["Content"]
            .as_str()
            .is_some_and(|text| text.contains("仍能继续"))
    );
    assert_eq!(messages[1]["ToolActivities"].as_array().unwrap().len(), 2);
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dispatched_summary_cancellation_keeps_formal_diagnostics_equal_to_task_usage() {
    use kynxa_agent_core_native::storage::task_repository::TaskRepository;
    let fixture = Fixture::new(true, false).await;
    fixture
        .provider
        .context_pressure
        .store(true, Ordering::Release);
    fixture
        .provider
        .cancel_summary
        .store(true, Ordering::Release);
    std::fs::write(
        fixture.root.join("work/fixture.txt"),
        "Public source body with operative value 431. ".repeat(512),
    )
    .unwrap();
    let config: Value = fixture
        .client
        .get(format!("{}/api/models", fixture.url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut profile = config["providers"][0].clone();
    profile["contextWindowTokens"] = json!(16384);
    profile["maxOutputTokens"] = json!(4096);
    profile["apiKey"] = json!("");
    let saved = fixture
        .client
        .post(format!("{}/api/models", fixture.url))
        .json(&profile)
        .send()
        .await
        .unwrap();
    assert_eq!(saved.status(), 200);
    let input = fixture.request("读取文件 fixture.txt 并告诉我内容。");
    let response = fixture
        .client
        .post(format!("{}/api/chat/stream", fixture.url))
        .json(&input)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let mut stream = response.bytes_stream();
    tokio::time::timeout(Duration::from_secs(15), async {
        while fixture.provider.summary_requests.load(Ordering::Acquire) == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("The actual current-step summary request must reach the local HTTP provider.");
    let cancelled: Value = fixture
        .client
        .post(format!("{}/api/chat/cancel", fixture.url))
        .json(&json!({"conversationId":fixture.conversation,"requestId":input["requestId"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cancelled["cancelled"], true);
    let mut buffer = Vec::new();
    let mut terminal = Value::Null;
    while let Some(bytes) = tokio::time::timeout(Duration::from_secs(15), stream.next())
        .await
        .unwrap()
    {
        buffer.extend_from_slice(&bytes.unwrap());
        for event in event_values(&mut buffer) {
            if ["interrupted", "error"].contains(&event["type"].as_str().unwrap_or("")) {
                terminal = event;
            }
        }
    }
    assert_eq!(terminal["type"], "interrupted", "{terminal}");
    let repository = TaskRepository::open(&fixture.runtime.config.data_root).unwrap();
    let task = repository
        .find_by_request_id(&fixture.conversation, input["requestId"].as_str().unwrap())
        .unwrap()
        .unwrap();
    let calls = fixture.provider.calls.load(Ordering::Acquire) as u64;
    assert_eq!(
        fixture.provider.summary_requests.load(Ordering::Acquire),
        1,
        "The cancelled summary must not be retried."
    );
    assert_eq!(
        calls, 2,
        "Only the source-reading step and the cancelled summary may be dispatched."
    );
    assert_eq!(task.usage.model_requests, calls);
    assert_eq!(task.usage.failed_requests, 1);
    assert_eq!(task.usage.unknown_token_requests, 1);
    let messages = fixture.messages().await;
    let assistant = messages
        .iter()
        .find(|message| message["Role"] == "assistant" && message["Id"] == input["requestId"])
        .unwrap();
    assert_eq!(
        assistant["ToolRun"]["diagnostics"]["modelCalls"], calls,
        "The cancelled, dispatched summary belongs in the persisted message diagnostics: {assistant}"
    );
    assert_eq!(
        assistant["ToolRun"]["diagnostics"]["totalModelMs"],
        task.usage.model_duration_ms
    );
    assert_eq!(
        repository.operations(&task.task_id).unwrap().len(),
        1,
        "Cancellation must not replay the source read or dispatch any additional tool."
    );
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_keeps_partial_stream_and_does_not_repeat_provider_request() {
    let fixture = Fixture::new(false, true).await;
    let input = fixture.request("开始取消验证。");
    let response = fixture
        .client
        .post(format!("{}/api/chat/stream", fixture.url))
        .json(&input)
        .send()
        .await
        .unwrap();
    let mut stream = response.bytes_stream();
    let mut buffer = Vec::new();
    let mut seen_text = false;
    while !seen_text {
        let bytes = tokio::time::timeout(Duration::from_secs(15), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        buffer.extend_from_slice(&bytes);
        seen_text = event_values(&mut buffer)
            .iter()
            .any(|event| event["type"] == "text_delta");
    }
    let cancelled: Value = fixture
        .client
        .post(format!("{}/api/chat/cancel", fixture.url))
        .json(&json!({"conversationId":fixture.conversation,"requestId":input["requestId"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cancelled["cancelled"], true);
    let mut finished = false;
    while let Some(bytes) = tokio::time::timeout(Duration::from_secs(15), stream.next())
        .await
        .unwrap()
    {
        buffer.extend_from_slice(&bytes.unwrap());
        for event in event_values(&mut buffer) {
            if ["interrupted", "error"].contains(&event["type"].as_str().unwrap_or("")) {
                finished = true;
            }
        }
    }
    assert!(finished);
    assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 1);
    let messages = fixture.messages().await;
    assert!(
        messages[1]["Content"]
            .as_str()
            .unwrap_or("")
            .contains("保留这段正文"),
        "{}",
        messages[1]
    );
    assert_eq!(messages[1]["Status"], "interrupted");
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn durable_disconnect_keeps_task_alive_until_explicit_cancel() {
    use kynxa_agent_core_native::storage::task_repository::{TaskRepository, TaskState};
    let fixture = Fixture::new(false, true).await;
    let input = fixture.request("开始取消验证。");
    let response = fixture
        .client
        .post(format!("{}/api/chat/stream", fixture.url))
        .json(&input)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let mut stream = response.bytes_stream();
    let mut buffer = Vec::new();
    let mut started = Value::Null;
    while started.is_null() {
        let bytes = tokio::time::timeout(Duration::from_secs(15), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        buffer.extend_from_slice(&bytes);
        for event in event_values(&mut buffer) {
            if event["type"] == "started" {
                started = event;
            }
        }
    }
    assert!(started["taskId"].is_string(), "{started}");
    drop(stream);
    let run = fixture.runtime.begin(input.clone()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        while fixture.provider.calls.load(Ordering::Acquire) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !*run.cancellation.borrow(),
        "SSE disconnect must not cancel durable work"
    );
    let repository = TaskRepository::open(&fixture.runtime.config.data_root).unwrap();
    let accepted = repository
        .find_by_request_id(&fixture.conversation, input["requestId"].as_str().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(accepted.task_id, started["taskId"]);
    fixture
        .runtime
        .cancel(&fixture.conversation, input["requestId"].as_str().unwrap())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !run.finished.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let cancelled = repository.load(&accepted.task_id).unwrap();
    assert_eq!(cancelled.state, TaskState::Cancelled);
    assert_eq!(cancelled.usage.model_requests, 1);
    assert_eq!(fixture.provider.calls.load(Ordering::Acquire), 1);
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn durable_http_reads_real_scope_events_and_reuses_completed_request_after_restart() {
    let fixture = Fixture::new(false, false).await;
    let input = fixture.request("你好");
    let reply: Value = fixture
        .client
        .post(format!("{}/api/chat", fixture.url))
        .json(&input)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(reply["type"], "completed", "{reply}");
    assert!(reply["persistenceError"].is_null(), "{reply}");
    let list: Value = fixture
        .client
        .get(format!(
            "{}/api/tasks?conversationId={}",
            fixture.url, fixture.conversation
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let task = &list["tasks"][0];
    assert_eq!(task["state"], "completed", "{task}");
    assert_eq!(task["usage"]["modelRequests"], 1);
    let events: Value = fixture
        .client
        .get(format!(
            "{}/api/tasks/{}/events?conversationId={}&afterSequence=0",
            fixture.url,
            task["taskId"].as_str().unwrap(),
            fixture.conversation
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(events["throughSequence"].as_u64().unwrap() > 0);
    assert!(
        events["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["kind"] == "task.final")
    );
    let replacement = GatewayRuntime::new(fixture.runtime.config.clone());
    let run = replacement.begin(input.clone()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !run.finished.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(run.result.lock().await.as_ref().unwrap()["reused"], true);
    assert_eq!(fixture.provider.calls.load(Ordering::Acquire), 1);
    let mut changed = input;
    changed["message"] = json!("different request");
    assert_eq!(
        replacement.begin(changed).await.err().unwrap().code,
        "CHAT_REQUEST_CONFLICT"
    );
    replacement.shutdown().await;
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn durable_pause_preserves_budget_and_late_receipt_then_cancel_is_explicit() {
    use kynxa_agent_core_native::storage::task_repository::{
        TaskControl, TaskRepository, TaskState,
    };
    let fixture = Fixture::new(false, true).await;
    let input = fixture.request("开始取消验证。");
    let run = fixture.runtime.begin(input).await.unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        while fixture.provider.calls.load(Ordering::Acquire) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let journal = run.journal.as_ref().unwrap();
    let task = journal.load_current().unwrap();
    fixture
        .runtime
        .tasks
        .control(&task.task_id, task.version, TaskControl::Pause)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !run.finished.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let repository = TaskRepository::open(&fixture.runtime.config.data_root).unwrap();
    let paused = repository.load(&task.task_id).unwrap();
    assert_eq!(paused.state, TaskState::Paused);
    assert_eq!(paused.usage.model_requests, 1);
    assert_eq!(paused.usage.failed_requests, 1);
    assert!(
        repository
            .latest_checkpoint(&task.task_id)
            .unwrap()
            .is_some()
    );
    fixture
        .runtime
        .tasks
        .control(&task.task_id, paused.version, TaskControl::Cancel)
        .await
        .unwrap();
    assert_eq!(
        repository.load(&task.task_id).unwrap().state,
        TaskState::Cancelled
    );
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn user_steering_anchors_the_new_reply_after_formal_input_without_restarting_attempt() {
    use kynxa_agent_core_native::storage::task_repository::{TaskRepository, TaskState};
    let fixture = Fixture::new(false, true).await;
    fixture
        .provider
        .steering_cycle
        .store(true, Ordering::Release);
    let input = fixture.request("开始取消验证。");
    let response = fixture
        .client
        .post(format!("{}/api/chat/stream", fixture.url))
        .json(&input)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let mut stream = response.bytes_stream();
    let mut buffer = Vec::new();
    let mut task_id = String::new();
    let mut saw_text = false;
    while !saw_text || task_id.is_empty() {
        let bytes = tokio::time::timeout(Duration::from_secs(15), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        buffer.extend_from_slice(&bytes);
        for event in event_values(&mut buffer) {
            if event["type"] == "started" {
                task_id = event["taskId"].as_str().unwrap().to_owned();
            }
            saw_text |= event["type"] == "text_delta";
        }
    }
    let repository = TaskRepository::open(&fixture.runtime.config.data_root).unwrap();
    let before = repository.load(&task_id).unwrap();
    let initial_attempt = repository.current_attempt(&task_id).unwrap().unwrap();
    let message_id = new_id();
    let reply_id = new_id();
    let amendment = "更新方案：保留原范围，只解释结果，不再继续写入。";
    let steering_input = json!({"conversationId":fixture.conversation,"expectedVersion":before.version,"userMessageId":message_id,"replyMessageId":reply_id,"message":amendment,"action":"update"});
    let steering_started = std::time::Instant::now();
    let response = fixture
        .client
        .post(format!("{}/api/tasks/{task_id}/steer", fixture.url))
        .json(&steering_input)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let accepted: Value = response.json().await.unwrap();
    assert_eq!(status, 200, "{accepted}");
    assert_eq!(accepted["reconnectRequired"], false);
    assert_eq!(accepted["resumeRequired"], false);
    assert_eq!(accepted["task"]["state"], "running");
    assert_eq!(accepted["task"]["taskId"], task_id);
    assert_eq!(accepted["affectedTaskIds"][0], task_id);
    assert_eq!(accepted["replyMessageId"], reply_id);
    let mut events = Vec::new();
    while !events.iter().any(|event:&Value|event["type"]=="text_delta" && event["replyMessageId"]==reply_id) {
        let bytes=tokio::time::timeout(Duration::from_secs(8),stream.next()).await.unwrap().unwrap().unwrap();
        buffer.extend_from_slice(&bytes);
        events.extend(event_values(&mut buffer));
    }
    assert!(steering_started.elapsed()<Duration::from_secs(8),"Guidance must not wait for the 30-second obsolete generation.");
    let latest_message_id = new_id();
    let latest_reply_id = new_id();
    let latest_amendment = "再补充：用简短结论回答，接着当前工作。";
    let latest_input = json!({"conversationId":fixture.conversation,"expectedVersion":repository.load(&task_id).unwrap().version,"userMessageId":latest_message_id,"replyMessageId":latest_reply_id,"message":latest_amendment,"action":"update"});
    let latest: Value = fixture.client.post(format!("{}/api/tasks/{task_id}/steer",fixture.url)).json(&latest_input).send().await.unwrap().json().await.unwrap();
    assert_eq!(latest["replyMessageId"],latest_reply_id,"{latest}");
    while let Some(bytes) = tokio::time::timeout(Duration::from_secs(15), stream.next())
        .await
        .unwrap()
    {
        buffer.extend_from_slice(&bytes.unwrap());
        events.extend(event_values(&mut buffer));
    }
    assert_eq!(events.last().unwrap()["type"], "completed");
    assert_eq!(events.last().unwrap()["requestId"], input["requestId"]);
    assert_eq!(events.last().unwrap()["replyMessageId"], latest_reply_id);
    let anchor = events.iter().rposition(|event|event["type"]=="reply_anchor").unwrap();
    assert_eq!(events[anchor]["replyMessageId"],latest_reply_id);
    assert_eq!(events[anchor]["userMessageId"],latest_message_id);
    assert!(events[anchor]["supersededReplyMessageId"]==input["requestId"] || events[anchor]["supersededReplyMessageId"]==reply_id);
    assert!(events[anchor+1..].iter().all(|event|event["replyMessageId"]==latest_reply_id));
    assert!(!events.last().unwrap()["content"].as_str().unwrap().contains("已开始，保留这段正文。"));
    let completed_model_calls = fixture.provider.calls.load(Ordering::Acquire);
    assert_eq!(
        completed_model_calls,
        3,
        "Two superseded generations and the final context-only answer need no independent execution review."
    );
    let actual = fixture.provider.requests.lock().await;
    assert_eq!(actual.len(), 3);
    assert_eq!(
        actual.iter().filter(|request| is_completion_review_request(request)).count(),
        0,
        "Guidance that requests explanation without external work must not introduce an execution review."
    );
    assert!(
        actual[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["role"] == "user" && message["content"] == amendment)
    );
    // Runtime guidance must not impersonate a later user request and obscure the real steering text.
    // 调度说明不能冒充更晚的用户请求，覆盖用户实际插话。
    assert_eq!(actual[1]["messages"].as_array().unwrap().last().unwrap()["content"], amendment);
    drop(actual);
    let settled=repository.load(&task_id).unwrap();
    assert_eq!(settled.usage.model_requests,3);
    assert_eq!(settled.usage.failed_requests,0,"User guidance is not a failed model request.");
    assert_eq!(settled.usage.retries,0);
    assert!(!fixture.root.join("work/obsolete.txt").exists(),"Partial tool arguments must never execute.");
    assert!(repository.operations(&task_id).unwrap().is_empty());
    assert_eq!(
        repository.load(&task_id).unwrap().state,
        TaskState::Completed
    );
    assert_eq!(repository.user_steering(&task_id).unwrap().len(), 2);
    assert_eq!(
        repository.current_attempt(&task_id).unwrap().unwrap().token,
        initial_attempt.token,
        "steering must retain the original execution fence and receipt owner"
    );
    let messages = fixture.messages().await;
    assert_eq!(
        messages
            .iter()
            .filter(|message| message["Id"] == message_id && message["Content"] == amendment)
            .count(),
        1
    );
    let old = messages.iter().find(|message|message["Id"]==input["requestId"]).unwrap();
    assert!(old["SupersededByUserMessageId"]==message_id || old["SupersededByUserMessageId"]==latest_message_id);
    assert_eq!(old["Status"],"interrupted");
    assert!(old["Content"].as_str().unwrap().contains("已开始，保留这段正文。"));
    let user_position = messages.iter().position(|message|message["Id"]==latest_message_id).unwrap();
    let reply_position = messages.iter().position(|message|message["Id"]==latest_reply_id).unwrap();
    assert!(reply_position>user_position,"{messages:?}");
    let reply=&messages[reply_position];
    assert_eq!(reply["RequestId"],input["requestId"]);
    assert_eq!(reply["ReplyTo"],latest_message_id);
    assert_eq!(reply["InReplyToUserMessageId"],latest_message_id);
    assert!(reply["SupersededByUserMessageId"].is_null());
    assert!(!reply["Content"].as_str().unwrap().is_empty());
    assert!(!reply["Content"].as_str().unwrap().contains("已开始，保留这段正文。"));
    let replay: Value = fixture
        .client
        .post(format!("{}/api/tasks/{task_id}/steer", fixture.url))
        .json(&steering_input)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(replay["reused"], true);
    assert_eq!(
        fixture.provider.calls.load(Ordering::Acquire),
        completed_model_calls,
        "Replaying accepted guidance must not dispatch another model request."
    );
    drop(stream);
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_tool_result_read_pages_the_formal_archive_without_foreign_or_private_data() {
    use kynxa_agent_core_native::storage::gateway as data;
    let fixture = Fixture::new(false, false).await;
    let root = fixture.root.join("Data");
    let other = new_id();
    data::ensure_conversation(&root, &other, "foreign fixture").unwrap();
    let mut ids = Vec::new();
    for (chat, name, marker) in [
        (
            &fixture.conversation,
            "filesystem.read",
            "PUBLIC_ARCHIVE_中文😀",
        ),
        (&other, "filesystem.read", "FOREIGN_ARCHIVE_MUST_NOT_LEAK"),
        (
            &fixture.conversation,
            "model.continuation",
            "PRIVATE_ARCHIVE_MUST_NOT_LEAK",
        ),
    ] {
        let reference = data::save_tool_result(&root, chat, &new_id(),
            &json!({"id":new_id(),"name":name,"arguments":{}}),
            &json!({"text":marker,"content":[{"type":"image","mimeType":"image/png","data":"MEDIA_BASE64_MUST_NOT_LEAK"}]}), None).unwrap();
        ids.push(reference["id"].as_str().unwrap().to_owned());
    }
    *fixture.provider.archived_read_ids.lock().await = ids;
    let response = fixture
        .client
        .post(format!("{}/api/chat", fixture.url))
        .json(&fixture.request("你好"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let result: Value = response.json().await.unwrap();
    assert_eq!(result["type"], "completed", "{result}");
    let requests = fixture.provider.requests.lock().await;
    let last = requests.last().unwrap();
    let receipts: Vec<Value> = last["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "tool")
        .map(|message| serde_json::from_str(message["content"].as_str().unwrap()).unwrap())
        .collect();
    assert_eq!(receipts.len(), 4);
    assert_ne!(receipts[1]["isError"], true);
    assert!(receipts[1].to_string().contains("PUBLIC_ARCHIVE_中文😀"));
    assert_eq!(
        receipts[2]["code"], "TOOL_RESULT_NOT_FOUND",
        "{}",
        receipts[2]
    );
    assert_eq!(
        receipts[3]["code"], "PRIVATE_MODEL_ATTACHMENT",
        "{}",
        receipts[3]
    );
    let model_view = last.to_string();
    for marker in [
        "FOREIGN_ARCHIVE_MUST_NOT_LEAK",
        "PRIVATE_ARCHIVE_MUST_NOT_LEAK",
        "MEDIA_BASE64_MUST_NOT_LEAK",
    ] {
        assert!(!model_view.contains(marker), "{marker}");
    }
    drop(requests);
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn context_pressure_keeps_loaded_tools_callable_and_normalizes_exact_wire_aliases() {
    let mut fixture = Fixture::new(true, false).await;
    // Observe the extended read-to-write flow; this changes only the test client's wait, not task grants.
    // 完整观察读后切换写入的流程；仅调整测试客户端等待，不放宽产品任务或上下文额度。
    fixture.client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(60))
        .build()
        .unwrap();
    fixture
        .provider
        .context_pressure
        .store(true, Ordering::Release);
    fixture
        .provider
        .pressure_read_reentry
        .store(true, Ordering::Release);
    std::fs::write(
        fixture.root.join("work/fixture.txt"),
        "当前证据与版本规则。".repeat(2000),
    )
    .unwrap();
    std::fs::write(fixture.root.join("work/second.txt"), "SECOND_CURRENT_FILE").unwrap();
    let connection = fixture.provider.requests.lock().await;
    assert!(connection.is_empty());
    drop(connection);
    let config: Value = fixture
        .client
        .get(format!("{}/api/models", fixture.url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut profile = config["providers"][0].clone();
    profile["contextWindowTokens"] = json!(16384);
    profile["maxOutputTokens"] = json!(4096);
    profile["apiKey"] = json!("");
    let saved = fixture
        .client
        .post(format!("{}/api/models", fixture.url))
        .json(&profile)
        .send()
        .await
        .unwrap();
    assert_eq!(saved.status(), 200);
    let mut input = fixture.request(
        "读取 fixture.txt 和 second.txt，记录分析，写 pressure-result.txt 并读回，最后回复交付结果。",
    );
    input["userMessageId"] = json!(new_id());
    input["permissionMode"] = json!("full");
    let result: Value = fixture
        .client
        .post(format!("{}/api/chat", fixture.url))
        .json(&input)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(result["type"], "completed", "{result}");
    let tasks = kynxa_agent_core_native::storage::task_repository::TaskRepository::open(
        &fixture.root.join("Data"),
    )
    .unwrap();
    let task = tasks
        .find_by_request_id(&fixture.conversation, input["requestId"].as_str().unwrap())
        .unwrap()
        .unwrap();
    let operations = tasks.operations(&task.task_id).unwrap();
    assert!(task.usage.compactions > 0, "{result}");
    assert_eq!(
        operations
            .iter()
            .filter(|operation| operation.intent.tool_name == "filesystem.read"
                && operation.state
                    == kynxa_agent_core_native::storage::task_repository::OperationState::Succeeded)
            .count(),
        4,
        "{operations:?}"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("work/pressure-result.txt")).unwrap(),
        "SECOND_CURRENT_FILE"
    );
    assert_eq!(
        operations
            .iter()
            .filter(|operation| operation.intent.tool_name == "task.document"
                && operation.state
                    == kynxa_agent_core_native::storage::task_repository::OperationState::Succeeded)
            .count(),
        2,
        "The recorded analysis must be read back through its real document identity."
    );
    assert_eq!(
        operations
            .iter()
            .filter(|operation| operation.intent.tool_name == "task.document"
                && operation.state
                    == kynxa_agent_core_native::storage::task_repository::OperationState::Failed)
            .count(),
        1,
        "Missing document identity must remain an error; it cannot select another document."
    );
    let final_body = result["content"].as_str().expect("A completed reply must have a final public body.");
    assert!(final_body.contains("pressure-result.txt"), "{result}");
    assert!(final_body.contains("SECOND_CURRENT_FILE"), "{result}");
    assert!(!final_body.contains("<kynxa_task>"), "{result}");
    let requests = fixture.provider.requests.lock().await;
    let reentry_request = requests
        .iter()
        .filter(|request| {
            request["tools"]
                .as_array()
                .is_some_and(|tools| !tools.is_empty())
        })
        .nth(7)
        .expect("The model must receive the post-load read reentry request.");
    assert!(
        reentry_request["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| {
                tool["function"]["name"]
                    == kynxa_agent_core_native::models::protocol::wire_name("filesystem.read")
            }),
        "Consolidation displaced the reader that was explicitly loaded for the next gap."
    );
    // Summary requests may precede review; select the last executable request by its actual tool schemas.
    // 验收前可能还有摘要请求；按实际工具声明选择最后一条主执行请求。
    assert!(requests.last().is_some_and(is_completion_review_request), "Effectful work must end with a completion review.");
    let last_request = requests.iter().rev().find(|request| !is_completion_review_request(request)
        && request["tools"].as_array().is_some_and(|tools| !tools.is_empty()))
        .expect("The model must receive an executable delivery request after compaction.");
    let last = last_request.to_string();
    assert!(last.contains("SECOND_CURRENT_FILE"));
    assert!(!last.contains("TOOL_NOT_LOADED"));
    assert!(
        last_request["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["function"]["name"]
                == kynxa_agent_core_native::models::protocol::wire_name("filesystem.write")),
        "The requested writer was displaced after compaction: {last_request}"
    );
    // Delivery may trim an unused reader only while its real loading path remains discoverable.
    // 交付时可裁剪暂不使用的读取声明，但必须保留真实可用的重新加载入口和已知名称。
    let declares = |name: &str| last_request["tools"].as_array().unwrap().iter().any(|tool|
        tool["function"]["name"] == kynxa_agent_core_native::models::protocol::wire_name(name));
    let reader_navigation = last_request["messages"].as_array().into_iter().flatten()
        .filter_map(|message| message["content"].as_str().and_then(|content| serde_json::from_str::<Value>(content).ok()))
        .any(|checkpoint| checkpoint["contextCheckpoint"] == true
            && checkpoint["capabilities"]["load"] == "tool.load"
            && checkpoint["capabilities"]["loadableNames"].as_array().is_some_and(|names|
                names.iter().any(|name| name == "filesystem.read")));
    assert!(declares("filesystem.read") || declares("tool.load") && reader_navigation,
        "A pressure-trimmed reader needs both the loader schema and explicit checkpoint navigation: {last_request}");
    // Actual read receipts, restored analysis and the final file above verify reentry and delivery.
    // 上面的真实读取回执、恢复分析及最终文件已验证接续交付，不依赖已移除的强制收尾提示文案。
    drop(requests);
    fixture.close().await;
}

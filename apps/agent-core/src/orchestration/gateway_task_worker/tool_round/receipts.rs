//! Child receipt projection preserves originals and updates only the affected execution evidence.
//! 子回执投影保留原文，只更新本次执行涉及的证据。

use super::*;
use super::dispatch::PreparedCall;

pub(super) async fn settle(round: &mut ToolRound<'_>, call: &Value, prepared: &mut PreparedCall,
    mut receipt: Value) -> Result<()> {
    let runtime = round.runtime;
    let parent = round.parent;
    let run = round.run;
    let journal = round.journal;
    let child = round.child;
    let state = &mut *round.state;
    let effects = &mut *round.effects;
    let catalog = &mut *round.catalog;
    let ctx = &prepared.context;
    let name = call["name"].as_str().unwrap_or("");
    let observation = prepared.observation;
    let before = &prepared.before;
    let can_use_workspace = prepared.can_use_workspace;
    let failure_definition = &prepared.failure_definition;
    let failure_environment = &prepared.failure_environment;
    gateway_tool_execution::normalize_reused_receipt(name, &mut receipt);
    if receipt["resultRef"].is_null() {
        receipt = archive_receipt(runtime, run, call, receipt).await?;
    }
    if observation && receipt["isError"] != true {
        let effect_state = effects
            .apply("effects.state", &json!({}))
            .unwrap_or(Value::Null);
        let unknown = effect_state["unknown"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let proofs = tools::verify_unknown_effects(
            &ctx,
            &unknown,
            &json!({"call":call,"result":receipt}),
        )
        .await?;
        for proof in &proofs {
            let original = unknown
                .iter()
                .find(|entry| entry["call"]["id"] == proof["toolCallId"]);
            if let Some(original) = original {
                let verified = json!({"status":"completed","isError":false,"verified":true,"content":proof["result"]["value"],"verificationRef":receipt["resultRef"]});
                let verified =
                    archive_receipt(runtime, run, &original["call"], verified).await?;
                let owned = journal.save_blob("worker-log", &serde_json::to_vec(&verified).map_err(|_| failure("INVALID_TASK_RECEIPT", "Verification receipt encoding failed. / 核验回执编码失败。"))?).await?;
                let operation_id = journal.repository().operations(journal.task_id())?.iter().rev().find(|operation|operation.intent.tool_call_id==original["call"]["id"].as_str().unwrap_or("") && operation.state==OperationState::Unknown).map(|operation|operation.intent.operation_id.clone()).ok_or_else(||failure("TASK_OPERATION_UNKNOWN", "Unknown effect lost its formal operation identity. / 未知副作用缺少正式操作身份。"))?;
                journal.finish_operation(
                    &operation_id,
                    OperationState::Succeeded,
                    Some(owned),
                    Some(receipt["resultRef"].clone()),
                )?;
            }
        }
        let _ = effects.apply("effects.resolve", &json!({"proofs":proofs}));
    }
    if name == "tool.load" && receipt["isError"] != true {
        for tool in receipt["content"]["tools"].as_array().into_iter().flatten() {
            if let Some(name) = tool["name"]
                .as_str()
                .filter(|name| child_tool_allowed(name))
            {
                if !["tool.search", "tool.load"].contains(&name) {
                    let descriptor = gateway_model_request::descriptor(tool, name);
                    state.loaded_tools.retain(|loaded| loaded["name"] != name);
                    state.loaded_tools.insert(0, descriptor.clone());
                    catalog.retain(|loaded| loaded["name"] != name);
                    catalog.insert(0, descriptor);
                }
            }
        }
    }
    state.deterministic_failures.observe(call, &failure_definition, &failure_environment, &receipt);
    let _ = effects.apply("effects.observe", &json!({"call":call,"result":effect_receipt(&receipt),"observation":observation,"workspaceRoot":ctx.workdir}));
    let after = if can_use_workspace && (!before.is_null() || name.starts_with("filesystem.")) {
        tools::capture_code_version(&ctx).await?
    } else {
        Value::Null
    };
    if !after.is_null() {
        state.code_version = after.clone();
    }
    if !observation && receipt["status"] == "completed" && receipt["reused"] != true {
        if let Some(path) = receipt["content"]["path"].as_str().or_else(|| call["arguments"]["path"].as_str()) {
            state.changed_files.push(
                json!({"path":path,"resultRef":receipt["resultRef"],"codeVersion":after}),
            );
        }
    }
    if name.starts_with("terminal.") && receipt["content"]["exitCode"].is_i64() {
        state.verification.push(json!({"tool":name,"arguments":call["arguments"],"exitCode":receipt["content"]["exitCode"],"resultRef":receipt["resultRef"],"codeVersion":after,"unchangedDuringExecution":*before==after}));
    }
    let source_metadata = crate::models::tool_context::source_metadata(&receipt["content"]);
    state.result_sources.push(json!({"callId":call["id"],"name":name,"arguments":call["arguments"],"sourceMetadata":source_metadata,"originalResultRef":if name=="tool.result.read" {source_metadata["resultRef"].clone()} else {Value::Null},"observation":observation,"executed":receipt["executed"]!=false,"originalExcerpt":crate::models::tool_context::source_excerpt(&receipt["content"],512),"round":state.rounds,"status":receipt["status"],"resultRef":receipt["resultRef"],"archiveOwner":{"requestId":run.request_id,"toolCallId":call["id"],"toolName":name}}));
    let completion_proof = completion_evidence::from_receipt(call, &receipt);
    let declared_read_only = ctx.runtime.schemas.read().await.iter().any(|descriptor|
        descriptor["name"] == name && descriptor["annotations"]["readOnlyHint"] == true);
    let scene_read_only = name.rsplit('.').next().is_some_and(|method|
        matches!(method, "get_scene_info" | "get_object_info" | "list_scene_items" | "get_world_state_snapshot"));
    let mutating = !observation && !declared_read_only && !scene_read_only
        && receipt["executed"] != false && receipt["reused"] != true
        && task::successful_completion_receipt(&receipt)
        && !name.starts_with("task.") && !matches!(name, "tool.load" | "context.compact");
    let projected = data::public_tool_result(
        &receipt["content"],
        Some(&receipt["resultRef"]),
        receipt["content"].get("executionEnvironment"),
        false,
    )?;
    let mut model_receipt = receipt.clone();
    model_receipt["content"] = projected;
    // Allocate evidence references after saving the receipt, using the root's protocol.
    // 保存原回执后再分配证据引用，沿用根任务协议。
    if ["knowledge.search", "knowledge.plan"].contains(&name) && receipt["isError"] != true
    {
        if let Some(archive_id) = receipt["resultRef"]["id"].as_str().filter(|_| {
            model_receipt["content"]["items"].is_array()
                || model_receipt["content"]["structuredContent"]["items"].is_array()
        }) {
            model_receipt["content"] = crate::retrieval::gateway::project_evidence_result(
                &model_receipt["content"],
                archive_id,
            )?;
        }
    }
    if let Some(content) = model_receipt["content"].as_object_mut() {
        content.remove("fullText");
        content.remove("_meta");
    }
    let result = json!({"call":call,"result":model_receipt,"observation":observation});
    state.pending_results.push(result);
    // Keep bounded projections in checkpoints and original media/bodies in tool archives.
    // 检查点保留有界投影与引用，原始媒体和正文保留在工具归档。
    let acceptance_scope = if crate::orchestration::progress::validation_call(call) {
        let mut records = state.receipts.iter().filter_map(|saved|
            saved["completionProof"].is_object().then(||saved["completionProof"].clone())).collect::<Vec<_>>();
        records.push(completion_proof.clone());
        completion_evidence::validation_scope(&state.acceptance_requirements, &records).checkpoint()
    } else { Value::Null };
    state
        .receipts
        .push(json!({"call":call,"result":model_receipt,"observation":observation,
            "completionProof":completion_proof,"mutating":mutating,
            "validationScope":{"cwd":ctx.workdir,"acceptanceScope":acceptance_scope},
            "visualAvailable":task::receipt_has_visual_observation(&receipt["content"]),
            "visualHashes":task::receipt_visual_hashes(&receipt["content"]),
            "beforeVersion":before,"afterVersion":after}));
    task::settle_unexecuted_tool_intent(&mut state.review_checkpoint, name, &receipt);
    state.acceptance_reviewed = false;
    state.tool_calls += 1;
    parent.emit(json!({"type":"task_tool_result","taskId":child.task_id,"toolCallId":call["id"],"status":receipt["status"],"resultRef":receipt["resultRef"]})).await;
    checkpoint(run,journal, state, "tool-received").await?;
    Ok(())
}

pub(super) async fn append_results(round: &mut ToolRound<'_>) -> Result<()> {
    let runtime = round.runtime;
    let run = round.run;
    let connection = round.connection;
    let response = round.response;
    let state = &mut *round.state;
    let mut model_results = state.pending_results.clone();
    for pair in &mut model_results {
        let content = &pair["result"]["content"];
        let image_content = content.get("content").or_else(|| {
            (content["sessionId"].is_string() && content["tabId"].is_string())
                .then(|| content["value"].get("content")).flatten()
        });
        let image_placeholder = image_content.unwrap_or(&Value::Null).as_array().into_iter().flatten()
            .any(|block| block["type"] == "image");
        if image_placeholder && task::successful_completion_receipt(&pair["result"]) {
            let root = runtime.config.data_root.clone();
            let chat = run.conversation_id.clone();
            let reference = pair["result"]["resultRef"].clone();
            let owner = json!({"requestId":run.request_id,"toolCallId":pair["call"]["id"],"toolName":pair["call"]["name"]});
            let canonical = tokio::task::spawn_blocking(move || data::model_media(&root,&chat,&reference,&owner))
                .await.map_err(|_| failure("TASK_RESULT_READ_FAILED","Child visual receipt reader stopped. / 子任务图像回执读取者退出。"))??;
            if task::receipt_has_visual_observation(&canonical) {
                for saved in &mut state.receipts {
                    if saved["call"]["id"] == pair["call"]["id"] {
                        saved["visualAvailable"] = json!(true);
                        saved["visualHashes"]=json!(task::receipt_visual_hashes(&canonical));
                    }
                }
            }
            pair["result"]["content"] = canonical;
        }
    }
    state.history = protocol::append_results(
        protocol::protocol(&connection),
        &state.history,
        &response,
        &model_results,
    )?;
    state.pending = None;
    state.pending_review = false;
    state.pending_results.clear();
    Ok(())
}

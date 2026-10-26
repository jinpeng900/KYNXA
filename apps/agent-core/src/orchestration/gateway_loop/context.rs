//! Restore verified working sources before resuming the root loop.
//! 核验工作来源后恢复根任务，保留逐文档失效和原有状态提交顺序。

use super::*;
use crate::orchestration::gateway_task_journal::TaskJournal;

struct RestoredSources {
    messages: Vec<Value>,
    history_sources: Vec<Value>,
    result_sources: Vec<Value>,
    formal_sources: Vec<Value>,
}

struct VerifiedSources {
    files: HashSet<(String, String)>,
    knowledge: HashSet<String>,
}

struct SourceReferences {
    references: Vec<Value>,
    file_versions: HashSet<(String, String)>,
    knowledge_reads: HashSet<String>,
}

struct VerifiedReceipts {
    legacy_calls: Vec<Value>,
    archive_count: usize,
}

fn invalid_context() -> GatewayError {
    GatewayError::new(
        "INVALID_ROOT_WORK_CONTEXT",
        "工作上下文与当前正式来源不一致，需回读原文。",
        409,
    )
}

pub(super) async fn restore_root_work_context(
    runtime: &GatewayRuntime,
    run: &RequestRun,
    ctx: &tools::GatewayToolContext,
    formal: &[Value],
    original_goal: &Value,
    state: &mut LoopState,
    cancellation: tokio::sync::watch::Receiver<bool>,
) -> Result<bool, GatewayError> {
    let Some(journal) = &run.journal else {
        return Ok(false);
    };
    if let Some(recovered_sequence) = state.assistant["RecoveredReceiptVersion"].as_u64() {
        if journal.latest_checkpoint()?.is_none_or(|checkpoint| checkpoint.through_event_sequence < recovered_sequence) {
            // Prefer the updated formal log when receipts postdate the private snapshot.
            // 回执晚于私有快照时优先使用更新后的正式记录。
            state.assistant["WorkContextRestore"] = json!({"restored":false,"reason":"recovered-receipt-newer-than-checkpoint","fallback":"formal-original-readback"});
            return Ok(false);
        }
    }
    let Some((context, reference)) = journal
        .read_root_work_context(
            state.assistant["Id"].as_str().unwrap_or(&run.request_id),
            &run.request_id,
            &run.fingerprint,
            &state.origin,
        )
        .await?
    else {
        return Ok(false);
    };
    let mut sources = identity_sources(&context, formal, original_goal)?;
    let mut verified =
        restore_document_sources(runtime, run, ctx, journal, &context, state, &cancellation).await?;
    let references =
        collect_context_sources(runtime, run, &mut sources, cancellation.clone()).await?;
    let saved_progress = saved_validation_progress(&context, &state.assistant);
    let receipts = verify_receipt_sources(
        runtime,
        run,
        ctx,
        &state.assistant,
        references.references,
        &saved_progress,
        &mut verified,
        &cancellation,
    )
    .await?;
    verify_live_sources(
        runtime,
        run,
        ctx,
        references.file_versions,
        references.knowledge_reads,
        &verified,
    )
    .await?;
    apply_restored_context(
        runtime,
        run,
        &context,
        reference,
        sources,
        saved_progress,
        receipts,
        state,
        &cancellation,
    )
    .await?;
    Ok(true)
}

fn identity_sources(
    context: &Value,
    formal: &[Value],
    original_goal: &Value,
) -> Result<RestoredSources, GatewayError> {
    let messages = context["messages"].as_array().ok_or_else(invalid_context)?.clone();
    let history_sources = context["historySources"]
        .as_array()
        .ok_or_else(invalid_context)?
        .clone();
    let result_sources = context["resultSources"]
        .as_array()
        .ok_or_else(invalid_context)?
        .clone();
    let formal_sources = context["formalSources"]
        .as_array()
        .ok_or_else(invalid_context)?
        .clone();
    if !crate::models::protocol::complete_pairs(&messages)
        || history_sources.len() > messages.len()
        || !messages
            .iter()
            .any(|message| message["role"] == "user" && (message["content"] == *original_goal
                || message["attachmentMessageId"].as_str().is_some_and(|id| formal.iter().any(|saved|
                    saved["Id"] == id && saved["Role"] == "user" && saved["Content"] == *original_goal))))
        || formal_sources.iter().any(|source| {
            !formal.iter().any(|message| {
                message["Id"] == source["id"]
                    && crate::retrieval::gateway_index::hash(&message.to_string()) == source["hash"]
            })
        })
    {
        return Err(invalid_context());
    }
    Ok(RestoredSources {
        messages,
        history_sources,
        result_sources,
        formal_sources,
    })
}

async fn restore_document_sources(
    runtime: &GatewayRuntime,
    run: &RequestRun,
    ctx: &tools::GatewayToolContext,
    journal: &TaskJournal,
    context: &Value,
    state: &mut LoopState,
    cancellation: &tokio::sync::watch::Receiver<bool>,
) -> Result<VerifiedSources, GatewayError> {
    // Restore document work independently; invalidate only changed files.
    // 独立恢复逐文档成果，仅撤销已变化文件的成果。
    let mut checked_document_files = HashSet::new();
    let mut checked_document_knowledge = HashSet::new();
    let mut changed_document_count = 0;
    state.work_progress =
        serde_json::from_value(context["workProgress"].clone()).unwrap_or_default();
    if context["documentReadbackTargets"].is_array() {
        native(state.progress.apply("progress.restoreDocumentReadbackTargets",&json!({
            "targets":context["documentReadbackTargets"],"sameTaskCheckpoint":true})))?;
    }
    if context["documentDirectory"].is_object() {
        let mut restored_documents =
            DocumentWorkRecords::restore(journal, &context["documentDirectory"]).await?;
        let entries = restored_documents.directory()["documents"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let mut verification_context = ctx.clone();
        verification_context.events = None;
        for entry in entries.iter().filter(|entry| entry["active"] == true) {
            if *cancellation.borrow() {
                return Err(GatewayError::new("ABORT_ERR", "任务恢复已取消。", 499));
            }
            let checked:Result<(),GatewayError>=async {
                let record=restored_documents.read_record(journal,entry["sourceIdentity"].as_str().unwrap_or("")).await?;
                let file_version=entry["path"].as_str().zip(entry["sourceVersion"]["sha256"].as_str());
                if let Some((path,hash))=file_version {
                    let current=tools::execute("filesystem.stat",json!({"path":path,"reason":"Verify saved document version before reuse."}),&verification_context).await?;
                    if crate::models::tool_context::source_metadata(&current)["sha256"]!=hash {
                        return Err(GatewayError::new("ROOT_WORK_SOURCE_CHANGED","已读取文件已变化，需重新取证。",409));
                    }
                    checked_document_files.insert((path.to_owned(),hash.to_owned()));
                }
                for observation in record["observations"].as_array().into_iter().flatten() {
                    let is_knowledge=observation["tool"]=="knowledge.read";
                    // Verify live file bytes; verify knowledge against its original read and current scope.
                    // 文件核验当前字节，知识资料核验原始读取和当前范围。
                    if !is_knowledge && file_version.is_some() {continue;}
                    let expected=observation["resultRef"].clone();
                    let root=runtime.config.data_root.clone();
                    let chat=run.conversation_id.clone();
                    let id=expected["id"].as_str().ok_or_else(invalid_context)?.to_owned();
                    let archive=tokio::task::spawn_blocking(move||data::load_private_tool_result(&root,&chat,&id)).await.map_err(|_|invalid_context())??;
                    if archive["conversationId"]!=run.conversation_id || archive["id"]!=expected["id"]
                        || archive["sha256"]!=expected["sha256"] || archive["bytes"]!=expected["bytes"] {
                        return Err(invalid_context());
                    }
                    if is_knowledge {
                        if archive["toolName"]!="knowledge.read" {return Err(invalid_context());}
                        let source_ref=archive["canonical"]["sourceRef"].as_str().ok_or_else(invalid_context)?.to_owned();
                        if checked_document_knowledge.contains(&source_ref) {continue;}
                        let config=runtime.config.clone();
                        let read_context=json!({"conversationId":run.conversation_id,"requestId":run.request_id});
                        let saved_read=archive["canonical"].clone();
                        let checked_reference=source_ref.clone();
                        tokio::task::spawn_blocking(move||crate::retrieval::gateway_tools::verify_saved_read(&config,&read_context,&checked_reference,&saved_read))
                            .await.map_err(|_|invalid_context())??;
                        checked_document_knowledge.insert(source_ref);
                    }
                }
                Ok(())
            }.await;
            if let Err(error) = checked {
                if error.code == "ABORT_ERR" || *cancellation.borrow() {
                    return Err(GatewayError::new("ABORT_ERR", "任务恢复已取消。", 499));
                }
                restored_documents
                    .invalidate_source(
                        journal,
                        entry["sourceIdentity"].as_str().unwrap_or(""),
                        &error.code,
                    )
                    .await?;
                changed_document_count += 1;
            }
        }
        // Retain only verified document work when the conversation snapshot falls back.
        // 对话快照回退时，仅保留已核验的文档成果。
        state.documents = restored_documents;
    }
    if changed_document_count > 0 {
        return Err(GatewayError::new(
            "ROOT_WORK_SOURCE_CHANGED",
            "部分文档来源已变化；有效文档成果已保留，失效部分需重新取证。",
            409,
        ));
    }
    Ok(VerifiedSources {
        files: checked_document_files,
        knowledge: checked_document_knowledge,
    })
}

async fn collect_context_sources(
    runtime: &GatewayRuntime,
    run: &RequestRun,
    sources: &mut RestoredSources,
    cancellation: tokio::sync::watch::Receiver<bool>,
) -> Result<SourceReferences, GatewayError> {
    let mut references = Vec::new();
    let mut file_versions = HashSet::new();
    let mut knowledge_reads = HashSet::new();
    let mut collect_source = |source: &Value| {
        for field in ["resultRef", "originalResultRef"] {
            if source[field]["id"].is_string()
                && source[field]["sha256"].is_string()
                && source[field]["bytes"].is_u64()
            {
                references.push(source[field].clone());
            }
        }
        let metadata = source
            .get("source")
            .or_else(|| source.get("sourceMetadata"))
            .unwrap_or(&Value::Null);
        if let (Some(path), Some(hash)) = (metadata["path"].as_str(), metadata["sha256"].as_str()) {
            if hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                file_versions.insert((path.to_owned(), hash.to_owned()));
            }
        }
        if source["name"] == "knowledge.read" {
            if let Some(reference) = source["arguments"]["sourceRef"].as_str() {
                knowledge_reads.insert(reference.to_owned());
            }
        }
    };
    for source in &sources.result_sources {
        collect_source(source);
    }
    for source in &mut sources.history_sources {
        collect_source(source);
        for evidence in source["checkpointEvidence"]
            .as_array()
            .into_iter()
            .flatten()
        {
            collect_source(evidence);
        }
        if source["checkpointSemanticSummary"].is_object() {
            let summary = crate::models::gateway_context::verify_working_summary_archive(
                &runtime.config.data_root,
                &run.conversation_id,
                &source["checkpointSemanticSummary"],
                cancellation.clone(),
            )
            .await?;
            if summary["archive"].is_object() && summary["archive"]["verified"] != true {
                return Err(invalid_context());
            }
            for original in summary["sources"].as_array().into_iter().flatten() {
                collect_source(original);
            }
            source["checkpointSemanticSummary"] = summary;
        }
    }
    Ok(SourceReferences {
        references,
        file_versions,
        knowledge_reads,
    })
}

fn saved_validation_progress(context: &Value, assistant: &Value) -> Value {
    let mut saved_progress = context["runProgress"].clone();
    if !saved_progress.is_object() {
        let completion = &assistant["ToolRun"]["taskCompletion"];
        if completion["receipts"].as_array().is_some_and(|receipts| !receipts.is_empty()) {
            saved_progress = json!({"validationReceipts":completion["receipts"]});
            for field in ["mutationRevision", "checkedMutationRevision"] {
                if let Some(value) = completion.get(field) { saved_progress[field] = value.clone(); }
            }
            for (field, source) in [("latestCodeVersion", "latestSnapshot"), ("validatedCodeVersion", "validatedSnapshot")] {
                if let Some(value) = completion["codeVersionCoverage"].get(source) { saved_progress[field] = value.clone(); }
            }
        }
    }
    saved_progress
}

async fn verify_receipt_sources(
    runtime: &GatewayRuntime,
    run: &RequestRun,
    ctx: &tools::GatewayToolContext,
    assistant: &Value,
    mut references: Vec<Value>,
    saved_progress: &Value,
    verified: &mut VerifiedSources,
    cancellation: &tokio::sync::watch::Receiver<bool>,
) -> Result<VerifiedReceipts, GatewayError> {
    let legacy_host_debt =
        crate::orchestration::progress::needs_host_effect_reclassification(saved_progress);
    // Only old unclassified host debt or missing check identity needs supplemental archives.
    // 仅旧宿主待分类项或缺失的检查身份补读归档，新检查点不额外回读已压缩调用。
    let required_reference_count = references.len();
    for activity in assistant["ToolActivities"].as_array().into_iter().flatten().filter(|activity|
        matches!(activity["name"].as_str(), Some("terminal.run" | "terminal.host.run" | "terminal.host.start" | "skill.run"))
            && (legacy_host_debt || saved_progress["validationReceipts"].as_array().into_iter().flatten()
                .any(|receipt| receipt["checkId"].as_str().is_none() && receipt["toolCallId"] == activity["toolCallId"]))) {
        let reference = &activity["resultRef"];
        if reference["id"].is_string() && reference["sha256"].is_string() && reference["bytes"].is_u64() {
            references.push(reference.clone());
        }
    }
    let mut legacy_calls = Vec::new();
    let mut seen = HashSet::new();

    for (reference_index, reference) in references.into_iter().enumerate() {
        if *cancellation.borrow() {
            return Err(GatewayError::new("ABORT_ERR", "任务恢复已取消。", 499));
        }
        if !seen.insert(reference.to_string()) {
            continue;
        }
        let root = runtime.config.data_root.clone();
        let chat = run.conversation_id.clone();
        let id = reference["id"].as_str().unwrap().to_owned();
        // Verify private attachments internally; exclude their bodies from public results and archives.
        // 私有附件仅内部核验，正文不进入公开结果和归档。
        let classification_only = reference_index >= required_reference_count;
        let loaded = tokio::task::spawn_blocking(move || data::load_private_tool_result(&root, &chat, &id))
            .await.map_err(|_| invalid_context())?;
        // Missing optional legacy evidence retains the old debt; it must not reject an otherwise valid checkpoint.
        // 可选旧证据不可读时保留原待核验项，不拒绝其余有效检查点。
        let archive = match loaded {
            Ok(archive) => archive,
            Err(_) if classification_only => continue,
            Err(error) => return Err(error.into()),
        };
        if archive["conversationId"] != run.conversation_id
            || archive["id"] != reference["id"]
            || archive["sha256"] != reference["sha256"]
            || archive["bytes"] != reference["bytes"]
        {
            if classification_only { continue; }
            return Err(invalid_context());
        }
        if archive["requestId"] == run.request_id
            && let Some(activity) = assistant["ToolActivities"].as_array().into_iter().flatten()
                .find(|activity| activity["toolCallId"] == archive["toolCallId"] && activity["name"] == archive["toolName"]
                    && activity["resultRef"] == reference
                    && matches!(activity["name"].as_str(), Some("terminal.run" | "terminal.host.run" | "terminal.host.start" | "skill.run"))) {
            legacy_calls.push(json!({"call":{"id":activity["toolCallId"],"name":activity["name"],"arguments":activity["arguments"]},
                "result":{"status":activity["status"],"executed":activity["executed"] != false,
                    "isError":activity["status"] != "completed" || tools::explicit_tool_error(archive["toolName"].as_str().unwrap_or(""), &archive["canonical"]),
                    "content":archive["canonical"]},"output":archive["canonical"],"validationScope":{"cwd":ctx.workdir}}));
        }
        if archive["toolName"] == "knowledge.read" {
            if let Some(source_ref) = archive["canonical"]["sourceRef"].as_str() {
                // Only verified original reads survive index replacement, not stale structure or unread chunks.
                // 索引替换后仅复用已核验原始读取，不沿用旧结构或未读片段。
                let config = runtime.config.clone();
                let context =
                    json!({"conversationId":run.conversation_id,"requestId":run.request_id});
                let source_ref = source_ref.to_owned();
                let saved_read = archive["canonical"].clone();
                let checked_reference = source_ref.clone();
                tokio::task::spawn_blocking(move || {
                    crate::retrieval::gateway_tools::verify_saved_read(
                        &config,
                        &context,
                        &checked_reference,
                        &saved_read,
                    )
                })
                .await
                .map_err(|_| invalid_context())??;
                verified.knowledge.insert(source_ref);
            }
        }
    }
    Ok(VerifiedReceipts {
        legacy_calls,
        archive_count: seen.len(),
    })
}

async fn verify_live_sources(
    runtime: &GatewayRuntime,
    run: &RequestRun,
    ctx: &tools::GatewayToolContext,
    file_versions: HashSet<(String, String)>,
    knowledge_reads: HashSet<String>,
    verified: &VerifiedSources,
) -> Result<(), GatewayError> {
    let mut verification_context = ctx.clone();
    verification_context.events = None;
    for (path, hash) in file_versions {
        if verified.files.contains(&(path.clone(), hash.clone())) {
            continue;
        }
        let current = tools::execute(
            "filesystem.stat",
            json!({"path":path,"reason":"Verify saved work source version before continuation."}),
            &verification_context,
        )
        .await?;
        let metadata = crate::models::tool_context::source_metadata(&current);
        if metadata["sha256"] != hash {
            return Err(GatewayError::new(
                "ROOT_WORK_SOURCE_CHANGED",
                "已读取文件已变化，需重新取证。",
                409,
            ));
        }
    }
    for source_ref in knowledge_reads {
        if verified.knowledge.contains(&source_ref) {
            continue;
        }
        let config = runtime.config.clone();
        let context = json!({"conversationId":run.conversation_id,"requestId":run.request_id});
        tokio::task::spawn_blocking(move || {
            crate::retrieval::gateway::handle_tool(
                "knowledge.read",
                &json!({"sourceRef":source_ref,"limit":1}),
                &context,
                &config,
            )
            .ok_or_else(invalid_context)?
            .map(|_| ())
            .map_err(GatewayError::from)
        })
        .await
        .map_err(|_| invalid_context())??;
    }
    Ok(())
}

async fn apply_restored_context(
    runtime: &GatewayRuntime,
    run: &RequestRun,
    context: &Value,
    reference: Value,
    sources: RestoredSources,
    mut saved_progress: Value,
    receipts: VerifiedReceipts,
    state: &mut LoopState,
    cancellation: &tokio::sync::watch::Receiver<bool>,
) -> Result<(), GatewayError> {
    let RestoredSources {
        messages,
        history_sources,
        result_sources,
        formal_sources,
    } = sources;
    let applied_steering_ids: HashSet<String> = context["appliedSteeringIds"]
        .as_array()
        .ok_or_else(invalid_context)?
        .iter()
        .map(|id| id.as_str().map(str::to_owned).ok_or_else(invalid_context))
        .collect::<Result<_, _>>()?;
    let selected_names = context["selectedNames"].as_array().ok_or_else(invalid_context)?;
    let catalog = tools::builtin_catalog();
    let mut restored_selected = Vec::new();
    // Restore model-selected order, including recent loads, before schema admission.
    // 声明授额前恢复模型选择的顺序，包括最近加载项。
    for name in selected_names {
        if restored_selected
            .iter()
            .any(|tool: &Value| tool["name"] == *name)
        {
            continue;
        }
        if let Some(index) = state.tools.selected.iter().position(|tool| tool["name"] == *name) {
            restored_selected.push(state.tools.selected.remove(index));
        } else if let Some(tool) = catalog
            .iter()
            .find(|tool| tool["name"] == *name && tools::executable_descriptor(tool))
        {
            restored_selected.push(descriptor(tool, name.as_str().unwrap_or("")));
        }
    }
    restored_selected.append(&mut state.tools.selected);
    state.tools.selected = restored_selected;
    state.context.history = messages;
    for message in &mut state.context.history {
        if message["deferredAttachmentImages"] != true { continue; }
        if *cancellation.borrow() { return Err(GatewayError::new("ABORT_ERR", "附件恢复已取消。", 499)); }
        let message_id = message["attachmentMessageId"].as_str().ok_or_else(invalid_context)?.to_owned();
        // Restore images only; retain extracted text and read coverage.
        // 仅恢复图片，保留已提取文本和读取覆盖。
        let prepared = crate::orchestration::gateway_attachments::prepare_context(runtime, &run.conversation_id, &message_id, 0, true,
            run.native_cancellation.token.clone()).await?;
        let mut parts = message["content"].as_array().cloned().unwrap_or_else(|| vec![json!({"type":"text","text":message["content"]})]);
        for image in prepared["images"].as_array().into_iter().flatten() {
            if let (Some(mime), Some(data)) = (image["mimeType"].as_str(), image["data"].as_str()) {
                parts.push(json!({"type":"image_url","image_url":{"url":format!("data:{mime};base64,{data}")}}));
            }
        }
        message["content"] = json!(parts);
        message.as_object_mut().unwrap().remove("deferredAttachmentImages");
    }
    state.context.history_sources = history_sources;
    state.context.runtime_snapshot = context.get("runtimeSnapshot").filter(|value|
        crate::models::transcript::valid_context_before(&json!([value]))).cloned();
    state.transcript.pending_context_before = context["pendingContextBefore"].as_array()
        .filter(|_| crate::models::transcript::valid_context_before(&context["pendingContextBefore"]))
        .cloned().unwrap_or_default();
    state.context.result_sources = result_sources;
    state.cache.previous_summary = context["previousSummary"].clone();
    state.work_progress =
        serde_json::from_value(context["workProgress"].clone()).unwrap_or_default();
    state.context.formal_sources = formal_sources;
    state.applied_steering_ids = applied_steering_ids;
    state.completion.retained_completion_observations = context["retainedCompletionObservations"].as_array().cloned().unwrap_or_default();
    state.visual.visual_readback_delivered = context["visualReadbackDelivered"] == true;
    state.completion.acceptance_reviewed = context["acceptanceReviewed"] == true;
    state.delivery.pending_delivery = context["pendingDelivery"].as_str().unwrap_or("").to_owned();
    state.delivery.delivery_continuing = context["deliveryContinuing"] == true;
    state.completion.review_checkpoint = context["completionReview"].clone();
    if context["currentWorkScope"].is_object() {
        state.assistant["CurrentWorkScope"] = context["currentWorkScope"].clone();
    }
    state.completion.mutation_sequence = context["mutationSequence"].as_u64().unwrap_or(0);
    state.visual.pending_visual_readback = context["pendingVisualReadback"] == true;
    state.visual.pending_visual_hashes = context["pendingVisualHashes"].as_array().into_iter().flatten()
        .filter_map(Value::as_str).map(str::to_owned).collect();
    state.delivery.last_progress_ms = context["lastProgressMs"].as_u64().unwrap_or(0);
    if let Some(requirements) = context["acceptanceRequirements"].as_array() {
        state.completion.acceptance_requirements = requirements.iter().filter_map(Value::as_str).map(str::to_owned).collect();
    }
    if saved_progress.is_object() {
        // Restore checks from owned checkpoints or formal broker receipts, never model prose.
        // 从自有检查点或正式执行回执恢复检查状态，不采信模型文字。
        // Existing checkpoints can predate diagnostic wiring; recover the known lower bound once.
        // 旧检查点可能尚未接入诊断计数，只补齐已知停滞下界，不在每轮重复累加。
        if let Some(diagnostics) = saved_progress.get_mut("diagnostics").filter(|value| value.is_object()) {
            diagnostics["noProgressRounds"] = json!(diagnostics["noProgressRounds"].as_u64().unwrap_or(0)
                .max(state.work_progress.stagnant_rounds()));
        }
        native(state.progress.apply("progress.restoreValidation", &json!({"state":saved_progress,"legacyCalls":receipts.legacy_calls})))?;
    }
    state.context.work_context_reference = reference;
    state.context.work_context_ready = true;
    state.assistant["WorkContextRestore"] = json!({"restored":true,"sourceArchivesVerified":receipts.archive_count,"scopeVerified":true,"privateContentExposed":false});
    Ok(())
}

//! Initial model inputs preserve verified history, evidence and attachment provenance.
//! 初始模型输入维护已核验的历史、证据及附件来源。

use super::{RootExecution, prepare_in_parallel};
use super::super::{
    CacheState, CompletionState, ContextState, DeliveryState, DocumentWorkRecords,
    GatewayError, GatewayRuntime, LoopState, RequestRun, ToolsState,
    TranscriptState, VisualState, WorkProgress, model_tool_declarations, native,
    now_iso8601, protocol, refresh_document_navigation,
};
use serde_json::{Value, json};
use std::{collections::HashSet, sync::Arc, time::Instant};

pub(super) struct InitialState {
    pub(super) state: LoopState,
    pub(super) historical: Vec<Value>,
    pub(super) initial_history_length: usize,
}

pub(super) fn initialize_state(
    run: &RequestRun, input: &Value, provider: &str, model_name: &str,
    protocol_name: &str, connection: &Value, formal: &[Value],
    selected: Vec<Value>, tool_names: &[String],
) -> Result<InitialState, GatewayError> {
    let historical = formal
        .iter()
        .filter(|message| {
            message["Id"] != input["userMessageId"]
                && message["Id"] != run.request_id
                && message["RequestId"] != run.request_id
        })
        .cloned()
        .collect::<Vec<_>>();
    let projection = protocol::history_projection(protocol_name, &historical, tool_names)?;
    let history_sources = projection
        .sources
        .into_iter()
        .enumerate()
        .map(|(index, mut source)| {
            source["index"] = json!(index);
            source
        })
        .collect::<Vec<_>>();
    let summary_sources = projection.summary_sources;
    let mut history = projection.messages;
    history.push(json!({"role":"user","content":input["message"]}));
    let initial_history_length = history.len();
    let origin = json!({
        "providerId": provider,
        "model": model_name,
        "protocol": protocol_name,
        "connectionFingerprint": crate::retrieval::gateway_index::hash(
            &json!([
                provider,
                model_name,
                protocol_name,
                connection["baseUrl"],
                connection["apiKey"].as_str().unwrap_or("")
            ]).to_string()
        )
    });
    let state = LoopState {
        assistant: json!({
            "Id": run.request_id,
            "RequestId": run.request_id,
            "RequestFingerprint": run.fingerprint,
            "ReplyTo": input["userMessageId"],
            "Role": "assistant",
            "Status": "streaming",
            "Content": "",
            "Reasoning": "",
            "CreatedAt": now_iso8601(),
            "Provider": provider,
            "Model": model_name,
            "ToolActivities": []
        }),
        progress: Default::default(),
        task: Default::default(),
        origin,
        generated: 0,
        recoveries: 0,
        started: Instant::now(),
        applied_steering_ids: HashSet::new(),
        documents: DocumentWorkRecords::default(),
        work_progress: WorkProgress::default(),
        context: ContextState {
            history,
            history_sources,
            summary_sources,
            result_sources: vec![],
            runtime_snapshot: None,
            work_context_ready: false,
            work_context_reference: Value::Null,
            formal_sources: historical.iter().map(|message|json!({"id":message["Id"],"hash":crate::retrieval::gateway_index::hash(&message.to_string())})).collect(),
        },
        tools: ToolsState {
            selected,
            effects: Default::default(),
            observations: Default::default(),
        },
        completion: CompletionState::default(),
        delivery: DeliveryState {
            segments: vec![],
            pending_delivery: String::new(),
            delivery_continuing: false,
            last_progress_ms: 0,
            model_started: None,
            generation_duration_base_ms: None,
            timeline_order: 0,
        },
        visual: VisualState {
            visual_readback_delivered: false,
            pending_visual_readback: false,
            pending_visual_hashes: Vec::new(),
        },
        cache: CacheState {
            prefix_diagnostics: Default::default(),
            tool_series: Default::default(),
            previous_summary: Value::Null,
        },
        transcript: TranscriptState {
            rounds: vec![],
            pending_context_before: vec![],
        },
    };
    Ok(InitialState { state, historical, initial_history_length })
}

/// Preparation-only provenance never changes persisted checkpoint fields.
/// 仅准备阶段使用的来源参数不改变持久检查点字段。
pub(super) struct InputSources<'a> {
    pub(super) formal: &'a [Value],
    pub(super) historical: &'a [Value],
    pub(super) saved_attachments: &'a [Value],
    pub(super) initial_history_length: usize,
    pub(super) work_context_restored: bool,
    pub(super) resuming: bool,
    pub(super) base_system: &'a str,
    pub(super) dynamic_runtime: &'a str,
    pub(super) initial_runtime_authority: &'a str,
}

impl RootExecution {
    pub(super) async fn complete_inputs(
        &mut self, runtime: &Arc<GatewayRuntime>, run: &Arc<RequestRun>, input: &Value,
        sources: InputSources<'_>,
    ) -> Result<(), GatewayError> {
        let InputSources { formal, historical, saved_attachments, initial_history_length,
            work_context_restored, resuming, base_system, dynamic_runtime, initial_runtime_authority } = sources;
        let context_window = self.context_window;
        let supports_tools = self.supports_tools;
        let runtime_role = self.runtime_role;
        let Self { state, ctx, connection, model_name, protocol_name, tool_names, hints, system,
            preparation_stages, preparation_stage_started, .. } = self;
        let mut automatic_evidence_note: Option<(String, Value)> = None;
        let first_output = native(crate::models::output::calculate(
            "context.outputBudget",
            &json!({"contextWindowTokens":context_window,"phase":if supports_tools && !hints.is_empty() {"tool"} else {"answer"}}),
        ))?;
        let evidence_budget = first_output["inputBudgetTokens"]
            .as_u64()
            .unwrap_or(4096)
            .saturating_sub(
                crate::models::protocol::estimate_tokens(base_system)
                    + crate::models::protocol::estimate_tokens(input["message"].as_str().unwrap())
                    + 1024,
            )
            .min(context_window / 5);
        // Reuse restored evidence rather than retrieving it again on each internal retry.
        // 恢复后沿用原证据，不在每次内部重试时重复检索。
        let retrieval_needed = if evidence_budget > 0 && !work_context_restored {
            crate::retrieval::gateway::should_prepare_request(input["message"].as_str().unwrap())
        } else {
            Ok(false)
        };
        preparation_stages["automaticEvidenceSkipped"] = json!(matches!(&retrieval_needed, Ok(false)));
        let evidence_config = runtime.config.clone();
        let evidence_context = json!({"conversationId":run.conversation_id,"requestId":run.request_id,
            "bindingSnapshot":ctx.config["bindingSnapshot"],"workspaceMounts":ctx.config["workspaceMounts"]});
        let question = input["message"].as_str().unwrap().to_owned();
        let cancellation_key = run.cancellation_key.clone();
        let evidence_future = async move {
            match retrieval_needed {
                Ok(false) => Ok(Ok(json!({"items":[],"systemNotice":"","audit":{"skipped":"direct-response","actualTokens":0}}))),
                Err(error) => Ok(Err(error)),
                Ok(true) => tokio::task::spawn_blocking(move || {
                    let lease = crate::service_cancellation::CancellationLease::acquire(&cancellation_key)
                        .map_err(|_| crate::storage::ContractError::new("取消状态无法读取。", "INDEX_CANCELLED", 499))?;
                    let _scope = lease.enter();
                    crate::retrieval::gateway::prepare_request(&evidence_config, &evidence_context, &question, evidence_budget)
                }).await,
            }
        };
        let attachment_maximum = (!work_context_restored && !saved_attachments.is_empty())
            .then(|| (context_window / 3).clamp(1024, 131072) as usize);
        if let Some(maximum) = attachment_maximum {
            ctx.config["attachmentContextChars"] = json!(maximum);
        }
        let declarations = model_tool_declarations(&protocol_name, &state.tools.selected);
        let context_future = async {
            let projected = protocol::history_projection_verified(
                &connection, model_name, &historical, &tool_names, &runtime.config.data_root,
                &run.conversation_id, &system, &declarations,
            ).await?;
            let attachments = if let Some(maximum) = attachment_maximum {
                Some(crate::orchestration::gateway_attachments::prepare_context(&runtime, &run.conversation_id,
                    input["userMessageId"].as_str().unwrap(), maximum, true,
                    run.native_cancellation.token.clone()).await?)
            } else {
                None
            };
            Ok::<_, GatewayError>((projected, attachments))
        };
        // Prepare evidence and history/attachments concurrently, preserving final budgets and order.
        // 并行准备证据与历史/附件，保留最终请求的预算和顺序。
        let ((evidence, evidence_ms), (prepared_context, context_ms)) =
            prepare_in_parallel(evidence_future, context_future).await;
        let evidence_system_length = system.len();
        {
            match evidence{
                Ok(Ok(value))=>{
                    if let Some(reference)=value.get("resultRef").filter(|value|!value.is_null()){
                        state.assistant["RetrievalResultRef"]=reference.clone();
                        // Persist receipts before exposing their short evidence references to the model.
                        // 先保存回执，再向模型提供证据短引用。
                        state.save(&runtime,&run,"streaming",None).await?;
                    }
                    if let Some(notice)=value["systemNotice"].as_str().filter(|text|!text.is_empty()){
                        // Keep snippets in reclaimable working context; retain original archives for readback.
                        // 检索片段计入可回收工作上下文，原归档保留回读入口。
                        automatic_evidence_note=Some((notice.to_owned(),value["resultRef"].clone()));
                    }
                },
                Ok(Err(error))=>{system.push_str(&format!("\nBroker: automatic evidence preparation unavailable ({}); inspect tools and original authorized files. An incomplete index does not prove absence.",error.code()));},
                Err(_)=>system.push_str("\nBroker: automatic evidence worker unavailable; file/path/text search and direct reads remain independently available."),
            }
        }
        preparation_stages["automaticEvidenceMs"] = json!(evidence_ms);
        preparation_stages["historyAndAttachmentsMs"] = json!(context_ms);
        preparation_stages["preparationParallel"] = json!(true);
        preparation_stages["evidenceAndContextWallMs"] = json!(preparation_stage_started.elapsed().as_millis() as u64);
        *preparation_stage_started = Instant::now();
        let (mut projected, prepared_attachments) = prepared_context?;
        if system.len() != evidence_system_length {
            // Retrieval error notices change the prefix; recheck continuation against the final system.
            // 检索错误提示改变前缀后，按最终 system 重核续传。
            projected = protocol::history_projection_verified(&connection, model_name, &historical, &tool_names,
                &runtime.config.data_root, &run.conversation_id, &system, &declarations).await?;
        }
        if !work_context_restored {
            let preparation_notes = state.context.history.iter().enumerate().skip(initial_history_length)
                .map(|(index, message)| (message.clone(), state.context.history_sources.get(index).cloned().unwrap_or(Value::Null)))
                .collect::<Vec<_>>();
            state.context.history = projected.messages;
            state.context.history_sources = projected
                .sources
                .into_iter()
                .enumerate()
                .map(|(index, mut source)| {
                    source["index"] = json!(index);
                    source
                })
                .collect();
            state.context.history
                .push(json!({"role":"user","content":input["message"]}));
            if let Some(prepared) = prepared_attachments {
                *state.context.history.last_mut().unwrap() = crate::models::message_attachments::user_message(
                    input["message"].as_str().unwrap(), input["userMessageId"].as_str().unwrap(), &prepared);
            }
            if resuming {
                // Reuse the original request's public call/result pairs when no private snapshot survives.
                // 私有快照缺失时接回原请求的公开调用与结果配对，不重新派发。
                let mut original_turn = formal.iter().filter(|message| message["Id"] == input["userMessageId"])
                    .cloned().collect::<Vec<_>>();
                original_turn.extend(formal.iter().filter(|message| message["Role"] == "assistant"
                    && (message["Id"] == run.request_id || message["RequestId"] == run.request_id)).cloned());
                let resumed_projection = protocol::history_projection(&protocol_name, &original_turn, &tool_names)?;
                state.context.history_sources.resize(state.context.history.len(), Value::Null);
                for (message, mut source) in resumed_projection.messages.into_iter()
                    .zip(resumed_projection.sources).skip(1) {
                    source["index"] = json!(state.context.history.len());
                    state.context.history.push(message);
                    state.context.history_sources.push(source);
                }
                state.context.summary_sources.extend(resumed_projection.summary_sources);
            }
            // History reprojection must not discard newly prepared recovery or control notes.
            // 历史重投影不能丢弃本次准备的恢复或控制说明。
            for (message, mut source) in preparation_notes {
                state.context.history_sources.resize(state.context.history.len(), Value::Null);
                if source.is_object() { source["index"] = json!(state.context.history.len()); }
                state.context.history.push(message);
                state.context.history_sources.push(source);
            }
        }
        // Keep compact attachment links for follow-ups, not repeated full bodies.
        // 追问仅携带精简附件回读入口，不重复附入全文。
        let attachment_directory = formal.iter().rev().take(64).flat_map(|message| {
            message["Attachments"].as_array().into_iter().flatten().map(move |attachment|
                json!({"messageId":message["Id"],"attachmentId":attachment["Id"],"name":attachment["Name"]}))
        }).take(64).collect::<Vec<_>>();
        if !attachment_directory.is_empty() && !work_context_restored {
            // Persist attachment navigation at its original observation position.
            // 附件导航按原观测位置保存，避免恢复时重复或丢失。
            state.context.runtime_snapshot = crate::models::runtime_context::snapshot(runtime_role,
                &format!("{dynamic_runtime}\nSaved attachments in this conversation (read originals with attachment.read only when needed): {}", json!(attachment_directory)),
                initial_runtime_authority);
        }
        if let Some((notice, reference)) = automatic_evidence_note {
            state.context_note(&notice, json!({"projectionOrigin":"broker-summary","automaticEvidence":true,
                "resultRef":reference,"publicText":notice,"instructionAuthority":false,"semanticCorrectnessCertified":false}));
        }
        state.context.work_context_ready = true;
        state.refresh_runtime_snapshot(initial_runtime_authority, runtime_role, protocol::protocol(&connection) == "anthropic-messages");
        if resuming {
            state.broker_note("Continue from the saved checkpoint, original references and loaded capabilities. Read only information needed for the current goal. Reuse successful receipts; a missing receipt is not success or permission to repeat that operation, and does not block unrelated work.");
        }
        refresh_document_navigation(state);
        Ok(())
    }
}

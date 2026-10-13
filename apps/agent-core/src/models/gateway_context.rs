//! Request projections execute the existing native compaction plans without mutating formal history.
//! 请求投影执行现有原生压缩计划，不修改正式历史或重新派发已执行工具。

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};

use super::{
    gateway::{self, ModelError},
    gateway_protocol, projection, protocol, semantic,
    tool_context::ToolContextState,
};

static SUMMARY: OnceLock<Mutex<semantic::SummaryRuntimeState>> = OnceLock::new();
fn summary_call(operation: &str, args: &Value) -> Result<Value, ModelError> {
    SUMMARY
        .get_or_init(|| Mutex::new(semantic::SummaryRuntimeState::default()))
        .lock()
        .map_err(|_| ModelError::new("SUMMARY_RUNTIME_LOCK_FAILED", "摘要状态不可用。", 500))?
        .apply(&format!("models.lifecycle.summary.{operation}"), args)
        .map_err(ModelError::native)
}

async fn activity(
    events: &Option<mpsc::Sender<Value>>,
    id: &str,
    status: &str,
    metrics: &Value,
    cancellation: &mut watch::Receiver<bool>,
) -> Result<(), ModelError> {
    if let Some(events) = events {
        tokio::select! {_ = gateway::cancelled(cancellation)=>return Err(ModelError::cancelled()),sent=events.send(json!({"type":"tool_activity","tool":{"toolCallId":id,"name":"context.compact","arguments":{},"status":status,"summary":"上下文压缩","result":metrics.to_string(),"metrics":metrics,"executed":true}}))=>sent.map_err(|_|ModelError::cancelled())?};
    }
    Ok(())
}

fn source_wire(sources: &[Value]) -> Value {
    let fields: Vec<Value> = sources
        .iter()
        .map(|source| {
            json!(
                source["rounds"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|round| round["tools"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|tool| {
                            let mut fields = serde_json::Map::new();
                            for name in ["arguments", "resultRef", "executionEnvironment"] {
                                if let Some(value) = tool.get(name) {
                                    fields.insert(name.into(), json!(value.to_string()));
                                }
                            }
                            Value::Object(fields)
                        })
                        .collect::<Vec<_>>())
                    .collect::<Vec<_>>()
            )
        })
        .collect();
    json!({"sourceFacts":sources,"sourcesJson":sources.iter().map(Value::to_string).collect::<Vec<_>>(),"toolFieldsJson":fields})
}

async fn summarize(
    connection: &Value,
    model: &str,
    budget: &Value,
    input: u64,
    cancellation: watch::Receiver<bool>,
) -> Result<(Option<Value>, Value), ModelError> {
    let started = std::time::Instant::now();
    let sources = budget["summarySources"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if sources.is_empty() {
        return Ok((
            None,
            json!({"state":"fallback","reason":"no-complete-source-plan","modelCalls":0,"durationMs":started.elapsed().as_millis()}),
        ));
    }
    let costs: Vec<Value> = sources
        .iter()
        .map(|source| json!(protocol::estimate_tokens(&source.to_string())))
        .collect();
    let allocation=projection::calculate("models.context.projection.history",&json!({"turnCosts":costs,"inputPlan":{"inputBudgetTokens":input,"currentMessageTokens":budget["currentMessageTokens"].as_u64().unwrap_or(0)},"baseSystemTokens":budget["systemTokens"].as_u64().unwrap_or(0),"referenceNoticeTokens":protocol::estimate_tokens(projection::REFERENCE_NOTICE),"hasBaseSystem":true})).map_err(ModelError::native)?;
    let summary_budget = budget["summaryBudgetTokens"].as_u64().unwrap_or_else(|| {
        allocation["summaryBudgetTokens"]
            .as_f64()
            .unwrap_or(0.0)
            .floor() as u64
    });
    let wire = source_wire(&sources);
    let mut args = wire.clone();
    args["conversationId"] = budget["conversationId"].clone();
    args["coveredTurnCount"] = json!(sources.len());
    args["budgetTokens"] = json!(summary_budget);
    args["inputBudgetTokens"] = json!(input);
    args["maxOutputTokens"] = json!(summary_budget);
    args["trigger"] = json!("pressure");
    args["previous"] = budget["previousSummary"].clone();
    args["createdAtValid"] = json!(
        budget["previousSummary"]["createdAt"]
            .as_str()
            .is_some_and(|date| chrono::DateTime::parse_from_rfc3339(date).is_ok())
    );
    let planned = semantic::calculate("models.semantic.plan", &args).map_err(ModelError::native)?;
    let Some(plan) = planned.get("plan") else {
        let mut selection = args.clone();
        selection["summary"] = budget["previousSummary"].clone();
        selection["maximumCoveredTurnCount"] = json!(sources.len());
        let selected = semantic::calculate("models.semantic.select", &selection)
            .map_err(ModelError::native)?;
        if let Some(summary) = selected.get("value") {
            return Ok((
                Some(summary.clone()),
                json!({"state":"cached","reason":planned["reason"],"modelCalls":0,"sourceHash":summary["sourceHash"],"durationMs":started.elapsed().as_millis()}),
            ));
        }
        return Ok((
            None,
            json!({"state":"fallback","reason":planned["reason"],"modelCalls":0,"durationMs":started.elapsed().as_millis()}),
        ));
    };
    let opened = summary_call(
        "open",
        &json!({"plan":plan,"now":chrono::Utc::now().timestamp_millis(),"startedAt":0}),
    )?;
    if opened["deferred"] == true {
        let mut audit = opened["audit"].clone();
        audit["durationMs"] = json!(started.elapsed().as_millis());
        return Ok((None, audit));
    }
    let run = opened["runId"].clone();
    let outcome=async {
        let request=semantic::calculate("models.semantic.request",&json!({"plan":plan})).map_err(ModelError::native)?;
        let messages=request["messages"].as_array().cloned().unwrap_or_default();
        let body=gateway_protocol::build_request(connection,model,&messages,request["system"].as_str().unwrap_or(""),request["maxOutputTokens"].as_u64(),&[],false)?;
        let outcome=gateway::request(connection,body,cancellation.clone(),None).await;
        if outcome.as_ref().map_or_else(|error| error.details["requestDispatched"]==true, |_| true) {
            summary_call("dispatched",&json!({"runId":run}))?;
        }
        let response=outcome?;
        let observations:Vec<Value>=["input_tokens","output_tokens","prompt_tokens","completion_tokens","total_tokens"].iter().map(|field|json!({"name":field,"value":response.usage[*field]})).collect();
        summary_call("usage",&json!({"runId":run,"observations":observations}))?;
        let mut receipt=summary_call("response",&json!({"runId":run,"protocol":connection["protocol"],"finishReason":response.finish,"responseStatus":response.raw["status"],"contentTypes":response.raw["content"].as_array().map(|blocks|blocks.iter().map(|block|block["type"].clone()).collect::<Vec<_>>()),"outputTypes":response.raw["output"].as_array().map(|blocks|blocks.iter().map(|block|block["type"].clone()).collect::<Vec<_>>()),"completionCallCount":response.calls.len()}))?;
        receipt["content"]=json!(response.content);receipt["truncated"]=json!(response.output_truncated);
        let mut finalize=wire.clone();finalize["plan"]=plan.clone();finalize["response"]=receipt;finalize["createdAt"]=json!(chrono::Utc::now().to_rfc3339());finalize["createdAtValid"]=json!(true);
        let finalized=semantic::calculate("models.semantic.finalize",&finalize).map_err(ModelError::native)?;
        let candidate=summary_call("candidate",&json!({"runId":run,"hasValue":finalized["value"].is_object(),"reason":finalized["reason"],"sourceHash":finalized["value"]["sourceHash"],"contentHash":finalized["value"]["contentHash"],"contentTokens":finalized["contentTokens"]}))?;
        if candidate["rejected"]==true {let mut error=ModelError::new("SUMMARY_REJECTED","摘要候选校验未通过。",502);error.details=candidate;return Err(error);}
        let candidate=finalized["value"].clone();
        if let Some(path)=budget["dataRoot"].as_str(){
            let root=PathBuf::from(path);let conversation=budget["conversationId"].as_str().unwrap_or("").to_owned();let protocol=gateway_protocol::protocol(connection).to_owned();let names:Vec<String>=budget["availableTools"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect();let saved_candidate=candidate.clone();
            let receipt=tokio::task::spawn_blocking(move||crate::storage::gateway_memory::save_model_summary(&root,&conversation,&protocol,&names,&saved_candidate)).await.map_err(|_|ModelError::new("SUMMARY_SAVE_FAILED","摘要保存任务未完成。",500))?.map_err(|e|ModelError::new(e.code(),e.message(),e.status_code()))?;
            summary_call("saved",&json!({"runId":run,"sourceHash":receipt["sourceHash"],"contentHash":receipt["contentHash"]}))?;
        }
        Ok::<Value,ModelError>(candidate)
    }.await;
    let candidate = match outcome {
        Ok(candidate) => Some(candidate),
        Err(error) => {
            summary_call(
                "failed",
                &json!({"runId":run,"aborted":error.code=="ABORT_ERR"||*cancellation.borrow(),"code":error.code,"reason":error.details["reason"]}),
            )?;
            if error.code == "ABORT_ERR" || *cancellation.borrow() {
                let _ = summary_call(
                    "finish",
                    &json!({"runId":run,"finishedAt":started.elapsed().as_millis()}),
                );
                return Err(ModelError::cancelled());
            }
            None
        }
    };
    let mut audit = summary_call(
        "finish",
        &json!({"runId":run,"finishedAt":started.elapsed().as_millis()}),
    )?;
    if candidate.is_some() && audit["state"] != "saved" {
        audit["state"] = json!("validated-request-only");
    }
    Ok((candidate, audit))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactRequest {
    pub messages: Vec<Value>,
    pub system: String,
    pub audit: Value,
    pub budget_exceeded: bool,
    pub history_sources: Vec<Value>,
    pub summary_update: Option<Value>,
}

fn call(state: &mut ToolContextState, operation: &str, args: &Value) -> Result<Value, ModelError> {
    state
        .apply(&format!("models.lifecycle.toolContext.{operation}"), args)
        .map_err(ModelError::native)
}
fn descriptor(
    state: &mut ToolContextState,
    message: &Value,
    key: Option<Value>,
    previous: Option<&Value>,
) -> Result<Value, ModelError> {
    let key = match key {
        Some(key) => key,
        None => call(state, "object", &json!({}))?,
    };
    let field = if message["output"].is_string() {
        Some("output")
    } else if message["content"].is_string() {
        Some("content")
    } else {
        None
    };
    let text = field
        .and_then(|field| message[field].as_str())
        .unwrap_or("");
    let mut item = json!({"key":key,"role":message["role"],"type":message["type"],"id":message["id"],"call_id":message["call_id"],"tool_call_id":message["tool_call_id"],"tool_use_id":message["tool_use_id"],
        "field":field,"contentString":message["content"].is_string(),"characters":text.encode_utf16().count(),"excerpt":protocol::output_excerpt(text,4096),"textTokens":protocol::estimate_tokens(text),
        "contentTokens":protocol::estimate_tokens(message["content"].as_str().unwrap_or("")),"cost":protocol::tool_message_tokens(std::slice::from_ref(message),"")});
    if let Some(calls) = message["tool_calls"].as_array() {
        item["tool_calls"] = json!(
            calls
                .iter()
                .map(|call| json!({"id":call["id"]}))
                .collect::<Vec<_>>()
        );
    }
    if let Some(blocks) = message["content"].as_array() {
        let mut content = Vec::new();
        for (index, block) in blocks.iter().enumerate() {
            let old = previous.and_then(|item| item["content"].get(index));
            content.push(descriptor(
                state,
                block,
                old.map(|item| item["key"].clone()),
                old,
            )?);
        }
        item["content"] = json!(content);
    }
    Ok(item)
}
fn descriptors(items: &[(Value, Value)]) -> Vec<Value> {
    items.iter().map(|(_, item)| item.clone()).collect()
}
fn cost(items: &[(Value, Value)], system: &str, schema_tokens: u64) -> u64 {
    items
        .iter()
        .map(|(_, item)| item["cost"].as_u64().unwrap_or(0))
        .sum::<u64>()
        + if system.is_empty() {
            0
        } else {
            8 + protocol::estimate_tokens(system)
        }
        + schema_tokens
}

fn apply_actions(
    state: &mut ToolContextState,
    items: &mut Vec<(Value, Value)>,
    actions: &Value,
) -> Result<(), ModelError> {
    let mut originals = std::collections::HashMap::new();
    for action in actions.as_array().into_iter().flatten() {
        for recipe in action["replacements"].as_array().into_iter().flatten() {
            if recipe["modelHistory"] != true {
                let index = recipe["originalIndex"].as_u64().unwrap() as usize;
                let mut shell = serde_json::Map::new();
                for (key, value) in items[index].0.as_object().into_iter().flatten() {
                    if key != "content" {
                        shell.insert(key.clone(), value.clone());
                    }
                }
                originals.insert(index, Value::Object(shell));
            }
        }
    }
    for action in actions.as_array().into_iter().flatten() {
        let index = action["index"].as_u64().unwrap_or(0) as usize;
        if action["type"] == "result" {
            let (message, metadata) = &mut items[index];
            let previous = metadata.clone();
            let field = action["field"].as_str().unwrap_or("content");
            if let Some(block) = action["blockIndex"].as_u64() {
                message["content"][block as usize][field] = action["text"].clone();
                metadata["content"][block as usize]["key"] = action["key"].clone();
                *metadata = descriptor(
                    state,
                    message,
                    Some(previous["key"].clone()),
                    Some(metadata),
                )?;
            } else {
                message[field] = action["text"].clone();
                *metadata =
                    descriptor(state, message, Some(action["key"].clone()), Some(&previous))?;
            }
        } else {
            let mut replacement = Vec::new();
            for recipe in action["replacements"].as_array().into_iter().flatten() {
                let original = recipe["originalIndex"].as_u64().unwrap() as usize;
                let mut message = if recipe["modelHistory"] == true {
                    json!({"role":recipe["role"]})
                } else {
                    originals[&original].clone()
                };
                message["content"] = recipe["text"].clone();
                let metadata = descriptor(state, &message, Some(recipe["key"].clone()), None)?;
                replacement.push((message, metadata));
            }
            let count = action["count"].as_u64().unwrap() as usize;
            items.splice(index..index + count, replacement);
        }
    }
    Ok(())
}

fn opaque(message: &Value) -> bool {
    !message["reasoning_content"]
        .as_str()
        .unwrap_or("")
        .is_empty()
        || message["type"] == "reasoning"
        || message["content"].as_array().is_some_and(|blocks| {
            blocks.iter().any(|block| {
                matches!(
                    block["type"].as_str(),
                    Some("thinking" | "redacted_thinking")
                )
            })
        })
}

/// Budget metadata is supplied by the live run: original history sources and durable result receipts.
/// 授额元信息来自当前运行：原历史来源及持久工具回执；压缩不能依据正文猜测执行状态。
pub async fn compact_request(
    connection: &Value,
    model: &str,
    messages: &[Value],
    system: &str,
    budget: &Value,
    mut cancellation: watch::Receiver<bool>,
    events: Option<mpsc::Sender<Value>>,
) -> Result<CompactRequest, ModelError> {
    if *cancellation.borrow() {
        return Err(ModelError::cancelled());
    }
    let name = gateway_protocol::protocol(connection);
    let input = budget["inputBudgetTokens"]
        .as_u64()
        .ok_or_else(|| ModelError::new("INVALID_CONTEXT_WINDOW", "上下文输入授额无效。", 400))?;
    let schema_tokens = protocol::estimate_tokens(&budget["declarations"].to_string());
    let mut state = ToolContextState::default();
    let mut items = Vec::with_capacity(messages.len());
    for message in messages {
        let item = descriptor(&mut state, message, None, None)?;
        items.push((message.clone(), item));
    }
    let mut sources = budget["historySources"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let before = cost(&items, system, schema_tokens);
    let pressure = before as f64 > input as f64 * 0.9;
    let activity_id = format!("context-compact-{}", uuid::Uuid::new_v4());
    let started = std::time::Instant::now();
    let mut summary_update = None;
    let mut summary_audit = Value::Null;
    if pressure {
        activity(
            &events,
            &activity_id,
            "running",
            &json!({"beforeTokens":before,"inputBudgetTokens":input}),
            &mut cancellation,
        )
        .await?;
        let mut actual_budget = budget.clone();
        actual_budget["currentMessageTokens"] = json!(
            items
                .iter()
                .enumerate()
                .filter(|(index, _)| sources
                    .get(*index)
                    .is_none_or(|source| source["turnIndex"].is_null()))
                .map(|(_, (_, item))| item["cost"].as_u64().unwrap_or(0))
                .sum::<u64>()
        );
        actual_budget["systemTokens"] = json!(if system.is_empty() {
            0
        } else {
            8 + protocol::estimate_tokens(system)
        });
        let (candidate, audit) = summarize(
            connection,
            model,
            &actual_budget,
            input,
            cancellation.clone(),
        )
        .await?;
        summary_update = candidate;
        summary_audit = audit;
        if let Some(candidate) = &summary_update {
            let covered = candidate["coveredTurnCount"].as_u64().unwrap_or(0);
            let mut retained = Vec::new();
            let mut remaining = Vec::new();
            for (index, item) in items.into_iter().enumerate() {
                let source = sources.get(index).cloned().unwrap_or(Value::Null);
                if source["turnIndex"]
                    .as_u64()
                    .is_some_and(|turn| turn < covered)
                {
                    continue;
                }
                retained.push(item);
                remaining.push(source);
            }
            let note = json!({"role":"assistant","content":format!("{}\n{}",projection::REFERENCE_NOTICE,candidate["content"].as_str().unwrap_or(""))});
            let note_descriptor = descriptor(&mut state, &note, None, None)?;
            retained.insert(0, (note, note_descriptor));
            remaining.insert(0, Value::Null);
            items = retained;
            sources = remaining;
        }
    }
    for source in &mut sources {
        source["messageIdValid"] = json!(crate::storage::identity(&source["messageId"]).is_ok());
    }
    call(
        &mut state,
        "init",
        &json!({"protocol":name,"conversationId":budget["conversationId"],"conversationIdValid":crate::storage::identity(&budget["conversationId"]).is_ok(),"items":descriptors(&items),"sources":sources}),
    )?;
    for source in budget["resultSources"].as_array().into_iter().flatten() {
        // Match receipts to actual protocol identities, so earlier compaction cannot shift their indexed ownership.
        // 根据实际协议身份匹配回执，避免上一轮压缩后的数组偏移改变所属工具。
        for (message, item) in &items {
            let id = source["callId"].as_str().unwrap_or("");
            let location = if message["tool_call_id"] == id || message["call_id"] == id {
                Some(item)
            } else {
                message["content"]
                    .as_array()
                    .and_then(|blocks| blocks.iter().position(|block| block["tool_use_id"] == id))
                    .and_then(|index| item["content"].get(index))
            };
            let Some(location) = location else { continue };
            let field = location["field"].as_str().unwrap_or("content");
            let text = if message["content"].is_array() {
                message["content"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|block| block["tool_use_id"] == id)
                    .map(|block| block[field].as_str().unwrap_or(""))
                    .unwrap_or("")
            } else {
                message[field].as_str().unwrap_or("")
            };
            let mut receipt = source.clone();
            receipt["field"] = json!(field);
            receipt["originalCharacters"] = json!(text.encode_utf16().count());
            receipt["originalExcerpt"] = json!(protocol::output_excerpt(text, 4096));
            call(
                &mut state,
                "observe",
                &json!({"key":location["key"],"round":source["round"],"result":{"resultRef":source["resultRef"]},"source":receipt}),
            )?;
        }
    }
    let actions = call(
        &mut state,
        "observations",
        &json!({"items":descriptors(&items)}),
    )?;
    apply_actions(&mut state, &mut items, &actions)?;
    let actions = call(
        &mut state,
        "compaction",
        &json!({"items":descriptors(&items),"tokens":cost(&items,system,schema_tokens),"inputBudgetTokens":input}),
    )?;
    apply_actions(&mut state, &mut items, &actions)?;
    let after = cost(&items, system, schema_tokens);
    let mut audit = match call(
        &mut state,
        "finalize",
        &json!({"tokens":after,"beforeTokens":before,"inputBudgetTokens":input,"schemaTokens":schema_tokens,"changed":after!=before}),
    ) {
        Ok(value) => value,
        Err(error) if error.code == "TOOL_CONTEXT_BUDGET_EXCEEDED" => {
            json!({"beforeTokens":before,"estimatedInputTokens":after,"inputBudgetTokens":input,"budgetExceeded":true})
        }
        Err(error) => return Err(error),
    };
    if audit.is_null() {
        audit = json!({});
    }
    let images: u64 = items
        .iter()
        .map(|(message, _)| protocol::image_inputs(message))
        .sum();
    if images > 0 {
        audit["imageInputs"] = json!(images);
        audit["mediaTokenCoverage"] = json!("unobserved-provider-cost");
    }
    if !summary_audit.is_null() {
        audit["semanticSummary"] = summary_audit.clone();
    }
    if after > input {
        let plan = call(
            &mut state,
            "checkpointPlan",
            &json!({"items":descriptors(&items)}),
        )?;
        if plan.is_object() && budget["dataRoot"].is_string() && budget["requestId"].is_string() {
            let mut private_reference = Value::Null;
            if items.iter().any(|(message, _)| opaque(message)) {
                use sha2::{Digest, Sha256};
                let root = PathBuf::from(budget["dataRoot"].as_str().unwrap());
                let conversation = budget["conversationId"].as_str().unwrap_or("").to_owned();
                let request = budget["requestId"].as_str().unwrap().to_owned();
                let continuation: Vec<Value> =
                    items.iter().map(|(message, _)| message.clone()).collect();
                let origin = json!({"providerId":connection["providerId"],"model":model,"protocol":connection["protocol"],"connectionFingerprint":format!("{:x}",Sha256::digest(json!([connection["providerId"],model,connection["protocol"],connection["baseUrl"],connection["apiKey"].as_str().unwrap_or("")]).to_string().as_bytes()))});
                let prefix = format!(
                    "{:x}",
                    Sha256::digest(
                        json!([system, budget["declarations"], continuation])
                            .to_string()
                            .as_bytes()
                    )
                );
                let canonical = json!({"_meta":{"modelContinuation":{"version":1,"origin":origin,"prefixFingerprint":prefix,"round":budget["currentRound"].as_u64().unwrap_or(1),"continuation":continuation}}});
                let result=tokio::task::spawn_blocking(move||crate::storage::gateway::save_tool_result(&root,&conversation,&request,&json!({"id":format!("native-checkpoint-{}",uuid::Uuid::new_v4()),"name":"model.continuation"}),&canonical,None)).await.map_err(|_|ModelError::new("CONTEXT_CHECKPOINT_FAILED","私有续传归档任务未完成。",500))?;
                match result {
                    Ok(reference) => private_reference = reference,
                    Err(_) => {
                        audit["privateCheckpointUnavailable"] = json!(true);
                        activity(&events, &activity_id, "error", &audit, &mut cancellation).await?;
                        return Ok(CompactRequest {
                            history_sources: state.history_sources(&descriptors(&items)),
                            summary_update,
                            messages: items.into_iter().map(|(message, _)| message).collect(),
                            system: system.into(),
                            audit,
                            budget_exceeded: true,
                        });
                    }
                }
            }
            let mut public = Vec::new();
            for recipe in plan["public"].as_array().into_iter().flatten() {
                let original = &items[recipe["index"].as_u64().unwrap() as usize].0;
                let mut value = serde_json::Map::new();
                for field in recipe["fields"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                {
                    if let Some(field_value) = original.get(field) {
                        value.insert(field.into(), field_value.clone());
                    }
                }
                let mut value = Value::Object(value);
                if let Some(blocks) = recipe["blocks"].as_array() {
                    value["content"] = json!(
                        blocks
                            .iter()
                            .map(
                                |index| original["content"][index.as_u64().unwrap() as usize]
                                    .clone()
                            )
                            .collect::<Vec<_>>()
                    );
                }
                public.push(value);
            }
            let root = PathBuf::from(budget["dataRoot"].as_str().unwrap());
            let conversation = budget["conversationId"].as_str().unwrap_or("").to_owned();
            let request = budget["requestId"].as_str().unwrap().to_owned();
            let canonical = json!({"content":json!(public).to_string()});
            let reference = tokio::select! {_ = gateway::cancelled(&mut cancellation)=>return Err(ModelError::cancelled()),result=tokio::task::spawn_blocking(move||crate::storage::gateway::save_tool_result(&root,&conversation,&request,&json!({"id":format!("context-checkpoint-{}",uuid::Uuid::new_v4()),"name":"context.public-checkpoint"}),&canonical,None))=>result.map_err(|_|ModelError::new("CONTEXT_CHECKPOINT_FAILED","上下文检查点保存任务未完成。",500))?.map_err(|e|ModelError::new(e.code(),e.message(),e.status_code()))};
            if let Ok(reference) = reference {
                let latest = plan["latestTextIndex"]
                    .as_u64()
                    .map(|index| items[index as usize].0["content"].clone())
                    .unwrap_or(json!(""));
                let checkpoint = call(
                    &mut state,
                    "checkpointFinalize",
                    &json!({"plan":plan,"resultRef":reference,"latestPublicText":latest}),
                )?;
                let mut retained: Vec<Value> = plan["retained"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|index| items[index.as_u64().unwrap() as usize].0.clone())
                    .collect();
                retained.push(checkpoint["message"].clone());
                let reduced = protocol::tool_message_tokens(&retained, system) + schema_tokens;
                if reduced < after {
                    audit = checkpoint["metrics"].clone();
                    audit["estimatedInputTokens"] = json!(reduced);
                    audit["inputBudgetTokens"] = json!(input);
                    audit["budgetExceeded"] = json!(reduced > input);
                    if !private_reference.is_null() {
                        audit["privateContinuationRef"] = private_reference;
                    }
                    if !summary_audit.is_null() {
                        audit["semanticSummary"] = summary_audit;
                    }
                    audit["durationMs"] = json!(started.elapsed().as_millis());
                    activity(
                        &events,
                        &activity_id,
                        if reduced > input {
                            "error"
                        } else {
                            "completed"
                        },
                        &audit,
                        &mut cancellation,
                    )
                    .await?;
                    return Ok(CompactRequest {
                        history_sources: vec![Value::Null; retained.len()],
                        summary_update,
                        messages: retained,
                        system: system.into(),
                        audit,
                        budget_exceeded: reduced > input,
                    });
                }
            } else {
                audit["checkpointUnavailable"] = json!(true);
            }
        }
    }
    if *cancellation.borrow() {
        return Err(ModelError::cancelled());
    }
    audit["durationMs"] = json!(started.elapsed().as_millis());
    if pressure {
        activity(
            &events,
            &activity_id,
            if after > input { "error" } else { "completed" },
            &audit,
            &mut cancellation,
        )
        .await?;
    }
    Ok(CompactRequest {
        history_sources: state.history_sources(&descriptors(&items)),
        summary_update,
        messages: items.into_iter().map(|(message, _)| message).collect(),
        system: system.into(),
        audit,
        budget_exceeded: after > input,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn archived_tool_excerpts_keep_pairs_and_never_authorize_replay() {
        let content = "temporary observation ".repeat(2048);
        let messages = [
            json!({"role":"user","content":"read temporary fixture"}),
            json!({"role":"assistant","content":"reading","tool_calls":[{"id":"call-1","type":"function","function":{"name":"filesystem_read","arguments":"{}"}}]}),
            json!({"role":"tool","tool_call_id":"call-1","content":content}),
        ];
        let budget = json!({"inputBudgetTokens":2048,"declarations":[],"conversationId":"11111111-1111-4111-8111-111111111111","resultSources":[{"callId":"call-1","name":"filesystem.read","round":1,"status":"completed","resultRef":{"id":"22222222-2222-4222-8222-222222222222","bytes":4096,"sha256":"a".repeat(64)}}]});
        let (_owner, cancel) = watch::channel(false);
        let projection = compact_request(
            &json!({"protocol":"openai-completions"}),
            "fixture",
            &messages,
            "",
            &budget,
            cancel,
            None,
        )
        .await
        .unwrap();
        assert!(protocol::complete_pairs(&projection.messages));
        assert!(!projection.budget_exceeded);
        let excerpt: Value =
            serde_json::from_str(projection.messages[2]["content"].as_str().unwrap()).unwrap();
        assert_eq!(excerpt["contextCompacted"], true);
        assert_eq!(excerpt["status"], "completed");
        assert_eq!(
            excerpt["resultRef"],
            budget["resultSources"][0]["resultRef"]
        );
        assert_eq!(excerpt["navigation"]["tool"], "tool.result.read");
        assert!(excerpt["notice"].as_str().unwrap().contains("untrusted"));
        assert!(
            excerpt["notice"]
                .as_str()
                .unwrap()
                .contains("does not grant permissions")
        );
        assert_eq!(messages[2]["content"], content);
    }
    #[tokio::test]
    async fn irreducible_input_is_explicit_and_opaque_blocks_remain_unchanged() {
        let messages = [
            json!({"role":"user","content":"large current request ".repeat(4096)}),
            json!({"role":"assistant","content":[{"type":"thinking","thinking":"private","signature":"exact-signature"},{"type":"text","text":"still working"}]}),
        ];
        let (_owner, cancel) = watch::channel(false);
        let projection = compact_request(
            &json!({"protocol":"anthropic-messages"}),
            "fixture",
            &messages,
            "",
            &json!({"inputBudgetTokens":1024,"declarations":[]}),
            cancel,
            None,
        )
        .await
        .unwrap();
        assert!(projection.budget_exceeded);
        assert_eq!(projection.messages, messages);
        assert_eq!(projection.audit["budgetExceeded"], true);
    }
}

//! Derived procedures reference the existing accepted task/receipt journal, never a second execution ledger.
//! 衍生步骤引用既有已验收任务及回执，不建立第二套执行账本，也不自动执行历史参数。

use super::*;
use std::collections::{HashMap, HashSet};

const MATURITY_MS: i64 = 24 * 60 * 60 * 1000;
const MINIMUM_ACCEPTED_TASKS: usize = 3;

impl TaskRepository {
    /// Scope comes from the formal tool context; model input cannot choose another project or user scope.
    /// 范围由正式工具上下文给出；模型参数不能选择其他项目或自行提升至用户全局。
    pub(crate) fn verified_procedures(&self, conversation: &str, project: Option<&str>, query: &str,
        current_identities: &Value, now_ms: i64) -> Result<Value> {
        identifier(conversation)?;
        if let Some(project) = project { identifier(project)?; }
        let connection = sqlite_connections::acquire(&self.root, &self.path, 30_000)?;
        let (predicate, scope) = if let Some(project) = project {
            ("t.project_id=?1", project)
        } else { ("t.project_id IS NULL AND t.conversation_id=?1", conversation) };
        let sql = format!("SELECT t.task_id,e.document FROM tasks t JOIN task_events e ON e.task_id=t.task_id WHERE {predicate} AND t.state='completed' AND json_extract(e.document,'$.kind')='experience.candidate' ORDER BY json_extract(t.document,'$.updatedAtMs') DESC,t.task_id DESC,e.sequence DESC LIMIT 2000");
        let mut statement = connection.prepare_cached(&sql).map_err(|_| failure())?;
        let rows = statement.query_map([scope], |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?)))
            .map_err(|_| failure())?;
        let mut candidates = Vec::new();
        // Adjacent rows of one task share a single receipt read; retain at most one task's operation list.
        // 同一任务相邻候选共用一次回执读取；只保留一项任务的操作列表，避免重复I/O和整项目操作驻留。
        let mut receipt_task = String::new();
        let mut operations = Vec::new();
        for row in rows {
            let (task_id, document) = row.map_err(|_| failure())?;
            let event: TaskEvent = decode(document)?;
            // A stored broker candidate is usable only while its original successful operation receipts exist.
            // 程序保存的候选仅在原成功操作回执仍存在时可复用，不凭模型自报或派生统计认证。
            if receipt_task != task_id {
                operations = read_operations(&connection, &task_id)?;
                receipt_task = task_id.clone();
            }
            let supported = event.payload["steps"].as_array().is_some_and(|steps| !steps.is_empty() && steps.iter().all(|step|
                operations.iter().any(|operation| operation.state == OperationState::Succeeded
                    && operation.intent.tool_call_id == step["callId"].as_str().unwrap_or("")
                    && operation.intent.tool_name == step["tool"].as_str().unwrap_or("")
                    && original_receipt_matches(operation.result_reference.as_ref(), &step["receipt"]))));
            if supported {
                candidates.push(json!({"taskId":task_id,"createdAtMs":event.created_at_ms,"candidate":event.payload}));
            }
        }
        Ok(select_procedures(&candidates, query, current_identities, now_ms))
    }
}

fn original_receipt_matches(original: Option<&Value>, candidate: &Value) -> bool {
    let Some(original) = original else { return false; };
    let Some(id) = candidate["id"].as_str().filter(|id| !id.is_empty() && id.len() <= 512) else { return false; };
    let Some(hash) = candidate["sha256"].as_str().filter(|hash|
        hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())) else { return false; };
    let Some(bytes) = candidate["bytes"].as_u64() else { return false; };
    original["id"].as_str() == Some(id) && original["sha256"].as_str() == Some(hash)
        && original["bytes"].as_u64() == Some(bytes)
}

fn select_procedures(candidates: &[Value], query: &str, identities: &Value, now_ms: i64) -> Value {
    let mut groups: HashMap<String, Vec<&Value>> = HashMap::new();
    let mut invalid_conditions = 0;
    for candidate in candidates {
        let record = &candidate["candidate"];
        if record["schemaVersion"] != 1 || record["identityPolicy"] != "per-tool-contract-v2"
            || record["acceptanceReviewed"] != true || record["completeSequence"] != true { continue; }
        let fresh = record["steps"].as_array().is_some_and(|steps| !steps.is_empty() && steps.iter().all(|step|
            step["identity"].as_str().is_some_and(|identity| identity.len() == 64
                && identities[step["tool"].as_str().unwrap_or("")].as_str() == Some(identity))));
        if !fresh { invalid_conditions += 1; continue; }
        if candidate["createdAtMs"].as_i64().is_none_or(|created| created > now_ms) { continue; }
        if let Some(signature) = record["signature"].as_str() { groups.entry(signature.to_owned()).or_default().push(candidate); }
    }
    let terms = query.split(|character: char| !character.is_alphanumeric()).filter(|term| !term.is_empty())
        .map(str::to_lowercase).collect::<Vec<_>>();
    let mut collecting = 0;
    let mut entries = Vec::new();
    for (signature, records) in groups {
        let tasks = records.iter().filter_map(|record| record["taskId"].as_str()).collect::<HashSet<_>>();
        let first = records.iter().filter_map(|record| record["createdAtMs"].as_i64()).min().unwrap_or(now_ms);
        let last = records.iter().filter_map(|record| record["createdAtMs"].as_i64()).max().unwrap_or(now_ms);
        if now_ms.saturating_sub(last) > 30 * 24 * 60 * 60 * 1000 { continue; }
        if tasks.len() < MINIMUM_ACCEPTED_TASKS || last.saturating_sub(first) < MATURITY_MS { collecting += 1; continue; }
        let latest = records.iter().max_by_key(|record| record["createdAtMs"].as_i64()).unwrap();
        let candidate = &latest["candidate"];
        let trigger = candidate["trigger"].as_str().unwrap_or("");
        let lower = trigger.to_lowercase();
        let score = terms.iter().filter(|term| lower.contains(term.as_str())).count();
        if !terms.is_empty() && score == 0 { continue; }
        let steps = candidate["steps"].as_array().unwrap().iter().map(|step|
            json!({"tool":step["tool"],"argumentsTemplate":step["argumentsTemplate"]})).collect::<Vec<_>>();
        entries.push((score,last,json!({"id":signature,"kind":"procedure","state":"active","trigger":trigger,
            "steps":steps,"sourceTaskIds":tasks.into_iter().take(8).collect::<Vec<_>>(),
            "acceptedTaskCount":records.iter().filter_map(|record|record["taskId"].as_str()).collect::<HashSet<_>>().len(),
            "firstObservedAtMs":first,"lastValidatedAtMs":last,"navigationOnly":true,
            "preconditions":"Re-derive current arguments, paths and application state; current permissions and normal tool approval still apply."})));
    }
    entries.sort_by(|a,b|b.0.cmp(&a.0).then_with(||b.1.cmp(&a.1)).then_with(||a.2["id"].as_str().cmp(&b.2["id"].as_str())));
    json!({"entries":entries.into_iter().take(20).map(|entry|entry.2).collect::<Vec<_>>(),
        "collecting":collecting,"invalidatedConditions":invalid_conditions,"scope":"current-work-or-chat",
        "policy":{"minimumAcceptedTasks":MINIMUM_ACCEPTED_TASKS,"minimumObservationSpanMs":MATURITY_MS,
            "globalPromotion":"requires existing explicit memory confirmation; never automatic"},
        "freshnessRequired":true,"navigationOnly":true})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verified_procedure_repository_requires_matching_receipts_and_current_work_scope() {
        let root=std::env::temp_dir().join(format!("kynxa-procedure-proof-{}",uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let repository=TaskRepository::open(&root).unwrap();
        let project="01c71587-7759-4b60-8938-225fed06d34e";
        let chat="b9d20773-6e3f-4a69-a2da-83e07036c0d0";
        let identity="a".repeat(64);
        let base=now_ms();
        let run=std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            for index in 0..4 {
                let task=repository.accept_task(&NewTask {
                    request_id:format!("proof-task-{index}"),goal:"render fixture".into(),acceptance:vec!["render receipt".into()],
                    scope:TaskScope {project_id:Some(if index==3{"08d261fe-c238-4318-8d30-bcaaef468f89"}else{project}.into()),
                        conversation_id:chat.into(),permission_snapshot:json!({"mode":"work"})},
                    parent_task_id:None,dependency_ids:vec![],budget:TaskBudget::default(),input_reference:None,
                }).unwrap();
                let attempt=repository.start_attempt(&task.task_id,task.version,"fixture",60_000).unwrap();
                let call_id=format!("call-{index}");let operation_id=format!("operation-{index}");
                let receipt=json!({"id":format!("receipt-{index}"),"sha256":"b".repeat(64),"bytes":7});
                repository.record_operation(&attempt.token,&OperationIntent {operation_id:operation_id.clone(),
                    tool_call_id:call_id.clone(),tool_name:"render".into(),input_sha256:"c".repeat(64),
                    input_reference:None,effect:OperationEffect::Observation}).unwrap();
                repository.dispatch_operation(&attempt.token,&operation_id).unwrap();
                repository.finish_operation(&attempt.token,&operation_id,OperationState::Succeeded,Some(receipt.clone()),None).unwrap();
                let mut candidate_receipt=receipt;
                if index==2 {candidate_receipt["sha256"]=json!("d".repeat(64));}
                let candidate=json!({"schemaVersion":1,"identityPolicy":"per-tool-contract-v2","acceptanceReviewed":true,
                    "completeSequence":true,"signature":"fixture-procedure","trigger":"render fixture",
                    "steps":[{"tool":"render","identity":identity,"callId":call_id,"receipt":candidate_receipt,"argumentsTemplate":{}}]});
                let event_id=format!("candidate-{index}");
                let mut event=repository.append_event(&attempt.token,&event_id,"experience.candidate",&candidate).unwrap();
                event.created_at_ms=base+if index==0{0}else{MATURITY_MS};
                let connection=rusqlite::Connection::open(root.join("Agent/tasks.sqlite")).unwrap();
                connection.execute("UPDATE task_events SET document=?1 WHERE task_id=?2 AND event_id=?3",
                    rusqlite::params![serde_json::to_string(&event).unwrap(),task.task_id,event_id]).unwrap();
                drop(connection);
                let current=repository.load(&task.task_id).unwrap();
                repository.transition(&attempt.token,current.version,TaskState::Completed,None).unwrap();
            }
            let selected=repository.verified_procedures(chat,Some(project),"render",&json!({"render":identity}),base+2*MATURITY_MS).unwrap();
            assert!(selected["entries"].as_array().unwrap().is_empty(),"A wrong receipt and another work cannot become the third accepted task.");
            assert_eq!(selected["collecting"],1);
        }));
        let _=sqlite_connections::close(&root,&root.join("Agent/tasks.sqlite"));
        std::fs::remove_dir_all(&root).unwrap();
        if let Err(error)=run {std::panic::resume_unwind(error);}
    }

    #[test]
    fn verified_procedure_maturity_requires_real_distinct_tasks_and_time_then_invalidates_contracts() {
        let identity = "a".repeat(64);
        let candidate = json!({"schemaVersion":1,"identityPolicy":"per-tool-contract-v2","acceptanceReviewed":true,
            "completeSequence":true,"signature":"fixture","trigger":"render cathedral",
            "steps":[{"tool":"render","identity":identity,"argumentsTemplate":{"path":"{{string}}"}}]});
        let row = |task: &str,time: i64|json!({"taskId":task,"createdAtMs":time,"candidate":candidate});
        let identities = json!({"render":identity});
        let mut rows = vec![row("one",0),row("one",MATURITY_MS)];
        assert_eq!(select_procedures(&rows,"cathedral",&identities,MATURITY_MS)["collecting"],1);
        rows.push(row("two",1)); rows.push(row("three",MATURITY_MS));
        assert_eq!(select_procedures(&rows,"cathedral",&identities,MATURITY_MS)["entries"].as_array().unwrap().len(),1);
        let revoked = select_procedures(&rows,"cathedral",&json!({"render":"b".repeat(64)}),MATURITY_MS);
        assert!(revoked["entries"].as_array().unwrap().is_empty());
        assert_eq!(revoked["invalidatedConditions"],4);
    }
}

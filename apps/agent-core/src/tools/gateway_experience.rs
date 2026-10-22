//! Verified procedure lookup is a scoped projection of the existing task ledger, never an automatic executor.
//! 已核验方案查询是既有任务账本的范围投影，不是自动执行器。

use serde_json::{Value,json};
use super::gateway::{GatewayToolContext,check_configuration_generation,experience_identities};
use crate::{gateway_contract::GatewayError,storage::gateway};

fn unavailable(reason: &str) -> Value {
    json!({"entries":[],"state":"collecting","navigationOnly":true,"reason":reason,
        "instructions":"No verified procedure is available. Continue with normal tool discovery and current receipts; do not execute a candidate or repeat a completed action."})
}

/// Task context, not model-supplied scope fields, selects the accessible chat and work.
/// 查询范围只取真实任务上下文，模型提供的projectId/scope字段不改变可访问聊天或工作。
async fn procedures(arguments: Value, context: &GatewayToolContext) -> Result<Value,GatewayError> {
    if check_configuration_generation(context).is_err() { return Ok(unavailable("configuration-changed")); }
    let Some(repository)=context.runtime.experience_repository.get().cloned() else {
        return Ok(unavailable("existing-task-repository-not-ready"));
    };
    let identities=match experience_identities(context,&[]) {
        Ok(identities) => identities,
        Err(_) => return Ok(unavailable("tool-identities-unavailable")),
    };
    let root=context.data_home.clone();
    let chat=context.conversation_id.clone();
    let project=context.project_id.clone();
    let query=arguments["query"].as_str().or_else(||context.config["trustedUserText"].as_str())
        .unwrap_or("").to_owned();
    let result=tokio::task::spawn_blocking(move || {
        let relation=gateway::current_relationship(&root,&chat)?;
        let current=relation["projectId"].as_str();
        if current!=project.as_deref() { return Ok(unavailable("work-scope-changed")); }
        repository.verified_procedures(&chat,project.as_deref(),&query,&identities,
            chrono::Utc::now().timestamp_millis())
    }).await;
    if check_configuration_generation(context).is_err() { return Ok(unavailable("configuration-changed")); }
    match result {
        Ok(Ok(value)) => Ok(value),
        // Optional derived advice must not fail the task or trigger a tool/model retry storm.
        // 可选衍生建议不可中断任务或触发工具/模型重试风暴；已有正式回执仍是执行依据。
        _ => Ok(unavailable("verified-procedures-unavailable")),
    }
}

fn requested_limit(arguments: &Value) -> usize {
    arguments["limit"].as_u64().unwrap_or(3).clamp(1,20) as usize
}

async fn lookup(arguments: Value, context: &GatewayToolContext, legacy: bool) -> Result<Value,GatewayError> {
    let limit=requested_limit(&arguments);
    let query=arguments["query"].as_str().or_else(||context.config["trustedUserText"].as_str()).unwrap_or("");
    let forwarded=json!({"query":query,"limit":limit});
    let mut value=procedures(forwarded.clone(),context).await?;
    if let Some(entries)=value["entries"].as_array_mut() { entries.truncate(limit); }
    value["limit"]=json!(limit);
    if legacy {
        // Reuse the existing scoped specialist callback; it reads only this chat's legacy navigation file.
        // 复用既有范围绑定的专用读取回调，只读取本聊天的旧导航文件，不另建范围或账本。
        let previous=if check_configuration_generation(context).is_ok() {
            if let Some(execute)=&context.external_execute {
                execute("knowledge.experience".into(),forwarded).await.ok()
            } else { None }
        } else { None };
        let mut entries=previous.as_ref().and_then(|previous|previous["entries"].as_array()).cloned().unwrap_or_default();
        entries.truncate(limit);
        if check_configuration_generation(context).is_err() { entries.clear(); }
        value["legacyNavigation"]=json!({"entries":entries,"freshnessRequired":true,"navigationOnly":true,
            "verifiedProcedure":false,"instructions":"Historical retrieval navigation is not current evidence or a verified procedure. Reread original sources and validate versions; parameters and permission must be bound again."});
    }
    Ok(value)
}

pub(crate) async fn query(arguments: Value, context: &GatewayToolContext) -> Result<Value,GatewayError> {
    lookup(arguments,context,true).await
}

/// Automatic task guidance uses only activated procedures; legacy navigation stays explicit and on demand.
/// 任务自动提示只用已激活方案；旧检索导航仅显式调用时按需读取，不拖慢每次聊天准备。
pub(crate) async fn hint(query: &str, context: &GatewayToolContext) -> Result<Value,GatewayError> {
    lookup(json!({"query":query,"limit":3}),context,false).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn verified_experience_query_without_verified_identity_keeps_normal_discovery_available() {
        let (context,_keep)=super::super::gateway::test_context();
        let value=query(json!({"query":"repeat a prior workflow","projectId":"outside-project"}),&context).await.unwrap();
        assert_eq!(value["entries"],json!([]));
        assert_eq!(value["navigationOnly"],true);
        assert!(context.runtime.mcp.lock().await.is_empty());
        assert!(context.runtime.schemas.read().await.is_empty());
        let _=std::fs::remove_dir_all(context.workdir.parent().unwrap());
    }

    #[tokio::test]
    async fn verified_experience_keeps_legacy_navigation_scoped_and_honors_existing_limit() {
        use std::sync::{Arc,atomic::{AtomicUsize,Ordering}};
        let (mut context,_keep)=super::super::gateway::test_context();
        let calls=Arc::new(AtomicUsize::new(0));
        let observed=calls.clone();
        context.external_execute=Some(Arc::new(move|name,arguments| {
            let observed=observed.clone();
            Box::pin(async move {
                assert_eq!(name,"knowledge.experience");
                assert!(arguments.get("projectId").is_none());
                assert_eq!(arguments["limit"],2);
                observed.fetch_add(1,Ordering::AcqRel);
                Ok(json!({"entries":[{"sourceRef":"old-one"},{"sourceRef":"old-two"},{"sourceRef":"old-three"}],"freshnessRequired":true}))
            })
        }));
        let result=query(json!({"query":"prior sources","limit":2,"projectId":"outside-project"}),&context).await.unwrap();
        assert_eq!(result["entries"],json!([]));
        assert_eq!(result["legacyNavigation"]["entries"].as_array().unwrap().len(),2);
        assert_eq!(result["legacyNavigation"]["freshnessRequired"],true);
        assert_eq!(result["legacyNavigation"]["verifiedProcedure"],false);
        assert_eq!(calls.load(Ordering::Acquire),1);
        let automatic=hint("current task",&context).await.unwrap();
        assert!(automatic.get("legacyNavigation").is_none());
        assert_eq!(automatic["limit"],3);
        assert_eq!(calls.load(Ordering::Acquire),1,"Automatic guidance does not perform the legacy lookup.");
        assert_eq!(requested_limit(&json!({"limit":20})),20);
        let _=std::fs::remove_dir_all(context.workdir.parent().unwrap());
    }
}

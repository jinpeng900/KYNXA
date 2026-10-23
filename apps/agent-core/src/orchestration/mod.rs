pub mod budget;
mod completion_evidence;
pub mod context;
pub mod embedding_policy;
pub mod gateway_runtime;
pub mod gateway_attachments;
pub mod gateway_loop;
pub mod gateway_task_journal;
pub mod gateway_document_progress;
pub mod gateway_work_progress;
pub mod gateway_task_input;
pub mod gateway_task_http;
pub mod gateway_model_pool;
pub mod gateway_task_hub;
pub mod gateway_task_worker;
pub mod gateway_tool_batch;
pub mod evidence_projection;
pub mod evidence_publication;
pub mod interpretation;
pub mod navigation;
pub mod progress;
pub mod retrieval_policy;
pub mod runtime_policy;
pub mod segments;
pub mod source_collections;
pub mod source_coordination;
pub mod source_jobs;
pub mod source_management;
pub mod source_pipeline;
pub mod source_preparation;
pub mod source_sync;
pub mod task;
pub mod workflow;

pub fn calculate(
    operation: &str,
    arguments: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    if operation.starts_with("orchestration.segment.") {
        return segments::calculate(operation, arguments);
    }
    if operation == "orchestration.sourceManagementSummary" {
        return Ok(source_management::scan_summary(arguments));
    }
    if operation == "orchestration.projectionAudit" {
        return Ok(evidence_publication::projection_audit(arguments));
    }
    if matches!(
        operation,
        "orchestration.completedContext"
            | "orchestration.safeFailure"
            | "orchestration.evidenceWarning"
            | "orchestration.httpFailure"
    ) {
        return runtime_policy::calculate(operation, arguments);
    }
    // Outcome classification belongs to interpretation, before the retrieval policy prefix.
    // 结果分类属于意图解释，须先于检索策略的宽前缀分派，避免合法操作被误拒绝。
    if operation == "orchestration.retrievalOutcome" {
        return interpretation::calculate(operation, arguments);
    }
    if operation.starts_with("orchestration.retrieval")
        || operation == "orchestration.conversationSourceSelection"
    {
        return retrieval_policy::calculate(operation, arguments);
    }
    if matches!(
        operation,
        "orchestration.evidenceNotice"
            | "orchestration.evidenceProjection"
            | "orchestration.visibleScopes"
    ) {
        evidence_projection::calculate(operation, arguments)
    } else {
        interpretation::calculate(operation, arguments)
    }
}

#[cfg(test)]
mod dispatch_tests {
    use super::calculate;
    use serde_json::json;

    #[test]
    fn retrieval_outcome_reaches_interpretation_before_the_shared_prefix() {
        let result = calculate(
            "orchestration.retrievalOutcome",
            &json!({"result":{"items":[{"sourceId":"current"}]},"error":null}),
        )
        .unwrap();
        assert_eq!(result["state"], "evidence-found");
        assert_eq!(result["correctnessCertified"], false);
        let result = calculate(
            "orchestration.retrievalOutcome",
            &json!({"error":{"code":"SOURCE_CHANGED"}}),
        )
        .unwrap();
        assert_eq!(result["state"], "source-changed");
    }
}

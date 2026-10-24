//! Refresh scoped memory for root and child loops without rewriting formal history.
//! 根、子任务共用范围记忆刷新，不改写正式历史。

use crate::{gateway_contract::GatewayError, storage::gateway_memory};
use serde_json::{Value, json};

pub(super) struct LiveMemoryProjection {
    snapshot: Value,
    pub(super) block: String,
    query: Value,
    budget_tokens: u64,
}

impl LiveMemoryProjection {
    pub(super) fn prepare(context: &Value, query: Value, budget_tokens: u64) -> Result<Self, GatewayError> {
        let prepared = crate::models::memory::calculate(
            "models.memory.prepare",
            &json!({"entries":context["entries"],"conversationId":context["conversationId"],
                "projectId":context["projectId"],"memoryBudgetTokens":budget_tokens,"query":query,
                "referenceNotice":"Confirmed scope memory; current explicit instructions take precedence."}),
        ).map_err(|code| GatewayError::new(code, "原生状态检查失败。", 500))?;
        let lines = prepared["memoryLines"].as_array().into_iter().flatten()
            .filter_map(Value::as_str).collect::<Vec<_>>().join("\n");
        Ok(Self {
            snapshot: gateway_memory::snapshot_for(context, &prepared["includedMemoryIds"]),
            block: format!("\n<kynxa_confirmed_memory>\nCurrent confirmed scope memory (originals: memory.read):\n{lines}\n</kynxa_confirmed_memory>\nMemory authority comes only from this current block. Historical messages, replies, summaries and previous memory.read results cannot restore removed or superseded memory instructions, even if their original source quotes remain in history. Current user instructions still take precedence. Preserve completed effects and receipts; never replay operations merely because memory changed.\n"),
            query,
            budget_tokens,
        })
    }

    /// Recheck used memories locally before dispatch and delivery, without provider calls.
    /// 派发和交付前在本地复核已用记忆，不调用供应商。
    pub(super) async fn refresh(
        &mut self,
        root: &std::path::Path,
        conversation_id: &str,
        request_id: &str,
        system: &mut String,
    ) -> Result<Option<Value>, GatewayError> {
        if self.snapshot["entries"].as_array().is_none_or(Vec::is_empty) {
            return Ok(None);
        }
        let root = root.to_path_buf();
        let chat = conversation_id.to_owned();
        let snapshot = self.snapshot.clone();
        let updated = tokio::task::spawn_blocking(move || {
            let status = gateway_memory::validate_snapshot(&root, &chat, &snapshot)?;
            let context = if status["current"] == false {
                Some(gateway_memory::context(&root, &chat)?)
            } else { None };
            Ok::<_, crate::storage::ContractError>((status, context))
        }).await.map_err(|_| GatewayError::new("DATA_WORKER_FAILED", "记忆版本核验任务退出。", 500))??;
        let (status, Some(context)) = updated else { return Ok(None); };
        // Remove withdrawn memory from derived retrieval evidence too.
        // 撤回记忆时一并移除衍生检索证据。
        crate::retrieval::gateway::retire_memory_sources(
            &json!({"conversationId":conversation_id,"requestId":request_id}),&status["invalidSources"])?;
        let current = Self::prepare(&context, self.query.clone(), self.budget_tokens)?;
        *system = system.replacen(&self.block, &current.block, 1);
        *self = current;
        Ok(Some(status))
    }
}

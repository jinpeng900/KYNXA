//! Provider calculations own interpretation; transport and journal I/O remain in adapters.
//! 供应商计算负责解析与决策；网络传输及正式日志 I/O 仍留在适配器。

pub mod batches;
pub mod capabilities;
pub(crate) mod cache_policy;
pub mod connections;
pub mod context;
pub mod context_control;
pub mod discovery;
pub mod document_fit;
pub mod gateway;
pub mod gateway_config;
pub mod gateway_context;
pub mod gateway_inference;
pub mod gateway_local_engine;
pub mod gateway_local_models;
pub mod gateway_preparation;
pub mod gateway_protocol;
pub mod gateway_stream;
pub mod history;
pub mod history_service;
pub mod inference;
pub mod lifecycle;
pub mod memory;
pub(crate) mod message_attachments;
pub mod output;
pub mod plain;
pub mod portable;
pub mod projection;
pub mod protocol;
pub mod recovery;
pub mod registry;
pub mod routing;
pub mod semantic;
pub mod stream;
pub mod tool_context;
pub mod transcript;
pub(crate) mod usage;
pub mod worker;

use serde_json::Value;

pub fn calculate(operation: &str, arguments: &Value) -> Result<Value, String> {
    if operation.starts_with("models.portable.") {
        return portable::calculate(operation, arguments);
    }
    if operation.starts_with("models.transcript.") {
        return transcript::calculate(operation, arguments);
    }
    if operation.starts_with("models.capabilities.") {
        return capabilities::calculate(operation, arguments);
    }
    if operation.starts_with("models.registry.") {
        return registry::calculate(operation, arguments);
    }
    if operation.starts_with("models.connections.") {
        return connections::calculate(operation, arguments);
    }
    if operation.starts_with("models.inference.") {
        return inference::calculate(operation, arguments);
    }
    if operation.starts_with("models.semantic.") {
        return semantic::calculate(operation, arguments);
    }
    if operation.starts_with("models.context.projection.") {
        return projection::calculate(operation, arguments);
    }
    if operation.starts_with("models.memory.") {
        return memory::calculate(operation, arguments);
    }
    if operation.starts_with("models.recovery.") {
        return recovery::calculate(operation, arguments);
    }
    if operation.starts_with("models.protocol.") || operation.starts_with("models.tokens.") {
        return protocol::calculate(operation, arguments);
    }
    if operation.starts_with("models.history.") && operation != "models.history.compaction" {
        if [
            "models.history.extractCheckpoint",
            "models.history.validCall",
            "models.history.executionWitness",
            "models.history.activityIndex",
            "models.history.publicPlan",
            "models.history.brokerText",
            "models.history.observation",
            "models.history.summaryTool",
            "models.history.summarySource",
            "models.history.publicChronology",
        ]
        .contains(&operation)
        {
            return history_service::calculate(operation, arguments);
        }
        return history::calculate(operation, arguments);
    }
    if operation == "models.history.compaction" || operation.starts_with("models.context.") {
        return context::calculate(operation, arguments);
    }
    Err("AGENT_CORE_UNKNOWN_OPERATION".to_owned())
}

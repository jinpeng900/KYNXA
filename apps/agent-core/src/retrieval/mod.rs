pub mod ann_queue;
pub mod budget;
pub mod candidates;
pub mod chunking;
pub mod code_units;
pub mod document;
pub mod evidence;
pub mod fusion;
pub mod gateway;
pub mod gateway_archives;
pub mod gateway_document_mail;
pub mod gateway_documents;
pub mod gateway_embedding;
pub mod gateway_history;
pub mod gateway_index;
pub mod gateway_legacy_office;
pub mod gateway_message_attachments;
pub mod gateway_office_documents;
pub mod gateway_spreadsheets;
pub mod gateway_structure;
pub mod gateway_tools;
pub mod gateway_url_sources;
pub mod indexing_run;
pub mod lexical;
pub mod query;
pub mod relations;
pub mod service_lifecycle;
pub mod token_fit;
pub mod window;

use serde_json::Value;

/// Pure retrieval rules share one Rust implementation across native and process transports.
/// 检索纯规则由同一 Rust 实现提供给原生绑定和进程协议，不新增存储写入者。
pub fn calculate(operation: &str, args: &Value) -> Result<Value, String> {
    if operation.starts_with("retrieval.indexer-") {
        return indexing_run::apply(operation, args);
    }
    match operation {
        "retrieval.document-blocks" => document::parse(args),
        "retrieval.fusion" => Ok(fusion::apply(args)),
        "retrieval.source-relations" => relations::derive(args),
        "retrieval.candidate-deduplicate"
        | "retrieval.candidate-assess"
        | "retrieval.candidate-select" => candidates::apply(operation, args),
        "retrieval.budget" => budget::apply(args),
        "retrieval.chunk-plan" => chunking::apply(args),
        "retrieval.code-unit-select" => code_units::select(args),
        "retrieval.greeting"
        | "retrieval.query-interpret"
        | "retrieval.query-plan"
        | "retrieval.exact-target"
        | "retrieval.acquisition-plan"
        | "retrieval.source-priorities" => query::apply(operation, args),
        "retrieval.lexical-terms"
        | "retrieval.lexical-text"
        | "retrieval.query-concepts"
        | "retrieval.query-terms"
        | "retrieval.match-expression" => lexical::apply(operation, args),
        "retrieval.window-options" | "retrieval.window-preflight" | "retrieval.source-window" => {
            window::apply(operation, args)
        }
        "retrieval.token-projection" | "retrieval.token-fit" | "retrieval.fitted-text" => {
            token_fit::apply(operation, args)
        }
        _ => Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
    }
}

pub mod contracts;
pub mod discovery;
pub mod effects;
pub mod intent;
mod local_files;
pub mod observations;
pub mod policy;
mod sensitive_files;
pub mod service;
mod service_approval;
mod service_artifacts;
mod service_boundary;
mod service_broker;
mod service_broker_completion;
mod service_broker_preflight;
mod service_browser;
mod service_browser_script;
mod service_catalog;
mod service_code_versions;
mod service_config;
pub(crate) mod service_document_extraction;
mod service_error;
mod service_filesystem;
pub(crate) mod service_history;
mod service_host_jobs;
mod service_http;
mod service_mcp;
mod service_mcp_diagnostics;
mod service_observation_cache;
mod service_runner;
mod service_search;
mod service_skill_discovery;
mod service_skill_environment;
mod service_skill_import;
mod service_skill_metadata;
mod service_skills;
pub(crate) mod service_source_reading;
mod service_web;
mod service_web_sources;
mod system_prompt;

use serde_json::Value;

/// Shared pure tool rules never dispatch host operations or change permissions.
/// 共用的纯工具规则不派发宿主操作，也不更改权限。
pub fn calculate(operation: &str, arguments: &Value) -> Result<Value, String> {
    if operation.starts_with("tools.localPath.") {
        return Ok(service_error::envelope(local_files::path_operation(
            operation, arguments,
        )));
    }
    if operation == "tools.systemPrompt" {
        return system_prompt::calculate(arguments);
    }
    if operation == "tools.sensitiveFilePath" {
        return Ok(serde_json::json!(sensitive_files::is_sensitive(
            arguments["path"].as_str().unwrap_or("")
        )));
    }
    match operation {
        "tools.browserOperation"
        | "tools.browserPrepareOptions"
        | "tools.browserTargetGuard"
        | "tools.browserTiming"
        | "tools.browserReferences"
        | "tools.browserReceiptEvidence"
        | "tools.browserApprovalGuard"
        | "tools.browserRecoveryAllowed"
        | "tools.browserUrlAllowed"
        | "tools.browserPrivateQueryNames" => browser::calculate(operation, arguments),
        "tools.mcpUrl"
        | "tools.normalizeMcpConnection"
        | "tools.connectionConfiguration"
        | "tools.configuredEnabled"
        | "tools.configurationChanged"
        | "tools.changedMcpServerIds"
        | "tools.skillHeader"
        | "tools.validateSkillHeader" => configuration::calculate(operation, arguments),
        "tools.discoveryCategory"
        | "tools.searchTools"
        | "tools.selectionSignals"
        | "tools.catalogOrder"
        | "tools.relevantMcpServers" => discovery::calculate(operation, arguments),
        "tools.normalizeEnvironment"
        | "tools.resolveEnvironment"
        | "tools.environmentServer"
        | "tools.validateBuiltinInput" => contracts::calculate(operation, arguments),
        "tools.needsApproval"
        | "tools.nativeAvailability"
        | "tools.desktopObservation"
        | "tools.observationFailure"
        | "tools.canParallel"
        | "tools.desktopLaunchOptions" => policy::calculate(operation, arguments),
        _ => intent::calculate(operation, arguments),
    }
}
pub mod browser;
pub mod configuration;
pub mod gateway;
mod gateway_browser;
mod gateway_mcp;
mod gateway_mcp_budget;
pub(crate) mod gateway_process;
mod gateway_web;
mod gateway_web_proxy;

pub fn execute_source_io(
    operation: &str,
    arguments: &Value,
    cancellation: &Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Value {
    service_error::envelope(service_source_reading::execute_io(
        operation,
        arguments,
        cancellation,
    ))
}

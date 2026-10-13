use std::sync::{Arc, atomic::AtomicBool};

use serde_json::Value;

use super::service_approval::ApprovalRegistry;
use super::service_boundary::StorageBoundary;
use super::service_broker::BrokerState;
use super::service_browser::BrowserState;
use super::service_catalog::CatalogState;
use super::service_code_versions::CodeVersions;
use super::service_config::ConfigRepository;
use super::service_document_extraction::DocumentExtractionState;
use super::service_error::{ServiceError, envelope};
use super::service_history::HistoryState;
use super::service_host_jobs::HostJobs;
use super::service_http::HttpState;
use super::service_mcp::McpState;
use super::service_observation_cache::ObservationCache;
use super::service_runner::RunnerState;
use super::service_search::SearchService;
use super::service_skill_discovery::SkillDiscovery;
use super::service_skill_environment::SkillEnvironment;
use super::service_skill_metadata::SkillMetadata;
use super::service_skills::SkillInventory;
use super::service_source_reading::SourceReadingState;
use super::service_web::WebState;

/// Each request-owned native helper releases business state when its runtime owner closes or is collected.
/// 请求所属原生辅助对象在运行时所有者关闭或回收时释放业务状态。
#[derive(Default)]
pub struct ToolSession {
    pub cancellation: Option<Arc<AtomicBool>>,
    pub committed: bool,
    approvals: ApprovalRegistry,
    catalog: CatalogState,
    configuration: ConfigRepository,
    skills: SkillInventory,
    search: SearchService,
    skill_metadata: SkillMetadata,
    web: WebState,
    broker: BrokerState,
    http: HttpState,
    skill_environment: SkillEnvironment,
    skill_discovery: SkillDiscovery,
    mcp: McpState,
    boundary: StorageBoundary,
    host_jobs: HostJobs,
    browser: BrowserState,
    code_versions: CodeVersions,
    observation_cache: ObservationCache,
    history: HistoryState,
    source_reading: SourceReadingState,
    document_extraction: DocumentExtractionState,
    runner: RunnerState,
}

impl ToolSession {
    pub fn apply(&mut self, operation: &str, arguments: &Value) -> Result<Value, String> {
        self.committed = false;
        let operation = operation
            .strip_prefix("tools.service.")
            .ok_or("AGENT_CORE_UNKNOWN_OPERATION")?;
        let result = if operation.starts_with("approval.") {
            self.approvals.apply(operation, arguments)
        } else if operation.starts_with("catalog.") {
            self.catalog.apply(operation, arguments)
        } else if operation.starts_with("config.") {
            self.configuration.apply(operation, arguments)
        } else if operation == "skill.import" {
            super::service_skill_import::import_package(arguments, &self.cancellation)
        } else if operation.starts_with("skill.") {
            self.skills.apply(operation, arguments, &self.cancellation)
        } else if operation.starts_with("file.") {
            super::service_filesystem::apply(operation, arguments, &self.cancellation)
        } else if operation.starts_with("search.") {
            self.search.apply(operation, arguments, &self.cancellation)
        } else if operation.starts_with("metadata.") {
            self.skill_metadata.apply(operation, arguments)
        } else if operation.starts_with("web.") {
            self.web.apply(operation, arguments)
        } else if operation.starts_with("broker.") {
            self.broker.apply(operation, arguments)
        } else if operation.starts_with("http.") {
            self.http.apply(operation, arguments)
        } else if operation.starts_with("sources.") {
            super::service_web_sources::apply(operation, arguments)
        } else if operation.starts_with("environment.") {
            self.skill_environment.apply(operation, arguments)
        } else if operation.starts_with("discovery.") {
            self.skill_discovery
                .apply(operation, arguments, &self.cancellation)
        } else if operation.starts_with("mcp.") {
            self.mcp.apply(operation, arguments)
        } else if operation.starts_with("boundary.") {
            self.boundary.apply(operation, arguments)
        } else if operation.starts_with("host.") {
            self.host_jobs.apply(operation, arguments)
        } else if operation.starts_with("browser.") {
            self.browser.apply(operation, arguments)
        } else if operation.starts_with("versions.") {
            self.code_versions
                .apply(operation, arguments, &self.cancellation)
        } else if operation.starts_with("observationCache.") {
            self.observation_cache.apply(operation, arguments)
        } else if operation.starts_with("history.") {
            self.history.apply(operation, arguments)
        } else if operation.starts_with("source.reading.") {
            self.source_reading
                .apply(operation, arguments, &self.cancellation)
        } else if operation.starts_with("source.document.") {
            self.document_extraction.apply(operation, arguments)
        } else if operation.starts_with("artifact.") {
            super::service_artifacts::apply(operation, arguments, &self.cancellation)
        } else if operation.starts_with("runner.") {
            self.runner.apply(operation, arguments)
        } else if operation == "local.inspect" {
            super::local_files::inspect_projection(arguments)
        } else {
            Err(ServiceError::new(
                "AGENT_CORE_UNKNOWN_OPERATION",
                "Unknown tool service operation.",
                400,
            ))
        };
        self.committed = result.is_ok()
            && matches!(
                operation,
                "config.update"
                    | "file.write"
                    | "file.edit"
                    | "file.delete"
                    | "file.mkdir"
                    | "skill.import"
                    | "runner.sandboxCleanup"
            );
        Ok(envelope(result))
    }
}

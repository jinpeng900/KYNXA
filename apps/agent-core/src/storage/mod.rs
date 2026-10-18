pub mod administration;
pub mod ann_cache;
pub mod cancellation;
pub mod candidates;
pub mod catalog;
pub mod catalog_transactions;
pub mod checkpoints;
pub mod conversation_history;
pub mod conversation_index;
pub mod conversation_logs;
pub mod document_coverage;
pub mod evidence;
pub mod experiences;
pub mod extensions;
pub mod gateway;
pub mod gateway_memory;
pub mod index_registry;
pub mod index_validation;
pub mod job_contracts;
pub mod jobs;
pub mod layout;
pub mod library;
pub mod managed_files;
pub mod memory;
pub mod memory_lifecycle;
pub mod memory_repository;
pub mod memory_service;
pub mod message_attachments;
#[cfg(test)]
mod native_services_tests;
pub mod raw_json;
pub mod retrieval;
pub mod settings;
pub mod task_repository;
pub mod source_coverage;
pub mod source_manifest;
pub mod sqlite_bootstrap;
pub mod sqlite_channels;
pub mod sqlite_connections;
pub mod sqlite_index;
pub mod sqlite_lexical;
pub mod sqlite_queries;
pub mod sqlite_schema;
pub mod sqlite_search;
pub mod sqlite_store;
pub mod sqlite_vector_reads;
pub(crate) mod tool_memory;
pub mod tool_results;
pub mod workspace_mounts;

use serde_json::{Value, json};

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Debug)]
pub struct ContractError {
    code: &'static str,
    message: String,
    status_code: u16,
}

pub(crate) type ContractResult = Result<Value, ContractError>;

impl ContractError {
    pub fn code(&self) -> &str { self.code }
    pub fn message(&self) -> &str { &self.message }
    pub fn status_code(&self) -> u16 { self.status_code }
    pub(crate) fn new(message: impl Into<String>, code: &'static str, status_code: u16) -> Self {
        Self {
            code,
            message: message.into(),
            status_code,
        }
    }
}

fn utf16_length(value: &str) -> usize {
    value.encode_utf16().count()
}

fn js_space(character: char) -> bool {
    matches!(character, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' |
        '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' |
        '\u{3000}' | '\u{feff}')
}

fn trim(value: &str) -> &str {
    value.trim_matches(js_space)
}

fn safe_integer(value: &Value, minimum: u64, maximum: u64) -> bool {
    value
        .as_u64()
        .is_some_and(|value| value >= minimum && value <= maximum)
        || value.as_f64().is_some_and(|value| {
            value.is_finite()
                && value.fract() == 0.0
                && value >= minimum as f64
                && value <= maximum as f64
        })
}

fn hash(value: &Value) -> bool {
    value.as_str().is_some_and(|value| {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn member(value: &Value, choices: &[&str]) -> bool {
    value.as_str().is_some_and(|value| choices.contains(&value))
}

fn object<'a>(
    value: &'a Value,
    message: &str,
    code: &'static str,
    status: u16,
) -> Result<&'a serde_json::Map<String, Value>, ContractError> {
    value
        .as_object()
        .ok_or_else(|| ContractError::new(message, code, status))
}

fn keys(value: &Value, allowed: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|value| value.keys().all(|key| allowed.contains(&key.as_str())))
}

pub fn identity(value: &Value) -> ContractResult {
    let invalid = || ContractError::new("会话或消息 ID 无效。", "INVALID_CONVERSATION_DATA", 400);
    let text = value.as_str().ok_or_else(invalid)?;
    if text.is_empty()
        || text.len() > 128
        || !text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(invalid());
    }
    let lower = text.to_ascii_lowercase();
    if ["con", "prn", "aux", "nul"].contains(&lower.as_str())
        || lower.len() == 4
            && (lower.starts_with("com") || lower.starts_with("lpt"))
            && matches!(lower.as_bytes()[3], b'1'..=b'9')
    {
        return Err(invalid());
    }
    if text.len() == 32 && text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Ok(json!(format!(
            "{}-{}-{}-{}-{}",
            &lower[..8],
            &lower[8..12],
            &lower[12..16],
            &lower[16..20],
            &lower[20..]
        )));
    }
    if text.len() == 36
        && text.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
    {
        return Ok(json!(lower));
    }
    Ok(json!(text))
}

fn optional_fields(paths: &mut Vec<Value>, prefix: &[&str], fields: &[&str]) {
    for field in fields {
        let mut path: Vec<&str> = prefix.to_vec();
        path.push(field);
        paths.push(json!(path));
    }
}

fn source_optional_fields(paths: &mut Vec<Value>, prefix: &[&str]) {
    optional_fields(
        paths,
        prefix,
        &["type", "role", "conversationId", "messageId"],
    );
}

fn settings_optional_fields(
    paths: &mut Vec<Value>,
    defaults: &mut Vec<Value>,
    prefix: &[&str],
    project: bool,
) {
    if project {
        optional_fields(paths, prefix, &["overrides", "indexingSources"]);
        let mut overrides = prefix.to_vec();
        overrides.push("overrides");
        settings_optional_fields(paths, defaults, &overrides, false);
        let mut sources = prefix.to_vec();
        sources.push("indexingSources");
        optional_fields(paths, &sources, &["mountedFolder", "knowledgeIds"]);
    } else {
        optional_fields(paths, prefix, &["local", "web", "cache"]);
        let mut indexing = prefix.to_vec();
        indexing.extend(["local", "indexing"]);
        defaults.push(json!({"path": indexing, "value": {}}));
    }
}

/// Optionality belongs to the contract, while Node projects only actual JavaScript undefined values.
/// 可选字段归合同管理；Node 只投影真实 JavaScript undefined，不把用户对象解释成缺省标记。
fn undefined_policy(arguments: &Value) -> Value {
    let operation = arguments["operation"].as_str().unwrap_or("");
    let mut optional_paths = Vec::new();
    let mut defaults = Vec::new();
    match operation {
        "storage.manifest.files" => {
            optional_fields(&mut optional_paths, &["files", "*"], &["extraction"]);
        }
        "storage.library.remove" => {
            optional_fields(&mut optional_paths, &[], &["expectedRevision"]);
        }
        "storage.index.relations" => {
            optional_fields(
                &mut optional_paths,
                &[],
                &["sourceRef", "sourceId", "symbol", "kind"],
            );
        }
        "storage.jobs.read"
        | "storage.jobs.create"
        | "storage.jobs.update"
        | "storage.jobs.commitBatch"
        | "storage.jobs.recover" => {
            optional_fields(
                &mut optional_paths,
                &["document", "jobs", "*", "coverage", "sources", "*"],
                &["errorCode"],
            );
            optional_fields(
                &mut optional_paths,
                &["patch", "coverage", "sources", "*"],
                &["errorCode"],
            );
            // The job adapter sends explicit clear facts; omission must not erase required identity or status.
            // 作业适配器另传实际清除事实；缺省不得清除身份与状态等必需字段。
            optional_fields(
                &mut optional_paths,
                &["patch"],
                &[
                    "error",
                    "startedAt",
                    "finishedAt",
                    "checkpoint",
                    "semantic",
                    "coverage",
                ],
            );
        }
        "storage.coverage.record" => {
            optional_fields(
                &mut optional_paths,
                &["entries", "*"],
                &["errorCode", "documentCoverage"],
            );
        }
        "storage.memory.kind" | "storage.memory.revision" | "storage.memory.status" => {
            optional_fields(&mut optional_paths, &[], &["value"]);
        }
        "storage.memory.settings" => {
            optional_fields(&mut optional_paths, &[], &["previous"]);
            defaults.push(json!({"path": ["value"], "value": {}}));
        }
        "storage.memory.source" => source_optional_fields(&mut optional_paths, &["value"]),
        "storage.memory.candidate" => {
            optional_fields(&mut optional_paths, &["value"], &["batchThroughMessageId"]);
        }
        "storage.memory.proposal" => {
            optional_fields(&mut optional_paths, &["value"], &["target", "appliedAt"]);
        }
        "storage.memory.document" => {
            optional_fields(
                &mut optional_paths,
                &["value"],
                &[
                    "dismissedSources",
                    "candidateSettings",
                    "candidateSettingsRevision",
                ],
            );
            let entry = ["value", "entries", "*"];
            optional_fields(
                &mut optional_paths,
                &entry,
                &["kind", "candidate", "proposal"],
            );
            source_optional_fields(&mut optional_paths, &["value", "entries", "*", "source"]);
            optional_fields(
                &mut optional_paths,
                &["value", "entries", "*", "candidate"],
                &["batchThroughMessageId"],
            );
            optional_fields(
                &mut optional_paths,
                &["value", "entries", "*", "proposal"],
                &["target", "appliedAt"],
            );
            optional_fields(
                &mut optional_paths,
                &["value", "dismissedSources", "*"],
                &[
                    "candidateFingerprint",
                    "proposalFingerprint",
                    "candidateBatchThroughMessageId",
                ],
            );
        }
        "storage.memory.extract" => {
            optional_fields(
                &mut optional_paths,
                &[],
                &["userMessageId", "afterMessageId"],
            );
        }
        "storage.retrieval.extraction" => {
            optional_fields(&mut optional_paths, &[], &["textLength"]);
            optional_fields(
                &mut optional_paths,
                &["value"],
                &[
                    "pageCount",
                    "pages",
                    "pageWindow",
                    "complete",
                    "uncoveredRanges",
                    "ocr",
                ],
            );
            optional_fields(&mut optional_paths, &["value", "pages", "*"], &["method"]);
        }
        "storage.retrieval.fileWindow" => {
            optional_fields(&mut optional_paths, &[], &["textLength"])
        }
        "storage.retrieval.structure" => {
            optional_fields(&mut optional_paths, &["value"], &["diagnosticCodes"]);
        }
        "storage.retrieval.chunkStructure" => {
            optional_fields(&mut optional_paths, &[], &["sourceStructure"]);
            optional_fields(
                &mut optional_paths,
                &["value"],
                &[
                    "symbolName",
                    "qualifiedName",
                    "parentSymbol",
                    "sectionTitle",
                    "sectionPath",
                ],
            );
        }
        "storage.retrieval.intent" => {
            optional_fields(
                &mut optional_paths,
                &["value"],
                &["domain", "symbol", "path", "preferredDomain"],
            );
        }
        "storage.retrieval.reference" => {
            optional_fields(&mut optional_paths, &["value"], &["derivationSignature"]);
        }
        "storage.settings.indexing" | "storage.settings.ann" => {
            defaults.push(json!({"path": ["value"], "value": {}}));
        }
        "storage.settings.patch" => {
            settings_optional_fields(&mut optional_paths, &mut defaults, &["value"], false)
        }
        "storage.settings.projectPatch" => {
            settings_optional_fields(&mut optional_paths, &mut defaults, &["value"], true)
        }
        "storage.settings.mutation" => {
            settings_optional_fields(
                &mut optional_paths,
                &mut defaults,
                &["value", "patch"],
                arguments["project"] == true,
            );
        }
        "storage.settings.document" => {
            optional_fields(&mut optional_paths, &[], &["projectId"]);
            let project = arguments["hasProjectId"] == true;
            settings_optional_fields(&mut optional_paths, &mut defaults, &["value"], project);
            if project {
                optional_fields(
                    &mut optional_paths,
                    &["value", "overrides"],
                    &["local", "web", "cache"],
                );
            }
        }
        "storage.settings.mergeLocal" => {
            defaults.push(json!({"path": ["base"], "value": {}}));
            defaults.push(json!({"path": ["patch"], "value": {}}));
        }
        "storage.catalog.message" => {
            optional_fields(
                &mut optional_paths,
                &["value"],
                &[
                    "CreatedAt",
                    "ReasoningDurationMs",
                    "Content",
                    "Reasoning",
                    "Status",
                    "Error",
                    "Provider",
                    "Model",
                ],
            );
        }
        "storage.catalog.chat" => {
            optional_fields(&mut optional_paths, &["value"], &["Draft", "Messages"]);
        }
        "storage.catalog.projectValue" => {
            optional_fields(&mut optional_paths, &["value"], &["FolderPath"]);
        }
        _ => {}
    }
    json!({"optionalPaths": optional_paths, "defaults": defaults})
}

/// Domain contracts and declared I/O services share error envelopes, preserving exact adapter identities.
/// 领域合同与已声明 I/O 服务共用错误封装；适配器保留原始字节、日期与既有哈希身份。
pub fn calculate(operation: &str, arguments: &Value) -> Result<Value, String> {
    let boundary_before = sqlite_connections::boundary_counts();
    let result = if operation == "storage.undefinedPolicy" {
        Ok(undefined_policy(arguments))
    } else if operation == "storage.identity" {
        identity(&arguments["value"])
    } else if operation.starts_with("storage.administration.") {
        administration::apply(operation, arguments)
    } else if operation.starts_with("storage.files.") {
        managed_files::apply(operation, arguments)
    } else if operation == "storage.index.lexical" {
        sqlite_lexical::apply(arguments)
    } else if operation == "storage.retrieval.indexedSource" {
        index_validation::validate(arguments)
    } else if operation == "storage.retrieval.derivedChunks" {
        index_validation::derive_chunks(arguments)
    } else if operation == "storage.retrieval.lexicalBatch" {
        index_validation::lexical_batch(arguments)
    } else if operation == "storage.retrieval.vectorDistances" {
        index_validation::distances(arguments)
    } else if operation == "storage.retrieval.vectorRank" {
        index_validation::rank_vectors(arguments)
    } else if [
        "storage.index.bootstrap",
        "storage.index.migrationBatch",
        "storage.index.migrationSource",
        "storage.index.completeMigration",
    ]
    .contains(&operation)
    {
        sqlite_bootstrap::apply(operation, arguments)
    } else if sqlite_search::handles(operation) {
        sqlite_search::apply(operation, arguments)
    } else if sqlite_queries::handles(operation) {
        sqlite_queries::apply(operation, arguments)
    } else if operation.starts_with("storage.index.") {
        sqlite_index::apply(operation, arguments)
    } else if operation.starts_with("storage.repository.") {
        memory_repository::apply(operation, arguments)
    } else if operation.starts_with("storage.jobs.") {
        jobs::apply(operation, arguments)
    } else if operation.starts_with("storage.registry.") {
        index_registry::apply(operation, arguments)
    } else if operation.starts_with("storage.checkpoints.") {
        checkpoints::apply(operation, arguments)
    } else if operation == "storage.coverage.document" {
        document_coverage::validate(&arguments["value"])
    } else if operation.starts_with("storage.coverage.") {
        source_coverage::apply(operation, arguments)
    } else if operation.starts_with("storage.manifest.") {
        source_manifest::apply(operation, arguments)
    } else if operation.starts_with("storage.layout.") {
        layout::apply(operation, arguments)
    } else if operation.starts_with("storage.library.") {
        library::apply(operation, arguments)
    } else if operation.starts_with("storage.experiences.") {
        experiences::apply(operation, arguments)
    } else if operation.starts_with("storage.extensions.") {
        extensions::apply(operation, arguments)
    } else if operation.starts_with("storage.toolResults.") {
        tool_results::apply(operation, arguments)
    } else if operation.starts_with("storage.evidence.") {
        evidence::apply(operation, arguments)
    } else if operation.starts_with("storage.ann.") {
        ann_cache::apply(operation, arguments)
    } else if operation.starts_with("storage.logs.") {
        conversation_logs::apply(operation, arguments)
    } else if operation.starts_with("storage.history.") {
        conversation_history::apply(operation, arguments)
    } else if operation.starts_with("storage.conversations.") {
        catalog_transactions::apply(operation, arguments)
    } else if operation.starts_with("storage.catalogIndex.") {
        conversation_index::apply(operation, arguments)
    } else if operation.starts_with("storage.catalog.") {
        catalog::apply(operation, arguments)
    } else if operation.starts_with("storage.memoryService.") {
        memory_service::apply(operation, arguments)
    } else if operation.starts_with("storage.memory.") {
        memory::apply(operation, arguments)
    } else if operation.starts_with("storage.retrieval.") {
        retrieval::apply(operation, arguments)
    } else if operation.starts_with("storage.settings.") {
        settings::apply(operation, arguments)
    } else {
        return Err("AGENT_CORE_UNKNOWN_OPERATION".into());
    };
    let mut response = match result {
        Ok(value) => json!({"value": value}),
        Err(error) => {
            json!({"error": {"code": error.code, "message": error.message, "statusCode": error.status_code}})
        }
    };
    if operation.starts_with("storage.index.") || operation.starts_with("storage.ann.") {
        response["nativeAudit"] = sqlite_connections::boundary_audit(boundary_before);
    }
    Ok(response)
}

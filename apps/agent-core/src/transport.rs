use serde::Serialize;

const MIB: usize = 1024 * 1024;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameLimits {
    pub input_bytes: usize,
    pub response_bytes: usize,
}

/// Both adapters and executors use the same caps; expanded frames never expand source permissions.
/// 适配器和执行器共用额度表；扩大的传输帧不扩大来源权限或正文合同。
pub fn frame_limits(operation: &str, kind: &str) -> FrameLimits {
    let input_bytes = match kind {
        "calculation" if operation == "storage.manifest.files" => 288 * MIB,
        "calculation" if operation == "storage.catalog.message" => 128 * MIB,
        "calculation"
            if matches!(
                operation,
                "storage.evidence.projection"
                    | "storage.retrieval.indexedSource"
                    | "storage.retrieval.derivedChunks"
                    | "storage.retrieval.lexicalBatch"
            ) =>
        {
            128 * MIB
        }
        "calculation"
            if operation == "storage.memoryService.query"
                || operation.starts_with("storage.library.") =>
        {
            128 * MIB
        }
        "calculation" if operation == "models.protocol.finalParts" => 64 * MIB,
        "calculation" => 32 * MIB,
        "session" if operation.starts_with("tools.service.source.document.") => 64 * MIB,
        "session"
            if [
                "models.stream.",
                "models.lifecycle.history.",
                "models.lifecycle.toolContext.",
            ]
            .iter()
            .any(|prefix| operation.starts_with(prefix)) =>
        {
            64 * MIB
        }
        "session"
            if (operation.starts_with("retrieval.indexing.")
                || operation.starts_with("retrieval.ann.")
                || operation.starts_with("orchestration.sourceManagement.")
                || operation.starts_with("storage.memoryLifecycle.")
                || operation.starts_with("tools.service.")) =>
        {
            32 * MIB
        }
        "session" => 8 * MIB,
        "service"
            if operation == "storage.index.publish"
                || operation.starts_with("storage.library.") =>
        {
            128 * MIB
        }
        "service" if operation.starts_with("tools.sourceIO.") => 64 * MIB,
        "session-service"
            if operation.starts_with("tools.service.source.reading.")
                || operation.starts_with("tools.service.source.document.") =>
        {
            64 * MIB
        }
        "service" if matches!(operation, "storage.files.read" | "storage.files.commit") => 72 * MIB,
        "service"
            if matches!(
                operation,
                "storage.logs.read"
                    | "storage.logs.append"
                    | "storage.logs.finalize"
                    | "storage.conversations.commit"
                    | "storage.conversations.apply"
                    | "storage.coverage.reconcile"
                    | "storage.manifest.read"
                    | "storage.manifest.write"
            ) =>
        {
            288 * MIB
        }
        _ => 32 * MIB,
    };
    let response_bytes = if kind == "service"
        && matches!(
            operation,
            "storage.checkpoints.read" | "storage.history.page"
        ) {
        288 * MIB
    } else if kind == "service" && operation == "storage.index.publish" {
        32 * MIB
    } else {
        input_bytes
    };
    FrameLimits {
        input_bytes,
        response_bytes,
    }
}

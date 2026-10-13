use super::{ContractError, ContractResult, raw_json, safe_integer, utf16_length};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

const MAX_DERIVATION_CHUNKS: usize = 40000;

fn original_text(value: &Value) -> Result<Vec<u16>, ContractError> {
    value
        .as_str()
        .and_then(raw_json::utf16_identity)
        .ok_or_else(|| invalid("Invalid source text encoding. / 原文编码无效。"))
}

fn original_hash(units: &[u16]) -> String {
    // Node UTF-8 encoding repairs isolated surrogates; retain that established hash identity.
    // Node 的 UTF-8 编码修复孤立代理项；哈希继续采用既有编码身份。
    format!(
        "{:x}",
        Sha256::digest(String::from_utf16_lossy(units).as_bytes())
    )
}

fn split_pair(units: &[u16], offset: usize) -> bool {
    offset > 0
        && offset < units.len()
        && (0xd800..=0xdbff).contains(&units[offset - 1])
        && (0xdc00..=0xdfff).contains(&units[offset])
}

/// One native call handles a bounded lexical batch, rather than one transport round trip per chunk.
/// 有界词法批次仅调用一次原生接口，避免每块单独跨语言往返。
pub fn lexical_batch(arguments: &Value) -> ContractResult {
    let texts = arguments["texts"]
        .as_array()
        .filter(|texts| texts.len() <= MAX_DERIVATION_CHUNKS)
        .ok_or_else(|| invalid("Invalid lexical batch. / 词法批次无效。"))?;
    let mut output = Vec::with_capacity(texts.len());
    for text in texts {
        super::sqlite_store::cancelled()?;
        let text = text
            .as_str()
            .ok_or_else(|| invalid("Invalid lexical text. / 词法正文无效。"))?;
        output.push(crate::retrieval::lexical::collect_terms(text, 20000, false).join(" "));
    }
    Ok(json!(output))
}

/// Validate all derived chunk metadata and calculate lexical/input hashes in one native pass.
/// 单次原生处理校验整批分块元信息并计算词法与嵌入输入哈希。
pub fn derive_chunks(arguments: &Value) -> ContractResult {
    let cancellation = arguments["cancellationId"]
        .as_str()
        .map(super::cancellation::CancellationLease::acquire)
        .transpose()
        .map_err(|code| {
            ContractError::new(
                "Invalid derivation cancellation. / 派生取消身份无效。",
                if code == "AGENT_CORE_SERVICE_CAPACITY" {
                    "AGENT_CORE_SERVICE_CAPACITY"
                } else {
                    "AGENT_CORE_INVALID_ARGUMENT"
                },
                503,
            )
        })?;
    let _context = cancellation
        .as_ref()
        .map(super::cancellation::CancellationLease::enter);
    super::sqlite_store::cancelled()?;
    let source = original_text(&arguments["sourceRaw"])?;
    let source_structure = if arguments["sourceStructureProvided"] != true {
        Value::Null
    } else {
        super::retrieval::apply(
            "storage.retrieval.structure",
            &json!({"value": arguments["sourceStructure"]}),
        )?
    };
    if !source_structure.is_null()
        && source_structure["parserVersion"] != arguments["parserVersion"]
    {
        return Err(ContractError::new(
            "Source parser versions differ. / 资料解析版本不一致。",
            "INVALID_RETRIEVAL_STRUCTURE",
            400,
        ));
    }
    let chunks = arguments["chunks"]
        .as_array()
        .filter(|chunks| chunks.len() <= MAX_DERIVATION_CHUNKS)
        .ok_or_else(|| invalid("Invalid derivation batch. / 派生批次无效。"))?;
    let mut output = Vec::with_capacity(chunks.len());
    let mut indexed_chunks = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        super::sqlite_store::cancelled()?;
        let body = original_text(&chunk["textRaw"])?;
        let lexical_input = original_text(&chunk["lexicalInputRaw"])?;
        let embedding_input = original_text(&chunk["embeddingInputRaw"])?;
        let lexical_text = crate::retrieval::lexical::collect_terms(
            &String::from_utf16_lossy(&lexical_input),
            20000,
            false,
        )
        .join(" ");
        let mut prepared = json!({
            "chunkHash": original_hash(&body),
            "lexicalText": lexical_text,
            "lexicalHash": format!("{:x}", Sha256::digest(lexical_text.as_bytes())),
            "inputHash": original_hash(&embedding_input)
        });
        if arguments.get("indexedValidation").is_some() {
            let start = chunk["startOffset"]
                .as_u64()
                .and_then(|offset| usize::try_from(offset).ok());
            let end = chunk["endOffset"]
                .as_u64()
                .and_then(|offset| usize::try_from(offset).ok());
            let mut indexed = json!({"chunkId": chunk["chunkId"], "chunkIndex": chunk["chunkIndex"],
                "startOffset": chunk["startOffset"], "endOffset": chunk["endOffset"],
                "startLine": chunk["startLine"], "endLine": chunk["endLine"]});
            indexed["matchesBytes"] = json!(start.zip(end).is_some_and(|(start, end)| {
                source
                    .get(start..end)
                    .is_some_and(|original| original == body)
                    && chunk["chunkHash"] == prepared["chunkHash"]
            }));
            indexed_chunks.push(indexed);
        }
        if let Some(structure) = chunk.get("structure") {
            let start = structure["unitStartOffset"].as_u64().unwrap_or(u64::MAX);
            let end = structure["unitEndOffset"].as_u64().unwrap_or(u64::MAX);
            prepared["structure"] = super::retrieval::apply(
                "storage.retrieval.chunkStructure",
                &json!({"value": structure, "sourceStructure": source_structure,
                    "textLength": source.len(), "chunk": chunk,
                    "splitsStart": usize::try_from(start).is_ok_and(|start| split_pair(&source, start)),
                    "splitsEnd": usize::try_from(end).is_ok_and(|end| split_pair(&source, end))}),
            )?;
        }
        if let Some(projection) = chunk.get("embeddingProjection") {
            let validated = crate::retrieval::token_fit::apply(
                "retrieval.fitted-text",
                &json!({"chunk": {"text": String::from_utf16_lossy(&body), "embeddingProjection": projection}}),
            )
            .map_err(|code| ContractError::new("Invalid token fitting projection. / token 投影无效。", if code == "INVALID_RETRIEVAL_TOKEN_FIT" { "INVALID_RETRIEVAL_TOKEN_FIT" } else { "INVALID_RETRIEVAL_INPUT" }, 400))?;
            let expected = validated
                .as_str()
                .unwrap()
                .encode_utf16()
                .collect::<Vec<_>>();
            if String::from_utf16_lossy(&embedding_input)
                .encode_utf16()
                .collect::<Vec<_>>()
                != expected
            {
                return Err(invalid(
                    "Embedding projection differs from its input. / 嵌入投影与输入不一致。",
                ));
            }
            prepared["embeddingProjection"] = crate::retrieval::token_fit::apply(
                "retrieval.token-projection",
                &json!({"value": projection}),
            )
            .map_err(|_| {
                ContractError::new(
                    "Invalid token fitting projection. / token 投影无效。",
                    "INVALID_RETRIEVAL_TOKEN_FIT",
                    400,
                )
            })?;
        }
        output.push(prepared);
    }
    let mut result = json!({"chunks": output, "structure": source_structure});
    if let Some(indexed) = arguments.get("indexedValidation") {
        if !indexed.is_object() {
            return Err(invalid("Invalid publication admission. / 发布准入无效。"));
        }
        let mut validation = indexed.clone();
        validation["sourceRaw"] = arguments["sourceRaw"].clone();
        validation["chunks"] = json!(indexed_chunks);
        result["dimensions"] = validate(&validation)?["dimensions"].clone();
    }
    Ok(result)
}

pub fn distances(arguments: &Value) -> ContractResult {
    let mut items = arguments["items"].as_array().cloned().unwrap_or_default();
    let mut invalid = arguments["invalid"].as_u64().unwrap_or(0);
    for item in arguments["incoming"].as_array().into_iter().flatten() {
        if item["distance"].as_f64().is_some_and(f64::is_finite) {
            items.push(item.clone());
        } else {
            invalid += 1;
        }
    }
    items.sort_by(|left, right| {
        left["distance"]
            .as_f64()
            .unwrap()
            .total_cmp(&right["distance"].as_f64().unwrap())
            .then_with(|| {
                left["chunk_id"]
                    .as_str()
                    .unwrap_or("")
                    .cmp(right["chunk_id"].as_str().unwrap_or(""))
            })
    });
    items.truncate(arguments["limit"].as_u64().unwrap_or(40).clamp(1, 160) as usize);
    Ok(json!({"items":items,"invalid":invalid}))
}

pub fn rank_vectors(arguments: &Value) -> ContractResult {
    let mut items = arguments["items"].as_array().cloned().unwrap_or_default();
    items.retain(|item| item["distance"].as_f64().is_some_and(f64::is_finite));
    items.sort_by(|left, right| {
        left["domainRank"]
            .as_f64()
            .unwrap_or(0.0)
            .total_cmp(&right["domainRank"].as_f64().unwrap_or(0.0))
            .then_with(|| {
                left["distance"]
                    .as_f64()
                    .unwrap()
                    .total_cmp(&right["distance"].as_f64().unwrap())
            })
            .then_with(|| {
                left["id"]
                    .as_i64()
                    .unwrap_or(0)
                    .cmp(&right["id"].as_i64().unwrap_or(0))
            })
    });
    items.truncate(arguments["limit"].as_u64().unwrap_or(40).clamp(1, 160) as usize);
    Ok(json!(items))
}

fn changed(message: &str) -> ContractError {
    ContractError::new(message, "RETRIEVAL_SOURCE_CHANGED", 409)
}
fn invalid(message: &str) -> ContractError {
    ContractError::new(message, "INVALID_RETRIEVAL_INPUT", 400)
}

/// Admission owns ordered chunk coverage and vector-space consistency; receipts retain the existing SDK codecs.
/// 准入拥有有序分块覆盖及向量空间一致性；回执保留既有 SDK 的字符串与 Float32 编码。
pub fn validate(arguments: &Value) -> ContractResult {
    let source = raw_json::utf16_identity(arguments["sourceRaw"].as_str().unwrap_or(""))
        .ok_or_else(|| invalid("资料正文编码无效。"))?;
    let chunks = arguments["chunks"]
        .as_array()
        .filter(|chunks| chunks.len() <= 40000)
        .ok_or_else(|| invalid("Invalid source chunks. / 资料分块无效。"))?;
    if chunks.is_empty() && arguments["hasText"] == true {
        return Err(changed("Source chunks are missing. / 资料正文缺少分块。"));
    }
    let mut ids = HashSet::new();
    let mut cursor = 0;
    let mut line = 1;
    let mut previous_end = 0;
    for (index, chunk) in chunks.iter().enumerate() {
        let id = chunk["chunkId"].as_str();
        if !chunk.is_object()
            || chunk["chunkIndex"].as_f64() != Some(index as f64)
            || id.is_none_or(|id| {
                id.is_empty()
                    || utf16_length(id) > 256
                    || id.chars().any(|character| character <= '\u{1f}')
            })
            || !ids.insert(id.unwrap_or_default().to_owned())
            || !safe_integer(&chunk["startOffset"], 0, source.len() as u64)
            || !safe_integer(&chunk["endOffset"], 0, source.len() as u64)
            || chunk["matchesBytes"] != true
        {
            return Err(changed(
                "Source chunks do not match the original. / 资料分块与原文不一致。",
            ));
        }
        let start = chunk["startOffset"].as_f64().unwrap() as usize;
        let end = chunk["endOffset"].as_f64().unwrap() as usize;
        if start < previous_end || end <= start {
            return Err(changed(
                "Source chunks do not match the original. / 资料分块与原文不一致。",
            ));
        }
        while cursor < start {
            if source[cursor] == 10 {
                line += 1;
            }
            cursor += 1;
        }
        if chunk["startLine"].as_f64() != Some(line as f64) {
            return Err(changed(
                "Chunk line range is invalid. / 分块行号与原文不一致。",
            ));
        }
        while cursor < end {
            if source[cursor] == 10 {
                line += 1;
            }
            cursor += 1;
        }
        let splits = |offset: usize| {
            offset > 0
                && offset < source.len()
                && (0xd800..=0xdbff).contains(&source[offset - 1])
                && (0xdc00..=0xdfff).contains(&source[offset])
        };
        if chunk["endLine"].as_f64() != Some(line as f64) || splits(start) || splits(end) {
            return Err(changed(
                "Chunk line or Unicode range is invalid. / 分块行号或 Unicode 边界无效。",
            ));
        }
        previous_end = end;
    }
    let vectors = arguments["vectors"]
        .as_array()
        .ok_or_else(|| invalid("Vector and chunk counts differ. / 向量数量与分块数量不一致。"))?;
    if arguments["vectorsArray"] != true || !vectors.is_empty() && vectors.len() != chunks.len() {
        return Err(invalid(
            "Vector and chunk counts differ. / 向量数量与分块数量不一致。",
        ));
    }
    let mut dimensions = None;
    for vector in vectors {
        if vector.is_null() {
            continue;
        }
        if vector["typed"] != true
            || !safe_integer(&vector["length"], 1, 4096)
            || vector["finite"] != true
            || vector["nonzero"] != true
        {
            return Err(invalid("Invalid embedding vector. / 嵌入向量无效。"));
        }
        if vector["floatFinite"] != true || vector["floatNonzero"] != true {
            return Err(invalid(
                "Invalid float32 embedding vector. / float32 嵌入向量无效。",
            ));
        }
        let current = vector["length"].as_u64().unwrap();
        if dimensions.is_some_and(|dimension| dimension != current) {
            return Err(invalid("Embedding dimensions differ. / 嵌入维度不一致。"));
        }
        dimensions = Some(current);
    }
    if (dimensions.is_some() || arguments["profileProvided"] == true)
        && !arguments["profile"].as_str().is_some_and(|value| {
            !super::trim(value).is_empty()
                && utf16_length(value) <= 256
                && !value.chars().any(|character| character <= '\u{1f}')
        })
    {
        return Err(invalid(
            "An embedding profile is required. / 向量必须关联嵌入模型配置。",
        ));
    }
    if arguments["modelProvided"] == true
        && !arguments["model"]
            .as_str()
            .is_some_and(|value| utf16_length(value) <= 512)
    {
        return Err(invalid(
            "Invalid embedding model version. / 嵌入模型版本无效。",
        ));
    }
    if arguments["spaceProvided"] == true && !super::hash(&arguments["space"]) {
        return Err(invalid("Invalid embedding space. / 嵌入空间身份无效。"));
    }
    Ok(json!({"dimensions": dimensions}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn publication() -> Value {
        let body_raw = "\"a\\ud83d\\ude00b\\ud800c\"";
        let body = raw_json::utf16_identity(body_raw).unwrap();
        json!({"sourceRaw": body_raw, "sourceStructureProvided": false,
            "parserVersion": "plain-text-v1",
            "chunks": [{"chunkId":"fixture:0", "chunkIndex":0,
                "chunkHash": original_hash(&body), "startOffset":0, "endOffset":6,
                "startLine":1, "endLine":1, "textRaw":body_raw,
                "lexicalInputRaw":body_raw, "embeddingInputRaw":body_raw}],
            "indexedValidation": {"hasText":true, "vectorsArray":true,
                "vectors":[{"typed":true, "length":2, "finite":true, "nonzero":true,
                    "floatFinite":true, "floatNonzero":true}],
                "profileProvided":true, "profile":"fixture", "modelProvided":false,
                "spaceProvided":false}})
    }

    #[test]
    fn batch_admission_preserves_original_utf16_and_rejects_changed_chunks() {
        let mut arguments = publication();
        let prepared = derive_chunks(&arguments).unwrap();
        assert_eq!(prepared["dimensions"], 2);
        assert_eq!(
            prepared["chunks"][0]["chunkHash"],
            arguments["chunks"][0]["chunkHash"]
        );
        arguments["chunks"][0]["textRaw"] = json!("\"a😀b replacement\"");
        assert_eq!(
            derive_chunks(&arguments).unwrap_err().code,
            "RETRIEVAL_SOURCE_CHANGED"
        );
    }

    #[test]
    fn batch_admission_reuses_the_existing_structure_and_projection_contracts() {
        let mut arguments = publication();
        arguments["chunks"][0]["embeddingProjection"] = json!({"context":"", "tokenCount":2,
            "maxInputTokens":512, "version":"fixture-tokenizer-v1"});
        assert!(derive_chunks(&arguments).is_ok());
        arguments["chunks"][0]["embeddingProjection"]["tokenCount"] = json!(513);
        assert_eq!(
            derive_chunks(&arguments).unwrap_err().code,
            "INVALID_RETRIEVAL_TOKEN_FIT"
        );
        arguments["chunks"][0]
            .as_object_mut()
            .unwrap()
            .remove("embeddingProjection");
        arguments["sourceStructureProvided"] = json!(true);
        arguments["sourceStructure"] = Value::Null;
        assert_eq!(
            derive_chunks(&arguments).unwrap_err().code,
            "INVALID_RETRIEVAL_INPUT"
        );
    }

    #[test]
    fn a_cancelled_derivation_never_returns_a_publication_admission() {
        let id = "test:publication-batch:cancelled";
        super::super::cancellation::retain(id).unwrap();
        super::super::cancellation::cancel(id).unwrap();
        let mut arguments = publication();
        arguments["cancellationId"] = json!(id);
        assert_eq!(derive_chunks(&arguments).unwrap_err().code, "ABORT_ERR");
        super::super::cancellation::release(id).unwrap();
    }
}
